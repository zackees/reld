from ci.phase1_summary import counts


def test_counts_rust_and_mimic_results() -> None:
    text = """
test result: ok. 7 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
100 seeds, 0 differential failures
"""
    assert counts(text) == (107, 2)


def test_counts_failed_rust_result() -> None:
    text = """
test result: FAILED. 396 passed; 10 failed; 1 ignored; 0 measured; 0 filtered out
"""
    assert counts(text) == (406, 1)


def test_counts_ansi_colored_results() -> None:
    text = """
test result: \x1b[32mok\x1b[0m. 7 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
test result: \x1b[31mFAILED\x1b[0m. 396 passed; 10 failed; 1 ignored; 0 measured; 0 filtered out
"""
    assert counts(text) == (413, 3)
