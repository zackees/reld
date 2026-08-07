pub const CREATE_WIDGETS: &str = "\
CREATE TABLE IF NOT EXISTS widgets (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    quantity INTEGER NOT NULL
);";

pub fn all_statements() -> Vec<&'static str> {
    vec![CREATE_WIDGETS]
}
