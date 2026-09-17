// Neutral cfgs are allowed everywhere. Host selectors inside strings, comments such as
// cfg!(windows), and lookalike identifiers like `my_unix_cfg` do not count.
#![allow(dead_code)]

const NOTE: &str = "#[cfg(unix)] cfg!(windows) libc:: std::os::unix platform_imp";

#[cfg(feature = "extra")]
fn feature_gated() {}

#[cfg(not(feature = "extra"))]
fn feature_absent() {}

#[cfg(target_arch = "x86_64")]
const LINKER_TARGET: &str = "x86_64";

#[cfg(not(target_arch = "x86_64"))]
const LINKER_TARGET: &str = "other";

#[cfg(any(my_unix_cfg, target_endian = "big"))]
fn custom_cfg() {}

#[cfg_attr(debug_assertions, allow(unused_variables))]
fn neutral_macros() -> bool {
    cfg!(target_endian = "little") || cfg!(debug_assertions) || cfg!(feature = "windows")
}

#[cfg(test)]
mod tests {}

fn main() {
    let _ = (NOTE, LINKER_TARGET, neutral_macros());
}
