//! Link benchmark harness.
//!
//! Compiles a set of synthetic workloads once, then times *only the link step* across every
//! available linker. Compilation is excluded deliberately — a benchmark that measures
//! compile+link buries the thing being measured, which is how most linker comparisons end up
//! reporting noise.
//!
//! Emits markdown tables on stdout. That is the entire contract with `ci/benchmark_stats.py`,
//! which scrapes the tables and renders the chart.
//!
//! Linkers that aren't installed are reported as `n/a` rather than skipped silently, so a
//! missing competitor is visible in the published chart instead of quietly improving our
//! numbers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use reld_testkit::{WorkloadSpec, generate};

#[derive(Parser, Debug)]
#[command(
    name = "reld-bench",
    about = "Link-time benchmark against reference linkers"
)]
struct Args {
    /// C compiler used to produce objects and drive the link.
    #[arg(long, default_value = "clang")]
    cc: String,

    /// Timed trials per (scenario, linker). The median is reported.
    #[arg(long, default_value_t = 5)]
    trials: usize,

    /// Untimed warmup links before measurement, to page in the linker and the inputs.
    #[arg(long, default_value_t = 1)]
    warmup: usize,

    /// Restrict to these linkers (repeatable). Default: all known.
    #[arg(long = "linker")]
    linkers: Vec<String>,

    /// Working directory for generated workloads.
    #[arg(long)]
    workdir: Option<PathBuf>,

    /// Explicit path to the reld driver shim (e.g. `ld.reld`), produced by
    /// `ci/install-driver-shims.sh`. If omitted, the bench looks for `ld.reld` next to its own
    /// executable.
    #[arg(long)]
    reld: Option<PathBuf>,

    /// Label written in the benchmark heading (for example, a Rust target triple).
    #[arg(long)]
    target: Option<String>,

    /// Mark reld as *pending / unsupported-by-design* with this reason when it can't be measured
    /// on this platform (e.g. the bridge measurement hasn't landed yet on Windows/macOS). The
    /// reld cell then renders `pending` (carrying the reason as an HTML comment) instead of a bare
    /// `n/a`, so the chart and `latest.json` can tell a documented gap apart from a real failure
    /// (#63). Ignored when a working `ld.reld` shim is discovered — real data always wins.
    #[arg(long)]
    reld_pending: Option<String>,

    /// Replay a frozen link corpus instead of generating synthetic workloads: this directory
    /// must contain `corpus.json` plus the referenced object files (as produced by the
    /// benchmark-assets pipeline and fetched via `ci/benchmark_assets.py`). Times only the link
    /// step across every linker — zero compilation in the loop.
    #[arg(long)]
    replay_corpus: Option<PathBuf>,
}

/// Recipe for replaying a frozen link, read from `<corpus>/corpus.json`. Unknown fields are
/// ignored so the JSON can carry extra metadata for other consumers. Only the fields needed to
/// re-run the link are deserialized here.
#[derive(serde::Deserialize)]
struct Corpus {
    /// C compiler / link driver to invoke. Falls back to `--cc` when absent.
    #[serde(default)]
    cc: Option<String>,
    /// Object files (and other linker inputs) relative to the corpus directory.
    objects: Vec<String>,
    /// Extra arguments appended to the link (native libs, `-l…`, etc.).
    #[serde(default)]
    extra_link_args: Vec<String>,
    /// Configuration label for the table row (e.g. `quick` / `thin-lto` / `full-lto`).
    #[serde(default)]
    configuration: Option<String>,
    /// Optional process exit-code oracle for the linked corpus. When present, the benchmark
    /// executes each final output once after timing and requires this code. A corpus without an
    /// oracle still has its final artifact checked for existence and nonzero length.
    #[serde(default)]
    expected_exit_code: Option<i32>,
}

/// Workload sizes. Small is where incremental linking will eventually matter most; large is
/// where throughput differences become visible at all.
fn scenarios() -> Vec<(String, WorkloadSpec)> {
    vec![
        ("small (16 units)".into(), WorkloadSpec::small(0xBE)),
        (
            "medium (128 units)".into(),
            WorkloadSpec {
                seed: 0xBE,
                units: 128,
                symbols_per_unit: 32,
                comdat_fns: 16,
                ..Default::default()
            },
        ),
        ("large (512 units)".into(), WorkloadSpec::large(0xBE)),
    ]
}

/// `-fuse-ld=` values. `""` means the compiler default.
fn default_linkers() -> Vec<&'static str> {
    cfg_select! {
        target_os = "windows" => vec!["", "lld"],
        target_os = "macos" => vec!["", "ld64.lld"],
        target_os = "linux" => vec!["bfd", "lld", "mold", "wild"],
    }
}

fn display_linker(linker: &str) -> &str {
    if !linker.is_empty() {
        return linker;
    }
    cfg_select! {
        target_os = "windows" => "link.exe",
        target_os = "macos" => "ld",
        target_os = "linux" => "default",
    }
}

/// Search `PATH` for an executable named `name`, returning its absolute path.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|cand| cand.is_file())
}

/// Translate a linker token into the value handed to clang's `-fuse-ld=`.
///
/// clang maps `-fuse-ld=NAME` to a linker named `ld.NAME` (the GNU convention — `bfd` -> `ld.bfd`,
/// `gold` -> `ld.gold`), with `lld` the one special-cased name. So the bare token `ld64.lld`
/// resolves to a nonexistent `ld.ld64.lld` and the link fails (macOS showed `ld64.lld` as `n/a`).
/// Rewrite that token to the absolute path of `ld64.lld` on `PATH` — matching ci.yml's proven
/// macOS invocation and pinning the exact binary — falling back to `lld` (clang selects `ld64.lld`
/// for Mach-O). Every other token, including reld's absolute-path shim, passes through unchanged.
fn fuse_ld_value(token: &str) -> String {
    resolve_fuse_ld(token, which_on_path)
}

