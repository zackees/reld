from ci.phase1_summary import counts


def test_counts_rust_and_mimic_results() -> None:
    text = """
test result: ok. 7 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
100 seeds, 0 differential failures
"""
    assert counts(text) == (107, 2)
