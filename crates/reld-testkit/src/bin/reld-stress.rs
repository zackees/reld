//! End-to-end link stress harness.
//!
//! For each seed: generate a random workload, compile it, link it with the linker under test,
//! run the result, and check the exit code.
//!
//! The exit-code check is the point. A harness that only looks for crashes misses the failure
//! mode that actually matters in a linker — a binary that links "successfully" and then
//! behaves wrong because a symbol resolved to the wrong address.
//!
//! Failures are reported as a seed. One integer reproduces the entire input set.
//!
//! ```text
//! reld-stress --seeds 200 --linker lld
//! reld-stress --seed 1234 --keep      # reproduce one failure, keep the tree
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Parser;
use reld_testkit::{generate, WorkloadSpec};

#[derive(Parser, Debug)]
#[command(name = "reld-stress", about = "Randomized end-to-end link stress testing")]
struct Args {
    /// Run seeds 0..N.
    #[arg(long, default_value_t = 50, conflicts_with = "seed")]
    seeds: u64,

    /// Run exactly one seed (for reproducing a reported failure).
    #[arg(long)]
    seed: Option<u64>,

    /// C compiler used to produce objects.
    ///
    /// clang is the default rather than cc/gcc: it exists on all three target platforms,
    /// drives lld natively via `-fuse-ld=lld`, and keeps the harness comparable across
    /// Linux, Windows, and macOS runners.
    #[arg(long, default_value = "clang")]
    cc: String,

    /// Linker to select via `-fuse-ld=`. Omit to use the compiler default.
    #[arg(long)]
    linker: Option<String>,

    /// Extra flags passed through to the link step (repeatable).
    #[arg(long = "link-arg")]
    link_args: Vec<String>,

    /// Keep the working tree even when a seed passes.
    #[arg(long)]
    keep: bool,

    /// Stop at the first failing seed.
    #[arg(long)]
    fail_fast: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let seeds: Vec<u64> = match args.seed {
        Some(s) => vec![s],
        None => (0..args.seeds).collect(),
    };

    let root = std::env::temp_dir().join("reld-stress");
    std::fs::create_dir_all(&root)?;

    let mut failures = Vec::new();
    for seed in &seeds {
        let dir = root.join(format!("seed-{seed:08}"));
        let _ = std::fs::remove_dir_all(&dir);

        match run_seed(&args, *seed, &dir) {
            Ok(()) => {
                if !args.keep {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                println!("seed {seed:>8}  ok");
            }
            Err(e) => {
                // Never clean up a failure — the tree is the evidence.
                println!("seed {seed:>8}  FAIL  {e:#}");
                println!("                   tree kept at {}", dir.display());
                failures.push(*seed);
                if args.fail_fast {
                    break;
                }
            }
        }
    }

    println!("\n{} seeds, {} failures", seeds.len(), failures.len());
    if !failures.is_empty() {
        println!("reproduce with:");
        for f in &failures {
            println!("  reld-stress --seed {f} --keep");
        }
        bail!("{} seed(s) failed", failures.len());
    }
    Ok(())
}

fn run_seed(args: &Args, seed: u64, dir: &Path) -> Result<()> {
    let spec = WorkloadSpec::random(seed);
    let workload = generate(&spec, dir).context("generating workload")?;

    // Compile each translation unit separately. Separate objects are the whole point —
    // a single-TU link exercises almost nothing a linker does.
    let mut objects = Vec::with_capacity(workload.sources.len());
    for src in &workload.sources {
        let obj = src.with_extension("o");
        let mut cmd = Command::new(&args.cc);
        cmd.arg("-c").arg(src).arg("-o").arg(&obj).arg("-I").arg(dir).arg("-O0");
        // PIC is the default and the flag is rejected outright on Windows targets.
        if !cfg!(windows) {
            cmd.arg("-fPIC");
        }
        let out = cmd
            .output()
            .with_context(|| format!("spawning {} (is it on PATH?)", args.cc))?;
        if !out.status.success() {
            bail!(
                "compile failed for {}:\n{}",
                src.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        objects.push(obj);
    }

    let exe = link(args, dir, &objects)?;

    let run = Command::new(&exe)
        .output()
        .with_context(|| format!("running {}", exe.display()))?;

    let got = run.status.code();
    let want = workload.expected_exit_code();
    if got != Some(want) {
        bail!(
            "wrong exit code: want {want}, got {got:?} (spec: {} units, {} syms/unit, \
             ref_density {:.2}, weak {:.2}, tls {:.2}, comdat {})",
            spec.units,
            spec.symbols_per_unit,
            spec.ref_density,
            spec.weak_ratio,
            spec.tls_ratio,
            spec.comdat_fns
        );
    }
    Ok(())
}

fn link(args: &Args, dir: &Path, objects: &[PathBuf]) -> Result<PathBuf> {
    let exe = dir.join(if cfg!(windows) { "a.exe" } else { "a.out" });
    let mut cmd = Command::new(&args.cc);
    cmd.args(objects).arg("-o").arg(&exe);
    if let Some(l) = &args.linker {
        cmd.arg(format!("-fuse-ld={l}"));
    }
    for a in &args.link_args {
        cmd.arg(a);
    }

    let out = cmd.output().context("spawning linker")?;
    if !out.status.success() {
        bail!("link failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(exe)
}
