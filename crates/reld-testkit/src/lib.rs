//! Synthetic link-workload generation.
//!
//! One generator serves three consumers, deliberately:
//!
//! * **benchmarks** — reproducible workloads of a known shape, so a number means something
//! * **stress tests** — many random shapes, looking for crashes and wrong output
//! * **differential tests** — the same inputs fed to `reld` and to a reference linker
//!
//! Everything is derived from a `u64` seed. A stress failure is therefore reported as a single
//! integer that reproduces the exact input set, which is the only property that makes a random
//! test suite actionable.
//!
//! C is used as the source language rather than Rust because it is the shortest path to real
//! object files on all three target platforms, and because it lets us dial individual linker
//! stressors (weak symbols, TLS, COMDAT-equivalents) independently.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

/// Shape of a generated workload. Every field is a linker stressor, not just a size knob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadSpec {
    /// Reproduces the workload exactly.
    pub seed: u64,
    /// Number of translation units.
    pub units: usize,
    /// Defined functions per unit.
    pub symbols_per_unit: usize,
    /// Probability that a given function calls into another unit. Drives symbol-resolution
    /// pressure and cross-unit relocation count.
    pub ref_density: f64,
    /// Fraction of symbols emitted `__attribute__((weak))`. Weak resolution is a classic
    /// source of linker divergence.
    pub weak_ratio: f64,
    /// Fraction of units that define thread-local storage.
    pub tls_ratio: f64,
    /// Number of inline functions in the shared header. These land in every TU and must be
    /// deduplicated — COMDAT on PE/COFF, section groups on ELF.
    pub comdat_fns: usize,
}

impl Default for WorkloadSpec {
    fn default() -> Self {
        Self {
            seed: 0,
            units: 64,
            symbols_per_unit: 32,
            ref_density: 0.35,
            weak_ratio: 0.05,
            tls_ratio: 0.10,
            comdat_fns: 16,
        }
    }
}

impl WorkloadSpec {
    /// A small workload — fast enough for per-commit CI.
    pub fn small(seed: u64) -> Self {
        Self {
            seed,
            units: 16,
            symbols_per_unit: 16,
            comdat_fns: 4,
            ..Default::default()
        }
    }

    /// A large workload — for benchmarking and nightly stress.
    pub fn large(seed: u64) -> Self {
        Self {
            seed,
            units: 512,
            symbols_per_unit: 64,
            comdat_fns: 64,
            ..Default::default()
        }
    }

    /// Derive a randomized spec from a seed. Used by stress runs so that the *shape* varies,
    /// not just the contents — the bugs that matter tend to live in unusual shapes.
    pub fn random(seed: u64) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        Self {
            seed,
            units: rng.gen_range(1..=128),
            symbols_per_unit: rng.gen_range(1..=64),
            ref_density: rng.gen_range(0.0..=1.0),
            weak_ratio: rng.gen_range(0.0..=0.30),
            tls_ratio: rng.gen_range(0.0..=0.50),
            comdat_fns: rng.gen_range(0..=32),
        }
    }

    /// Total defined function symbols, excluding the COMDAT set and `main`.
    pub fn total_symbols(&self) -> usize {
        self.units * self.symbols_per_unit
    }
}

/// A generated workload on disk.
#[derive(Debug, Clone)]
pub struct Workload {
    pub root: PathBuf,
    /// Every `.c` file, including `main.c`. Compile each to an object, then link.
    pub sources: Vec<PathBuf>,
    pub spec: WorkloadSpec,
}

impl Workload {
    /// Expected process exit code once compiled, linked, and run.
    ///
    /// This is the differential-test oracle: a linker that resolves symbols incorrectly
    /// produces a binary that runs and returns the wrong number, which a crash-only test
    /// would miss entirely.
    pub fn expected_exit_code(&self) -> i32 {
        (self.spec.units % 256) as i32
    }
}

/// Generate a workload into `root`, creating it if needed.
pub fn generate(spec: &WorkloadSpec, root: &Path) -> Result<Workload> {
    fs::create_dir_all(root)
        .with_context(|| format!("creating workload dir {}", root.display()))?;

    let mut rng = ChaCha8Rng::seed_from_u64(spec.seed);
    let mut sources = Vec::with_capacity(spec.units + 2);

    write_common_header(spec, root)?;

    for unit in 0..spec.units {
        let path = root.join(format!("unit_{unit:04}.c"));
        fs::write(&path, render_unit(spec, unit, &mut rng))
            .with_context(|| format!("writing {}", path.display()))?;
        sources.push(path);
    }

    let main_path = root.join("main.c");
    fs::write(&main_path, render_main(spec)).context("writing main.c")?;
    sources.push(main_path);

    let manifest = root.join("workload.json");
    fs::write(&manifest, serde_json::to_string_pretty(spec)?).context("writing workload.json")?;

    Ok(Workload {
        root: root.to_path_buf(),
        sources,
        spec: spec.clone(),
    })
}

fn write_common_header(spec: &WorkloadSpec, root: &Path) -> Result<()> {
    let mut h = String::from(
        "/* generated by reld-testkit — do not edit */\n\
         #ifndef RELD_COMMON_H\n#define RELD_COMMON_H\n\n",
    );
    // Inline functions land in every TU that includes this header. On ELF these become
    // section groups; on PE/COFF, COMDATs. Deduplicating them is a correctness requirement
    // and a measurable cost, so the count is a dial.
    for i in 0..spec.comdat_fns {
        h.push_str(&format!(
            "static inline int reld_inline_{i}(int x) {{ return (x * {mul}) ^ {xor}; }}\n",
            i = i,
            mul = 2 * i + 1,
            xor = i * 7 + 3
        ));
    }
    h.push_str("\n#endif /* RELD_COMMON_H */\n");
    fs::write(root.join("common.h"), h).context("writing common.h")?;
    Ok(())
}

