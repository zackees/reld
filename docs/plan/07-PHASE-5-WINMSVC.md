# Phase 5 — MSVC ABI (`x86_64-pc-windows-msvc`)

Most of the COFF machinery already exists from Phase 3. This phase is the MSVC **argument
dialect**, the MSVC **CRT conventions**, and the **debug-information decision**.

---

## P5-T0 — Resolve D5 before any other task in this phase

**Do not start P5-T1 until this is decided and written into `01-DECISIONS.md`.**

The problem: a dev-loop linker whose output cannot be debugged is not a dev-loop linker, and
MSVC-ABI debugging means PDB. The evidence:

- radlink's PDB/CodeView subsystem is ~20k lines — larger than its entire linking core.
- The Rust ecosystem's `pdb` crates are **read-oriented**; writing is not a solved dependency.
- **Nobody has ever shipped incremental PDB generation** except Microsoft's `/DEBUG:FASTLINK`,
  which was deprecated and removed. Zig has never implemented PDB at all (issue #6031, open
  since 2020).

Options, in the order I would consider them:

| Option | Cost | Consequence |
|---|---|---|
| (a) Write PDB | ~15–20k LOC, months | Full VS/WinDbg debugging. Dominates the phase. |
| (b) DWARF-in-PE | small — Phase 3 already does it | LLDB works, VS and WinDbg do not. |
| (c) No debug info | zero | MSVC ABI usable for release-shaped builds only, which contradicts reld's entire premise. |

Recommendation: **(b) for v0, (a) only if user demand proves it necessary.** Option (b) keeps
the phase small and honest; option (c) should be rejected outright — it produces a Windows
linker nobody can debug with, which is worse than not shipping the target.

Whatever is chosen must be stated plainly in the README, not buried in an issue.

## P5-T1 — `link.exe` argument dialect

The `reld-link` / `-flavor link` path. Roughly 40 switches matter for rustc and MSVC C++:
`/OUT: /LIBPATH: /DEFAULTLIB: /NODEFAULTLIB /ENTRY: /SUBSYSTEM: /DLL /DEF: /EXPORT: /INCLUDE:
/ALTERNATENAME: /MACHINE: /DEBUG /PDB: /OPT:REF /OPT:NOREF /OPT:ICF /OPT:NOICF /INCREMENTAL
/BASE: /ALIGN: /MERGE: /SECTION: /STACK: /WHOLEARCHIVE: /MANIFEST*`.

Case-insensitive, `@response` files, and `.drectve` directives embedded in objects (MSVC emits
`/DEFAULTLIB:` there — this is how the CRT gets pulled in without being named on the command
line). radlink's `lnk_parse_msvc_linker_directive` (`lnk_obj.c:848`) is the reference.

Unknown switches must warn or error per an explicit policy, never be silently dropped.

`Acceptance:` `reld-link /?` lists supported switches; rustc's full MSVC link line parses.

## P5-T2 — MSVC CRT conventions

- Entry points: `mainCRTStartup`, `wmainCRTStartup`, `WinMainCRTStartup`, `_DllMainCRTStartup`.
- `.CRT$XC*` / `.CRT$XI*` initializer ordering (the MSVC analogue of `.ctors`, and already
  handled by the grouped-section `$` ordering from P3-T8).
- `_load_config_used` → Load Config data directory.
- `__ImageBase`, `_tls_used`.
- Default library selection: `libcmt` / `msvcrt` variants pulled via `.drectve`.

`Acceptance:` a C++ fixture with static initializers, built by `cl.exe`, links and runs.

## P5-T3 — `/OPT:REF` parity and `/INCREMENTAL` handling

`/OPT:REF` maps to P3-T18. `/OPT:ICF` is **rejected** per D11 — accept the flag and warn that it
is ignored, since MSBuild passes it unconditionally and erroring would break every build.

`/INCREMENTAL` is Microsoft's own incremental scheme (padding + thunks + `.ilk` files) and is
**not** reld's. Accept and ignore it with a warning; reld's incremental path is orthogonal and
is selected by the daemon, not by this flag.

`Acceptance:` an MSBuild-generated link line completes with warnings but no errors.

## P5-T4 — Phase gate

On the `windows-latest` MSVC runner:

1. `cl.exe`-built hello-world links and runs, exit code 42.
2. `cargo build --target x86_64-pc-windows-msvc` linked by reld produces a running binary.
3. Debug story works per whichever D5 option was chosen, with a scripted test proving it.
4. Differential oracle green against `lld-link`.
5. `reld-difftest --seeds 500` green.

`Acceptance:` `ci/gate-winmsvc.sh` exits 0.
