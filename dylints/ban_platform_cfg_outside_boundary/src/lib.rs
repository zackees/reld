#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents, Span};
use std::collections::HashSet;

#[derive(Default)]
struct BanPlatformCfgOutsideBoundary {
    scanned_files: HashSet<String>,
}

dylint_linting::impl_pre_expansion_lint! {
    /// ### What it does
    ///
    /// Enforces the reld host-platform boundary. The only hand-written sources allowed to select
    /// the host platform or touch native OS APIs are the `cfg_select!` selector in
    /// `crates/reld-core/src/platforms/mod.rs` and the concrete trees
    /// `crates/reld-core/src/platforms/platform_<tree>.rs` and
    /// `crates/reld-core/src/platforms/platform_<tree>/` for `<tree>` in `unix`, `linux`,
    /// `macos`, `illumos`, `other_unix`, `win`, and `wasi`.
    ///
    /// Every other Rust source of every workspace crate — production code, unit-test modules,
    /// integration tests, bins, and the neutral facade leaves
    /// `platforms/{fs,host,linker_plugin,path,process}.rs` — denies:
    ///
    /// - `#[cfg]`, `#[cfg_attr]`, `#![cfg]`, `#![cfg_attr]`, `cfg!()`, and `cfg_select!`
    ///   mentioning `windows`, `unix`, `target_os`, `target_family`, `target_env`, `target_abi`,
    ///   `target_vendor`, or `target_pointer_width`;
    /// - native OS references (`std::os::{unix,windows,linux,macos,fd,wasi}`, `windows_sys`,
    ///   `windows::Win32`, `libc::`).
    ///
    /// Outside `crates/reld-core/src/platforms/`, direct references to the concrete trees
    /// (`platform_imp`, `platform_unix`, `platform_linux`, ...) are denied too: callers reach
    /// host mechanics only through `reld_core::platforms::{fs,host,linker_plugin,path,process}`.
    ///
    /// ### Why single-host CI is sufficient
    ///
    /// Each physical source file is scanned pre-expansion, so code cfg'd away on the CI host is
    /// still inspected. Since host selection cannot exist outside the concrete trees, no other
    /// host could reveal host-specific code elsewhere. Only whole module files reached through a
    /// feature-gated `mod`/`path` need their feature state active, which is why CI lints both the
    /// default and `--no-default-features` feature sets.
    ///
    /// ### reld nuance: `target_arch` is not host selection
    ///
    /// reld is a cross-target linker. `#[cfg(target_arch = "x86_64" | "aarch64" | …)]` in
    /// `args/elf.rs` selects the *linker target* architecture (reld defaults to the host arch),
    /// which is a different concept from host OS mechanics. `target_arch`/`target_endian` are
    /// therefore allowed everywhere, unlike soldr's equivalent lint.
    pub BAN_PLATFORM_CFG_OUTSIDE_BOUNDARY,
    Deny,
    "keep host-platform cfg and native OS APIs inside reld-core's platforms/ concrete trees",
    BanPlatformCfgOutsideBoundary::default()
}

const SELECTORS: [&str; 8] = [
    "windows",
    "unix",
    "target_os",
    "target_family",
    "target_env",
    "target_abi",
    "target_vendor",
    "target_pointer_width",
];

const PLATFORMS_DIR: &str = "crates/reld-core/src/platforms/";

const CONCRETE_TREES: [&str; 7] = [
    "unix",
    "linux",
    "macos",
    "illumos",
    "other_unix",
    "win",
    "wasi",
];

const CONCRETE_TREE_NAMES: [&str; 8] = [
    "platform_imp",
    "platform_unix",
    "platform_linux",
    "platform_macos",
    "platform_illumos",
    "platform_other_unix",
    "platform_win",
    "platform_wasi",
];

const NATIVE_MARKERS: [&str; 9] = [
    "std::os::windows",
    "std::os::unix",
    "std::os::linux",
    "std::os::macos",
    "std::os::fd",
    "std::os::wasi",
    "windows_sys",
    "windows::Win32",
    "libc::",
];

