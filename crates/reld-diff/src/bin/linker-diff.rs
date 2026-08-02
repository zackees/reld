fn main() -> anyhow::Result<()> {
    reld_diff::enable_diagnostics();

    let config = reld_diff::Config::from_env();
    let report = reld_diff::Report::from_config(config)?;

    if report.has_problems() {
        println!("{report}");
        std::process::exit(1);
    } else {
        println!("No differences or validation failures detected");
    }

    if let Some(coverage) = report.coverage.as_ref() {
        println!("{coverage}");
    }

    Ok(())
}
