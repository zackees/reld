#[global_allocator]
static MIMALLOC: mimalloc_pprof::MiMalloc = mimalloc_pprof::MiMalloc;

#[cfg(feature = "mimalloc-pprof-dhat")]
struct DhatGuard;

#[cfg(feature = "mimalloc-pprof-dhat")]
impl DhatGuard {
    fn start() -> Self {
        if !mimalloc_pprof::dhat::is_enabled() {
            assert!(
                mimalloc_pprof::dhat::start(),
                "failed to start mimalloc DHAT profiling"
            );
        }
        Self
    }
}

#[cfg(feature = "mimalloc-pprof-dhat")]
impl Drop for DhatGuard {
    fn drop(&mut self) {
        mimalloc_pprof::dhat::stop();
        let output = std::env::var_os("RELD_DHAT_OUTPUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("dhat-heap.json"));
        if let Err(error) = mimalloc_pprof::dhat::dump_file(&output) {
            eprintln!(
                "reld: failed to write DHAT profile {}: {error}",
                output.display()
            );
        }
    }
}

fn main() {
    #[cfg(feature = "mimalloc-pprof-dhat")]
    let dhat = DhatGuard::start();

    let result = run();

    // `report_error_and_exit` terminates the process without running destructors.
    // Finish the exact profile first so failed links remain diagnosable.
    #[cfg(feature = "mimalloc-pprof-dhat")]
    drop(dhat);

    if let Err(error) = result {
        reld_core::error::report_error_and_exit(&error)
    }
}

/// The current Reld version as written by build.rs.
const VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/version.txt"));

fn run() -> reld_core::error::Result {
    reld_core::init_timing()?;

    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let mut args = reld_core::Args::new(std::env::args)?;
    let route = reld_core::select_route(&raw_args, args.link_target())?;

    if route.is_bridge() {
        reld_core::run_bridge(raw_args.clone(), route)?;
    } else {
        route.log_native();

        args.set_version(VERSION);
        args.parse(std::env::args)?;

        if reld_core::should_fork(&args) {
            // Safety: We haven't spawned any threads yet.
            unsafe { reld_core::run_in_subprocess(args)? };
        } else {
            // Run the linker in this process without forking.

            // Note, we need to setup tracing before worker, otherwise the threads won't contribute to
            // counters such as --time=cycles,instructions etc.
            reld_core::setup_tracing(&args)?;

            reld_core::run(args)?;
        }
    }

    reld_core::log_successful_invocation(&raw_args, route)
}
