import pytest
from web_health.__main__ import main


def broken() -> str:
    raise RuntimeError("DuckDuckGo result markup not recognised")


def test_failure_is_reported(capsys: pytest.CaptureFixture[str]) -> None:
    assert main([("web.search auto", broken, True), ("web.fetch", lambda: "web.fetch ok", True)]) == 1
    out = capsys.readouterr()
    assert "web.fetch ok" in out.out
    assert "- web.search auto: RuntimeError: DuckDuckGo result markup not recognised" in out.err
    assert "tools/web/src/web/" in out.err


def test_one_backend_down_is_a_warning(capsys: pytest.CaptureFixture[str]) -> None:
    checks = [("web.search duckduckgo", broken, False), ("web.search auto", lambda: "auto ok", True)]
    assert main(checks) == 0
    out = capsys.readouterr()
    assert "WARNING" in out.out and "web.search duckduckgo: RuntimeError" in out.out
    assert out.err == ""
