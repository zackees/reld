from pathlib import Path

import ci.quarantine_cleanup as cleanup


def test_quarantine_cleaner_waits_then_attempts_delete_once(tmp_path: Path, monkeypatch):
    quarantine = tmp_path / ".app-reld.exe.trash-test"
    quarantine.write_bytes(b"verified executable")
    sleeps: list[float] = []

    monkeypatch.setattr(cleanup.time, "sleep", sleeps.append)

    cleanup.discard_once(quarantine, delay_seconds=2.5)

    assert sleeps == [2.5]
    assert not quarantine.exists()
