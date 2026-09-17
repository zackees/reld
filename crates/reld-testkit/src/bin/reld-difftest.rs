//! Seeded differential execution oracle.

use std::env::consts::EXE_SUFFIX;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use clap::Parser;
use reld_testkit::{CompilerTarget, WorkloadSpec, generate};

#[derive(Parser, Debug)]
#[command(
    name = "reld-difftest",
    about = "Compare reld execution with a reference linker"
)]
struct Args {
    /// Run seeds 0..N.
    #[arg(long, default_value_t = 100, conflicts_with = "seed")]
    seeds: u64,

    /// Run one reproducing seed.
    #[arg(long)]
    seed: Option<u64>,

    /// C compiler used for object generation and linker-driver invocation.
    #[arg(long, default_value = "clang")]
    cc: String,

    /// Reference linker selected through -fuse-ld. Empty means the platform default.
    #[arg(long, default_value = "")]
    reference_linker: String,

    /// Path to the reld executable. Defaults to the executable beside reld-difftest.
    #[arg(long)]
    reld: Option<PathBuf>,

    /// Keep generated trees for all seeds, not only failures.
    #[arg(long)]
    keep: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let seeds: Vec<u64> = args
        .seed
        .map_or_else(|| (0..args.seeds).collect(), |s| vec![s]);
    let root = std::env::temp_dir().join("reld-difftest");
    std::fs::create_dir_all(&root)?;
    let env_reld = std::env::var_os("RELD_DIFFTEST_RELD").map(PathBuf::from);
    let reld = resolve_reld(args.reld.as_deref().or(env_reld.as_deref()))?;
    let target = CompilerTarget::detect(&args.cc)?;

    for seed in &seeds {
        let dir = root.join(format!("seed-{seed:020}"));
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(error) = run_seed(&args, &reld, target, *seed, &dir) {
            eprintln!("differential failure; reproducing seed: {seed}");
            eprintln!("tree kept at {}", dir.display());
            eprintln!("reproduce: reld-difftest --seed {seed} --keep");
            return Err(error);
        }
        if !args.keep {
            let _ = std::fs::remove_dir_all(&dir);
        }
        println!("seed {seed:>8}  ok");
    }
    println!("{} seeds, 0 differential failures", seeds.len());
    Ok(())
}

fn resolve_reld(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    let name = format!("reld{EXE_SUFFIX}");
    let sibling = std::env::current_exe()?.with_file_name(&name);
    if sibling.exists() {
        return Ok(sibling);
    }
    which::which(&name).with_context(|| {
        format!("could not find {name}; build `reld` first or pass --reld / RELD_DIFFTEST_RELD")
    })
}

fn run_seed(args: &Args, reld: &Path, target: CompilerTarget, seed: u64, dir: &Path) -> Result<()> {
    // Four translation units keep 100-seed PR runs inexpensive while still exercising a seeded
    // cross-object graph, weak definitions, TLS, and COMDAT/section-group inputs.
    let spec = WorkloadSpec {
        seed,
        units: 4,
        symbols_per_unit: 8,
        ref_density: 0.35,
        weak_ratio: 0.05,
        tls_ratio: 0.10,
        comdat_fns: 4,
    };
    let workload = generate(&spec, dir).context("generating differential workload")?;
    let objects = compile_all(&args.cc, target, dir, &workload.sources)?;

    let reld_exe = link(&args.cc, dir, &objects, &reld.display().to_string(), "reld")?;
    let reference_exe = link(&args.cc, dir, &objects, &args.reference_linker, "reference")?;

    let mut reld_out = run(&reld_exe)?;
    let reference_out = run(&reference_exe)?;
    if std::env::var_os("RELD_DIFFTEST_INJECT_BUG").is_some() {
        reld_out.stdout.extend_from_slice(b"oracle-mutation\n");
    }
    compare_outputs(seed, &reld_out, &reference_out)
}

fn compile_all(
    cc: &str,
    target: CompilerTarget,
    dir: &Path,
    sources: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let mut objects = Vec::with_capacity(sources.len());
    for source in sources {
        let object = source.with_extension(if target.is_windows() { "obj" } else { "o" });
        let mut command = Command::new(cc);
        command
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object)
            .arg("-I")
            .arg(dir)
            .arg("-O0");
        if !target.is_windows() {
            command.arg("-fPIC");
        }
        let output = command.output().with_context(|| format!("spawning {cc}"))?;
        if !output.status.success() {
            bail!(
                "compile failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        objects.push(object);
    }
    Ok(objects)
}

fn link(cc: &str, dir: &Path, objects: &[PathBuf], linker: &str, name: &str) -> Result<PathBuf> {
    let executable = dir.join(format!("{name}{EXE_SUFFIX}"));
    let mut command = Command::new(cc);
    command.args(objects).arg("-o").arg(&executable);
    if !linker.is_empty() {
        command.arg(format!("-fuse-ld={linker}"));
    }
    let output = command
        .output()
        .with_context(|| format!("spawning {name} linker"))?;
    if !output.status.success() {
        bail!(
            "{name} link failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(executable)
}

fn run(executable: &Path) -> Result<Output> {
    Command::new(executable)
        .output()
        .with_context(|| format!("running {}", executable.display()))
}

fn compare_outputs(seed: u64, reld: &Output, reference: &Output) -> Result<()> {
    let reld_exit = reld.status.code();
    let reference_exit = reference.status.code();
    if reld_exit != reference_exit || reld.stdout != reference.stdout {
        bail!(
            "seed {seed}: execution diverged: reld exit={reld_exit:?} stdout={:?}; reference exit={reference_exit:?} stdout={:?}",
            String::from_utf8_lossy(&reld.stdout),
            String::from_utf8_lossy(&reference.stdout)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_is_detected_and_reports_seed() {
        let success = || Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let mut mutated = success();
        mutated.stdout.extend_from_slice(b"mutation");
        let reference = success();
        let error = compare_outputs(0x5eed, &mutated, &reference).unwrap_err();
        assert!(error.to_string().contains("24301"));
    }
}
