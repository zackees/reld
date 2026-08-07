pub struct Config {
    pub expected_count: i64,
    pub expected_total: i64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            expected_count: 3,
            expected_total: 60,
        }
    }
}
