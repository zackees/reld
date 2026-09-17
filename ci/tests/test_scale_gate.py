"""The >2 GiB scale gate's own logic (reld#16).

The gate itself runs on a schedule and takes gigabytes of disk. These tests cover the parts that
decide whether it passes — sizing, the ELF structural checks, and the generated payload — against
hand-built bytes, so a validator that stops catching truncation is caught on every PR instead of
on the next scheduled run.
"""

from __future__ import annotations

import struct
from pathlib import Path

import pytest

from ci.scale_gate import DEFAULT_CHUNK_BYTES
from ci.scale_gate import DEFAULT_TARGET_BYTES
from ci.scale_gate import MINIMUM_OUTPUT_BYTES
from ci.scale_gate import SUCCESS_EXIT_CODE
from ci.scale_gate import ScaleGateError
from ci.scale_gate import chunk_source
from ci.scale_gate import driver_source
from ci.scale_gate import plan_chunks
from ci.scale_gate import validate_elf

SHDR_SIZE = 64
PHDR_SIZE = 56


def build_elf(
    tmp_path: Path,
    *,
    sections: list[tuple[int, int, int]] | None = None,
    segments: list[tuple[int, int, int, int]] | None = None,
    trailing_bytes: int = 4096,
    name: str = "a.out",
) -> Path:
    """A minimal ELF64 little-endian file whose headers say what the caller asked them to.

    `sections` are `(sh_type, sh_offset, sh_size)` and `segments` are
    `(p_type, p_offset, p_filesz, p_memsz)`. Header tables are placed after the payload region so
    a caller can describe a section that runs past the end of the file.
    """
    sections = sections or []
    segments = segments or []

    phoff = 64 + trailing_bytes
    shoff = phoff + len(segments) * PHDR_SIZE

    header = bytearray(64)
    header[0:4] = b"\x7fELF"
    header[4] = 2  # ELFCLASS64
    header[5] = 1  # ELFDATA2LSB
    header[6] = 1  # EV_CURRENT
    struct.pack_into("<HHI", header, 16, 2, 0x3E, 1)  # e_type ET_EXEC, e_machine x86-64
    struct.pack_into("<QQ", header, 32, phoff if segments else 0, shoff if sections else 0)
    struct.pack_into(
        "<HHHH",
        header,
        54,
        PHDR_SIZE,
        len(segments),
        SHDR_SIZE,
        len(sections),
    )

    blob = bytearray(header)
    blob.extend(b"\0" * trailing_bytes)
    for p_type, p_offset, p_filesz, p_memsz in segments:
        entry = bytearray(PHDR_SIZE)
        struct.pack_into("<II", entry, 0, p_type, 4)
        struct.pack_into("<QQQQQ", entry, 8, p_offset, 0, 0, p_filesz, p_memsz)
        blob.extend(entry)
    for sh_type, sh_offset, sh_size in sections:
        entry = bytearray(SHDR_SIZE)
        struct.pack_into("<II", entry, 0, 0, sh_type)
        struct.pack_into("<QQ", entry, 24, sh_offset, sh_size)
        blob.extend(entry)

    path = tmp_path / name
    path.write_bytes(bytes(blob))
    return path


def test_plan_chunks_rounds_up_and_clears_the_threshold() -> None:
    count = plan_chunks(DEFAULT_TARGET_BYTES, DEFAULT_CHUNK_BYTES)
    assert count * DEFAULT_CHUNK_BYTES >= DEFAULT_TARGET_BYTES
    assert count * DEFAULT_CHUNK_BYTES > MINIMUM_OUTPUT_BYTES
    # An exact multiple must not allocate a spare chunk.
    assert plan_chunks(MINIMUM_OUTPUT_BYTES, MINIMUM_OUTPUT_BYTES // 4) == 4


def test_plan_chunks_refuses_a_target_that_would_not_test_anything() -> None:
    with pytest.raises(ValueError):
        plan_chunks(MINIMUM_OUTPUT_BYTES - 1, DEFAULT_CHUNK_BYTES)
    with pytest.raises(ValueError):
        plan_chunks(DEFAULT_TARGET_BYTES, 0)


def test_validate_elf_accepts_headers_that_stay_inside_the_file(tmp_path: Path) -> None:
    path = build_elf(
        tmp_path,
        sections=[(1, 64, 1024)],
        segments=[(1, 0, 2048, 4096)],
    )
    summary = validate_elf(path)
    assert summary.file_bytes == path.stat().st_size
    assert summary.section_count == 1
    assert summary.segment_count == 1
    assert summary.max_section_end == 64 + 1024


def test_validate_elf_rejects_a_truncated_section(tmp_path: Path) -> None:
    path = build_elf(tmp_path, sections=[(1, 64, 1 << 40)])
    with pytest.raises(ScaleGateError, match="truncated output"):
        validate_elf(path)


def test_validate_elf_rejects_a_truncated_segment(tmp_path: Path) -> None:
    path = build_elf(tmp_path, segments=[(1, 64, 1 << 40, 1 << 40)])
    with pytest.raises(ScaleGateError, match="truncated output"):
        validate_elf(path)


def test_validate_elf_rejects_an_overflowed_filesz(tmp_path: Path) -> None:
    path = build_elf(tmp_path, segments=[(1, 0, 2048, 1024)])
    with pytest.raises(ScaleGateError, match="overflowed field"):
        validate_elf(path)


def test_validate_elf_ignores_nobits_sections(tmp_path: Path) -> None:
    # .bss claims address space, not file space, so a huge SHT_NOBITS is not truncation.
    path = build_elf(tmp_path, sections=[(8, 64, 1 << 40)])
    assert validate_elf(path).section_count == 1


def test_validate_elf_rejects_a_non_elf_file(tmp_path: Path) -> None:
    path = tmp_path / "not-elf"
    path.write_bytes(b"MZ" + b"\0" * 4096)
    with pytest.raises(ScaleGateError, match="not an ELF file"):
        validate_elf(path)


def test_payload_references_every_chunk() -> None:
    # A chunk nothing references is a chunk the linker is free to drop, which would quietly shrink
    # the output below the threshold the gate exists to cross.
    driver = driver_source(3)
    for index in range(3):
        assert f"chunk{index}" in driver
        assert f".globl chunk{index}" in chunk_source(index, 64)
    assert ".fill 64, 1," in chunk_source(0, 64)
    # Freestanding: an image this size does not fit the small code model the crt objects use.
    assert "_start" in driver
    assert "#include" not in driver
    assert f"{SUCCESS_EXIT_CODE}" in driver
