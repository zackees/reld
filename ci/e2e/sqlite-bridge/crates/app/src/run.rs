use crate::banner;
use crate::cli;
use crate::config::Config;
use crate::report;
use crate::seed;
use anyhow::{bail, Context, Result};
use dbcore::util;
use dbcore::Store;

pub fn run() -> Result<()> {
    let opts = cli::parse();
    let cfg = Config::default();

    banner::print();

    let store = Store::open_in_memory().context("opening in-memory sqlite store")?;
    seed::seed(&store).context("seeding widgets")?;

    let count = store.count().context("counting widgets")?;
    let total = store.total_quantity().context("summing quantity")?;

    report::print_all(&store, opts.verbose).context("printing report")?;
    println!("{}", util::describe(count, total));

    // Round-trip one row through serde as an extra dependency exercise.
    let w = store.get(2).context("fetching widget 2")?;
    let json = dbcore::serialize::to_json(&w);
    let back = dbcore::serialize::from_json(&json).context("deserializing widget")?;
    if back != w {
        bail!("serde round-trip mismatch");
    }

    if count != cfg.expected_count || total != cfg.expected_total {
        bail!(
            "unexpected totals: count={count} (want {}), total={total} (want {})",
            cfg.expected_count,
            cfg.expected_total
        );
    }

    println!("OK");
    Ok(())
}
