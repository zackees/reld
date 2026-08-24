mod archive;
mod audit;
mod model;
mod report;
mod rules;
mod store;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "artifact-auditor",
    about = "Audit a portable build-artifact manifest"
)]
struct Cli {
    #[arg(long, default_value_t = 4)]
    copies: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let result = audit::run(cli.copies)?;
    println!("OK {}", result.fingerprint);
    Ok(())
}