/// Select the `-fuse-ld` token clang can actually execute, plus an optional directory to prepend
/// to the child PATH. Windows clang treats an absolute `-fuse-ld=C:\...\reld-link.exe` as a
/// linker *name* and prefixes it with the target triple, producing a nonexistent executable.
/// A basename on PATH is the supported form there. Unix clang accepts absolute linker paths.
fn compiler_driver_linker(linker: &str, windows: bool) -> Result<(String, Option<PathBuf>)> {
    let path = Path::new(linker);
    if windows && path.is_absolute() {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("linker path has no UTF-8 file name: {}", path.display()))?;
        let parent = path
            .parent()
            .with_context(|| format!("linker path has no parent: {}", path.display()))?;
        return Ok((name.to_owned(), Some(parent.to_path_buf())));
    }
    Ok((fuse_ld_value(linker), None))
}

fn select_compiler_driver_linker(cmd: &mut Command, linker: &str) -> Result<()> {
    if linker.is_empty() {
        return Ok(());
    }
    let (value, search_dir) = compiler_driver_linker(linker, cfg!(windows))?;
    if let Some(search_dir) = search_dir {
        let mut paths = vec![search_dir];
        if let Some(current_path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&current_path));
        }
        let joined = std::env::join_paths(paths).context("prepending linker directory to PATH")?;
        cmd.env("PATH", joined);
    }
    cmd.arg(format!("-fuse-ld={value}"));
    Ok(())
}

/// Core of [`fuse_ld_value`] with the PATH lookup injected, so the mapping is unit-testable
/// without depending on what is actually installed on the test machine.
fn resolve_fuse_ld(token: &str, lookup: impl Fn(&str) -> Option<PathBuf>) -> String {
    if token == "ld64.lld" {
        if let Some(path) = lookup("ld64.lld") {
            return path.to_string_lossy().into_owned();
        }
        return "lld".to_string();
    }
    token.to_string()
}

/// Locate the `ld.reld` driver shim produced by `ci/install-driver-shims.sh`.
///
/// Resolution order:
/// 1. `explicit`, if given and it exists on disk.
/// 2. `ld.reld` (or `ld.reld.exe` on Windows) next to the running bench binary.
///
/// Returns `None` if neither location has a file, so the caller can report reld as `n/a`
/// honestly rather than pass a bogus path to `-fuse-ld=`.
fn discover_reld(explicit: &Option<PathBuf>) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    discover_reld_in(explicit, exe.as_deref().and_then(Path::parent))
}

/// Core of [`discover_reld`], with the "next to the bench binary" directory injected so tests can
/// exercise the resolution logic hermetically against a temp dir instead of the real executable
/// directory (which parallel tests would otherwise race on).
fn discover_reld_in(explicit: &Option<PathBuf>, sibling_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit
        && p.exists()
    {
        return Some(p.clone());
    }

    let dir = sibling_dir?;
    let name = if cfg!(windows) {
        "ld.reld.exe"
    } else {
        "ld.reld"
    };
    let candidate = dir.join(name);
    if candidate.exists() {
        return Some(candidate);
    }
    None
}

fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(dir) = args.replay_corpus.clone() {
        return run_replay(&args, &dir);
    }

    let root = match &args.workdir {
        Some(p) => p.clone(),
        None => std::env::temp_dir().join("reld-bench"),
    };
    std::fs::create_dir_all(&root)?;

    let requested: Vec<String> = if args.linkers.is_empty() {
        default_linkers().into_iter().map(String::from).collect()
    } else {
        args.linkers.clone()
    };

    // Probe once, up front, so the table header is stable and missing tools are explicit.
    let mut available = Vec::new();
    for l in &requested {
        available.push((l.clone(), probe_linker(&args.cc, l, &root).unwrap_or(false)));
    }

    // reld is measured through its `ld.reld` driver shim, invoked via `-fuse-ld=<abs-path>`.
    // The column's display label ("reld") is decoupled from the `-fuse-ld` value (the shim's
    // absolute path) so the table stays honest about what's actually being run.
    let reld_shim = discover_reld(&args.reld).and_then(|p| std::fs::canonicalize(&p).ok());
    let reld_shim_str = reld_shim.as_ref().map(|p| p.to_string_lossy().into_owned());
    let reld_unavailable_reason = if reld_shim_str.is_none() {
        Some("no ld.reld shim discovered (run ci/install-driver-shims.sh)")
    } else {
        None
    };
    let reld_ok = match &reld_shim_str {
        Some(linker) => probe_linker(&args.cc, linker, &root).unwrap_or(false),
        None => false,
    };
    let reld_unavailable_reason = if reld_shim_str.is_some() && !reld_ok {
        Some("probe link failed")
    } else {
        reld_unavailable_reason
    };

    let reld_measured = reld_shim_str.is_some() && reld_ok;
    // When reld can't be measured but the caller declared it pending-by-design, render `pending`
    // (carrying the reason) instead of a silent `n/a`. A working shim always wins over the flag.
    let reld_pending = if reld_measured {
        None
    } else {
        args.reld_pending.clone()
    };

    let target = args.target.clone().unwrap_or_else(target_triple);
    println!("## Link Benchmark: {target}");
    println!();
    print!("| Scenario |");
    for (l, _) in &available {
        print!(" {} |", display_linker(l));
    }
    println!(" reld |");
    print!("|:---------|");
    for _ in &available {
        print!("----:|");
    }
    println!("----:|");

    // (linker display name, scenario name, error) for every `time_link` failure, so the
    // linker's stderr isn't silently discarded behind the table's `n/a` cell — see #38.
    let mut failures: Vec<(String, String, String)> = Vec::new();

    for (name, spec) in scenarios() {
        let dir = root.join(name.split_whitespace().next().unwrap_or("s"));
        let _ = std::fs::remove_dir_all(&dir);
        let workload = generate(&spec, &dir)?;
        let objects = compile_all(&args, &dir, &workload.sources)?;

        print!("| {name} |");
        for (linker, ok) in &available {
            if !ok {
                print!(" n/a |");
                continue;
            }
            match time_link(&args, &dir, &objects, linker).and_then(|d| {
                validate_generated_output(
                    &bench_output_path(&dir, linker),
                    workload.expected_exit_code(),
                )?;
                Ok(d)
            }) {
                Ok(d) => print!(" {:.4} |", d.as_secs_f64()),
                Err(e) => {
                    failures.push((linker.clone(), name.clone(), format!("{e:#}")));
                    print!(" n/a |");
                }
            }
        }
        if reld_measured {
            let linker = reld_shim_str
                .as_ref()
                .expect("measured implies a shim path");
            match time_link(&args, &dir, &objects, linker).and_then(|d| {
                validate_generated_output(
                    &bench_output_path(&dir, linker),
                    workload.expected_exit_code(),
                )?;
                Ok(d)
            }) {
                Ok(d) => println!(" {:.4} |", d.as_secs_f64()),
                Err(e) => {
                    failures.push(("reld".to_string(), name.clone(), format!("{e:#}")));
                    println!(" n/a |");
                }
            }
        } else if reld_pending.is_some() {
            println!(" pending |");
        } else {
            println!(" n/a |");
        }
    }

    println!();
    for (l, ok) in &available {
        if !ok {
            println!(
                "<!-- linker {} not available on this runner -->",
                display_linker(l)
            );
        }
    }
    if let Some(reason) = &reld_pending {
        println!("{}", format_pending_comment("reld", reason));
    } else if let Some(reason) = reld_unavailable_reason {
        println!("<!-- linker reld not available: {reason} -->");
    }
    for (linker, scenario, error) in &failures {
        println!("{}", format_failure_comment(linker, scenario, error));
    }
    Ok(())
}

