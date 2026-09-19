"""Package built reld/reld-link binaries into a versioned release archive.

Produces `reld-vX.Y.Z-<triple>.tar.gz` (or `.zip` on Windows) containing a single
top-level `reld-vX.Y.Z-<triple>/` directory with the binaries plus the license and
attribution files. This layout is the asset contract published binaries downstream
(soldr, see reld#148) depend on -- keep it stable.
"""

from __future__ import annotations

import argparse
import shutil
import tarfile
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LICENSE_FILES = ("LICENSE-MIT", "LICENSE-APACHE", "NOTICE")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="Release version, e.g. 0.1.0")
    parser.add_argument("--triple", required=True, help="Rust target triple, e.g. x86_64-unknown-linux-gnu")
    parser.add_argument("--binary-ext", default="", help="Executable suffix, e.g. .exe")
    parser.add_argument("--input-dir", required=True, type=Path, help="Directory containing reld[.exe] and reld-link[.exe]")
    parser.add_argument("--output-dir", required=True, type=Path, help="Directory to write the archive into")
    parser.add_argument(
        "--extra-dir",
        type=Path,
        help=(
            "Directory whose files ship alongside the binaries, e.g. the llvm-ld-coff library and its "
            "notices staged by ci/fetch_llvm_ld.py (reld#96). reld loads that library from next to "
            "itself, so it must land in the same directory."
        ),
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    ext = args.binary_ext
    archive_stem = f"reld-v{args.version}-{args.triple}"

    staging_root = args.output_dir / "_staging"
    if staging_root.exists():
        shutil.rmtree(staging_root)
    payload_dir = staging_root / archive_stem
    payload_dir.mkdir(parents=True)

    for name in (f"reld{ext}", f"reld-link{ext}"):
        src = args.input_dir / name
        if not src.is_file():
            raise SystemExit(f"missing binary: {src}")
        shutil.copy2(src, payload_dir / name)

    if args.extra_dir is not None:
        for src in sorted(args.extra_dir.iterdir()):
            if src.is_file():
                if (payload_dir / src.name).exists():
                    raise SystemExit(f"extra file {src.name} would overwrite a packaged file")
                shutil.copy2(src, payload_dir / src.name)

    for name in LICENSE_FILES:
        src = REPO_ROOT / name
        if not src.is_file():
            raise SystemExit(f"missing attribution file: {src}")
        shutil.copy2(src, payload_dir / name)

    args.output_dir.mkdir(parents=True, exist_ok=True)
    is_windows = "windows" in args.triple
    if is_windows:
        archive_path = args.output_dir / f"{archive_stem}.zip"
        with zipfile.ZipFile(archive_path, "w", zipfile.ZIP_DEFLATED) as zf:
            for path in sorted(payload_dir.rglob("*")):
                zf.write(path, path.relative_to(staging_root))
    else:
        archive_path = args.output_dir / f"{archive_stem}.tar.gz"
        with tarfile.open(archive_path, "w:gz") as tf:
            tf.add(payload_dir, arcname=archive_stem)

    shutil.rmtree(staging_root)
    print(archive_path)


if __name__ == "__main__":
    main()
