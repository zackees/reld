"""Link an ELF output larger than 2 GiB and prove it is well formed (reld#16).

The bugs this catches are threshold bugs — a `u32` offset, a signed `i32` addend, a `p_filesz`
that wrapped — and they only appear past sizes no ordinary fixture reaches. Every-PR CI cannot
pay for it: the inputs alone are gigabytes. So this runs on a schedule, from
`.github/workflows/scale-gate-linux.yml`.

The payload is assembly rather than C: `.fill` emits real bytes into `.rodata` without asking a
compiler to chew through a multi-gigabyte initializer, and every chunk is referenced from the
entry point so nothing is free to drop it.

Structural validation is done here rather than by shelling out to `readelf`, both so a truncated
or overflowed output is reported as the specific field that is wrong and so the checks are
testable without building a 2 GiB file.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import struct
import subprocess
import sys
import time
from dataclasses import dataclass
from dataclasses import field
from pathlib import Path

GIB = 1024**3
MIB = 1024**2

#: The threshold the gate exists to cross. An output at or below this proves nothing.
MINIMUM_OUTPUT_BYTES = 2 * GIB

#: Headroom over the threshold, so a layout that rounds sections down still clears 2 GiB.
DEFAULT_TARGET_BYTES = MINIMUM_OUTPUT_BYTES + 256 * MIB

DEFAULT_CHUNK_BYTES = 64 * MIB

#: What the linked binary exits with when it has read every chunk and found the right bytes. A
#: distinctive value, so a binary that dies before reaching our code cannot pass by exiting 0.
SUCCESS_EXIT_CODE = 42


class ScaleGateError(RuntimeError):
    """A failure that must fail the gate: truncation, overflow, panic, malformed output."""


@dataclass
class Metrics:
    """What the run records even when it passes, so a slow drift is visible in the log."""

    output_bytes: int = 0
    link_wall_seconds: float = 0.0
    link_peak_rss_bytes: int = 0
    chunk_count: int = 0
    chunk_bytes: int = 0
    runner_image: str = ""
    runner_image_version: str = ""
    reld_commit: str = ""
    platform: str = ""
    executed: bool = False
    extra: dict[str, str] = field(default_factory=dict)

    def as_json(self) -> str:
        return json.dumps(self.__dict__, indent=2, sort_keys=True)


def plan_chunks(target_bytes: int, chunk_bytes: int) -> int:
    """How many payload objects are needed to push the output past `target_bytes`."""
    if chunk_bytes <= 0:
        raise ValueError("chunk_bytes must be positive")
    if target_bytes < MINIMUM_OUTPUT_BYTES:
        raise ValueError(
            f"target {target_bytes} is below the {MINIMUM_OUTPUT_BYTES} byte threshold this gate exists to cross"
        )
    return -(-target_bytes // chunk_bytes)


def chunk_source(index: int, chunk_bytes: int) -> str:
    """Assembly for one payload object.

    Each chunk is its own section with a distinct fill byte, so a chunk laid down at the wrong
    offset shows up as the wrong value rather than as zeros, which a hole would also produce.
    """
    fill = (index % 251) + 1
    return (
        f'\t.section .rodata.chunk{index},"a",@progbits\n'
        f"\t.globl chunk{index}\n"
        f"\t.type chunk{index}, @object\n"
        f"\t.size chunk{index}, {chunk_bytes}\n"
        f"chunk{index}:\n"
        f"\t.fill {chunk_bytes}, 1, {fill}\n"
    )


def driver_source(chunk_count: int) -> str:
    """A freestanding `_start` that touches every chunk, so the link cannot quietly drop one.

    It sums the first byte of each chunk and compares against the same sum computed here. Reading
    one byte per chunk keeps the run near-instant while still forcing every chunk to be mapped.

    There is no libc. An image this size does not fit the small code model, and the distributed
    crt objects are built for it: `crtbegin.o` takes an `R_X86_64_32S` to a symbol that 2 GiB of
    payload has pushed out of range, and the link fails before reld's own layout is exercised.
    That failure is about the C runtime's code model, not about the linker, so the gate drops the
    runtime, builds the payload with `-mcmodel=large`, and exits by syscall.
    """
    externs = "\n".join(f"extern const unsigned char chunk{i};" for i in range(chunk_count))
    terms = " + ".join(f"chunk{i}" for i in range(chunk_count))
    expected = sum((i % 251) + 1 for i in range(chunk_count))
    return (
        f"{externs}\n\n"
        "__attribute__((noreturn)) static void exit_syscall(int code) {\n"
        "#if defined(__x86_64__)\n"
        '  __asm__ volatile("syscall" :: "a"(60L), "D"((long)code) : "memory");\n'
        "#elif defined(__aarch64__)\n"
        '  register long w8 __asm__("x8") = 93;\n'
        '  register long x0 __asm__("x0") = code;\n'
        '  __asm__ volatile("svc #0" :: "r"(w8), "r"(x0) : "memory");\n'
        "#else\n"
        '#error "the scale gate exits by syscall and knows only x86_64 and aarch64"\n'
        "#endif\n"
        "  __builtin_unreachable();\n"
        "}\n\n"
        "void _start(void) {\n"
        f"  unsigned long sum = {terms};\n"
        f"  exit_syscall(sum == {expected}UL ? {SUCCESS_EXIT_CODE} : 1);\n"
        "}\n"
    )


def _run(command: list[str], *, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise ScaleGateError(
            f"`{' '.join(command[:3])} …` exited {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result


def build_inputs(workdir: Path, chunk_count: int, chunk_bytes: int, cc: str) -> list[Path]:
    objects: list[Path] = []
    for index in range(chunk_count):
        source = workdir / f"chunk{index}.s"
        obj = workdir / f"chunk{index}.o"
        source.write_text(chunk_source(index, chunk_bytes), encoding="utf-8")
        _run([cc, "-c", "-o", str(obj), str(source)])
        # The assembly is regenerated cheaply; the object is what costs disk.
        source.unlink()
        objects.append(obj)

    driver = workdir / "driver.c"
    driver_obj = workdir / "driver.o"
    driver.write_text(driver_source(chunk_count), encoding="utf-8")
    _run([cc, "-c", "-mcmodel=large", "-ffreestanding", "-fno-builtin", "-o", str(driver_obj), str(driver)])
    return [driver_obj, *objects]


def run_measured(command: list[str], workdir: Path) -> tuple[subprocess.CompletedProcess[str], float, int]:
    """Run a command and return its result, wall seconds, and *its own* peak RSS.

    `getrusage(RUSAGE_CHILDREN)` would not do: it is a high-water mark across every child this
    process has ever reaped, so after assembling dozens of 64 MiB objects it reports whichever of
    them peaked highest, not the link. `os.wait4` gives the rusage of one specific child, which is
    the number reld#16 asks to record.
    """
    out_path = workdir / "link.stdout"
    err_path = workdir / "link.stderr"
    started = time.monotonic()
    with out_path.open("wb") as out, err_path.open("wb") as err:
        process = subprocess.Popen(command, stdout=out, stderr=err)
        _, status, usage = os.wait4(process.pid, 0)
    elapsed = time.monotonic() - started
    # Popen must not try to reap a child that no longer exists.
    process.returncode = os.waitstatus_to_exitcode(status)

    result = subprocess.CompletedProcess(
        command,
        process.returncode,
        out_path.read_text(encoding="utf-8", errors="replace"),
        err_path.read_text(encoding="utf-8", errors="replace"),
    )
    out_path.unlink()
    err_path.unlink()
    # ru_maxrss is KiB on Linux.
    return result, elapsed, usage.ru_maxrss * 1024


def link(reld: Path, cc: str, objects: list[Path], output: Path) -> tuple[float, int]:
    """Link through the driver with `--ld-path`, and return wall seconds and peak RSS.

    reld is a pure linker, so it is always driven by the compiler driver rather than invoked with
    `-C linker=`.
    """
    result, elapsed, peak_rss = run_measured(
        [
            cc,
            f"--ld-path={reld}",
            "-o",
            str(output),
            *[str(o) for o in objects],
            "-nostdlib",
            "-static",
            "-no-pie",
        ],
        output.parent,
    )

    combined = f"{result.stdout}\n{result.stderr}"
    if "panicked at" in combined or "RUST_BACKTRACE" in combined:
        raise ScaleGateError(f"reld panicked while linking a {MINIMUM_OUTPUT_BYTES // GIB} GiB output:\n{combined}")
    if result.returncode != 0:
        raise ScaleGateError(f"link failed with status {result.returncode}:\n{combined}")
    if not output.exists():
        raise ScaleGateError("link reported success but wrote no output")

    return elapsed, peak_rss


@dataclass
class ElfSummary:
    file_bytes: int
    section_count: int
    segment_count: int
    max_section_end: int
    max_segment_end: int


def validate_elf(path: Path) -> ElfSummary:
    """Check that every header in a 64-bit little-endian ELF addresses bytes that exist.

    A truncated or overflowed >2 GiB output shows up here as a section or segment whose
    `offset + size` runs past the end of the file, or as a `u32` field that wrapped and now
    points backwards.
    """
    file_bytes = path.stat().st_size
    with path.open("rb") as handle:
        header = handle.read(64)
        if len(header) < 64 or header[:4] != b"\x7fELF":
            raise ScaleGateError(f"{path} is not an ELF file")
        if header[4] != 2:
            raise ScaleGateError(f"{path} is not ELFCLASS64")
        if header[5] != 1:
            raise ScaleGateError(f"{path} is not little-endian; this gate is x86_64/aarch64 only")

        e_phoff, e_shoff = struct.unpack_from("<QQ", header, 32)
        e_phentsize, e_phnum, e_shentsize, e_shnum = struct.unpack_from("<HHHH", header, 54)

        if e_shnum and e_shoff + e_shnum * e_shentsize > file_bytes:
            raise ScaleGateError(
                f"section header table ends at {e_shoff + e_shnum * e_shentsize}, past the {file_bytes} byte file"
            )
        if e_phnum and e_phoff + e_phnum * e_phentsize > file_bytes:
            raise ScaleGateError(
                f"program header table ends at {e_phoff + e_phnum * e_phentsize}, past the {file_bytes} byte file"
            )

        max_section_end = 0
        for index in range(e_shnum):
            handle.seek(e_shoff + index * e_shentsize)
            entry = handle.read(e_shentsize)
            sh_type = struct.unpack_from("<I", entry, 4)[0]
            sh_offset, sh_size = struct.unpack_from("<QQ", entry, 24)
            if sh_type == 8:  # SHT_NOBITS occupies no file space.
                continue
            end = sh_offset + sh_size
            if end > file_bytes:
                raise ScaleGateError(
                    f"section {index} spans [{sh_offset}, {end}) but the file is {file_bytes} bytes: truncated output"
                )
            max_section_end = max(max_section_end, end)

        max_segment_end = 0
        for index in range(e_phnum):
            handle.seek(e_phoff + index * e_phentsize)
            entry = handle.read(e_phentsize)
            p_type = struct.unpack_from("<I", entry, 0)[0]
            p_offset, _p_vaddr, _p_paddr, p_filesz, p_memsz = struct.unpack_from("<QQQQQ", entry, 8)
            if p_type == 1 and p_filesz > p_memsz:  # PT_LOAD
                raise ScaleGateError(
                    f"segment {index} has p_filesz {p_filesz} greater than p_memsz {p_memsz}: overflowed field"
                )
            end = p_offset + p_filesz
            if end > file_bytes:
                raise ScaleGateError(
                    f"segment {index} spans [{p_offset}, {end}) but the file is {file_bytes} bytes: truncated output"
                )
            max_segment_end = max(max_segment_end, end)

    return ElfSummary(
        file_bytes=file_bytes,
        section_count=e_shnum,
        segment_count=e_phnum,
        max_section_end=max_section_end,
        max_segment_end=max_segment_end,
    )


def execute(output: Path) -> None:
    result = subprocess.run([str(output)], capture_output=True, text=True, check=False)
    if result.returncode != SUCCESS_EXIT_CODE:
        raise ScaleGateError(
            f"the linked binary exited {result.returncode}, expected {SUCCESS_EXIT_CODE}\n"
            f"{result.stdout}\n{result.stderr}"
        )


def reld_commit(reld: Path) -> str:
    result = subprocess.run([str(reld), "--version"], capture_output=True, text=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def run_gate(
    reld: Path,
    workdir: Path,
    *,
    cc: str = "clang",
    target_bytes: int = DEFAULT_TARGET_BYTES,
    chunk_bytes: int = DEFAULT_CHUNK_BYTES,
    run_output: bool = True,
) -> Metrics:
    chunk_count = plan_chunks(target_bytes, chunk_bytes)
    workdir.mkdir(parents=True, exist_ok=True)
    output = workdir / "scale-gate.exe"

    metrics = Metrics(
        chunk_count=chunk_count,
        chunk_bytes=chunk_bytes,
        runner_image=os.environ.get("ImageOS", ""),
        runner_image_version=os.environ.get("ImageVersion", ""),
        reld_commit=reld_commit(reld),
        platform=platform.platform(),
    )

    objects = build_inputs(workdir, chunk_count, chunk_bytes, cc)
    metrics.link_wall_seconds, metrics.link_peak_rss_bytes = link(reld, cc, objects, output)

    # The inputs are gigabytes and nothing else needs them; the runner's disk does.
    for obj in objects:
        obj.unlink()

    summary = validate_elf(output)
    metrics.output_bytes = summary.file_bytes
    if summary.file_bytes <= MINIMUM_OUTPUT_BYTES:
        raise ScaleGateError(
            f"output is {summary.file_bytes} bytes, at or below the {MINIMUM_OUTPUT_BYTES} byte threshold: "
            "the gate did not test what it exists to test"
        )

    if run_output:
        execute(output)
        metrics.executed = True

    metrics.extra["sections"] = str(summary.section_count)
    metrics.extra["segments"] = str(summary.segment_count)
    return metrics


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reld", type=Path, default=Path("target/release/reld"))
    parser.add_argument("--workdir", type=Path, default=Path("target/scale-gate"))
    parser.add_argument("--cc", default="clang")
    parser.add_argument("--target-bytes", type=int, default=DEFAULT_TARGET_BYTES)
    parser.add_argument("--chunk-bytes", type=int, default=DEFAULT_CHUNK_BYTES)
    parser.add_argument(
        "--no-run",
        action="store_true",
        help="skip executing the linked binary, for a host that cannot run what it links",
    )
    parser.add_argument("--json-out", type=Path, help="write the recorded metrics here")
    args = parser.parse_args(argv)

    try:
        metrics = run_gate(
            args.reld.resolve(),
            args.workdir,
            cc=args.cc,
            target_bytes=args.target_bytes,
            chunk_bytes=args.chunk_bytes,
            run_output=not args.no_run,
        )
    except ScaleGateError as error:
        # The workdir is left in place: the runner is ephemeral, and a multi-gigabyte output that
        # is wrong is the only copy of the evidence.
        print(f"scale gate failed: {error}", file=sys.stderr)
        return 1

    shutil.rmtree(args.workdir, ignore_errors=True)
    print(metrics.as_json())
    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(metrics.as_json(), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
