import os
from io import StringIO
from pathlib import Path

import pytest

import ci.benchmark_runner as runner_module
from ci.benchmark_runner import (
    BENCHMARK_PACKAGE,
    CONFIGURATIONS,
    DEFAULT_MANIFEST,
    BenchmarkError,
    LinkCommand,
    Linker,
    _discard_verified_output,
    _parse_phase_timings,
    assert_significant_workload,
    benchmark_environment,
    benchmark_linkers_round_robin,
    benchmark_replay,
    cargo_capture_command,
    parse_print_link_args,
    prune_capture_artifacts,
    reld_output_mode_linkers,
    replay_command,
    replace_output,
    startup_probe_command,
)


def test_configurations_are_exactly_the_three_public_lto_rows():
    assert [(configuration.label, configuration.profile) for configuration in CONFIGURATIONS] == [
        ("no-LTO", "linkbench-no-lto"),
        ("ThinLTO", "linkbench-thin-lto"),
        ("full-LTO", "linkbench-full-lto"),
    ]


def test_profiles_use_idiomatic_bounded_codegen_units():
    manifest = DEFAULT_MANIFEST.read_text(encoding="utf-8")

    assert manifest.count("codegen-units = 16") == 3
    assert "codegen-units = 256" not in manifest


def test_cargo_capture_command_builds_the_idiomatic_project_once():
    command = cargo_capture_command(
        cargo="cargo",
        manifest=Path("ci/e2e/link-workload/Cargo.toml"),
        profile="linkbench-thin-lto",
        target_dir=Path("target/link-benchmark"),
    )

    assert command == [
        "cargo",
        "rustc",
        "--locked",
        "--manifest-path",
        str(Path("ci/e2e/link-workload/Cargo.toml")),
        "--package",
        "linkbench-app",
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


def test_linux_capture_uses_clang_without_gccs_synthetic_lto_plugin():
    command = cargo_capture_command(
        cargo="cargo",
        manifest=Path("ci/e2e/link-workload/Cargo.toml"),
        profile="linkbench-no-lto",
        target_dir=Path("target/link-benchmark"),
        linker="clang",
    )

    separator = command.index("--")
    assert command[separator + 1 : separator + 3] == ["-C", "linker=clang"]
    assert "plugin" not in " ".join(command)


def test_each_configuration_is_replayed_then_released_before_the_next_capture(tmp_path: Path, monkeypatch):
    events: list[str] = []
    linker = Linker("wild", tmp_path / "wild")

    def fake_capture(configuration, **kwargs):
        del kwargs
        events.append(f"capture:{configuration.label}")
        output = tmp_path / configuration.profile / "app"
        return LinkCommand("cc", ("input.o", "-o", str(output)), output, False)

    def fake_prune(captured, *, profile_dir, log):
        del captured, profile_dir, log

    def fake_startup(*args, **kwargs):
        del args, kwargs
        events.append("startup")
        return 0.01

    def fake_replay(captured, selected_linker, **kwargs):
        del captured
        events.append(f"replay:{selected_linker.label}")
        kwargs["sample_sink"].append(1.0)
        kwargs["output_size_sink"].append(256 * 1024 * 1024)
        return 1.0

    def fake_release(*, profile_dir, target_dir, log):
        del target_dir
        del log
        events.append(f"release:{profile_dir.name}")

    monkeypatch.setattr(runner_module, "linkers_for_target", lambda target, reld: (linker,))
    monkeypatch.setattr(runner_module, "benchmark_environment", lambda: {})
    monkeypatch.setattr(runner_module, "capture_final_link", fake_capture)
    monkeypatch.setattr(runner_module, "prune_capture_artifacts", fake_prune)
    monkeypatch.setattr(runner_module, "benchmark_startup", fake_startup)
    monkeypatch.setattr(runner_module, "benchmark_replay", fake_replay)
    monkeypatch.setattr(runner_module, "release_capture_artifacts", fake_release)

    runner_module.run_benchmark(
        target="x86_64-linux",
        reld=tmp_path / "reld",
        manifest=tmp_path / "Cargo.toml",
        workdir=tmp_path / "work",
        target_dir=tmp_path / "target",
        cargo="cargo",
        trials=3,
        warmup=1,
        log=StringIO(),
    )

    assert events == [
        "capture:no-LTO",
        "startup",
        "replay:wild",
        "replay:wild",
        "replay:wild",
        "replay:wild",
        "release:linkbench-no-lto",
        "capture:ThinLTO",
        "replay:wild",
        "replay:wild",
        "replay:wild",
        "replay:wild",
        "release:linkbench-thin-lto",
        "capture:full-LTO",
        "replay:wild",
        "replay:wild",
        "replay:wild",
        "replay:wild",
        "release:linkbench-full-lto",
    ]


def test_linker_trials_rotate_order_and_preserve_every_sample(tmp_path: Path, monkeypatch):
    captured = LinkCommand("cc", ("main.o", "-o", "/old/app"), Path("/old/app"), False)
    linkers = tuple(Linker(label, tmp_path / label) for label in ("wild", "reld", "mmap"))
    calls: list[str] = []

    def fake_replay(captured, linker, *, sample_sink, output_size_sink, **kwargs):
        del captured, kwargs
        calls.append(linker.label)
        sample_sink.append(float(len(calls)))
        output_size_sink.append(256 * 1024 * 1024)
        return sample_sink[0]

    monkeypatch.setattr(runner_module, "benchmark_replay", fake_replay)

    medians, samples, sizes, orders = benchmark_linkers_round_robin(
        captured,
        linkers,
        output_dir=tmp_path / "out",
        cwd=tmp_path,
        environment={},
        warmup=1,
        trials=3,
        use_driver_shim=True,
    )

    assert calls == [
        "wild",
        "reld",
        "mmap",
        "reld",
        "mmap",
        "wild",
        "mmap",
        "wild",
        "reld",
        "wild",
        "reld",
        "mmap",
    ]
    assert orders == [
        ["reld", "mmap", "wild"],
        ["mmap", "wild", "reld"],
        ["wild", "reld", "mmap"],
    ]
    assert all(len(values) == 3 for values in samples.values())
    assert all(len(values) == 3 for values in sizes.values())
    assert set(medians) == {"wild", "reld", "mmap"}


def test_reld_phase_trace_parser_requires_dominant_output_phases():
    output = """\
│ ├──    12.50 Compute build ID
│ ├──   625.25 Write data to file
│ └──     8.00 Flush and unmap output file
"""

    assert _parse_phase_timings(output) == {
        "Compute build ID": 0.0125,
        "Write data to file": 0.62525,
        "Flush and unmap output file": 0.008,
    }

    with pytest.raises(BenchmarkError, match="omitted required phases"):
        _parse_phase_timings("12.50 Compute build ID\n")


def test_release_capture_artifacts_rejects_outside_directory(tmp_path: Path):
    target_dir = tmp_path / "target"
    outside = tmp_path / "linkbench-no-lto"
    outside.mkdir()
    sentinel = outside / "sentinel"
    sentinel.write_text("keep", encoding="utf-8")

    with pytest.raises(BenchmarkError, match="refusing to release unsafe capture profile"):
        runner_module.release_capture_artifacts(
            profile_dir=outside,
            target_dir=target_dir,
            log=StringIO(),
        )

    assert sentinel.read_text(encoding="utf-8") == "keep"


def test_default_workload_replaces_startup_dominated_sqlite_bridge():
    assert DEFAULT_MANIFEST.as_posix().endswith("ci/e2e/link-workload/Cargo.toml")
    assert BENCHMARK_PACKAGE == "linkbench-app"


def test_compiled_policy_is_target_calibrated_for_significant_final_links():
    rules = DEFAULT_MANIFEST.parent / "crates" / "app" / "src" / "rules.rs"

    # The first exact-SHA macOS run measured 160 MiB at only 0.3169s under full LTO,
    # while the bridged reld front door needed 0.1539s to start. Keep enough real,
    # runtime-consumed policy data for the strict <=10% fixed-startup gate.
    source = rules.read_text(encoding="utf-8")
    assert '#[cfg(target_os = "macos")]' in source
    assert "928 * 1024 * 1024" in source
    assert '#[cfg(not(target_os = "macos"))]' in source
    assert "256 * 1024 * 1024" in source


def test_startup_probe_is_target_correct(tmp_path: Path):
    linker = Linker("reference", tmp_path / "linker")

    assert startup_probe_command("x86_64-linux", linker) == [str(linker.path), "--version"]
    assert startup_probe_command("aarch64-apple-darwin", linker) == [str(linker.path), "-v"]
    assert startup_probe_command("x86_64-pc-windows-msvc", linker) == [str(linker.path), "/?"]


def test_output_mode_diagnostics_force_the_native_reld_engine(tmp_path: Path):
    modes = reld_output_mode_linkers(tmp_path / "ld.reld", 4)

    assert [mode.label for mode in modes] == ["default", "mmap", "buffer"]
    assert all("-Wl,--engine=reld" in mode.driver_arguments for mode in modes)
    assert all("-Wl,--threads=4" in mode.driver_arguments for mode in modes)
    assert "-Wl,--mmap-output-file" in modes[1].driver_arguments
    assert "-Wl,--no-mmap-output-file" in modes[2].driver_arguments


def test_old_sqlite_bridge_is_red_and_significant_workload_is_green():
    startups = {"bfd": 0.010, "lld": 0.008, "mold": 0.006, "wild": 0.005, "reld": 0.095}
    old_links = {
        "no-LTO": {"wild": 0.0204},
        "ThinLTO": {"wild": 0.0251},
        "full-LTO": {"wild": 0.0172},
    }

    with pytest.raises(BenchmarkError, match="startup-dominated"):
        assert_significant_workload("x86_64-linux", startups, old_links)

    significant_links = {
        "no-LTO": {"wild": 1.10},
        "ThinLTO": {"wild": 1.05},
        "full-LTO": {"wild": 1.20},
    }
    assert_significant_workload("x86_64-linux", startups, significant_links)


def test_capture_pruning_preserves_referenced_native_inputs(tmp_path: Path):
    profile = tmp_path / "linkbench-no-lto"
    profile.mkdir()
    retained = profile / "app.rcgu.o"
    retained.write_bytes(b"native object")
    stale_bitcode = profile / "app.pre-lto.bc"
    stale_bitcode.write_bytes(b"temporary bitcode")
    stale_metadata = profile / "dependency.rmeta"
    stale_metadata.write_bytes(b"temporary metadata")
    cargo_output = profile / "linkbench-app"
    cargo_output.write_bytes(b"captured executable")
    command = LinkCommand("cc", (str(retained), "-o", str(cargo_output)), cargo_output, False)

    prune_capture_artifacts(command, profile_dir=profile, log=StringIO())

    assert retained.is_file()
    assert not stale_bitcode.exists()
    assert not stale_metadata.exists()
    assert not cargo_output.exists()


def test_every_warmup_and_trial_executable_is_verified_outside_timing(tmp_path: Path, monkeypatch):
    original_output = tmp_path / "cargo-output"
    captured = LinkCommand("cc", ("main.o", "-o", str(original_output)), original_output, False)
    output_dir = tmp_path / "replays"
    validations: list[Path] = []

    def fake_run(command, *, cwd, environment):
        del cwd, environment
        output = Path(command[command.index("-o") + 1])
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(b"executable")
        return runner_module.subprocess.CompletedProcess([], 0, "", "")

    def fake_validate(output, *, cwd, environment):
        del cwd, environment
        assert output.is_file()
        validations.append(output)

    monkeypatch.setattr(runner_module, "_run_link", fake_run)
    monkeypatch.setattr(runner_module, "_validate_executable", fake_validate)

    benchmark_replay(
        captured,
        Linker("test", tmp_path / "ld.test"),
        output_dir=output_dir,
        cwd=tmp_path,
        environment={},
        warmup=2,
        trials=3,
        use_driver_shim=False,
    )

    assert [path.name for path in validations] == [
        "app-test-warmup-0",
        "app-test-warmup-1",
        "app-test-trial-0",
        "app-test-trial-1",
        "app-test-trial-2",
    ]
    assert all(not path.exists() for path in validations)


def test_locked_windows_output_is_quarantined_without_delete_retries(tmp_path: Path, monkeypatch):
    output = tmp_path / "app-reld.exe"
    output.write_bytes(b"verified executable")
    real_unlink = Path.unlink
    delete_attempts = 0

    def locked_once(path, *args, **kwargs):
        nonlocal delete_attempts
        if path == output:
            delete_attempts += 1
            raise PermissionError(32, "being used by another process", str(path))
        return real_unlink(path, *args, **kwargs)

    monkeypatch.setattr(runner_module.sys, "platform", "win32")
    monkeypatch.setattr(Path, "unlink", locked_once)

    quarantine = _discard_verified_output(output)

    assert delete_attempts == 1
    assert not output.exists()
    quarantined = list(tmp_path.glob(".app-reld.exe.trash-*"))
    assert len(quarantined) == 1
    assert quarantine == quarantined[0]


def test_quarantine_cleanup_runs_after_all_timed_links(tmp_path: Path, monkeypatch):
    original_output = tmp_path / "cargo-output"
    captured = LinkCommand("cc", ("main.o", "-o", str(original_output)), original_output, False)
    events: list[str] = []

    def fake_run(command, *, cwd, environment):
        del cwd, environment
        output = Path(command[command.index("-o") + 1])
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(b"executable")
        events.append(f"link:{output.name}")
        return runner_module.subprocess.CompletedProcess([], 0, "", "")

    def fake_validate(output, *, cwd, environment):
        del cwd, environment
        events.append(f"validate:{output.name}")

    def fake_discard(output):
        events.append(f"quarantine:{output.name}")
        return output.with_name(f".{output.name}.trash")

    def fake_reclaim(paths):
        events.append("reclaim:" + ",".join(path.name for path in paths))

    monkeypatch.setattr(runner_module, "_run_link", fake_run)
    monkeypatch.setattr(runner_module, "_validate_executable", fake_validate)
    monkeypatch.setattr(runner_module, "_discard_verified_output", fake_discard)
    monkeypatch.setattr(runner_module, "_reclaim_quarantines", fake_reclaim)

    benchmark_replay(
        captured,
        Linker("reld", tmp_path / "reld-link.exe"),
        output_dir=tmp_path / "replays",
        cwd=tmp_path,
        environment={},
        warmup=1,
        trials=3,
        use_driver_shim=False,
    )

    assert events[-1].startswith("reclaim:")
    assert sum(event.startswith("link:") for event in events[:-1]) == 4


def test_cargo_capture_command_rejects_shell_fragments():
    with pytest.raises(BenchmarkError, match="must name one executable"):
        cargo_capture_command(
            cargo="cargo --locked",
            manifest=Path("Cargo.toml"),
            profile="linkbench-no-lto",
            target_dir=Path("target/link-benchmark"),
        )


def test_parse_print_link_args_decodes_rust_escaped_windows_paths():
    output = 'compiler chatter\n"C:\\\\VS\\\\link.exe" "/NOLOGO" "C:\\\\target\\\\app.o" "/OUT:C:\\\\target\\\\app.exe"\n'

    command = parse_print_link_args(output, windows=True)

    assert command == LinkCommand(
        executable=r"C:\VS\link.exe",
        arguments=("/NOLOGO", r"C:\target\app.o", r"/OUT:C:\target\app.exe"),
        output=Path(r"C:\target\app.exe"),
        windows=True,
    )


def test_parse_print_link_args_reads_unix_driver_command():
    output = 'LC_ALL="C" PATH="/toolchain/bin:/usr/bin" VSLANG="1033" "cc" "main.o" "-Wl,--gc-sections" "-o" "/tmp/app"\n'

    command = parse_print_link_args(output, windows=False)

    assert command.output == Path("/tmp/app")
    assert command.executable == "cc"


def test_parse_print_link_args_handles_macos_environment_removals():
    output = 'env -u IPHONEOS_DEPLOYMENT_TARGET -u SDKROOT LC_ALL="C" PATH="/toolchain/bin:/usr/bin" VSLANG="1033" ZERO_AR_DATE="1" "cc" "symbols.o" "libapp.rlib" "-arch" "arm64" "-o" "/tmp/app"\n'

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
