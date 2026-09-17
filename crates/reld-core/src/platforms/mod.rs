//! Host-platform services.
//!
//! This module is the only place in reld that selects code based on the *host* operating system
//! (the machine reld runs on). The host is unrelated to the linker *target*: reld is a cross-target
//! linker, so output format and architecture selection lives elsewhere and must not use host cfg.
//! It is also unrelated to `crate::platform`, which is the linker-format (ELF vs Mach-O) trait.
//!
//! The selector below picks exactly one concrete `platform_*` tree for the host. The neutral facade
//! leaves (`fs`, `host`, `linker_plugin`, `path`, `process`) define host-independent types and
//! re-export the selected tree's items, so the rest of the workspace never names a host.

use std::cfg_select;

pub mod fs;
pub mod host;
pub mod linker_plugin;
pub mod path;
pub mod process;

cfg_select! {
    windows => {
        mod platform_win;
        use platform_win as platform_imp;
    }
    target_os = "wasi" => {
        mod platform_wasi;
        use platform_wasi as platform_imp;
    }
    any(target_os = "linux", target_os = "android") => {
        mod platform_unix;
        mod platform_linux;
        use platform_linux as platform_imp;
    }
    target_os = "macos" => {
        mod platform_unix;
        mod platform_macos;
        use platform_macos as platform_imp;
    }
    target_os = "illumos" => {
        mod platform_unix;
        mod platform_illumos;
        use platform_illumos as platform_imp;
    }
    unix => {
        mod platform_unix;
        mod platform_other_unix;
        use platform_other_unix as platform_imp;
    }
}
