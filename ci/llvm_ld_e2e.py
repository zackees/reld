"""End-to-end proof that reld links Windows COFF in-process through llvm-ld-coff (reld#96).

llvm-ld-coff is LLD's COFF linker as a shared library; it links Windows PE/COFF on any host, so
this runs on Linux (as a Windows cross-link) and on Windows alike:

1. compile a freestanding COFF object with clang (no SDK or CRT needed);
2. link it with `reld-link` and the pinned library, and require the engine log to say the link ran
   in-process and the output to be a PE image for the right machine;
3. link it again through the `lld-link` subprocess bridge and require byte-identical output, so the
   in-process path is not merely *a* linker but the same one;
4. on a Windows host, run the result and check its exit code.
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

SOURCE = "int mainCRTStartup(void) { return 42; }\n"
LINK_ARGS = ["/entry:mainCRTStartup", "/subsystem:console", "/nodefaultlib"]
MACHINE_AMD64 = 0x8664


class E2eError(RuntimeError):
    pass


def pe_machine(image: bytes) -> int:
    if image[:2] != b"MZ":
        raise E2eError("output is not an MZ/PE image")
    pe_offset = struct.unpack_from("<I", image, 0x3C)[0]
    if image[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise E2eError("output has no PE signature")
    return struct.unpack_from("<H", image, pe_offset + 4)[0]


def link(reld_link: Path, obj: Path, out: Path, library: str) -> str:
    env = dict(os.environ, RELD_LOG_ENGINE="1", RELD_LLVM_LD=library)
    result = subprocess.run(
        [str(reld_link), *LINK_ARGS, f"/out:{out}", str(obj)],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    log = result.stdout + result.stderr
    if result.returncode != 0:
        raise E2eError(f"link failed ({result.returncode}) with RELD_LLVM_LD={library}:\n{log}")
    return log


def run(reld_link: Path, library: Path, clang: str, workdir: Path) -> None:
    source = workdir / "a.c"
    obj = workdir / "a.obj"
    source.write_text(SOURCE, encoding="utf-8")
    subprocess.run([clang, "--target=x86_64-pc-windows-msvc", "-c", str(source), "-o", str(obj)], check=True)

    in_process = workdir / "in-process.exe"
    log = link(reld_link, obj, in_process, str(library))
    print(log.strip())
    if "in-process llvm-ld-coff" not in log:
        raise E2eError("the link did not run in-process through llvm-ld-coff")
    machine = pe_machine(in_process.read_bytes())
    if machine != MACHINE_AMD64:
        raise E2eError(f"expected an AMD64 image, got machine {machine:#x}")

    bridged = workdir / "bridge.exe"
    print(link(reld_link, obj, bridged, "off").strip())
    if in_process.read_bytes() != bridged.read_bytes():
        raise E2eError("in-process llvm-ld-coff output differs from the lld-link subprocess output")

    if platform.system() == "Windows":
        code = subprocess.run([str(in_process)], check=False).returncode
        if code != 42:
            raise E2eError(f"the linked program exited {code}, expected 42")
    print("llvm-ld-coff e2e: linked in-process, byte-identical to lld-link"
          + (", runs" if platform.system() == "Windows" else ""))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reld-link", type=Path, required=True)
    parser.add_argument("--library", type=Path, required=True, help="the staged llvm-ld-coff shared library")
    parser.add_argument("--clang", default=shutil.which("clang") or "clang")
    args = parser.parse_args(argv)
    try:
        with tempfile.TemporaryDirectory() as tmp:
            run(args.reld_link.resolve(), args.library.resolve(), args.clang, Path(tmp))
    except (E2eError, subprocess.CalledProcessError) as error:
        print(f"llvm-ld-coff e2e failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
