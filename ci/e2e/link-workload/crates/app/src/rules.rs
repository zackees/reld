//! Retained compiled policy data for the link benchmark workload.
//!
//! Real artifact scanners commonly compile signature/rule databases into their distributable.
//! Keeping this table in the executable makes the final native link large enough to measure. The
//! audit consumes the complete table when deriving its fingerprint; it is workload data, not
//! unreferenced linker padding.

// Exact-SHA calibration on GitHub's macOS-14 runner showed that 160 MiB still let the bridged
// front door dominate ld64.lld's full-LTO link. Linux and Windows exceeded the significance gate
// at that size, while their hosted compilers cannot retain a macOS-sized corpus reliably. Real
// scanners ship target-specific rules, so keep one application and calibrate only its bundled
// target policy. The unchanged per-platform startup gate verifies both choices empirically.
#[cfg(target_os = "macos")]
const COMPILED_POLICY_BYTES: usize = 928 * 1024 * 1024;
#[cfg(not(target_os = "macos"))]
const COMPILED_POLICY_BYTES: usize = 256 * 1024 * 1024;

#[used]
static COMPILED_POLICY: [u8; COMPILED_POLICY_BYTES] = [0xA5; COMPILED_POLICY_BYTES];

pub fn compiled_policy() -> &'static [u8] {
    &COMPILED_POLICY
}
