# Phase 5 — MinGW ABI (`x86_64-pc-windows-gnu`)

> **RESTRUCTURED after adversarial review.** This was originally "Phase 5 — MSVC." D3 is revised:
> msvc is now Phase 3 and carries the COFF core; MinGW lands here as an **additive** phase.

The COFF core — parsing, COMDAT, weak externals, relocations, base relocations, the writer,
`.pdata`, `.rsrc`, imports/exports, TLS, `--gc-sections` — already exists from Phase 3. This
phase adds what is specific to the GNU toolchain, and the review showed that list is
substantially longer than originally believed.

## P5-T1 — GNU argument dialect, and the `Args::new()` pre-scan ⚠️ blocked work

MinGW drives a **GNU-syntax `ld`**, not `link.exe`. The original plan proposed selecting the
COFF platform via `-m i386pep`; **that is architecturally impossible** (R2). wild picks the
`Args` variant in `Args::new()` (`args.rs:113-149`) from argv[0], a literal `-flavor` at argv[1],
or the host — before any parsing. `-m` is a sub-option inside the ELF parser whose closures take
`&mut ElfArgs`.

Resolve by either a pre-scan of argv before `Args::new()`, or a `CoffArgs` hosting the whole GNU
dialect. Either way the shared-dialect refactor — making the parser combinators generic over the
args struct — is real work that the original plan assumed was free.

**Flags rustc actually passes** (measured), none of which were in the original list:
`-l:libpthread.a` (literal-filename form), `-Bstatic`/`-Bdynamic`, `--nxcompat`, `--dynamicbase`,
`--high-entropy-va`, `--disable-auto-image-base`, `--gc-sections`, `-no-pie`. rustc invokes
`x86_64-w64-mingw32-gcc`, not `gcc`. Plain `gcc`/CMake additionally pass **18 `-plugin-opt=`
arguments** (rustc dodges this with `-fno-use-linker-plugin`), which must be accepted and
diagnosed per D13 rather than aborting the link on argument one.

`Acceptance:` `cargo test --test acceptance -- coff/gnu-dialect`

## P5-T2 — Auto-import and runtime pseudo-relocs ⚠️ REQUIRED (D10 revised)

Refuted by measurement: a plain `g++` hello-world using `std::string` and exceptions **fails to
link** without these, because `libstdc++-6.dll` data exports are referenced from constant
initializers and MinGW headers do not mark them `dllimport`.

Implement IAT redirection of data references plus `--enable-runtime-pseudo-reloc` v2 emission
into `.rdata_runtime_pseudo_reloc`. ~200 LOC. Reference `lld/COFF/MinGW.cpp` and mingw-w64's
`pseudo-reloc.c`. **radlink gives zero help.**

`Acceptance:` a C++ fixture with `std::string` and a thrown exception links and returns 42.

## P5-T3 — Default `i386pep` linker-script emulation

`-T` is still rejected (verified: MinGW gcc never passes it). But the built-in script **provides
~25 symbols the CRT undefined-references** — `__CTOR_LIST__`, `__DTOR_LIST__`,
`__RUNTIME_PSEUDO_RELOC_LIST__(_END__)`, `__rt_psrelocs_{start,end,size}`, `__data_start__`,
`__bss_start__`, `__IAT_{start,end}__`, `___crt_x{c,i,l,p,t}_{start,end}__`, `___tls_{start,end}__`,
`etext`, `end` — verified consumed via `nm libmingw32.a`. Each missing one is a hard
undefined-reference failure on hello-world.

Its `KEEP` set doubles as the `--gc-sections` root set: `.idata$2..$7`, `.CRT$X*`, `.ctors*`,
`.dtors*`, `.rsrc*`, `.rdata_runtime_pseudo_reloc`. Miss any and gc silently deletes the import
descriptors or the CRT initializer table. **rustc passes `--gc-sections` by default on
windows-gnu**, so this is not optional.

`Acceptance:` `cargo test --test acceptance -- coff/gnu-script-symbols`

## P5-T4 — CRT integration: `.ctors`/`.dtors` **and** `.CRT$X*`

