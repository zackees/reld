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
    vec!["bfd", "lld", "mold", "wild"]
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

    println!("## Link Benchmark: {}", target_triple());
    println!();
    print!("| Scenario |");
    for (l, _) in &available {
        print!(" {l} |");
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
            match time_link(&args, &dir, &objects, linker) {
                Ok(d) => print!(" {:.4} |", d.as_secs_f64()),
                Err(e) => {
                    failures.push((linker.clone(), name.clone(), e.to_string()));
                    print!(" n/a |");
                }
            }
        }
        match (&reld_shim_str, reld_ok) {
            (Some(linker), true) => match time_link(&args, &dir, &objects, linker) {
                Ok(d) => println!(" {:.4} |", d.as_secs_f64()),
                Err(e) => {
                    failures.push(("reld".to_string(), name.clone(), e.to_string()));
                    println!(" n/a |");
                }
            },
            _ => println!(" n/a |"),
        }
    }

    println!();
    for (l, ok) in &available {
        if !ok {
            println!("<!-- linker {l} not available on this runner -->");
        }
    }
    if let Some(reason) = reld_unavailable_reason {
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

fn link_once(args: &Args, objects: &[PathBuf], linker: &str, out: &Path) -> Result<()> {
    let mut cmd = Command::new(&args.cc);
    cmd.args(objects).arg("-o").arg(out);
    if !linker.is_empty() {
        cmd.arg(format!("-fuse-ld={linker}"));
    }
    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("spawning linker")?;
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
    if !linker.is_empty() {
        cmd.arg(format!("-fuse-ld={linker}"));
    }
    Ok(cmd.output().map(|o| o.status.success()).unwrap_or(false))
}

fn time_link(args: &Args, dir: &Path, objects: &[PathBuf], linker: &str) -> Result<Duration> {
    let out = dir.join(format!("bench-{linker}.bin"));

    for _ in 0..args.warmup {
        link_once(args, objects, linker, &out)?;
    }

    let mut samples = Vec::with_capacity(args.trials);
    for _ in 0..args.trials {
        // Remove the previous output so we never measure a linker short-circuiting on an
        // up-to-date target.
        let _ = std::fs::remove_file(&out);
        let t = Instant::now();
        link_once(args, objects, linker, &out)?;
        samples.push(t.elapsed());
    }
    samples.sort();
    Ok(samples[samples.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