/// Max length (in characters) of the sanitized error text embedded in a failure comment, so a
/// huge linker stderr dump doesn't bloat the log.
const FAILURE_COMMENT_ERROR_LIMIT: usize = 500;

/// How many leading lines of the linker's error output to keep before sanitizing/truncating.
const FAILURE_COMMENT_ERROR_LINES: usize = 3;

/// Format a `time_link` failure as a single-line HTML comment, so the linker's stderr is visible
/// in the benchmark output instead of being silently discarded behind the table's `n/a` cell
/// (see #38). The result is always safe to emit as one `<!-- ... -->` line: it never contains a
/// literal `-->` sequence or an embedded newline, and the error text is capped in length.
fn format_failure_comment(linker: &str, scenario: &str, error: &str) -> String {
    let head: String = error
        .lines()
        .take(FAILURE_COMMENT_ERROR_LINES)
        .collect::<Vec<_>>()
        .join("; ");
    let collapsed = head.split_whitespace().collect::<Vec<_>>().join(" ");
    let neutralized = collapsed.replace("-->", "- >");
    let truncated: String = if neutralized.chars().count() > FAILURE_COMMENT_ERROR_LIMIT {
        let mut s: String = neutralized
            .chars()
            .take(FAILURE_COMMENT_ERROR_LIMIT)
            .collect();
        s.push_str("...");
        s
    } else {
        neutralized
    };
    format!("<!-- linker {linker} link failed ({scenario}): {truncated} -->")
}

/// Format reld's pending-by-design marker as a single-line HTML comment carrying the reason. Same
/// one-line / `-->`-safe / length-capped guarantees as [`format_failure_comment`], so it is always
/// safe to emit as one `<!-- ... -->` line.
fn format_pending_comment(linker: &str, reason: &str) -> String {
    let collapsed = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let neutralized = collapsed.replace("-->", "- >");
    let truncated: String = if neutralized.chars().count() > FAILURE_COMMENT_ERROR_LIMIT {
        let mut s: String = neutralized
            .chars()
            .take(FAILURE_COMMENT_ERROR_LIMIT)
            .collect();
        s.push_str("...");
        s
    } else {
        neutralized
    };
    format!("<!-- linker {linker} pending: {truncated} -->")
}

