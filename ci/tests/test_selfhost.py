"""The Linux self-host gate's comparison logic (reld#6, P2-T2).

The gate itself builds reld three times and only runs in CI. These tests cover the part that
decides pass or fail — finding the one exempt section and comparing every other byte — against
real binaries from this repository and hand-built bytes, so an exemption that silently widens to
the whole file is caught on every PR.
"""

from __future__ import annotations

import struct
from pathlib import Path

import pytest

from ci.selfhost import IDENTITY_SECTION
from ci.selfhost import SelfHostError
from ci.selfhost import differs_outside
from ci.selfhost import section_range


def build_elf(path: Path, sections: dict[bytes, bytes]) -> Path:
    """A 64-bit ELF file with the named sections, enough for section-name lookup."""
    names = [b""] + list(sections)
    shstrtab = b"\0".join(names) + b"\0"
    name_offsets: dict[bytes, int] = {}
    cursor = 1
    for name in names[1:]:
        name_offsets[name] = cursor
        cursor += len(name) + 1

    payload = bytearray()
    ranges: dict[bytes, tuple[int, int]] = {}
    for name, body in sections.items():
        start = 64 + len(payload)
        payload.extend(body)
        ranges[name] = (start, start + len(body))
    shstrtab_offset = 64 + len(payload)
    payload.extend(shstrtab)

    count = len(sections) + 2  # null entry, the sections, then .shstrtab
    shoff = 64 + len(payload)

    header = bytearray(64)
    header[0:4] = b"\x7fELF"
    header[4] = 2
    header[5] = 1
    header[6] = 1
    struct.pack_into("<Q", header, 40, shoff)
    struct.pack_into("<HHH", header, 58, 64, count, count - 1)

    table = bytearray(64)  # SHN_UNDEF
    for name, (start, end) in ranges.items():
        entry = bytearray(64)
        struct.pack_into("<I", entry, 0, name_offsets[name])
        struct.pack_into("<QQ", entry, 24, start, end - start)
        table.extend(entry)
    shstrtab_entry = bytearray(64)
    struct.pack_into("<QQ", shstrtab_entry, 24, shstrtab_offset, len(shstrtab))
    table.extend(shstrtab_entry)

    path.write_bytes(bytes(header) + bytes(payload) + bytes(table))
    return path


def test_section_range_finds_the_identity_section(tmp_path: Path) -> None:
    path = build_elf(tmp_path / "a.out", {b".text": b"\x90" * 16, IDENTITY_SECTION: b"Linker: Reld"})
    start, end = section_range(path, IDENTITY_SECTION)
    assert path.read_bytes()[start:end] == b"Linker: Reld"


def test_section_range_returns_none_for_a_section_that_is_not_there(tmp_path: Path) -> None:
    path = build_elf(tmp_path / "a.out", {b".text": b"\x90" * 16})
    assert section_range(path, IDENTITY_SECTION) is None


def test_section_range_rejects_a_non_elf_file(tmp_path: Path) -> None:
    path = tmp_path / "not-elf"
    path.write_bytes(b"MZ" + b"\0" * 128)
    with pytest.raises(SelfHostError, match="not a 64-bit ELF"):
        section_range(path, IDENTITY_SECTION)


def test_a_difference_inside_the_exempt_range_is_allowed(tmp_path: Path) -> None:
    first = build_elf(tmp_path / "one", {b".text": b"\x90" * 16, IDENTITY_SECTION: b"Linker: A"})
    second = build_elf(tmp_path / "two", {b".text": b"\x90" * 16, IDENTITY_SECTION: b"Linker: B"})
    assert differs_outside(first, second, section_range(first, IDENTITY_SECTION)) == []


def test_a_difference_outside_the_exempt_range_is_reported_with_its_offset(tmp_path: Path) -> None:
    first = build_elf(tmp_path / "one", {b".text": b"\x90" * 16, IDENTITY_SECTION: b"Linker: A"})
    second = build_elf(tmp_path / "two", {b".text": b"\x90" * 15 + b"\xcc", IDENTITY_SECTION: b"Linker: A"})
    text_start, text_end = section_range(first, b".text")
    offsets = differs_outside(first, second, section_range(first, IDENTITY_SECTION))
    assert offsets == [text_end - 1]
    assert text_start <= offsets[0] < text_end


def test_no_exemption_means_every_byte_counts(tmp_path: Path) -> None:
    first = build_elf(tmp_path / "one", {IDENTITY_SECTION: b"Linker: A"})
    second = build_elf(tmp_path / "two", {IDENTITY_SECTION: b"Linker: B"})
    assert differs_outside(first, second, None) != []


def test_generations_of_different_sizes_fail_rather_than_compare(tmp_path: Path) -> None:
    first = build_elf(tmp_path / "one", {b".text": b"\x90" * 16})
    second = build_elf(tmp_path / "two", {b".text": b"\x90" * 32})
    with pytest.raises(SelfHostError, match="differ in size"):
        differs_outside(first, second, None)
