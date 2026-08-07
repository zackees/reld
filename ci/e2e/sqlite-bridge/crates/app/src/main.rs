mod banner;
mod cli;
mod config;
mod format;
mod report;
mod run;
mod seed;

use std::process::ExitCode;

fn main() -> ExitCode {
    match run::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