impl EarlyLintPass for BanPlatformCfgOutsideBoundary {
    fn check_item(&mut self, cx: &EarlyContext<'_>, item: &rustc_ast::ast::Item) {
        let current_file = source_filename(cx, item.span);
        if !in_scope(&current_file) || !self.scanned_files.insert(current_file.clone()) {
            return;
        }
        // Scan the physical source once, rather than only the active AST.
        let source = std::fs::read_to_string(&current_file)
            .or_else(|_| cx.sess().source_map().span_to_snippet(item.span));
        if let Ok(source) = source {
            for invocation in platform_cfg_invocations(&source) {
                emit(cx, item.span, format!("host cfg `{invocation}`"));
            }
            for reference in native_platform_references(&source) {
                emit(
                    cx,
                    item.span,
                    format!("direct native-platform reference `{reference}`"),
                );
            }
            if !inside_platforms_dir(&current_file) {
                for reference in concrete_tree_references(&source) {
                    emit(
                        cx,
                        item.span,
                        format!("direct concrete-tree reference `{reference}`"),
                    );
                }
            }
        }
    }
}

fn emit(cx: &EarlyContext<'_>, span: Span, detail: String) {
    cx.opt_span_lint(
        BAN_PLATFORM_CFG_OUTSIDE_BOUNDARY,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(format!(
                "host-platform selection outside the reld-core platforms boundary: {detail}; \
                 only crates/reld-core/src/platforms/mod.rs and the platform_<tree> concrete \
                 trees may select the host, everything else uses reld_core::platforms"
            ));
        }),
    );
}

/// Rust sources the boundary applies to: every `crates/**` file (production, unit tests,
/// integration tests, bins) plus this lint's own `ui/` fixtures. Only the selector and the
/// concrete trees are exempt; the facade leaves in `platforms/` stay in scope.
fn in_scope(filename: &str) -> bool {
    let normalized = filename.replace('\\', "/");
    if !normalized.ends_with(".rs") {
        return false;
    }
    if let Some(relative) = workspace_relative(&normalized) {
        return !is_boundary_file(relative);
    }
    normalized.starts_with("ui/") || normalized.contains("/ui/")
}

/// The selector `platforms/mod.rs`, `platforms/platform_<tree>.rs`, and anything under
/// `platforms/platform_<tree>/`. `platform_unix_extra.rs` is not a concrete tree.
fn is_boundary_file(relative: &str) -> bool {
    let Some(rest) = relative.strip_prefix(PLATFORMS_DIR) else {
        return false;
    };
    if rest == "mod.rs" {
        return true;
    }
    let Some(tree_path) = rest.strip_prefix("platform_") else {
        return false;
    };
    CONCRETE_TREES.iter().any(|tree| {
        tree_path
            .strip_prefix(tree)
            .is_some_and(|after| after == ".rs" || after.starts_with('/'))
    })
}

/// Facades and trees may name `platform_imp` and the concrete trees directly; nothing else may.
fn inside_platforms_dir(filename: &str) -> bool {
    let normalized = filename.replace('\\', "/");
    workspace_relative(&normalized).is_some_and(|relative| relative.starts_with(PLATFORMS_DIR))
}

/// The `crates/...` suffix of a path, anchored at a path-component boundary.
fn workspace_relative(normalized: &str) -> Option<&str> {
    normalized
        .match_indices("crates/")
        .find(|(offset, _)| *offset == 0 || normalized.as_bytes()[offset - 1] == b'/')
        .map(|(offset, _)| &normalized[offset..])
}

fn platform_cfg_invocations(source: &str) -> Vec<String> {
    let compact = compact_code(&code_without_comments_or_strings(source));
    let mut invocations = Vec::new();
    for start in [
        "#[cfg(",
        "#[cfg_attr(",
        "#![cfg(",
        "#![cfg_attr(",
        "cfg!(",
        "cfg_select!{",
        "cfg_select!(",
    ] {
        for (offset, _) in compact.match_indices(start) {
            // Attributes need no leading boundary; `my_cfg!(unix)` is not `cfg!`.
            if !start.starts_with('#') && !standalone_before(&compact, offset) {
                continue;
            }
            let Some(clause) = balanced_invocation(&compact, offset, start.len() - 1) else {
                continue;
            };
            if SELECTORS
                .iter()
                .any(|selector| contains_identifier(clause, selector))
            {
                let shown = if start.starts_with("cfg_select!") {
                    "cfg_select!"
                } else {
                    clause.trim_start_matches("#![").trim_start_matches("#[")
                };
                invocations.push(shown.to_owned());
            }
        }
    }
    invocations
}

