//! Native, in-process deployment of MinGW/LLVM runtime DLLs next to a linked Windows output.
//!
//! After linking a PE executable or DLL we read its import table (and delay-load imports), pick out
//! the toolchain runtime libraries it depends on (`libc++.dll`, `libunwind.dll`,
//! `libwinpthread-1.dll`, ...), find them in the toolchain, walk their own imports transitively and
//! place each one beside the output so the program runs without the toolchain on `PATH`.
//!
//! Everything here is best-effort: failures become warnings in the [`DeployReport`], never errors.
//! No subprocesses are spawned; the PE parser is hand-rolled on top of `std`.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;

/// Disables runtime-DLL deployment entirely.
pub const NO_DEPLOY_LIBS_ENV: &str = "RELD_NO_DEPLOY_LIBS";
/// Disables all of reld's automatic post-link conveniences, including runtime-DLL deployment.
pub const NO_AUTO_ENV: &str = "RELD_NO_AUTO";
/// Disables runtime-DLL deployment when the output is itself a `.dll`.
pub const NO_DEPLOY_SHARED_LIB_ENV: &str = "RELD_NO_DEPLOY_SHARED_LIB";
/// Directory searched for runtime DLLs before the toolchain directories.
pub const RUNTIME_DIR_ENV: &str = "RELD_RUNTIME_DIR";

pub const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
pub const IMAGE_FILE_MACHINE_ARM64: u16 = 0xaa64;
pub const IMAGE_FILE_MACHINE_I386: u16 = 0x14c;

const IMPORT_DIRECTORY_INDEX: usize = 1;
const DELAY_IMPORT_DIRECTORY_INDEX: usize = 13;
const MAX_DESCRIPTORS: usize = 4096;
const MAX_NAME_LEN: usize = 4096;

/// Outcome of [`deploy_runtime_dlls`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeployReport {
    /// Destination paths that were (re)written.
    pub deployed: Vec<PathBuf>,
    /// Destination paths that already matched their source.
    pub up_to_date: Vec<PathBuf>,
    /// Runtime DLL names that were needed but not found in any search directory.
    pub missing: Vec<String>,
    /// Non-fatal problems encountered along the way.
    pub warnings: Vec<String>,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let b = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

struct Section {
    virtual_address: u32,
    virtual_size: u32,
    raw_size: u32,
    raw_pointer: u32,
}

struct PeHeaders {
    machine: u16,
    sections: Vec<Section>,
    data_dirs: Vec<(u32, u32)>,
}