fn target_triple() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn compile_all(args: &Args, dir: &Path, sources: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut objects = Vec::with_capacity(sources.len());
    for src in sources {
        let obj = src.with_extension("o");
        let mut cmd = Command::new(&args.cc);
        cmd.arg("-c")
            .arg(src)
            .arg("-o")
            .arg(&obj)
            .arg("-I")
            .arg(dir)
            .arg("-O0");
        if !cfg!(windows) {
            cmd.arg("-fPIC");
        }
        let out = cmd
            .output()
            .with_context(|| format!("spawning {}", args.cc))?;
        if !out.status.success() {
            bail!("compile failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        objects.push(obj);
    }
    Ok(objects)
}

/// Response-file tokenization grammar used by the compiler driver on each platform. Keeping the
/// formatting independent of the host lets the Windows-specific path be regression-tested on
/// every developer machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseFileSyntax {
    Gnu,
    Windows,
}

fn compiler_driver_response_syntax(cc: &str) -> ResponseFileSyntax {
    let driver = cc
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(cc)
        .to_ascii_lowercase();
    let driver = driver.strip_suffix(".exe").unwrap_or(&driver);
    if matches!(driver, "clang-cl" | "cl") {
        ResponseFileSyntax::Windows
    } else {
        ResponseFileSyntax::Gnu
    }
}

/// Quote one response-file argument for clang's active driver-mode parser.
///
/// The `clang` driver uses GNU tokenization even on Windows, while `clang-cl` uses the Windows
/// CommandLineToArgvW rules. GNU response files treat every backslash as an escape; Windows only
/// gives special meaning to runs of backslashes immediately before a quote (or the closing quote).
/// Always quoting each argument makes whitespace, quotes, and non-ASCII paths unambiguous.
fn quote_response_file_argument(arg: &str, syntax: ResponseFileSyntax) -> String {
    match syntax {
        ResponseFileSyntax::Gnu => {
            let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        }
        ResponseFileSyntax::Windows => {
            let mut quoted = String::with_capacity(arg.len() + 2);
            quoted.push('"');
            let mut backslashes = 0;
            for ch in arg.chars() {
                if ch == '\\' {
                    backslashes += 1;
                    continue;
                }
                if ch == '"' {
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                } else {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                }
                backslashes = 0;
                quoted.push(ch);
            }
            // Backslashes immediately before the closing quote must be doubled.
            quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
            quoted.push('"');
            quoted
        }
    }
}

/// Produce UTF-8 contents for a clang compiler-driver response file. One quoted argument per
/// line also keeps the artifact inspectable when diagnosing a benchmark runner failure.
fn compiler_driver_response_file_contents(args: &[String], syntax: ResponseFileSyntax) -> String {
    let mut contents = args
        .iter()
        .map(|arg| quote_response_file_argument(arg, syntax))
        .collect::<Vec<_>>()
        .join("\n");
    contents.push('\n');
    contents
}

fn write_compiler_driver_response_file(path: &Path, args: &[String], cc: &str) -> Result<()> {
    let contents =
        compiler_driver_response_file_contents(args, compiler_driver_response_syntax(cc));
    std::fs::write(path, contents)
        .with_context(|| format!("writing compiler-driver response file {}", path.display()))
}

/// Add all linker inputs to `cmd`. Windows receives them through an `@response-file`, avoiding
/// the CreateProcess command-line limit for the large generated and frozen-corpus benchmarks.
/// Other platforms retain the existing direct-argument invocation.
enum PreparedLinkInputs<'a> {
    Direct {
        objects: &'a [PathBuf],
        extra: &'a [String],
    },
    ResponseFile(PathBuf),
}

/// Prepare the linker's input list once before the warmup/timed loop. In particular, response
/// file creation is deliberately excluded from benchmark timing.
fn prepare_link_inputs<'a>(
    objects: &'a [PathBuf],
    extra: &'a [String],
    response_file: &Path,
    cc: &str,
) -> Result<PreparedLinkInputs<'a>> {
    if cfg!(windows) {
        let mut inputs = Vec::with_capacity(objects.len() + extra.len());
        inputs.extend(
            objects
                .iter()
                .map(|object| response_file_path_argument(object))
                .collect::<Result<Vec<_>>>()?,
        );
        inputs.extend(extra.iter().cloned());
        write_compiler_driver_response_file(response_file, &inputs, cc)?;
        Ok(PreparedLinkInputs::ResponseFile(
            response_file.to_path_buf(),
        ))
    } else {
        Ok(PreparedLinkInputs::Direct { objects, extra })
    }
}

/// Convert a filesystem path into the UTF-8 response-file format used by clang. Never use a
/// lossy conversion here: changing one byte of an object path would turn a benchmark harness
/// failure into a misleading linker result.
fn response_file_path_argument(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).with_context(|| {
        format!(
            "cannot write non-UTF-8 linker input path to a compiler-driver response file: {}",
            path.display()
        )
    })
}

fn add_prepared_link_inputs(cmd: &mut Command, inputs: &PreparedLinkInputs<'_>) {
    match inputs {
        PreparedLinkInputs::Direct { objects, extra } => {
            cmd.args(*objects).args(*extra);
        }
        PreparedLinkInputs::ResponseFile(path) => {
            let mut response_arg = OsString::from("@");
            response_arg.push(path);
            cmd.arg(response_arg);
        }
    }
}

fn link_once(args: &Args, inputs: &PreparedLinkInputs<'_>, linker: &str, out: &Path) -> Result<()> {
    let mut cmd = Command::new(&args.cc);
    add_prepared_link_inputs(&mut cmd, inputs);
    cmd.arg("-o").arg(out);
    select_compiler_driver_linker(&mut cmd, linker)?;
    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawning linker via {}", args.cc))?;
    if !status.status.success() {
        bail!("link failed: {}", String::from_utf8_lossy(&status.stderr));
    }
    Ok(())
}

/// Cheap availability check: link a one-object program.
fn probe_linker(cc: &str, linker: &str, root: &Path) -> Result<bool> {
    let dir = root.join("probe");
    std::fs::create_dir_all(&dir)?;
    let src = dir.join("p.c");
    std::fs::write(&src, "int main(void){return 0;}")?;
    let obj = dir.join("p.o");
    let ok = Command::new(cc)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return Ok(false);
    }
    let exe = dir.join(if cfg!(windows) { "p.exe" } else { "p.out" });
    let mut cmd = Command::new(cc);
    cmd.arg(&obj).arg("-o").arg(&exe);
    select_compiler_driver_linker(&mut cmd, linker)?;
    Ok(cmd.output().map(|o| o.status.success()).unwrap_or(false))
}

/// Build a filesystem-safe output filename for a linker's benchmark artifact. `linker` is the
/// `-fuse-ld=` value, which for reld is the **absolute path** to the `ld.reld` shim; embedding it
/// raw would make `dir.join` interpret the path separators as nested directories that don't
/// exist, so reld (correctly) fails to open its output. Flatten separators so the artifact lands
/// directly in `dir`.
fn bench_output_name(linker: &str) -> String {
    let safe: String = linker
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!("bench-{safe}.bin")
}

fn bench_output_path(dir: &Path, linker: &str) -> PathBuf {
    dir.join(bench_output_name(linker))
}

