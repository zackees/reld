use crate::format;
use dbcore::store::Store;

pub fn print_all(store: &Store, verbose: bool) -> Result<(), dbcore::DbError> {
    if verbose {
        println!("{}", format::header());
        for id in 1..=store.count()? {
            let w = store.get(id)?;
            println!("{}", format::line(&w));
        }
    }
    Ok(())
}
