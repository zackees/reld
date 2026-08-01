//! `reld` — an incremental linker for the inner dev loop.
//!
//! There is no linker here yet. This binary exists so the workspace, CI, sanitizer, and
//! stress harnesses are wired and green from the first commit rather than retrofitted later.

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "reld", version, about = "relink. reweld. reload.", long_about = None)]
struct Args {
    /// Print the target formats reld intends to support and exit.
    #[arg(long)]
    targets: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.targets {
        for t in ["elf (linux)", "pe/coff (windows)", "mach-o (macos)"] {
            println!("{t}");
        }
        return Ok(());
    }

    eprintln!("reld is not implemented yet — see DESIGN.md");
    std::process::exit(2);
}
