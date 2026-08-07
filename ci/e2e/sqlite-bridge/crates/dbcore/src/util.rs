/// Returns the bundled SQLite library version string (proves the C library linked).
pub fn sqlite_version() -> String {
    rusqlite::version().to_string()
}

pub fn describe(count: i64, total: i64) -> String {
    format!("{count} widgets, {total} units total")
}
