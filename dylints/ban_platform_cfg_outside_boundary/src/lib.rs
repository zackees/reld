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
    /// Enforces the reld host-selection boundary (reld#137): the only hand-written sources
    /// allowed to choose the host platform are the `cfg_select!` block and the concrete
    /// implementation trees under `crates/reld-core/src/platforms/`. Every other production
    /// source file denies host-platform `#[cfg]`, `#[cfg_attr]`, and `cfg!()`.
    ///
    /// Inspection happens pre-expansion, so cfg'd-away code cannot hide. This is what makes
    /// single-platform CI sufficient: host-specific code can only live inside `platforms/`,
    /// which is precisely where raw platform APIs are legitimate.
    ///
    /// ### reld nuance: `target_arch` is not host selection
    ///
    /// reld is a cross-target linker. `#[cfg(target_arch = "x86_64" | "aarch64" | …)]` in
    /// `args/elf.rs` selects the *linker target* architecture (reld defaults to the host arch),
    /// which is a different concept from host OS mechanics. `target_arch`/`target_endian` are
    /// therefore NOT forbidden here, unlike soldr's equivalent lint.
    pub BAN_PLATFORM_CFG_OUTSIDE_BOUNDARY,
    Deny,
    "keep host-platform cfg inside reld-core's platforms/ boundary",
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
                emit(cx, item.span, format!("direct native-platform reference `{reference}`"));
            }
            if outside_platform_crate(&current_file) {
                for reference in concrete_tree_references(&source) {
                    emit(cx, item.span, format!("direct concrete-tree reference `{reference}`"));
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
                "host-platform selection outside the reld-core platforms/ boundary: {detail}; \
                 the only allowed selection sites are crates/reld-core/src/platforms/"
            ));
        }),
    );
}

/// Production sources the boundary applies to: every `crates/*/src/**` file, excluding the
/// generated/vendored trees and the `platforms/` boundary itself.
fn in_scope(filename: &str) -> bool {
    let normalized = filename.replace('\\', "/");
    if normalized.starts_with("ui/") || normalized.contains("/ui/") {
        return true;
    }
    let Some(marker) = normalized.find("crates/") else {
        return false;
    };
    let relative = &normalized[marker..];
    if !relative.ends_with(".rs") {
        return false;
    }
    if relative.starts_with("crates/reld-core/src/platforms/") {
        return false; // the boundary itself
    }
    true
}

fn outside_platform_crate(filename: &str) -> bool {
    let normalized = filename.replace('\\', "/");
    !normalized.contains("crates/reld-core/src/platforms/")
}

fn platform_cfg_invocations(source: &str) -> Vec<String> {
    let code = code_without_comments_or_strings(source);
    let compact: String = code
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let mut invocations = Vec::new();
    for start in ["#[cfg(", "#[cfg_attr(", "#![cfg(", "#![cfg_attr(", "cfg!("] {
        for (offset, _) in compact.match_indices(start) {
            let Some(clause) = balanced_invocation(&compact, offset) else {
                continue;
            };
            if SELECTORS.iter().any(|selector| clause.contains(selector)) {
                invocations.push(clause.trim_start_matches("#[").to_owned());
            }
        }
    }
    invocations
}

fn concrete_tree_references(source: &str) -> Vec<String> {
    let code = code_without_comments_or_strings(source);
    let mut references = Vec::new();
    for name in [
        "platform_imp",
        "platform_win",
        "platform_linux",
        "platform_macos",
        "platform_wasi",
        "platform_illumos",
    ] {
        for (offset, _) in code.match_indices(name) {
            let before = code[..offset].chars().next_back();
            let after = code[offset + name.len()..].chars().next();
            let standalone = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if standalone(before) && standalone(after) {
                references.push(name.to_owned());
            }
        }
    }
    references
}

fn native_platform_references(source: &str) -> Vec<String> {
    let code = code_without_comments_or_strings(source);
    [
        "std::os::windows",
        "std::os::unix",
        "std::os::linux",
        "std::os::macos",
        "windows_sys",
        "windows::Win32",
        "libc::",
    ]
    .into_iter()
    .filter(|marker| code.contains(marker))
    .map(str::to_owned)
    .collect()
}

fn balanced_invocation(source: &str, offset: usize) -> Option<&str> {
    let mut depth = 0_u32;
    let mut saw_open = false;
    for (relative, character) in source[offset..].char_indices() {
        match character {
            '(' => {
                saw_open = true;
                depth += 1;
            }
            ')' if saw_open => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[offset..offset + relative + 1]);
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
