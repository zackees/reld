import os
from pathlib import Path

import pytest

from ci.benchmark_runner import (
    CONFIGURATIONS,
    BenchmarkError,
    LinkCommand,
    benchmark_environment,
    cargo_capture_command,
    parse_print_link_args,
    replay_command,
    replace_output,
)


def test_configurations_are_exactly_the_three_public_lto_rows():
    assert [(configuration.label, configuration.profile) for configuration in CONFIGURATIONS] == [
        ("no-LTO", "linkbench-no-lto"),
        ("ThinLTO", "linkbench-thin-lto"),
        ("full-LTO", "linkbench-full-lto"),
    ]


def test_cargo_capture_command_builds_the_idiomatic_project_once():
    command = cargo_capture_command(
        cargo="cargo",
        manifest=Path("ci/e2e/sqlite-bridge/Cargo.toml"),
        profile="linkbench-thin-lto",
        target_dir=Path("target/link-benchmark"),
    )

    assert command == [
        "cargo",
        "rustc",
        "--locked",
        "--manifest-path",
        str(Path("ci/e2e/sqlite-bridge/Cargo.toml")),
        "--package",
        "app",
        "--profile",
        "linkbench-thin-lto",
        "--target-dir",
        str(Path("target/link-benchmark")),
        "--",
        "-C",
        "save-temps=yes",
        "--print",
        "link-args",
    ]


def test_cargo_capture_command_rejects_shell_fragments():
    with pytest.raises(BenchmarkError, match="must name one executable"):
        cargo_capture_command(
            cargo="cargo --locked",
            manifest=Path("Cargo.toml"),
            profile="linkbench-no-lto",
            target_dir=Path("target/link-benchmark"),
        )


def test_parse_print_link_args_decodes_rust_escaped_windows_paths():
    output = 'compiler chatter\n"C:\\\\VS\\\\link.exe" "/NOLOGO" ' '"C:\\\\target\\\\app.o" "/OUT:C:\\\\target\\\\app.exe"\n'

    command = parse_print_link_args(output, windows=True)

    assert command == LinkCommand(
        executable=r"C:\VS\link.exe",
        arguments=("/NOLOGO", r"C:\target\app.o", r"/OUT:C:\target\app.exe"),
        output=Path(r"C:\target\app.exe"),
        windows=True,
    )


def test_parse_print_link_args_reads_unix_driver_command():
    output = 'LC_ALL="C" PATH="/toolchain/bin:/usr/bin" VSLANG="1033" ' '"cc" "main.o" "-Wl,--gc-sections" "-o" "/tmp/app"\n'

    command = parse_print_link_args(output, windows=False)

    assert command.output == Path("/tmp/app")
    assert command.executable == "cc"


def test_parse_print_link_args_handles_macos_environment_removals():
    output = (
        'env -u IPHONEOS_DEPLOYMENT_TARGET -u SDKROOT LC_ALL="C" ' 'PATH="/toolchain/bin:/usr/bin" VSLANG="1033" ZERO_AR_DATE="1" ' '"cc" "symbols.o" "libapp.rlib" "-arch" "arm64" "-o" "/tmp/app"\n'
    )

    command = parse_print_link_args(output, windows=False)

    assert command.executable == "cc"
    assert command.arguments[0] == "symbols.o"
    assert command.output == Path("/tmp/app")


def test_replace_output_handles_unix_and_msvc_forms(tmp_path: Path):
    unix = LinkCommand("cc", ("main.o", "-o", "/old/app"), Path("/old/app"), False)
    windows = LinkCommand(
        "link.exe",
        ("main.obj", "/OUT:C:\\old\\app.exe"),
        Path(r"C:\old\app.exe"),
        True,
    )

    unix_output = tmp_path / "unix-app"
    windows_output = tmp_path / "windows-app.exe"
    assert replace_output(unix, unix_output).arguments[-1] == str(unix_output)
    assert replace_output(windows, windows_output).arguments[-1] == f"/OUT:{windows_output}"


def test_replay_command_selects_driver_linker_on_unix(tmp_path: Path):
    captured = LinkCommand(
        "clang",
        ("main.o", "-fuse-ld=lld", "-o", "/old/app"),
        Path("/old/app"),
        False,
    )

    argv, response = replay_command(
        captured,
        linker="reld",
        linker_path=tmp_path / "ld.reld",
        output=tmp_path / "app",
        response_file=tmp_path / "args.rsp",
        driver_linker_dir=tmp_path / "reld-driver",
    )

    assert argv[0] == "clang"
    assert f"-B{tmp_path / 'reld-driver'}" in argv
    assert "-fuse-ld=lld" not in argv
    assert response is None


def test_replay_command_preserves_macos_flavor_bearing_linker_path(tmp_path: Path):
    captured = LinkCommand(
        "clang",
        ("main.o", "-B/rust/gcc-ld", "-fuse-ld=lld", "-o", "/old/app"),
        Path("/old/app"),
        False,
    )
    ld64_reld = tmp_path / "ld64.reld"

    argv, response = replay_command(
        captured,
        linker="reld",
        linker_path=ld64_reld,
        output=tmp_path / "app",
        response_file=tmp_path / "args.rsp",
    )

    assert f"-fuse-ld={ld64_reld}" in argv
    assert not any(argument.startswith("-B") for argument in argv)
    assert response is None


def test_replay_command_uses_response_file_for_windows(tmp_path: Path):
    captured = LinkCommand(
        "link.exe",
        ("one.obj", "two.obj", "/OUT:C:\\old\\app.exe"),
        Path(r"C:\old\app.exe"),
        True,
    )
    response_file = tmp_path / "args.rsp"

    argv, response = replay_command(
        captured,
        linker="lld",
        linker_path=tmp_path / "lld-link.exe",
        output=tmp_path / "app.exe",
        response_file=response_file,
    )

    assert argv == [str(tmp_path / "lld-link.exe"), f"@{response_file}"]
    assert response is not None
    assert "/OUT:" in response
    assert "one.obj" in response


def test_non_windows_benchmark_environment_is_inherited(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr("ci.benchmark_runner.sys.platform", "linux")
    monkeypatch.setenv("RELD_BENCHMARK_TEST", "present")

    assert benchmark_environment()["RELD_BENCHMARK_TEST"] == "present"
    assert benchmark_environment() is not os.environ