fn concrete_tree_references(source: &str) -> Vec<String> {
    let code = code_without_comments_or_strings(source);
    let mut references = Vec::new();
    for name in CONCRETE_TREE_NAMES {
        for _ in standalone_matches(&code, name) {
            references.push(name.to_owned());
        }
    }
    references
}

fn native_platform_references(source: &str) -> Vec<String> {
    let code = code_without_comments_or_strings(source);
    NATIVE_MARKERS
        .into_iter()
        .filter(|marker| standalone_matches(&code, marker).next().is_some())
        .map(str::to_owned)
        .collect()
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn standalone_before(code: &str, offset: usize) -> bool {
    code[..offset]
        .chars()
        .next_back()
        .is_none_or(|character| !is_identifier_char(character))
}

/// Offsets of `needle` not embedded in a longer identifier. A trailing boundary is only
/// required when the needle itself ends in an identifier character (so `libc::` still
/// matches `libc::getpid`).
fn standalone_matches<'a>(code: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    let needs_trailing = needle.chars().next_back().is_some_and(is_identifier_char);
    code.match_indices(needle)
        .map(|(offset, _)| offset)
        .filter(move |offset| {
            standalone_before(code, *offset)
                && (!needs_trailing
                    || code[offset + needle.len()..]
                        .chars()
                        .next()
                        .is_none_or(|character| !is_identifier_char(character)))
        })
}

fn contains_identifier(code: &str, identifier: &str) -> bool {
    standalone_matches(code, identifier).next().is_some()
}

/// Drops whitespace so `# [cfg (unix)]` matches `#[cfg(unix)]`, but keeps one space between
/// identifier characters so `return cfg!(unix)` does not become `returncfg!(unix)`.
fn compact_code(code: &str) -> String {
    let mut compact = String::with_capacity(code.len());
    let mut pending_space = false;
    for character in code.chars() {
        if character.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space
            && is_identifier_char(character)
            && compact.chars().next_back().is_some_and(is_identifier_char)
        {
            compact.push(' ');
        }
        pending_space = false;
        compact.push(character);
    }
    compact
}

