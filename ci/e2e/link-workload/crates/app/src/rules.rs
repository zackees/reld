//! Retained compiled policy data for the link benchmark workload.
//!
//! Real artifact scanners commonly compile signature/rule databases into their distributable.
//! Keeping this table in the executable makes the final native link large enough to measure. The
//! audit consumes the complete table when deriving its fingerprint; it is workload data, not
//! unreferenced linker padding.

// Exact-SHA calibration on GitHub's macOS-14 runner showed that 160 MiB still let the
// bridged front door's 0.1539s startup dominate ld64.lld's 0.3169s full-LTO link. This
// policy size gives the fastest target enough real bytes to lay out and emit while keeping
// the workload a single distributable artifact-auditing application.
const COMPILED_POLICY_BYTES: usize = 896 * 1024 * 1024;

#[used]
static COMPILED_POLICY: [u8; COMPILED_POLICY_BYTES] = [0xA5; COMPILED_POLICY_BYTES];

pub fn compiled_policy() -> &'static [u8] {
    &COMPILED_POLICY
}
