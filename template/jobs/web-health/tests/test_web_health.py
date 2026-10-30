import pytest
from web_health.__main__ import main


def broken() -> str:
    raise RuntimeError("DuckDuckGo result markup not recognised")


def test_failure_is_reported(capsys: pytest.CaptureFixture[str]) -> None:
    assert main([("web.search", broken), ("web.fetch", lambda: "web.fetch ok")]) == 1
    out = capsys.readouterr()
    assert "web.fetch ok" in out.out
    assert "- web.search: RuntimeError: DuckDuckGo result markup not recognised" in out.err
    assert "tools/web/src/web/" in out.err


def test_success() -> None:
    assert main([("web.fetch", lambda: "ok")]) == 0