fn parse_headers(bytes: &[u8]) -> Option<PeHeaders> {
    if bytes.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = read_u32(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let machine = read_u16(bytes, coff)?;
    let num_sections = read_u16(bytes, coff + 2)? as usize;
    let opt_size = read_u16(bytes, coff + 16)? as usize;
    let opt = coff + 20;
    let opt_end = opt.checked_add(opt_size)?;
    if opt_end > bytes.len() {
        return None;
    }
    let (count_offset, dirs_offset) = match read_u16(bytes, opt)? {
        0x10b => (92, 96),
        0x20b => (108, 112),
        _ => return None,
    };
    let declared = read_u32(bytes, opt + count_offset)? as usize;
    let mut data_dirs = Vec::new();
    for i in 0..declared.min(16) {
        let at = opt + dirs_offset + i * 8;
        if at + 8 > opt_end {
            break;
        }
        data_dirs.push((read_u32(bytes, at)?, read_u32(bytes, at + 4)?));
    }
    let mut sections = Vec::with_capacity(num_sections.min(96));
    for i in 0..num_sections {
        let at = opt_end.checked_add(i.checked_mul(40)?)?;
        sections.push(Section {
            virtual_size: read_u32(bytes, at + 8)?,
            virtual_address: read_u32(bytes, at + 12)?,
            raw_size: read_u32(bytes, at + 16)?,
            raw_pointer: read_u32(bytes, at + 20)?,
        });
    }
    Some(PeHeaders {
        machine,
        sections,
        data_dirs,
    })
}

impl PeHeaders {
    fn rva_to_offset(&self, rva: u32, len: usize) -> Option<usize> {
        for s in &self.sections {
            let extent = s.virtual_size.max(s.raw_size);
            let end = s.virtual_address.checked_add(extent)?;
            if rva >= s.virtual_address && rva < end {
                let delta = rva - s.virtual_address;
                if u64::from(delta) + (len as u64) > u64::from(s.raw_size) {
                    return None;
                }
                return (s.raw_pointer as usize).checked_add(delta as usize);
            }
        }
        None
    }

    fn read_name(&self, bytes: &[u8], rva: u32) -> Option<String> {
        let start = self.rva_to_offset(rva, 1)?;
        let tail = bytes.get(start..)?;
        let limit = tail.len().min(MAX_NAME_LEN);
        let nul = tail[..limit].iter().position(|&b| b == 0)?;
        let name = &tail[..nul];
        if name.is_empty() || !name.is_ascii() {
            return None;
        }
        Some(String::from_utf8_lossy(name).into_owned())
    }
}

/// Returns the COFF `Machine` field of a PE image, or `None` if `bytes` isn't a valid PE.
#[must_use]
pub fn pe_machine(bytes: &[u8]) -> Option<u16> {
    parse_headers(bytes).map(|h| h.machine)
}

/// Returns the DLL names imported by a PE image (regular and delay-load imports, in table order).
/// Returns `None` for malformed input. Never panics.
#[must_use]
pub fn pe_imported_dll_names(bytes: &[u8]) -> Option<Vec<String>> {
    let headers = parse_headers(bytes)?;
    let mut names = Vec::new();

    if let Some(&(rva, _size)) = headers.data_dirs.get(IMPORT_DIRECTORY_INDEX)
        && rva != 0
    {
        for i in 0..MAX_DESCRIPTORS {
            let desc_rva = rva.checked_add((i * 20) as u32)?;
            let at = headers.rva_to_offset(desc_rva, 20)?;
            let desc = bytes.get(at..at + 20)?;
            if desc.iter().all(|&b| b == 0) {
                break;
            }
            let name_rva = read_u32(bytes, at + 12)?;
            names.push(headers.read_name(bytes, name_rva)?);
        }
    }

    if let Some(&(rva, _size)) = headers.data_dirs.get(DELAY_IMPORT_DIRECTORY_INDEX)
        && rva != 0
    {
        for i in 0..MAX_DESCRIPTORS {
            let desc_rva = rva.checked_add((i * 32) as u32)?;
            let at = headers.rva_to_offset(desc_rva, 32)?;
            let desc = bytes.get(at..at + 32)?;
            if desc.iter().all(|&b| b == 0) {
                break;
            }
            let attributes = read_u32(bytes, at)?;
            let name_field = read_u32(bytes, at + 4)?;
            // Old-style (attributes bit 0 clear) descriptors hold VAs; without resolving the
            // image base we can only use RVA-form descriptors, which is what all modern
            // toolchains emit.
            if attributes & 1 == 0 {
                continue;
            }
            names.push(headers.read_name(bytes, name_field)?);
        }
    }

    Some(names)
}

const RUNTIME_PREFIXES: &[&str] = &[
    "libwinpthread",
    "libgcc_s_",
    "libstdc++",
    "libc++",
    "libunwind",
    "libgomp",
    "libssp",
    "libquadmath",
    "libclang_rt.asan_dynamic",
    "libclang_rt.ubsan_dynamic",
    "libclang_rt.tsan_dynamic",
    "libclang_rt.msan_dynamic",
];

const SYSTEM_DLLS: &[&str] = &[
    "kernel32", "ntdll", "msvcrt", "user32", "advapi32", "ws2_32", "shell32", "ole32", "oleaut32",
    "gdi32", "comdlg32", "comctl32", "bcrypt", "crypt32",
];

/// Whether `name` is a toolchain runtime DLL that should be deployed next to the output.
#[must_use]
pub fn is_deployable_runtime(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    std::path::Path::new(&lower)
        .extension()
        .is_some_and(|e| e == "dll")
        && !is_system_dll(&lower)
        && RUNTIME_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Whether `name` is an OS-provided DLL that must never be deployed.
#[must_use]
pub fn is_system_dll(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("api-ms-win-") || lower.starts_with("ext-ms-") {
        return true;
    }
    let stem = lower.strip_suffix(".dll").unwrap_or(&lower);
    SYSTEM_DLLS.contains(&stem)
}

/// The MinGW triple directory name (`x86_64-w64-mingw32`, ...) for a PE machine type.
#[must_use]
pub fn mingw_triple_dir(machine: u16) -> Option<&'static str> {
    match machine {
        IMAGE_FILE_MACHINE_AMD64 => Some("x86_64-w64-mingw32"),
        IMAGE_FILE_MACHINE_ARM64 => Some("aarch64-w64-mingw32"),
        IMAGE_FILE_MACHINE_I386 => Some("i686-w64-mingw32"),
        _ => None,
    }
}

/// Directories searched for runtime DLLs, in priority order: `runtime_dir` (from
/// [`RUNTIME_DIR_ENV`]) first, then `<root>/<arch_triple_dir>/bin` and `<root>/bin`, where
/// `<root>` is the parent of the directory containing `linker`. `arch_triple_dir` is e.g.
/// `x86_64-w64-mingw32` (see [`mingw_triple_dir`]); empty skips the triple directory.
pub fn runtime_search_dirs(
    linker: Option<&Path>,
    runtime_dir: Option<&Path>,
    arch_triple_dir: &str,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = runtime_dir {
        dirs.push(dir.to_path_buf());
    }
    if let Some(root) = linker.and_then(Path::parent).and_then(Path::parent) {
        if !arch_triple_dir.is_empty() {
            dirs.push(root.join(arch_triple_dir).join("bin"));
        }
        dirs.push(root.join("bin"));
    }
    dirs
}

fn locate(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let lower = name.to_ascii_lowercase();
    for dir in dirs {
        let exact = dir.join(name);
        if exact.is_file() {
            return Some(exact);
        }
        let lowered = dir.join(&lower);
        if lowered.is_file() {
            return Some(lowered);
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                if file_name.to_string_lossy().to_ascii_lowercase() == lower
                    && entry.path().is_file()
                {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

type ImportCache = Mutex<HashMap<(PathBuf, u64, SystemTime), Vec<String>>>;

fn import_cache() -> &'static ImportCache {
    static CACHE: OnceLock<ImportCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_imports(path: &Path) -> Result<Vec<String>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let key = (path.to_path_buf(), meta.len(), mtime);
    if let Ok(cache) = import_cache().lock()
        && let Some(hit) = cache.get(&key)
    {
        return Ok(hit.clone());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let imports = pe_imported_dll_names(&bytes)
        .ok_or_else(|| format!("{}: not a valid PE image", path.display()))?;
    if let Ok(mut cache) = import_cache().lock() {
        cache.insert(key, imports.clone());
    }
    Ok(imports)
}

enum CopyOutcome {
    Deployed,
    UpToDate,
}

fn deploy_one(source: &Path, dest: &Path) -> Result<CopyOutcome, String> {
    let src_meta = std::fs::metadata(source).map_err(|e| format!("{}: {e}", source.display()))?;
    if let Ok(dst_meta) = std::fs::metadata(dest)
        && dst_meta.len() == src_meta.len()
        && let (Ok(dm), Ok(sm)) = (dst_meta.modified(), src_meta.modified())
        && dm >= sm
    {
        return Ok(CopyOutcome::UpToDate);
    }
    let dir = dest
        .parent()
        .ok_or_else(|| format!("{}: no parent directory", dest.display()))?;
    let file_name = dest
        .file_name()
        .ok_or_else(|| format!("{}: no file name", dest.display()))?;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(file_name);
    tmp_name.push(format!(".reld-tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let _ = std::fs::remove_file(&tmp);

    let staged = std::fs::hard_link(source, &tmp)
        .or_else(|_| std::fs::copy(source, &tmp).map(|_| ()))
        .and_then(|()| std::fs::rename(&tmp, dest));
    match staged {
        Ok(()) => Ok(CopyOutcome::Deployed),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!(
                "failed to deploy {} to {}: {e}",
                source.display(),
                dest.display()
            ))
        }
    }
}

fn is_set(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> bool {
    env(name).is_some_and(|v| !v.is_empty())
}

/// Deploys the runtime DLLs needed by `output` next to it, reading settings from the process
/// environment. See [`deploy_runtime_dlls_with_env`].
#[must_use]
pub fn deploy_runtime_dlls(output: &Path, linker: Option<&Path>) -> DeployReport {
    deploy_runtime_dlls_with_env(output, linker, &|name| std::env::var_os(name))
}

/// Like [`deploy_runtime_dlls`] but with an injectable environment lookup (for tests).
pub fn deploy_runtime_dlls_with_env(
    output: &Path,
    linker: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> DeployReport {
    let mut report = DeployReport::default();
    let ext = output
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext != "exe" && ext != "dll" {
        return report;
    }
    if is_set(env, NO_DEPLOY_LIBS_ENV) || is_set(env, NO_AUTO_ENV) {
        return report;
    }
    if ext == "dll" && is_set(env, NO_DEPLOY_SHARED_LIB_ENV) {
        return report;
    }

    let bytes = match std::fs::read(output) {
        Ok(b) => b,
        Err(e) => {
            report.warnings.push(format!("{}: {e}", output.display()));
            return report;
        }
    };
    let (Some(machine), Some(root_imports)) = (pe_machine(&bytes), pe_imported_dll_names(&bytes))
    else {
        report
            .warnings
            .push(format!("{}: not a valid PE image", output.display()));
        return report;
    };
    let runtime_dir = env(RUNTIME_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let dirs = runtime_search_dirs(
        linker,
        runtime_dir.as_deref(),
        mingw_triple_dir(machine).unwrap_or(""),
    );
    let Some(dest_dir) = output.parent() else {
        return report;
    };

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = root_imports;
    let mut index = 0;
    while index < queue.len() {
        let name = queue[index].clone();
        index += 1;
        if !is_deployable_runtime(&name) || !visited.insert(name.to_ascii_lowercase()) {
            continue;
        }
        let Some(source) = locate(&name, &dirs) else {
            report.missing.push(name);
            continue;
        };
        match cached_imports(&source) {
            Ok(imports) => queue.extend(imports),
            Err(e) => report.warnings.push(e),
        }
        let dest_name = source
            .file_name()
            .map_or_else(|| PathBuf::from(&name), PathBuf::from);
        let dest = dest_dir.join(dest_name);
        match deploy_one(&source, &dest) {
            Ok(CopyOutcome::Deployed) => report.deployed.push(dest),
            Ok(CopyOutcome::UpToDate) => report.up_to_date.push(dest),
            Err(e) => report.warnings.push(e),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal PE32+ image with one `.idata` section importing `dlls`.
    fn synthetic_pe(dlls: &[&str]) -> Vec<u8> {
        const E_LFANEW: usize = 0x40;
        const OPT_SIZE: usize = 112 + 16 * 8;
        const SECTION_RVA: u32 = 0x1000;
        const SECTION_FILE: usize = 0x200;

        let mut idata = vec![0u8; (dlls.len() + 1) * 20];
        for (i, dll) in dlls.iter().enumerate() {
            let name_rva = SECTION_RVA + idata.len() as u32;
            idata.extend_from_slice(dll.as_bytes());
            idata.push(0);
            idata[i * 20 + 12..i * 20 + 16].copy_from_slice(&name_rva.to_le_bytes());
        }
        while !idata.len().is_multiple_of(0x200) {
            idata.push(0);
        }

        let mut out = vec![0u8; SECTION_FILE];
        out[0..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&(E_LFANEW as u32).to_le_bytes());
        out[E_LFANEW..E_LFANEW + 4].copy_from_slice(b"PE\0\0");
        let coff = E_LFANEW + 4;
        out[coff..coff + 2].copy_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
        out[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        out[coff + 16..coff + 18].copy_from_slice(&(OPT_SIZE as u16).to_le_bytes());
        let opt = coff + 20;
        out[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        out[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        let import_dir = opt + 112 + 8 * IMPORT_DIRECTORY_INDEX;
        out[import_dir..import_dir + 4].copy_from_slice(&SECTION_RVA.to_le_bytes());
        out[import_dir + 4..import_dir + 8]
            .copy_from_slice(&(((dlls.len() + 1) * 20) as u32).to_le_bytes());
        let sec = opt + OPT_SIZE;
        out[sec..sec + 6].copy_from_slice(b".idata");
        out[sec + 8..sec + 12].copy_from_slice(&(idata.len() as u32).to_le_bytes());
        out[sec + 12..sec + 16].copy_from_slice(&SECTION_RVA.to_le_bytes());
        out[sec + 16..sec + 20].copy_from_slice(&(idata.len() as u32).to_le_bytes());
        out[sec + 20..sec + 24].copy_from_slice(&(SECTION_FILE as u32).to_le_bytes());
        out.extend_from_slice(&idata);
        out
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "reld-runtime-dlls-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn parses_synthetic_imports() {
        let pe = synthetic_pe(&["libc++.dll", "KERNEL32.dll"]);
        assert_eq!(
            pe_imported_dll_names(&pe),
            Some(vec!["libc++.dll".to_owned(), "KERNEL32.dll".to_owned()])
        );
        assert_eq!(pe_machine(&pe), Some(IMAGE_FILE_MACHINE_AMD64));
    }

    #[test]
    fn malformed_input_returns_none() {
        let pe = synthetic_pe(&["libc++.dll", "KERNEL32.dll"]);
        for len in [0, 1, 2, 0x3c, 0x44, 0x60, 0x150, 0x200, 0x210] {
            assert_eq!(pe_imported_dll_names(&pe[..len]), None, "len {len}");
        }
        assert_eq!(pe_imported_dll_names(b"garbage garbage garbage"), None);
        let mut bad = pe;
        bad[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(pe_imported_dll_names(&bad), None);
    }

    #[test]
    fn filters() {
        assert!(is_deployable_runtime("libc++.dll"));
        assert!(is_deployable_runtime("LIBWINPTHREAD-1.DLL"));
        assert!(is_deployable_runtime("libgcc_s_seh-1.dll"));
        assert!(is_deployable_runtime("libclang_rt.asan_dynamic-x86_64.dll"));
        assert!(!is_deployable_runtime("KERNEL32.dll"));
        assert!(!is_deployable_runtime("libc++.a"));
        assert!(!is_deployable_runtime("foo.dll"));
        assert!(is_system_dll("KERNEL32.dll"));
        assert!(is_system_dll("api-ms-win-crt-runtime-l1-1-0.dll"));
        assert!(is_system_dll("ext-ms-win-foo.dll"));
        assert!(!is_system_dll("libunwind.dll"));
    }

    #[test]
    fn search_dirs_order() {
        let dirs = runtime_search_dirs(
            Some(Path::new("/tc/bin/ld.lld")),
            Some(Path::new("/rt")),
            "x86_64-w64-mingw32",
        );
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/rt"),
                PathBuf::from("/tc/x86_64-w64-mingw32/bin"),
                PathBuf::from("/tc/bin"),
            ]
        );
    }

    fn fake_toolchain(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = unique_temp_dir(tag);
        let bin = root.join("bin");
        let triple_bin = root.join("x86_64-w64-mingw32").join("bin");
        let out_dir = root.join("out");
        for d in [&bin, &triple_bin, &out_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        let linker = bin.join("ld.lld");
        std::fs::write(&linker, b"").unwrap();
        std::fs::write(
            triple_bin.join("libc++.dll"),
            synthetic_pe(&["libunwind.dll", "KERNEL32.dll"]),
        )
        .unwrap();
        std::fs::write(
            triple_bin.join("libunwind.dll"),
            synthetic_pe(&["KERNEL32.dll"]),
        )
        .unwrap();
        let output = out_dir.join("app.exe");
        std::fs::write(&output, synthetic_pe(&["libc++.dll", "KERNEL32.dll"])).unwrap();
        (root, linker, output)
    }

    #[test]
    fn deploys_transitively_then_up_to_date() {
        let (root, linker, output) = fake_toolchain("deploy");
        let out_dir = output.parent().unwrap().to_path_buf();
        let expected = vec![out_dir.join("libc++.dll"), out_dir.join("libunwind.dll")];

        let first = deploy_runtime_dlls_with_env(&output, Some(&linker), &no_env);
        assert_eq!(first.deployed, expected, "{first:?}");
        assert!(
            first.missing.is_empty() && first.warnings.is_empty(),
            "{first:?}"
        );
        for p in &expected {
            assert!(p.is_file());
        }

        let second = deploy_runtime_dlls_with_env(&output, Some(&linker), &no_env);
        assert!(second.deployed.is_empty(), "{second:?}");
        assert_eq!(second.up_to_date, expected);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn opt_out_env_vars() {
        let (root, linker, output) = fake_toolchain("optout");
        for var in [NO_DEPLOY_LIBS_ENV, NO_AUTO_ENV] {
            let env = move |name: &str| (name == var).then(|| OsString::from("1"));
            let report = deploy_runtime_dlls_with_env(&output, Some(&linker), &env);
            assert_eq!(report, DeployReport::default());
        }
        // RELD_NO_DEPLOY_SHARED_LIB only affects .dll outputs.
        let env = |name: &str| (name == NO_DEPLOY_SHARED_LIB_ENV).then(|| OsString::from("1"));
        let dll_output = output.with_extension("dll");
        std::fs::copy(&output, &dll_output).unwrap();
        let report = deploy_runtime_dlls_with_env(&dll_output, Some(&linker), &env);
        assert_eq!(report, DeployReport::default());
        let report = deploy_runtime_dlls_with_env(&output, Some(&linker), &env);
        assert_eq!(report.deployed.len(), 2);

        // Non-PE extensions are skipped.
        let elf = output.with_extension("so");
        std::fs::copy(&output, &elf).unwrap();
        assert_eq!(
            deploy_runtime_dlls_with_env(&elf, Some(&linker), &no_env),
            DeployReport::default()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
