mod mold_tests;

use super::Filter;
use super::Result;
use libtest_mimic::Trial;
use std::env;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::sync::OnceLock;
use std::time::Duration;

use super::external_process;
use super::external_process::TimedOutput;

const EXTERNAL_TEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub(super) fn collect_tests(tests: &mut Vec<Trial>, filter: &Filter) -> Result {
    mold_tests::collect_tests(tests, filter)
}

#[derive(Clone, Debug)]
enum ExternalLinker {
    Reld,
    ThirdParty { name: String, path: PathBuf },
}

impl ExternalLinker {
    fn is_reld(&self) -> bool {
        matches!(self, ExternalLinker::Reld)
    }

    fn name(&self) -> &str {
        match self {
            ExternalLinker::Reld => "reld",
            ExternalLinker::ThirdParty { name, .. } => name.as_str(),
        }
    }
}

fn get_external_linker() -> &'static ExternalLinker {
    static VALUE: OnceLock<ExternalLinker> = OnceLock::new();
    VALUE.get_or_init(|| {
        let Ok(val) = env::var("RELD_EXTERNAL_LINKER") else {
            return ExternalLinker::Reld;
        };
        let val = val.trim();
        if val.is_empty() || val.eq_ignore_ascii_case("reld") {
            return ExternalLinker::Reld;
        }

        let (name, search_names): (&str, &[&str]) = match val.to_ascii_lowercase().as_str() {
            "ld" | "bfd" => ("ld", &["ld.bfd", "ld"]),
            "lld" => ("lld", &["ld.lld"]),
            "mold" => ("mold", &["mold"]),
            "gold" => ("gold", &["ld.gold", "gold"]),
            _ => {
                let p = PathBuf::from(&val);
                if p.exists() {
                    return ExternalLinker::ThirdParty {
                        name: val.to_string(),
                        path: std::fs::canonicalize(&p)
                            .expect("failed to canonicalize RELD_EXTERNAL_LINKER path"),
                    };
                }

                let path = which::which(val).unwrap_or_else(|_| {
                    panic!("RELD_EXTERNAL_LINKER={val}: not found as a file and not on PATH")
                });

                return ExternalLinker::ThirdParty {
                    name: val.to_string(),
                    path,
                };
            }
        };

        let path = search_names
            .iter()
            .find_map(|n| which::which(n).ok())
            .unwrap_or_else(|| {
                panic!(
                    "RELD_EXTERNAL_LINKER={val}: could not find any of [{}] on PATH",
                    search_names.join(", ")
                )
            });

        ExternalLinker::ThirdParty {
            name: name.to_string(),
            path,
        }
    })
}

fn get_fakes_dir() -> &'static Path {
    static DIR: OnceLock<FakesDir> = OnceLock::new();
    DIR.get_or_init(|| FakesDir::new(get_external_linker()).unwrap())
        .path()
}

enum FakesDir {
    Temp(tempfile::TempDir),
}

impl FakesDir {
    fn new(linker: &ExternalLinker) -> Result<Self> {
        let (path, name) = match linker {
            ExternalLinker::Reld => (super::reld_path().to_owned(), "reld"),
            ExternalLinker::ThirdParty { path, name } => (path.clone(), name.as_str()),
        };
        let tmp =
            tempfile::tempdir().expect("failed to create temp directory for external linker fakes");
        let tmp_path = tmp.path();

        for link_name in &["mold", "ld", "ld.lld"] {
            let link = tmp_path.join(link_name);
            // We can't use a symlink: lld selects its driver mode from argv[0].
            let script_contents = format!("#!/bin/bash\nexec '{}' \"$@\"\n", path.display());
            let mut file = std::fs::File::create(&link)?;
            file.write_all(script_contents.as_bytes())?;
            reld_core::make_executable(&file)?;
        }

        eprintln!(
            "external_tests: using linker '{name}' ({}) via fakes dir {}",
            path.display(),
            tmp_path.display()
        );

        Ok(FakesDir::Temp(tmp))
    }

    fn path(&self) -> &Path {
        match self {
            FakesDir::Temp(t) => t.path(),
        }
    }
}

#[allow(unused)]
fn should_not_ignore_tests(external_test: &str) -> bool {
    let reld_ignore_skip: Option<Vec<String>> =
        std::env::var("RELD_IGNORE_SKIP").ok().map(|test_suites| {
            test_suites
                .split(',')
                .map(|suite| suite.trim().to_string())
                .filter(|suite| !suite.is_empty())
                .collect()
        });

    reld_ignore_skip.is_some_and(|tests| {
        tests.contains(&external_test.to_string()) || tests.contains(&"all".to_string())
    })
}

#[allow(unused)]
fn using_third_party_linker() -> bool {
    !get_external_linker().is_reld()
}

#[allow(unused)]
fn external_linker_name() -> &'static str {
    get_external_linker().name()
}

#[allow(unused)]
fn run_external_test(external_test: &Path, extra_env: &[(&str, &str)]) -> Result<Output> {
    let fakes_dir = get_fakes_dir();

    let mut command = Command::new("bash");
    command
        .current_dir(fakes_dir)
        .arg("-c")
        .arg(format!("{} 2>&1", external_test.display()));

    for (key, value) in extra_env {
        command.env(key, value);
    }

    match external_process::output_with_timeout(&mut command, EXTERNAL_TEST_TIMEOUT)? {
        TimedOutput::Completed(output) => Ok(output),
        TimedOutput::TimedOut => Err(format!(
            "External test `{}` timed out after {} seconds; terminated its process group",
            external_test.display(),
            EXTERNAL_TEST_TIMEOUT.as_secs(),
        )
        .into()),
    }
}
