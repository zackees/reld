"""Published Linux linker comparator lock contract."""

import json
from pathlib import Path

LOCK = Path(__file__).parents[2] / "ci" / "linux-linker-comparators.lock.json"


def test_published_linux_comparator_assets_are_exact_and_ordered() -> None:
    lock = json.loads(LOCK.read_text(encoding="utf-8"))

    assert lock["schema_version"] == 1
    assert list(lock["comparators"]) == ["bfd", "lld", "mold", "wild"]
    expected = {
        "bfd": {
            "archive_sha256": "14c331732e1df80e5872832d1dc11193d622b7a657bc8f305096bd05676ddadd",
            "binary_path": "linux-linker-bfd-2.42-x86_64-linux/bin/ld.bfd",
            "binary_sha256": "80c9330e8cd66543e20fb4fda90dab88762e8b886164086e047720f165aaa904",
            "version_stdout": "GNU ld (GNU Binutils) 2.42\nCopyright (C) 2024 Free Software Foundation, Inc.\nThis program is free software; you may redistribute it under the terms of\nthe GNU General Public License version 3 or (at your option) a later version.\nThis program has absolutely no warranty.\n",
        },
        "lld": {
            "archive_sha256": "733c19f34e3a2877598c063c3c9d1f106db503e232e488e71519dea50cb818de",
            "binary_path": "linux-linker-lld-18.1.8-x86_64-linux/bin/ld.lld",
            "binary_sha256": "159920ab445dec79c852deddd33c269d06149896262904c9db4c45e2e669b1cd",
            "version_stdout": "LLD 18.1.8 (compatible with GNU linkers)\n",
        },
        "mold": {
            "archive_sha256": "642d4348ea4903e3a222a222367896127abeb356e6cbfbd10ed1caaf005d8a88",
            "binary_path": "linux-linker-mold-2.41.0-x86_64-linux/bin/mold",
            "binary_sha256": "74032ac7af36f7264156b61479dfd3ab235b4674f77b90601d11f38990f90a0b",
            "version_stdout": "mold 2.41.0 (7c4c0addcb833120bf41cc3db7b2652694e0d814; compatible with GNU ld)\n",
        },
        "wild": {
            "archive_sha256": "9f679981811f9442f28908b208c68e6947deb04064fcb6c9863d38d0466f947c",
            "binary_path": "linux-linker-wild-0.9.0-x86_64-linux/bin/ld.wild",
            "binary_sha256": "912db744a30fcef6035c663af2ee014254679ef46dc69f4338d6c9f8a62e66e8",
            "version_stdout": "Wild 0.9.0 (compatible with GNU linkers)\n",
        },
    }
    for name, fields in expected.items():
        entry = lock["comparators"][name]
        assert entry["url"].endswith({
            "bfd": "linux-linker-bfd-2.42-x86_64-linux.tar.gz",
            "lld": "linux-linker-lld-18.1.8-x86_64-linux.tar.gz",
            "mold": "linux-linker-mold-2.41.0-x86_64-linux.tar.gz",
            "wild": "linux-linker-wild-0.9.0-x86_64-linux.tar.gz",
        }[name])
        assert {field: entry[field] for field in fields} == fields
        assert entry["version_argv"] == ["--version"]
        assert entry["version_stderr"] == ""
        assert entry["recipe"] == {"remove_arguments": ["--no-fork"], "extra_arguments": []}
