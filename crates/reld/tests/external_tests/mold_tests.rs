use super::super::Architecture;
use super::super::Result;
use super::super::get_host_architecture;
use super::external_linker_name;
use super::run_external_test;
use super::should_not_ignore_tests;
use super::using_third_party_linker;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use reld_core::error::Context;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use std::str::FromStr;
use std::sync::OnceLock;

#[derive(Deserialize)]
struct Config {
    skipped_groups: HashMap<String, SkippedGroup>,
}

#[derive(Deserialize)]
struct SkippedGroup {
    reason: String,
    tracking_issue: u64,
    tests: Vec<String>,
}

static SKIP_TESTS_NAME: OnceLock<Option<HashMap<String, u64>>> = OnceLock::new();

const PREFIX: &str = "external_test_suites/mold";

/// Run a mold test with mold-specific environment setup.
fn run_mold_test(mold_test: &Path) -> Result<Output> {
    // Mold tests use the `arch-` prefix to indicate architecture-specific tests.
    // If the test is architecture-specific (e.g., arch-riscv64-*.sh),
    // set the TRIPLE environment variable for cross-compilation
    let triple = if let Some(file_name) = mold_test.file_name().and_then(|n| n.to_str())
        && let Some(arch_str) = file_name.strip_prefix("arch-")
        && let Some(arch_name) = arch_str.split('-').next()
        && let Ok(arch) = Architecture::from_str(arch_name)
        && arch != get_host_architecture()
    {
        Some(arch.cross_triplet())
    } else {
        None
    };

    let mut env_vars: Vec<(&str, &str)> = if let Some(ref triple_value) = triple {
        vec![("TRIPLE", triple_value.as_str())]
    } else {
        vec![]
    };

    // This corpus measures reld's native ELF compatibility. Routed LLD coverage lives in the
    // dedicated linker-mode matrix, so keep the native regression ratchet on the native engine.
    if !using_third_party_linker() {
        env_vars.extend([
            (reld_core::bridge::RELD_ENGINE_ENV, "reld"),
            (reld_core::args::RELD_UNSUPPORTED_ENV, "ignore"),
        ]);
    }

    run_external_test(mold_test, &env_vars)
}

pub(crate) fn collect_tests(tests: &mut Vec<Trial>, filter: &super::super::Filter) -> Result {
    if filter.excludes(PREFIX) {
        return Ok(());
    }

    let third_party = using_third_party_linker();
    let linker_name = external_linker_name();
    let test_dir_path = super::super::base_dir().join("../../external_test_suites/mold/test");
    let dir = std::fs::read_dir(&test_dir_path)
        .with_context(|| format!("Failed to read directory {}", test_dir_path.display()))?;

    let shell_tests = dir
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "sh"))
        .collect::<Vec<_>>();
    let arch_count = shell_tests
        .iter()
        .filter(|path| should_skip_mold_test_by_arch(path))
        .count();
    let native_count = shell_tests.len() - arch_count;
    if (shell_tests.len(), arch_count, native_count) != (518, 111, 407) {
        return Err(format!(
            "Pinned mold corpus drifted: expected 518 total / 111 arch / 407 native, got {} / {arch_count} / {native_count}",
            shell_tests.len()
        )
        .into());
    }

    for path in shell_tests {
        if !should_skip_mold_test_by_arch(&path) {
            let file_name =
                String::from_utf8_lossy(path.file_name().unwrap().as_encoded_bytes()).to_string();

            let name = if third_party {
                format!("{PREFIX}[{linker_name}]/test/{file_name}")
            } else {
                format!("{PREFIX}/test/{file_name}")
            };

            if !should_skip_mold_test(&path) && !should_skip_by_local_config(&path) {
                tests.push(Trial::test(name, move || {
                    check_mold_tests_regression(path).map_err(|e| Failed::from(e.to_string()))
                }));
            } else if should_skip_mold_test_by_toml(&path) && !should_skip_by_local_config(&path) {
                tests.push(Trial::ignorable_test(
                    format!("{name}/expect_failure"),
                    move || {
                        verify_skipped_mold_tests_still_fail(path)
                            .map_err(|e| Failed::from(e.to_string()))
                    },
                ));
            }
        }
    }
    Ok(())
}

fn check_mold_tests_regression(mold_test: PathBuf) -> Result {
    let output = run_mold_test(&mold_test)?;
    if !output.status.success() {
        let error_message = format!(
            "Mold test `{}` failed with status: {}\nOutput:\n{}",
            mold_test.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout)
        );
        return Err(error_message.into());
    }

    Ok(())
}

fn verify_skipped_mold_tests_still_fail(mold_test: PathBuf) -> Result<libtest_mimic::Completion> {
    let output = run_mold_test(&mold_test)?;
    if output.status.success() {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if combined.to_ascii_lowercase().contains("skip") {
            return Ok(libtest_mimic::Completion::ignored_with(
                "mold test prerequisite unavailable on this runner",
            ));
        }
        let linker = external_linker_name();
        let message = if using_third_party_linker() {
            format!(
                "Test `{}` is in the skip list (fails with reld) but passes with '{linker}'. This indicates the failure may be reld-specific.",
                mold_test.display()
            )
        } else {
            format!(
                "Test `{}` is in skip list but now passes. Should be removed from skip list.",
                mold_test.display()
            )
        };
        return Err(message.into());
    }

    Ok(libtest_mimic::Completion::Completed)
}

fn load_skip_tests_config() -> &'static Option<HashMap<String, u64>> {
    SKIP_TESTS_NAME.get_or_init(|| {
        let skip_tests_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("external_tests")
            .join("mold_skip_tests.toml");

        fs::read_to_string(&skip_tests_path)
            .map(|content| {
                let config: Config =
                    toml::from_str(&content).expect("Failed to parse skip_tests.toml");

                config
                    .skipped_groups
                    .into_values()
                    .flat_map(|group| {
                        assert!(
                            !group.reason.trim().is_empty(),
                            "mold skip reason is required"
                        );
                        assert!(
                            group.tracking_issue > 0,
                            "mold skip tracking issue is required"
                        );
                        group
                            .tests
                            .into_iter()
                            .map(move |test| (test, group.tracking_issue))
                    })
                    .collect()
            })
            .ok()
    })
}

fn should_skip_mold_test(path: &Path) -> bool {
    should_skip_mold_test_by_toml(path) || should_skip_mold_test_by_arch(path)
}

fn should_skip_mold_test_by_toml(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .expect("Must be a valid filename")
        .to_str()
        .expect("Expected valid string name");

    if should_not_ignore_tests("mold") {
        return false;
    }

    if let Some(skip_list) = load_skip_tests_config()
        && skip_list.contains_key(file_name)
    {
        return true;
    }

    false
}

/// Returns whether the user's test-config.toml says to skip a particular test. If this returns
/// true, then we skip both the positive and negative versions of the test.
fn should_skip_by_local_config(path: &Path) -> bool {
    if let Ok(config) = super::super::read_test_config()
        && let Some(name) = path.file_name().and_then(|name| name.to_str())
        && config.ignore_external_tests.iter().any(|n| n == name)
    {
        true
    } else {
        false
    }
}

// P1 deliberately excludes all architecture-prefixed tests: they are the 111-test qemu/cross
// matrix deferred by the plan, including tests whose prefix happens to match this host.
fn should_skip_mold_test_by_arch(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .expect("Must be a valid filename")
        .to_str()
        .expect("Expected valid string name");

    file_name.starts_with("arch-")
}