/// The invocation starting at `offset` whose opening delimiter sits at `offset + open`, up to
/// and including its matching closing delimiter.
fn balanced_invocation(source: &str, offset: usize, open: usize) -> Option<&str> {
    let mut depth = 0_u32;
    for (relative, character) in source[offset + open..].char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&source[offset..offset + open + relative + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn code_without_comments_or_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some((prefix_len, hashes)) = raw_string_prefix(&bytes[index..]) {
            let start = index;
            index += prefix_len;
            while index < bytes.len() {
                let suffix = &bytes[index + 1..];
                if bytes[index] == b'"'
                    && suffix.len() >= hashes
                    && suffix[..hashes].iter().all(|byte| *byte == b'#')
                {
                    index += 1 + hashes;
                    break;
                }
                index += 1;
            }
            mask_range(&mut output, bytes, start, index);
        } else if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(' ');
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/*") {
            let mut depth = 1_u32;
            output.push_str("  ");
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    output.push_str("  ");
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    output.push_str("  ");
                    index += 2;
                } else {
                    output.push(if bytes[index] == b'\n' { '\n' } else { ' ' });
                    index += 1;
                }
            }
        } else if let Some(len) = char_literal_len(&bytes[index..]) {
            // Masked so a `'"'` literal cannot open a phantom string.
            mask_range(&mut output, bytes, index, index + len);
            index += len;
        } else if bytes[index] == b'"' {
            output.push(' ');
            index += 1;
            while index < bytes.len() {
                let byte = bytes[index];
                output.push(if byte == b'\n' { '\n' } else { ' ' });
                index += 1;
                if byte == b'\\' && index < bytes.len() {
                    output.push(' ');
                    index += 1;
                } else if byte == b'"' {
                    break;
                }
            }
        } else {
            output.push(bytes[index] as char);
            index += 1;
        }
    }
    output
}

/// Length of an ASCII or escaped char literal at the start of `source`. Lifetimes and
/// multi-byte char literals return `None`; neither can contain a quote that confuses masking.
fn char_literal_len(source: &[u8]) -> Option<usize> {
    if source.first() != Some(&b'\'') {
        return None;
    }
    if source.get(1) == Some(&b'\\') {
        let close = source.iter().skip(3).position(|byte| *byte == b'\'')?;
        return Some(3 + close + 1);
    }
    (source.get(2) == Some(&b'\'') && source.get(1) != Some(&b'\'')).then_some(3)
}

fn raw_string_prefix(source: &[u8]) -> Option<(usize, usize)> {
    let mut index = usize::from(source.starts_with(b"br"));
    if source.get(index) != Some(&b'r') {
        return None;
    }
    index += 1;
    let hashes_start = index;
    while source.get(index) == Some(&b'#') {
        index += 1;
    }
    (source.get(index) == Some(&b'"')).then_some((index + 1, index - hashes_start))
}

fn mask_range(output: &mut String, source: &[u8], start: usize, end: usize) {
    for byte in &source[start..end] {
        output.push(if *byte == b'\n' { '\n' } else { ' ' });
    }
}

fn source_filename(cx: &EarlyContext<'_>, span: Span) -> String {
    match cx.sess().source_map().span_to_filename(span) {
        FileName::Real(real_filename) => real_filename
            .local_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                real_filename
                    .path(RemapPathScopeComponents::DIAGNOSTICS)
                    .to_string_lossy()
                    .into_owned()
            }),
        filename => filename
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}

#[test]
fn selector_and_concrete_trees_are_exempt() {
    for path in [
        "crates/reld-core/src/platforms/mod.rs",
        "crates/reld-core/src/platforms/platform_unix.rs",
        "crates/reld-core/src/platforms/platform_unix/fd.rs",
        "crates/reld-core/src/platforms/platform_linux.rs",
        "crates/reld-core/src/platforms/platform_linux/perf.rs",
        "crates/reld-core/src/platforms/platform_macos.rs",
        "crates/reld-core/src/platforms/platform_macos/fs.rs",
        "crates/reld-core/src/platforms/platform_illumos.rs",
        "crates/reld-core/src/platforms/platform_illumos/host.rs",
        "crates/reld-core/src/platforms/platform_other_unix.rs",
        "crates/reld-core/src/platforms/platform_other_unix/nested/deep.rs",
        "crates/reld-core/src/platforms/platform_win.rs",
        "crates/reld-core/src/platforms/platform_win/process.rs",
        "crates/reld-core/src/platforms/platform_wasi.rs",
        "crates/reld-core/src/platforms/platform_wasi/path.rs",
        "/home/dev/reld/crates/reld-core/src/platforms/platform_linux.rs",
        r"C:\work\reld\crates\reld-core\src\platforms\platform_win\fs.rs",
    ] {
        assert!(!in_scope(path), "{path}");
    }
}

#[test]
fn facade_leaves_and_lookalikes_are_in_scope() {
    for path in [
        "crates/reld-core/src/platforms/fs.rs",
        "crates/reld-core/src/platforms/host.rs",
        "crates/reld-core/src/platforms/linker_plugin.rs",
        "crates/reld-core/src/platforms/path.rs",
        "crates/reld-core/src/platforms/process.rs",
        "crates/reld-core/src/platforms/platform_unix_extra.rs",
        "crates/reld-core/src/platforms/platform_linuxish.rs",
        "crates/reld-core/src/platforms/platform_windows.rs",
        "crates/reld-core/src/platforms/platform_freebsd.rs",
        "crates/reld-core/src/platforms/platform_imp.rs",
        "crates/reld-core/src/platforms/mod_extra.rs",
        "crates/reld-core/src/platforms/other/mod.rs",
        "crates/reld-core/src/platforms/tests/platform_linux.rs",
        "crates/reld-core/src/platform.rs",
        "crates/reld-core/src/platform_linux.rs",
        "crates/reld-core/src/mod.rs",
        "crates/reld/src/platforms/platform_linux.rs",
        "crates/reld-core/tests/platforms/platform_linux.rs",
    ] {
        assert!(in_scope(path), "{path}");
    }
}

#[test]
fn every_workspace_crate_target_is_in_scope() {
    for path in [
        "crates/reld-core/src/lib.rs",
        "crates/reld-core/src/subprocess.rs",
        "crates/reld/src/main.rs",
        "crates/reld/tests/acceptance.rs",
        "crates/reld/tests/external_process.rs",
        "crates/reld-testkit/src/bin/reld-difftest.rs",
        "crates/reld-diff/src/lib.rs",
        "crates/demo/examples/host.rs",
        "crates/demo/benches/host.rs",
        "/abs/reld/crates/reld/src/main.rs",
        r"C:\work\reld\crates\reld\tests\acceptance.rs",
        "ui/disallowed_host_cfg.rs",
        "/tmp/lint/ui/disallowed_host_cfg.rs",
    ] {
        assert!(in_scope(path), "{path}");
    }
    assert!(!in_scope("crates/reld/Cargo.toml"));
    assert!(!in_scope(
        "/home/dev/.cargo/registry/src/libc-0.2/src/lib.rs"
    ));
}

#[test]
fn concrete_tree_references_are_allowed_only_under_platforms() {
    assert!(inside_platforms_dir("crates/reld-core/src/platforms/fs.rs"));
    assert!(inside_platforms_dir(
        "/abs/crates/reld-core/src/platforms/mod.rs"
    ));
    assert!(!inside_platforms_dir("crates/reld-core/src/lib.rs"));
    assert!(!inside_platforms_dir("crates/reld/tests/acceptance.rs"));
    assert!(!inside_platforms_dir(
        "mycrates/reld-core/src/platforms/fs.rs"
    ));
}

#[test]
fn banned_selectors_are_detected_in_every_cfg_form() {
    assert_eq!(
        platform_cfg_invocations("fn selected() { if cfg!(windows) {} }"),
        vec!["cfg!(windows)"]
    );
    assert_eq!(
        platform_cfg_invocations("#[cfg(all(test, target_os = \"windows\"))] fn f() {}"),
        vec!["cfg(all(test,target_os=))"]
    );
    assert_eq!(
        platform_cfg_invocations("#[cfg_attr(unix, allow(dead_code))] fn f() {}"),
        vec!["cfg_attr(unix,allow(dead_code))"]
    );
    assert_eq!(
        platform_cfg_invocations("#![cfg(target_family = \"wasm\")]\nfn f() {}"),
        vec!["cfg(target_family=)"]
    );
    assert_eq!(
        platform_cfg_invocations("#![cfg_attr(target_env = \"musl\", allow(unused))]"),
        vec!["cfg_attr(target_env=,allow(unused))"]
    );
    assert_eq!(
        platform_cfg_invocations("fn f() -> bool { return cfg!(target_pointer_width = \"64\"); }"),
        vec!["cfg!(target_pointer_width=)"]
    );
    assert_eq!(
        platform_cfg_invocations("# [ cfg ( target_vendor = \"apple\" ) ] fn f() {}"),
        vec!["cfg(target_vendor=)"]
    );
    assert_eq!(
        platform_cfg_invocations("#[cfg(not(target_abi = \"eabihf\"))] fn f() {}"),
        vec!["cfg(not(target_abi=))"]
    );
}

#[test]
fn cfg_select_host_selection_is_detected() {
    for source in [
        "cfg_select! { unix => { fn f() {} } _ => {} }",
        "std::cfg_select! {\n    target_os = \"linux\" => { mod a; }\n    _ => { mod b; }\n}",
        "core::cfg_select!(windows => { fn f() {} } _ => {});",
        "fn f() -> u8 { cfg_select! { all(test, target_env = \"musl\") => 1, _ => 2 } }",
    ] {
        assert_eq!(
            platform_cfg_invocations(source),
            vec!["cfg_select!"],
            "{source}"
        );
    }
    for source in [
        "cfg_select! { feature = \"zstd\" => { fn f() {} } _ => {} }",
        "std::cfg_select! { target_arch = \"x86_64\" => {} target_endian = \"big\" => {} }",
        "cfg_select! { my_unix_cfg => { let s = \"windows\"; } _ => {} } // unix",
        "my_cfg_select! { unix => {} }",
    ] {
        assert!(platform_cfg_invocations(source).is_empty(), "{source}");
    }
}

#[test]
fn allowed_selectors_are_not_detected() {
    for source in [
        "#[cfg(feature = \"plugins\")] fn f() {}",
        "#[cfg(test)] mod tests {}",
        "#[cfg(debug_assertions)] fn f() {}",
        "#[cfg(target_arch = \"x86_64\")] fn f() {}",
        "#[cfg(any(target_arch = \"aarch64\", target_endian = \"big\"))] fn f() {}",
        "#[cfg_attr(feature = \"windows\", allow(dead_code))] fn f() {}",
        "#[cfg_attr(not(feature = \"plugins\"), path = \"linker_plugins_disabled.rs\")] mod x;",
        "fn f() -> bool { cfg!(feature = \"macho\") }",
        "cfg_select! { feature = \"zstd\" => { fn f() {} } _ => {} }",
    ] {
        assert!(platform_cfg_invocations(source).is_empty(), "{source}");
    }
}

#[test]
fn selectors_match_on_identifier_boundaries() {
    for source in [
        "#[cfg(my_unix_cfg)] fn f() {}",
        "#[cfg(unix_like)] fn f() {}",
        "#[cfg(not(windows_host))] fn f() {}",
        "#[cfg(custom_target_os)] fn f() {}",
        "#[cfg(target_os_extra)] fn f() {}",
        "fn f() { my_cfg!(unix); }",
        "fn f() { notcfg!(windows); }",
    ] {
        assert!(platform_cfg_invocations(source).is_empty(), "{source}");
    }
    assert_eq!(
        platform_cfg_invocations("#[cfg(any(my_unix_cfg, unix))] fn f() {}"),
        vec!["cfg(any(my_unix_cfg,unix))"]
    );
    assert_eq!(
        platform_cfg_invocations("fn f() { std::cfg!(windows); }"),
        vec!["cfg!(windows)"]
    );
}

#[test]
fn cfg_in_comments_strings_and_char_literals_is_ignored() {
    assert!(platform_cfg_invocations(
        r####"fn neutral() {
            let _ = "#[cfg(windows)]";
            let _ = r###"cfg!(target_os = "linux")"###;
            let _ = b"cfg!(unix)";
            /* cfg!(unix) /* #[cfg(windows)] */ */
            // #[cfg(target_os = "macos")]
            let _ = '"';
            let _ = "cfg!(windows)";
        }"####
    )
    .is_empty());
    assert_eq!(
        platform_cfg_invocations("fn f() { let q = '\"'; if cfg!(unix) {} let r = '\\''; }"),
        vec!["cfg!(unix)"]
    );
    assert_eq!(
        platform_cfg_invocations("fn f<'a>(x: &'a str) -> bool { cfg!(windows) }"),
        vec!["cfg!(windows)"]
    );
}

