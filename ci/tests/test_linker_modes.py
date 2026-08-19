from pathlib import Path

import pytest

from ci.linker_modes import (
    HOSTS,
    MODES,
    PipelineError,
    compile_command,
    detect_platform,
    executable_from_cargo_output,
    is_llvm_bitcode,
    link_command,
    parse_route,
    select_modes,
    write_report,
)


def test_default_modes_cover_fast_thin_and_full_lto() -> None:
    assert [mode.name for mode in select_modes([])] == ["fast", "thin-lto", "full-lto"]


def test_repeated_modes_are_rejected() -> None:
    with pytest.raises(PipelineError, match="must not be repeated"):
        select_modes(["fast", "fast"])


@pytest.mark.parametrize(
    ("platform", "mode", "engine", "kind"),
    [
        ("linux", "fast", "reld", "native"),
        ("linux", "thin-lto", "lld", "bridge"),
        ("linux", "full-lto", "lld", "bridge"),
        ("windows", "fast", "lld-link", "bridge"),
        ("windows", "thin-lto", "lld-link", "bridge"),
        ("windows", "full-lto", "lld-link", "bridge"),
        ("macos", "fast", "ld64.lld", "bridge"),
        ("macos", "thin-lto", "ld64.lld", "bridge"),
        ("macos", "full-lto", "ld64.lld", "bridge"),
    ],
)
def test_expected_engine_matrix(platform: str, mode: str, engine: str, kind: str) -> None:
    assert HOSTS[platform].expected_engine(MODES[mode]) == (engine, kind)


def test_explicit_platform_detection() -> None:
    assert detect_platform("windows") is HOSTS["windows"]
    with pytest.raises(PipelineError, match="unsupported platform"):
        detect_platform("plan9")


def test_compile_and_link_commands_own_lto_selection(tmp_path: Path) -> None:
    mode = MODES["thin-lto"]
    source = tmp_path / "main.c"
    obj = tmp_path / "main.o"
    linker = tmp_path / "reld"
    executable = tmp_path / "fixture"

    compile = compile_command("clang", source, obj, mode)
    link = link_command("clang", [obj], executable, linker, mode, HOSTS["linux"])

    assert "-flto=thin" in compile
    assert "-flto=thin" in link
    assert f"-fuse-ld={linker}" in link


def test_windows_link_command_uses_path_resolvable_driver_name(tmp_path: Path) -> None:
    linker = tmp_path / "reld-link.exe"
    command = link_command(
        "clang",
        [tmp_path / "main.obj"],
        tmp_path / "fixture.exe",
        linker,
        MODES["fast"],
        HOSTS["windows"],
    )
    assert "-fuse-ld=reld-link" in command
    assert all(str(linker) not in str(arg) for arg in command)


def test_cargo_json_selects_exact_binary_path() -> None:
    output = "\n".join(
        [
            "soldr: wrapper note",
            '{"reason":"compiler-artifact","target":{"name":"other"},"executable":"/x/other"}',
            '{"reason":"compiler-artifact","target":{"name":"reld-link"},' '"executable":"C:/target/x86_64-pc-windows-msvc/debug/reld-link.exe"}',
        ]
    )
    assert executable_from_cargo_output(output, "reld-link") == Path("C:/target/x86_64-pc-windows-msvc/debug/reld-link.exe")


@pytest.mark.parametrize("magic", [b"BC\xc0\xde", b"\xde\xc0\x17\x0b"])
def test_bitcode_magic_detection(tmp_path: Path, magic: bytes) -> None:
    obj = tmp_path / "input.o"
    obj.write_bytes(magic + b"payload")
    assert is_llvm_bitcode(obj)


def test_native_object_is_not_bitcode(tmp_path: Path) -> None:
    obj = tmp_path / "input.o"
    obj.write_bytes(b"\x7fELFpayload")
    assert not is_llvm_bitcode(obj)


def test_route_parser_requires_expected_engine() -> None:
    engine, kind, reason = parse_route(
        "reld: engine=lld (bridge, reason=flag:--plugin) -> /tool/rust-lld\n",
        HOSTS["linux"],
        MODES["thin-lto"],
    )
    assert (engine, kind, reason) == ("lld", "bridge", "flag:--plugin")

    with pytest.raises(PipelineError, match="expected reld/native"):
        parse_route(
            "reld: engine=lld (bridge, reason=default) -> /tool/rust-lld\n",
            HOSTS["linux"],
            MODES["fast"],
        )


def test_report_is_machine_readable(tmp_path: Path) -> None:
    from ci.linker_modes import ModeResult

    report = tmp_path / "report.json"
    write_report(
        report,
        [ModeResult("linux", "fast", "reld", "native", "default", "/tmp/fixture")],
    )
    text = report.read_text(encoding="utf-8")
    assert '"schema": 1' in text
    assert '"mode": "fast"' in text
