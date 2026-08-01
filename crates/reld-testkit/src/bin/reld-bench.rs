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

use anyhow::{bail, Context, Result};
use clap::Parser;
use reld_testkit::{generate, WorkloadSpec};

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
                Err(_) => print!(" n/a |"),
            }
        }
        // reld does not exist yet. Publishing the column now, empty, keeps the chart's
        // shape stable and makes the gap visible rather than implied.
        println!(" n/a |");
    }

    println!();
    for (l, ok) in &available {
        if !ok {
            println!("<!-- linker {l} not available on this runner -->");
        }
    }
    Ok(())
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
