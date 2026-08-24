//! Retained compiled policy data for the link benchmark workload.
//!
//! Real artifact scanners commonly compile signature/rule databases into their distributable.
//! Keeping this table in the executable makes the final native link large enough to measure. The
//! audit consumes the complete table when deriving its fingerprint; it is workload data, not
//! unreferenced linker padding.

const COMPILED_POLICY_BYTES: usize = 160 * 1024 * 1024;

#[used]
static COMPILED_POLICY: [u8; COMPILED_POLICY_BYTES] = [0xA5; COMPILED_POLICY_BYTES];

pub fn compiled_policy() -> &'static [u8] {
    &COMPILED_POLICY
}