/// Confirm that a link left behind a real output artifact before attempting to execute it. This
/// makes a successful process spawn impossible to mistake for a usable benchmark result.
fn validate_nonempty_output_artifact(out: &Path) -> Result<()> {
    let metadata = std::fs::metadata(out)
        .with_context(|| format!("benchmark output artifact is missing: {}", out.display()))?;
    if !metadata.is_file() {
        bail!(
            "benchmark output artifact is not a regular file: {}",
            out.display()
        );
    }
    if metadata.len() == 0 {
        bail!("benchmark output artifact is empty: {}", out.display());
    }
    Ok(())
}

/// Check a process result against an exit-code oracle. Kept separate from process execution so
/// corpus-validation behavior is unit-testable without a compiler or generated binary.
fn validate_output_exit_code(actual: Option<i32>, expected: i32) -> Result<()> {
    match actual {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => bail!("unexpected exit code: expected {expected}, got {actual}"),
        None => bail!("output did not exit normally; expected exit code {expected}"),
    }
}

/// Run a final linked program after timing and verify its exit-code oracle. Execution is never
/// part of the timed interval: the caller invokes this only after [`time_link`] succeeds.
fn run_and_validate_output(out: &Path, expected_exit_code: i32) -> Result<()> {
    validate_nonempty_output_artifact(out)?;
    let run = Command::new(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running benchmark output {}", out.display()))?;
    validate_output_exit_code(run.status.code(), expected_exit_code)
        .with_context(|| format!("benchmark output {} failed validation", out.display()))
}

/// Validate a generated workload's final output after timing. Every generated workload has a
/// deterministic oracle, unlike arbitrary frozen corpora.
fn validate_generated_output(out: &Path, expected_exit_code: i32) -> Result<()> {
    run_and_validate_output(out, expected_exit_code)
}

/// Validate a frozen corpus output after timing. A corpus may opt into an execution oracle; old
/// corpora that omit it are still rejected if the linker failed to create a nonempty artifact.
fn validate_replay_output(out: &Path, expected_exit_code: Option<i32>) -> Result<()> {
    match expected_exit_code {
        Some(expected) => run_and_validate_output(out, expected),
        None => validate_nonempty_output_artifact(out).context(
            "corpus.json has no expected_exit_code, so replay validation can only require a \
             nonempty output artifact",
        ),
    }
}

fn time_link(args: &Args, dir: &Path, objects: &[PathBuf], linker: &str) -> Result<Duration> {
    let out = bench_output_path(dir, linker);
    let inputs = prepare_link_inputs(objects, &[], &out.with_extension("rsp"), &args.cc)?;

    for _ in 0..args.warmup {
        link_once(args, &inputs, linker, &out)?;
    }

    let mut samples = Vec::with_capacity(args.trials);
    for _ in 0..args.trials {
        // Remove the previous output so we never measure a linker short-circuiting on an
        // up-to-date target.
        let _ = std::fs::remove_file(&out);
        let t = Instant::now();
        link_once(args, &inputs, linker, &out)?;
        samples.push(t.elapsed());
    }
    samples.sort();
    Ok(samples[samples.len() / 2])
}

/// Link a frozen corpus once with `cc`, appending `extra` (native libs etc.) and selecting
/// `linker` via `-fuse-ld=`. Mirrors [`link_once`] but takes the compiler and extra args
/// explicitly so the corpus can carry its own toolchain, independent of `--cc`.
fn link_once_replay(
    cc: &str,
    inputs: &PreparedLinkInputs<'_>,
    linker: &str,
    out: &Path,
) -> Result<()> {
    let mut cmd = Command::new(cc);
    add_prepared_link_inputs(&mut cmd, inputs);
    cmd.arg("-o").arg(out);
    select_compiler_driver_linker(&mut cmd, linker)?;
    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawning linker via {cc}"))?;
    if !status.status.success() {
        bail!("link failed: {}", String::from_utf8_lossy(&status.stderr));
    }
    Ok(())
}

/// Warmup + timed median link of a frozen corpus. Compilation never happens here — the objects
/// are already on disk — so this measures only the link step.
fn time_link_replay(
    cc: &str,
    objects: &[PathBuf],
    extra: &[String],
    linker: &str,
    out_dir: &Path,
    warmup: usize,
    trials: usize,
) -> Result<Duration> {
    let out = bench_output_path(out_dir, linker);
    let inputs = prepare_link_inputs(objects, extra, &out.with_extension("rsp"), cc)?;
    for _ in 0..warmup {
        link_once_replay(cc, &inputs, linker, &out)?;
    }
    let mut samples = Vec::with_capacity(trials);
    for _ in 0..trials {
        let _ = std::fs::remove_file(&out);
        let t = Instant::now();
        link_once_replay(cc, &inputs, linker, &out)?;
        samples.push(t.elapsed());
    }
    samples.sort();
    Ok(samples[samples.len() / 2])
}

/// Replay a frozen link corpus: read `<dir>/corpus.json`, then time the link of its objects
/// across every available linker (and reld's shim), emitting the same markdown table as the
/// synthetic path. No compilation happens — only linking is measured.
fn run_replay(args: &Args, corpus_dir: &Path) -> Result<()> {
    let recipe = corpus_dir.join("corpus.json");
    let text = std::fs::read_to_string(&recipe)
        .with_context(|| format!("reading {}", recipe.display()))?;
    let corpus: Corpus =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", recipe.display()))?;

    let cc = corpus.cc.clone().unwrap_or_else(|| args.cc.clone());
    let objects: Vec<PathBuf> = corpus.objects.iter().map(|o| corpus_dir.join(o)).collect();
    for obj in &objects {
        if !obj.exists() {
            bail!("corpus object missing: {}", obj.display());
        }
    }

    let root = args
        .workdir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("reld-bench-replay"));
    std::fs::create_dir_all(&root)?;

    let requested: Vec<String> = if args.linkers.is_empty() {
        default_linkers().into_iter().map(String::from).collect()
    } else {
        args.linkers.clone()
    };
    let mut available = Vec::new();
    for l in &requested {
        available.push((l.clone(), probe_linker(&cc, l, &root).unwrap_or(false)));
    }

    let reld_shim = discover_reld(&args.reld).and_then(|p| std::fs::canonicalize(&p).ok());
    let reld_shim_str = reld_shim.as_ref().map(|p| p.to_string_lossy().into_owned());
    let reld_ok = match &reld_shim_str {
        Some(linker) => probe_linker(&cc, linker, &root).unwrap_or(false),
        None => false,
    };
    let reld_measured = reld_shim_str.is_some() && reld_ok;
    let reld_pending = if reld_measured {
        None
    } else {
        args.reld_pending.clone()
    };

    let target = args.target.clone().unwrap_or_else(target_triple);
    let scenario = corpus
        .configuration
        .clone()
        .unwrap_or_else(|| "corpus".to_string());
    println!("## Link Benchmark: {target}");
    println!();
    print!("| Scenario |");
    for (l, _) in &available {
        print!(" {} |", display_linker(l));
    }
    println!(" reld |");
    print!("|:---------|");
    for _ in &available {
        print!("----:|");
    }
    println!("----:|");

    let mut failures: Vec<(String, String, String)> = Vec::new();
    print!("| {scenario} |");
    for (linker, ok) in &available {
        if !ok {
            print!(" n/a |");
            continue;
        }
        match time_link_replay(
            &cc,
            &objects,
            &corpus.extra_link_args,
            linker,
            &root,
            args.warmup,
            args.trials,
        )
        .and_then(|d| {
            validate_replay_output(&bench_output_path(&root, linker), corpus.expected_exit_code)?;
            Ok(d)
        }) {
            Ok(d) => print!(" {:.4} |", d.as_secs_f64()),
            Err(e) => {
                failures.push((linker.clone(), scenario.clone(), format!("{e:#}")));
                print!(" n/a |");
            }
        }
    }
    if reld_measured {
        let linker = reld_shim_str
            .as_ref()
            .expect("measured implies a shim path");
        match time_link_replay(
            &cc,
            &objects,
            &corpus.extra_link_args,
            linker,
            &root,
            args.warmup,
            args.trials,
        )
        .and_then(|d| {
            validate_replay_output(&bench_output_path(&root, linker), corpus.expected_exit_code)?;
            Ok(d)
        }) {
            Ok(d) => println!(" {:.4} |", d.as_secs_f64()),
            Err(e) => {
                failures.push(("reld".to_string(), scenario.clone(), format!("{e:#}")));
                println!(" n/a |");
            }
        }
    } else if reld_pending.is_some() {
        println!(" pending |");
    } else {
        println!(" n/a |");
    }
    println!();
    if let Some(reason) = &reld_pending {
        println!("{}", format_pending_comment("reld", reason));
    }
    for (linker, scenario, error) in &failures {
        println!("{}", format_failure_comment(linker, scenario, error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_corpus_json_recipe() {
        let json = r#"{
            "schema_version": 1,
            "platform": "x86_64-linux",
            "configuration": "quick",
            "cc": "clang",
            "objects": ["objs/a.o", "objs/b.o"],
            "extra_link_args": ["-lm"],
            "expected_exit_code": 17,
            "output_name": "app"
        }"#;
        let corpus: Corpus = serde_json::from_str(json).unwrap();
        assert_eq!(corpus.cc.as_deref(), Some("clang"));
        assert_eq!(
            corpus.objects,
            vec!["objs/a.o".to_string(), "objs/b.o".to_string()]
        );
        assert_eq!(corpus.extra_link_args, vec!["-lm".to_string()]);
        assert_eq!(corpus.configuration.as_deref(), Some("quick"));
        assert_eq!(corpus.expected_exit_code, Some(17));
    }

    #[test]
    fn ld64_lld_token_maps_to_abs_path_when_found() {
        // The bug: bare `ld64.lld` makes clang look for `ld.ld64.lld`. The fix rewrites it to
        // the resolved absolute path of ld64.lld on PATH.
        let resolved = resolve_fuse_ld("ld64.lld", |name| {
            assert_eq!(name, "ld64.lld");
            Some(PathBuf::from("/opt/homebrew/opt/lld/bin/ld64.lld"))
        });
        assert_eq!(resolved, "/opt/homebrew/opt/lld/bin/ld64.lld");
    }

    #[test]
    fn ld64_lld_token_falls_back_to_lld_when_not_found() {
        let resolved = resolve_fuse_ld("ld64.lld", |_| None);
        assert_eq!(resolved, "lld");
    }

    #[test]
    fn expected_linker_tokens_never_resolve_to_a_bogus_ld_name() {
        // Regression guard for #60/#63 across the full expected matrix: no expected linker may
        // resolve to a bare `ld64.lld`, which clang would rewrite to a nonexistent `ld.ld64.lld`
        // and link silently to n/a. Reference linkers pass through so clang forms the real
        // `ld.bfd` / `ld.lld` / `ld.mold` / `ld.wild`; `ld64.lld` must be rewritten.
        for token in ["bfd", "lld", "mold", "wild"] {
            assert_eq!(
                resolve_fuse_ld(token, |_| panic!("reference tokens need no PATH lookup")),
                token,
                "{token} must pass through so clang forms ld.{token}"
            );
        }
        assert_eq!(
            resolve_fuse_ld("ld64.lld", |_| Some(PathBuf::from("/usr/bin/ld64.lld"))),
            "/usr/bin/ld64.lld"
        );
        let fallback = resolve_fuse_ld("ld64.lld", |_| None);
        assert_ne!(
            fallback, "ld64.lld",
            "must never stay a bare ld64.lld token"
        );
        assert_eq!(fallback, "lld");
    }

    #[test]
    fn windows_absolute_custom_linker_uses_basename_and_parent_path() {
        let absolute = if cfg!(windows) {
            r"C:\bench tools\reld-link.exe"
        } else {
            "/bench tools/reld-link.exe"
        };
        let (value, search_dir) = compiler_driver_linker(absolute, true).unwrap();
        assert_eq!(value, "reld-link.exe");
        assert_eq!(search_dir.unwrap(), Path::new(absolute).parent().unwrap());
    }

    #[test]
    fn unix_absolute_custom_linker_remains_an_absolute_fuse_ld_value() {
        let absolute = "/opt/reld drivers/ld64.reld";
        let (value, search_dir) = compiler_driver_linker(absolute, false).unwrap();
        assert_eq!(value, absolute);
        assert!(search_dir.is_none());
    }

    #[test]
    fn format_pending_comment_is_single_line_and_safe() {
        // The reason is caller-supplied, so it gets the same one-line / `-->`-safe treatment as a
        // failure comment: no embedded newline, exactly one `-->` (the closing delimiter).
        let comment =
            format_pending_comment("reld", "bridge measurement pending\nsee --> issue #17");
        assert!(
            comment.starts_with("<!-- linker reld pending: "),
            "{comment}"
        );
        assert!(comment.ends_with("-->"));
        assert_eq!(comment.matches("-->").count(), 1, "{comment}");
        assert!(!comment.contains('\n'), "{comment}");
        assert!(comment.contains("bridge measurement pending"));
        assert!(comment.contains("issue #17"));
    }

    #[test]
    fn other_linker_tokens_pass_through_unchanged() {
        // Reference linkers and reld's absolute-path shim must be untouched.
        for token in ["bfd", "lld", "mold", "wild", "/abs/path/to/ld.reld"] {
            assert_eq!(
                resolve_fuse_ld(token, |_| panic!("lookup must not run")),
                token
            );
        }
    }

    #[test]
    fn corpus_json_defaults_are_lenient() {
        // Only `objects` is required; cc/extra/configuration default; unknown fields ignored.
        let corpus: Corpus =
            serde_json::from_str(r#"{"objects":["a.o"],"unknown":"ignored"}"#).unwrap();
        assert!(corpus.cc.is_none());
        assert!(corpus.extra_link_args.is_empty());
        assert!(corpus.configuration.is_none());
        assert!(corpus.expected_exit_code.is_none());
        assert_eq!(corpus.objects, vec!["a.o".to_string()]);
    }

    #[test]
    fn output_exit_code_validation_reports_wrong_and_abnormal_exits() {
        validate_output_exit_code(Some(16), 16).unwrap();

        let wrong = validate_output_exit_code(Some(15), 16)
            .unwrap_err()
            .to_string();
        assert!(wrong.contains("expected 16, got 15"), "{wrong}");

        let abnormal = validate_output_exit_code(None, 16).unwrap_err().to_string();
        assert!(abnormal.contains("did not exit normally"), "{abnormal}");
    }

    #[test]
    fn replay_validation_without_oracle_requires_nonempty_artifact() {
        let dir = unique_tmp_dir("replay-output-validation");
        let empty = dir.join("empty-output");
        std::fs::write(&empty, []).unwrap();

        let error = validate_replay_output(&empty, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no expected_exit_code"), "{error}");
        assert!(error.contains("empty"), "{error}");

        let nonempty = dir.join("nonempty-output");
        std::fs::write(&nonempty, [0_u8]).unwrap();
        validate_replay_output(&nonempty, None).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn response_file_path_argument_preserves_unicode_paths() {
        let path = PathBuf::from("objects/日本語/🦀.o");
        assert_eq!(
            response_file_path_argument(&path).unwrap(),
            "objects/日本語/🦀.o"
        );
    }

    #[cfg(unix)]
    #[test]
    fn response_file_path_argument_rejects_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'o', b'.', 0xFF]));
        let error = response_file_path_argument(&path).unwrap_err().to_string();
        assert!(error.contains("non-UTF-8"), "{error}");
    }

    /// Unique temp directory per test, so the (hermetic) discovery tests never touch the real
    /// executable directory or race each other.
    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "reld-bench-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn shim_name() -> &'static str {
        if cfg!(windows) {
            "ld.reld.exe"
        } else {
            "ld.reld"
        }
    }

    #[test]
    fn discover_reld_returns_explicit_path_when_it_exists() {
        let dir = unique_tmp_dir("explicit");
        let shim = dir.join("ld.reld");
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();

        // Explicit takes precedence even when a sibling dir would also match.
        let found = discover_reld_in(&Some(shim.clone()), Some(&dir));
        assert_eq!(found, Some(shim));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reld_returns_none_for_missing_explicit_path_and_empty_sibling() {
        let dir = unique_tmp_dir("missing");
        let missing = dir.join("ld.reld"); // never created
        // Explicit does not exist and the sibling dir has no shim → None.
        let found = discover_reld_in(&Some(missing), Some(&dir));
        assert_eq!(found, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reld_falls_back_to_none_without_explicit_or_sibling_shim() {
        let dir = unique_tmp_dir("empty");
        let found = discover_reld_in(&None, Some(&dir));
        assert_eq!(found, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reld_finds_sibling_shim() {
        let dir = unique_tmp_dir("sibling");
        let shim = dir.join(shim_name());
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();

        let found = discover_reld_in(&None, Some(&dir));
        assert_eq!(found, Some(shim));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reld_returns_none_when_no_sibling_dir() {
        // `current_exe()` failing (sibling dir = None) must degrade to None, not panic.
        assert_eq!(discover_reld_in(&None, None), None);
    }

    #[test]
    fn format_failure_comment_neutralizes_collapses_and_caps() {
        // Embedded `-->` must not terminate the comment early, newlines must collapse so the
        // whole thing stays one line, and a huge dump must be capped in length.
        let huge_line = "x".repeat(1000);
        let error =
            format!("first line has --> in it\nsecond line\n{huge_line}\nfourth line dropped");
        let comment = format_failure_comment("reld", "large (512 units)", &error);

        assert_eq!(
            comment.matches("-->").count(),
            1,
            "the only `-->` in the comment must be the closing delimiter: {comment}"
        );
        assert!(comment.ends_with("-->"));
        assert!(!comment.contains('\n'), "must be a single line: {comment}");
        assert!(comment.contains("reld"));
        assert!(comment.contains("large (512 units)"));
        assert!(comment.starts_with("<!-- linker reld link failed (large (512 units)): "));
        assert!(
            comment.len() < error.len(),
            "must be capped shorter than the raw error: {comment}"
        );
        // Only the first FAILURE_COMMENT_ERROR_LINES lines are considered, so the dropped
        // fourth line's marker text must not appear.
        assert!(!comment.contains("fourth line dropped"));
    }

    #[test]
    fn format_failure_comment_passes_through_short_single_line_errors() {
        let comment = format_failure_comment("bfd", "small (16 units)", "undefined symbol: foo");
        assert_eq!(
            comment,
            "<!-- linker bfd link failed (small (16 units)): undefined symbol: foo -->"
        );
    }

    #[test]
    fn failure_comments_keep_the_full_anyhow_cause_chain() {
        let error = anyhow::anyhow!("The filename or extension is too long (os error 206)")
            .context("spawning linker via clang");
        let comment = format_failure_comment("lld", "large (512 units)", &format!("{error:#}"));
        assert!(comment.contains("spawning linker via clang"), "{comment}");
        assert!(comment.contains("os error 206"), "{comment}");
    }

    #[test]
    fn gnu_response_file_escapes_spaces_quotes_backslashes_and_unicode() {
        let args = vec![
            "plain.o".to_string(),
            "dir with spaces/object file.o".to_string(),
            "quote\"name.o".to_string(),
            r"C:\bench\object.o".to_string(),
            "日本語/🦀.o".to_string(),
        ];
        assert_eq!(
            compiler_driver_response_file_contents(&args, ResponseFileSyntax::Gnu),
            concat!(
                "\"plain.o\"\n",
                "\"dir with spaces/object file.o\"\n",
                "\"quote\\\"name.o\"\n",
                "\"C:\\\\bench\\\\object.o\"\n",
                "\"日本語/🦀.o\"\n",
            )
        );
    }

    #[test]
    fn windows_response_file_uses_windows_quote_rules() {
        let args = vec![
            "dir with spaces/object file.o".to_string(),
            "quote\"name.o".to_string(),
            r"C:\bench\object.o".to_string(),
            r"C:\trailing slash\\".to_string(),
            "日本語/🦀.o".to_string(),
        ];
        assert_eq!(
            compiler_driver_response_file_contents(&args, ResponseFileSyntax::Windows),
            concat!(
                "\"dir with spaces/object file.o\"\n",
                "\"quote\\\"name.o\"\n",
                "\"C:\\bench\\object.o\"\n",
                "\"C:\\trailing slash\\\\\\\\\"\n",
                "\"日本語/🦀.o\"\n",
            )
        );
    }

    #[test]
    fn response_file_syntax_follows_clang_driver_mode_not_host_os() {
        assert_eq!(
            compiler_driver_response_syntax("clang.exe"),
            ResponseFileSyntax::Gnu
        );
        assert_eq!(
            compiler_driver_response_syntax(r"C:\Program Files\LLVM\bin\clang.exe"),
            ResponseFileSyntax::Gnu
        );
        assert_eq!(
            compiler_driver_response_syntax("clang-cl.exe"),
            ResponseFileSyntax::Windows
        );
        assert_eq!(
            compiler_driver_response_syntax(r"C:\Program Files\LLVM\bin\clang-cl.exe"),
            ResponseFileSyntax::Windows
        );
        assert_eq!(
            compiler_driver_response_syntax(r"C:\LLVM\CLANG-CL.EXE"),
            ResponseFileSyntax::Windows
        );
    }

    #[test]
    fn prepared_link_inputs_writes_windows_response_file_before_linking() {
        let dir = unique_tmp_dir("prepared-inputs");
        let objects = vec![
            dir.join("objects with spaces").join("日本語.o"),
            dir.join("quote\"name.o"),
        ];
        let extra = vec!["-lcustom library".to_string()];
        let response_file = dir.join("bench.rsp");
        let prepared = prepare_link_inputs(&objects, &extra, &response_file, "clang.exe").unwrap();

        if cfg!(windows) {
            assert_eq!(
                std::fs::read_to_string(&response_file).unwrap(),
                compiler_driver_response_file_contents(
                    &[
                        objects[0].to_string_lossy().into_owned(),
                        objects[1].to_string_lossy().into_owned(),
                        extra[0].clone(),
                    ],
                    ResponseFileSyntax::Gnu,
                )
            );
            assert!(matches!(&prepared, PreparedLinkInputs::ResponseFile(_)));
        } else {
            assert!(matches!(&prepared, PreparedLinkInputs::Direct { .. }));
            assert!(!response_file.exists());
        }

        drop(prepared);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bench_output_name_flattens_path_separators() {
        // A short name is untouched...
        assert_eq!(bench_output_name("lld"), "bench-lld.bin");
        // ...but an absolute path (the reld shim) must not embed separators, or `dir.join` would
        // create nonexistent nested directories and the link would fail to open its output.
        let name = bench_output_name("/home/runner/reld/target/release/ld.reld");
        assert_eq!(name, "bench-_home_runner_reld_target_release_ld.reld.bin");
        assert!(
            !name.contains('/'),
            "no separators in the flattened name: {name}"
        );
        assert_eq!(
            bench_output_name(r"C:\tc\ld.reld"),
            "bench-C__tc_ld.reld.bin"
        );
        assert_eq!(
            bench_output_name(r"\\?\C:\tc\reld-link.exe"),
            "bench-____C__tc_reld-link.exe.bin"
        );
    }
}