#[test]
fn native_platform_references_are_detected_outside_strings() {
    assert_eq!(
        native_platform_references(
            "use std::os::unix::fs::PermissionsExt; use std::os::fd::AsRawFd; libc::getpid();"
        ),
        vec!["std::os::unix", "std::os::fd", "libc::"]
    );
    assert_eq!(
        native_platform_references(
            "use std::os::windows::fs::FileExt; use windows_sys::Win32::Foundation::HANDLE; \
             use windows::Win32::System; use std::os::linux::fs::MetadataExt; \
             use std::os::macos::raw; use std::os::wasi::ffi::OsStrExt;"
        ),
        vec![
            "std::os::windows",
            "std::os::linux",
            "std::os::macos",
            "std::os::wasi",
            "windows_sys",
            "windows::Win32",
        ]
    );
    let masked = "let text = \"windows_sys libc::\"; // std::os::unix";
    assert!(native_platform_references(masked).is_empty());
    assert!(native_platform_references(
        "use std::os::raw::c_int; use std::ffi::c_void; my_libc::f(); let windows_sysfoo = 1;"
    )
    .is_empty());
}

#[test]
fn concrete_references_are_word_boundary_matched() {
    assert_eq!(
        concrete_tree_references("crate::platform_imp::fs::x(); platform_win::y();"),
        vec!["platform_imp", "platform_win"]
    );
    assert_eq!(
        concrete_tree_references(
            "use super::platform_unix::fd; platform_other_unix::f(); platform_illumos::g(); \
             platform_linux::h(); platform_macos::i(); platform_wasi::j();"
        ),
        vec![
            "platform_unix",
            "platform_linux",
            "platform_macos",
            "platform_illumos",
            "platform_other_unix",
            "platform_wasi",
        ]
    );
    assert!(concrete_tree_references(
        "platform_win32; platform_windows; my_platform_unix; platform_unix_extra; \
         crate::platform::Platform; \"platform_imp\"; // platform_linux"
    )
    .is_empty());
}
