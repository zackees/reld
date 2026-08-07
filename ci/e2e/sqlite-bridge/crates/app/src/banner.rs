use dbcore::util;

pub fn print() {
    println!("app e2e — linked SQLite {}", util::sqlite_version());
}