Both run on mingw-w64 x86_64 — the original framing (".ctors is GNU, .CRT$XC* is MSVC") was
wrong. `crt2.o` contains `.CRT$XCAA`/`.CRT$XIAA` and references `__xc_a`/`__xc_z`/`__xi_a`/
`__xi_z`, while `g++` also emits `.ctors`. Implement `__CTOR_LIST__`/`__DTOR_LIST__` sentinels
alongside the `.CRT$X*` ordering inherited from Phase 3.

Entry points: `mainCRTStartup`, `WinMainCRTStartup`, `DllMainCRTStartup`, with subsystem
inference.

`Acceptance:` a C++ fixture with a global constructor observes it having run.

## P5-T5 — Long-form import libraries

MSYS2 import libs are the **`.idata$N` dlltool form**, not short-import members — `libuser32.a`
has 983 members (head/tail/stubs). The original claim that this "falls out of grouped-section
merging for free" is false: the script's `KEEP(SORT(*)(.idata$2))`…`$7` ordering sorts by
*filename*, not by `$` suffix, and the null-terminator objects must sort last.

`Acceptance:` a fixture calling `MessageBoxA` from `user32` links and runs.

## P5-T6 — DWARF-in-COFF

The payoff for the gnu path, and **not** the trivial concatenation originally described.
`.debug_*` sections in MinGW are **COMDAT-grouped** (`.debug_frame$_ZN1GC1Ev`,
`LINK_ONCE_DISCARD`), so they participate in both `$`-grouping and COMDAT election — a losing
`.debug_frame$X` must be discarded together with its `.text$X`. Failure mode is not a link error;
it is gdb reading stale CFI.

Needs `IMAGE_REL_AMD64_SECREL` (532 occurrences measured). `.debug_frame`, not `.eh_frame` —
mingw-w64 x86_64 is SEH-only and emits **zero** `.eh_frame` across `libstdc++.a`, `libgcc.a`,
`libgcc_eh.a`, `libwinpthread.a` and `libmingwex.a`.

`Acceptance:` scripted gdb (MSYS2) sets a breakpoint by file:line and hits it. **Note this is a
lightly-tested toolchain configuration and has not yet been demonstrated once** — prove it early
rather than at the gate.

## P5-T7 — Phase gate

1. C hello-world, exit 42. 2. C++ with `std::string` + exceptions (P5-T2). 3. `cargo build
--target x86_64-pc-windows-gnu` via the **`-B<dir>` wrapper-script** mechanism — `-fuse-ld=` is
measured not to work with MinGW gcc. 4. gdb breakpoint (P5-T6). 5. Global constructor (P5-T4).
6. DLL round-trip. 7. Differential oracle green with `--coverage` at its floor and a COFF-GNU
malfunction site (D17). 8. `reld-difftest --seeds 500`.

`Acceptance:` `ci/gate-wingnu.sh` exits 0.

---

# Appendix — the MSVC debug-information decision (D5), retained here for reference

This decision belongs to Phase 3 now that msvc leads, but the analysis is unchanged.

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

`/LTCG` and `/GL`: LTO is a **deferred stretch goal** (D13), not a non-goal. MSBuild passes
`/LTCG` unconditionally in release configurations, so it must be accepted-and-diagnosed — a
message naming LTO as not yet implemented — rather than either silently ignored (which would
mislink `/GL` objects containing IL, not machine code) or hard-errored (which would break every
release build outright). Note `/GL` objects are **not linkable at all** without LTO, so that
specific case must be a hard, clear error.

`Acceptance:` an MSBuild-generated link line completes with warnings but no errors.

## P5-T4 — Phase gate

On the `windows-latest` MSVC runner:

1. `cl.exe`-built hello-world links and runs, exit code 42.
2. `cargo build --target x86_64-pc-windows-msvc` linked by reld produces a running binary.
3. Debug story works per whichever D5 option was chosen, with a scripted test proving it.
4. Differential oracle green against `lld-link`.
5. `reld-difftest --seeds 500` green.

`Acceptance:` `ci/gate-winmsvc.sh` exits 0.
