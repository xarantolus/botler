from example import word_count


def test_word_count() -> None:
    assert word_count("one two  three\n") == 3
