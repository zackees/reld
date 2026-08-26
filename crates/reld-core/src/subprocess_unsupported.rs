/// # Safety
/// See function of the same name in `subprocess.rs`
pub unsafe fn run_in_subprocess(args: crate::args::Args) -> crate::error::Result {
    crate::run(args)
}
