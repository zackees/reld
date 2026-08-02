#[allow(dead_code)]
#[path = "acceptance.rs"]
mod acceptance;

fn main() -> acceptance::Result<std::process::ExitCode> {
    acceptance::external_main()
}
