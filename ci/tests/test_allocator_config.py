import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


def test_allocator_uses_exact_registry_release_without_vendor_state() -> None:
    workspace = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    dependency = workspace["workspace"]["dependencies"]["mimalloc-pprof"]
    assert dependency == {"version": "=0.9.5", "default-features": False}
    assert not (REPO_ROOT / "_vendor" / "mimalloc-pprof").exists()
    assert "mimalloc-pprof" not in (REPO_ROOT / ".gitmodules").read_text(encoding="utf-8")


def test_reld_defaults_to_system_allocator_and_keeps_one_diagnostic_allocator() -> None:
    manifest = tomllib.loads((REPO_ROOT / "crates" / "reld" / "Cargo.toml").read_text(encoding="utf-8"))
    assert "dhat" not in manifest["dependencies"]
    assert manifest["dependencies"]["mimalloc-pprof"] == {"workspace": True, "optional": True}
    assert manifest["features"]["mimalloc-pprof-profile"] == [
        "dep:mimalloc-pprof",
        "mimalloc-pprof/pprof",
    ]
    assert manifest["features"]["mimalloc-pprof-dhat"] == ["dep:mimalloc-pprof"]
    assert not set(manifest["features"]["default"]) & {
        "mimalloc-pprof-profile",
        "mimalloc-pprof-dhat",
    }

    source = (REPO_ROOT / "crates" / "reld" / "src" / "main.rs").read_text(encoding="utf-8")
    assert source.count("#[global_allocator]") == 1
    assert '#[cfg(any(feature = "mimalloc-pprof-profile", feature = "mimalloc-pprof-dhat"))]' in source
    assert "mimalloc_pprof::MiMalloc" in source
    assert "dhat::Alloc" not in source
    assert "mimalloc_pprof::dhat::start()" in source
    assert source.index("drop(dhat)") < source.rindex("report_error_and_exit")


def test_lockfile_records_registry_checksum_and_no_external_dhat() -> None:
    lock = tomllib.loads((REPO_ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    packages = [package for package in lock["package"] if package["name"] == "mimalloc-pprof"]
    assert len(packages) == 1
    package = packages[0]
    assert package["version"] == "0.9.5"
    assert package["source"].startswith("registry+")
    assert package["checksum"]
    assert all(package["name"] != "dhat" for package in lock["package"])
