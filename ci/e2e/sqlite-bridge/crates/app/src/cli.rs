pub struct Options {
    pub verbose: bool,
}

pub fn parse() -> Options {
    let verbose = std::env::args().any(|a| a == "-v" || a == "--verbose");
    Options { verbose }
}
