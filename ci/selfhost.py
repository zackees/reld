"""Link reld with reld, twice, and require the two generations to agree (reld#6, P2-T2).

Windows and macOS already self-host in `ci.yml`; Linux, the platform Phase 2 is about, did not.

The check is a bootstrap fixpoint. Generation 0 is built by the host toolchain's linker.
Generation 1 is the same source linked by generation 0, and generation 2 is the same source
linked by generation 1. If reld's output depends on anything but its input, the two generations
diverge — and they are compared byte for byte, not merely both run.

The one field allowed to differ is `.comment`, where the linker records its own identity: a
generation linked by a differently-built reld truthfully writes a different string there. Every
other byte must match. That exempts one named field and keeps the comparison of all the rest,
which is the artifact-equivalence policy in DESIGN.md §3.1.
"""

from __future__ import annotations

import argparse
import os
import struct
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

#: The linker writes its own version string here, so a generation linked by a different build of
#: reld legitimately differs in this section and nowhere else.
IDENTITY_SECTION = b".comment"


class SelfHostError(RuntimeError):
    """A failure that must fail the gate."""


@dataclass(frozen=True)
class Generation:
    index: int
    binary: Path
    linked_by: str


def section_range(path: Path, name: bytes) -> tuple[int, int] | None:
    """Return the file range of a section, or None when the binary has no such section."""
    data = path.read_bytes()
    if len(data) < 64 or data[:4] != b"\x7fELF" or data[4] != 2:
        raise SelfHostError(f"{path} is not a 64-bit ELF file")

    e_shoff = struct.unpack_from("<Q", data, 40)[0]
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 58)
    if e_shnum == 0:
        return None

    def entry(index: int) -> tuple[int, int, int]:
        base = e_shoff + index * e_shentsize
        sh_name = struct.unpack_from("<I", data, base)[0]
        sh_offset, sh_size = struct.unpack_from("<QQ", data, base + 24)
        return sh_name, sh_offset, sh_size

    _, strtab_offset, strtab_size = entry(e_shstrndx)
    strtab = data[strtab_offset : strtab_offset + strtab_size]
    for index in range(e_shnum):
        sh_name, sh_offset, sh_size = entry(index)
        end = strtab.find(b"\0", sh_name)
        if strtab[sh_name:end] == name:
            return sh_offset, sh_offset + sh_size
    return None


def differs_outside(first: Path, second: Path, exempt: tuple[int, int] | None) -> list[int]:
    """Offsets at which two binaries differ, ignoring one exempt range.

    Returns offsets rather than a bool so a failure can say where the divergence is instead of
    only that there is one.
    """
    left = first.read_bytes()
    right = second.read_bytes()
    if len(left) != len(right):
        raise SelfHostError(
            f"generations differ in size: {first} is {len(left)} bytes, {second} is {len(right)}"
        )
    start, stop = exempt if exempt else (-1, -1)
    return [offset for offset in range(len(left)) if left[offset] != right[offset] and not start <= offset < stop]


def build(linker: Path | None, target_dir: Path, *, cargo: str = "cargo") -> Path:
    """Build release reld into `target_dir`, optionally linking it with `linker`."""
    env = dict(os.environ)
    if linker is not None:
        # reld is a pure linker, so it is driven by clang rather than named as rustc's linker.
        env["RUSTFLAGS"] = f"-Clinker=clang -Clink-arg=--ld-path={linker.resolve()}"
    result = subprocess.run(
        [cargo, "build", "--release", "-p", "reld", "--target-dir", str(target_dir)],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise SelfHostError(
            f"building with linker {linker or 'the host toolchain'} failed:\n{result.stdout}\n{result.stderr}"
        )
    binary = target_dir / "release" / "reld"
    if not binary.is_file():
        raise SelfHostError(f"build reported success but produced no {binary}")
    return binary


def check_runs(binary: Path) -> str:
    result = subprocess.run([str(binary), "--version"], capture_output=True, text=True, check=False)
    if result.returncode != 0 or "Reld" not in result.stdout:
        raise SelfHostError(
            f"{binary} does not run: exit {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result.stdout.strip()


def link_and_run_a_program(linker: Path, workdir: Path, *, cc: str = "clang") -> None:
    """A self-hosted reld must still link and produce a working program, not just exist."""
    workdir.mkdir(parents=True, exist_ok=True)
    source = workdir / "smoke.c"
    source.write_text("int main(void) { return 42; }\n", encoding="utf-8")
    binary = workdir / "smoke"
    result = subprocess.run(
        [cc, f"--ld-path={linker.resolve()}", "-o", str(binary), str(source)],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise SelfHostError(f"the self-hosted reld failed to link a program:\n{result.stderr}")
    if subprocess.run([str(binary)], check=False).returncode != 42:
        raise SelfHostError("a program linked by the self-hosted reld did not run correctly")


def run_gate(root: Path, *, cargo: str = "cargo", cc: str = "clang") -> list[Generation]:
    generations = [Generation(0, build(None, root / "gen0", cargo=cargo), "the host toolchain")]
    for index in (1, 2):
        previous = generations[-1]
        binary = build(previous.binary, root / f"gen{index}", cargo=cargo)
        generations.append(Generation(index, binary, f"generation {previous.index}"))

    for generation in generations:
        version = check_runs(generation.binary)
        print(f"generation {generation.index}: linked by {generation.linked_by} -> {version}")

    first, second = generations[1].binary, generations[2].binary
    exempt = section_range(first, IDENTITY_SECTION)
    differences = differs_outside(first, second, exempt)
    if differences:
        raise SelfHostError(
            f"generations 1 and 2 differ at {len(differences)} byte(s) outside {IDENTITY_SECTION.decode()}, "
            f"first at offset {differences[0]}: reld's output depends on more than its input"
        )
    identical = first.read_bytes() == second.read_bytes()
    print(
        "generations 1 and 2 are byte-identical"
        if identical
        else f"generations 1 and 2 are byte-identical outside {IDENTITY_SECTION.decode()}, "
        "which records the linker's own identity"
    )

    link_and_run_a_program(generations[2].binary, root / "smoke", cc=cc)
    print("a program linked by the self-hosted reld runs correctly")
    return generations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("target/selfhost"), help="where to build the generations")
    parser.add_argument("--cargo", default=os.environ.get("CARGO_COMMAND", "cargo"))
    parser.add_argument("--cc", default="clang")
    args = parser.parse_args(argv)

    try:
        run_gate(args.root, cargo=args.cargo, cc=args.cc)
    except SelfHostError as error:
        print(f"self-host gate failed: {error}", file=sys.stderr)
        return 1
    print("self-host gate passed: reld links reld, twice, to the same bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