fn render_unit(spec: &WorkloadSpec, unit: usize, rng: &mut ChaCha8Rng) -> String {
    let mut s = format!("/* generated by reld-testkit — unit {unit} */\n#include \"common.h\"\n\n");

    if spec.units > 1 && spec.ref_density > 0.0 {
        // Forward-declare the entry point of every other unit so any symbol may reference
        // any other. Declaring all of them (rather than only those used) also exercises
        // undefined-symbol handling for the ones that go unreferenced.
        for other in 0..spec.units {
            if other != unit {
                s.push_str(&format!("int unit_{other:04}_entry(int);\n"));
            }
        }
        s.push('\n');
    }

    if rng.gen_bool(spec.tls_ratio.clamp(0.0, 1.0)) {
        s.push_str(&format!(
            "_Thread_local int unit_{unit:04}_tls = {};\n\n",
            unit as i32
        ));
    }

    for sym in 0..spec.symbols_per_unit {
        let weak = rng.gen_bool(spec.weak_ratio.clamp(0.0, 1.0));
        if weak {
            s.push_str("__attribute__((weak)) ");
        }
        s.push_str(&format!("int unit_{unit:04}_f{sym:04}(int x) {{\n"));

        if spec.comdat_fns > 0 {
            let which = rng.gen_range(0..spec.comdat_fns);
            s.push_str(&format!("    x = reld_inline_{which}(x);\n"));
        }
        if spec.units > 1 && rng.gen_bool(spec.ref_density.clamp(0.0, 1.0)) {
            let mut other = rng.gen_range(0..spec.units);
            if other == unit {
                other = (other + 1) % spec.units;
            }
            // Guard the recursion so the generated program terminates regardless of how
            // densely the call graph is wired.
            s.push_str(&format!(
                "    if (x > 0) x = unit_{other:04}_entry(x - 1);\n"
            ));
        }
        s.push_str("    return x;\n}\n\n");
    }

    // Each unit's entry returns 0, so the program's total is deterministic and independent
    // of the random call graph above.
    s.push_str(&format!("int unit_{unit:04}_entry(int x) {{\n"));
    if spec.symbols_per_unit > 0 {
        s.push_str(&format!("    (void)unit_{unit:04}_f0000(x);\n"));
    }
    s.push_str("    return 0;\n}\n");
    s
}

fn render_main(spec: &WorkloadSpec) -> String {
    let mut s = String::from("/* generated by reld-testkit */\n#include \"common.h\"\n\n");
    for unit in 0..spec.units {
        s.push_str(&format!("int unit_{unit:04}_entry(int);\n"));
    }
    s.push_str("\nint main(void) {\n    int acc = 0;\n");
    for unit in 0..spec.units {
        s.push_str(&format!("    acc += unit_{unit:04}_entry(1);\n"));
    }
    // acc is 0; the exit code encodes the unit count so a mislinked binary is detectable
    // by its return value rather than only by crashing.
    s.push_str(&format!("    return (acc + {}) % 256;\n}}\n", spec.units));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("reld-testkit-{name}"));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn generation_is_deterministic() {
        let spec = WorkloadSpec::small(42);
        let a = tmpdir("det-a");
        let b = tmpdir("det-b");
        generate(&spec, &a).unwrap();
        generate(&spec, &b).unwrap();

        for src in fs::read_dir(&a).unwrap() {
            let src = src.unwrap().path();
            let name = src.file_name().unwrap();
            assert_eq!(
                fs::read(&src).unwrap(),
                fs::read(b.join(name)).unwrap(),
                "{name:?} differs between two runs of the same seed"
            );
        }
    }

    #[test]
    fn different_seeds_differ() {
        let a = tmpdir("seed-a");
        let b = tmpdir("seed-b");
        generate(&WorkloadSpec::small(1), &a).unwrap();
        generate(&WorkloadSpec::small(2), &b).unwrap();
        assert_ne!(
            fs::read(a.join("unit_0000.c")).unwrap(),
            fs::read(b.join("unit_0000.c")).unwrap()
        );
    }

    #[test]
    fn emits_expected_file_count() {
        let spec = WorkloadSpec::small(7);
        let d = tmpdir("count");
        let w = generate(&spec, &d).unwrap();
        assert_eq!(w.sources.len(), spec.units + 1, "units plus main.c");
        assert!(d.join("common.h").exists());
        assert!(d.join("workload.json").exists());
    }

    #[test]
    fn degenerate_shapes_are_generable() {
        // Single unit, no cross-refs, no COMDATs — the shapes most likely to trip an
        // off-by-one in the generator itself.
        let spec = WorkloadSpec {
            seed: 3,
            units: 1,
            symbols_per_unit: 1,
            ref_density: 0.0,
            weak_ratio: 0.0,
            tls_ratio: 0.0,
            comdat_fns: 0,
        };
        let d = tmpdir("degenerate");
        let w = generate(&spec, &d).unwrap();
        assert_eq!(w.sources.len(), 2);
    }

    #[test]
    fn random_specs_stay_in_bounds() {
        for seed in 0..64 {
            let s = WorkloadSpec::random(seed);
            assert!(s.units >= 1 && s.units <= 128);
            assert!((0.0..=1.0).contains(&s.ref_density));
            assert!((0.0..=1.0).contains(&s.weak_ratio));
        }
    }
}
