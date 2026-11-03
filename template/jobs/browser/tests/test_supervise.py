"""Offline tests for the browser supervisor: no network, no Playwright, no child process."""

import importlib.util
import json
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any, cast

SUPERVISE = Path(__file__).resolve().parents[1] / "supervise.py"


def _load() -> object:
    spec = importlib.util.spec_from_file_location("browser_supervise", SUPERVISE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["browser_supervise"] = module
    spec.loader.exec_module(module)
    return module


supervise = cast("Any", _load())


class FakeChild:
    def __init__(self, codes: list[int | None]) -> None:
        self.codes, self.i = codes, 0

    def poll(self) -> int | None:
        code = self.codes[min(self.i, len(self.codes) - 1)]
        self.i += 1
        return code


class FakeResponse:
    def __init__(self, status: int = 200) -> None:
        self.status = status

    def __enter__(self) -> "FakeResponse":
        return self

    def __exit__(self, *args: object) -> bool:
        return False


class FakeMcpResponse:
    """An SSE body (Playwright MCP answers POSTs with `text/event-stream` by default)."""

    def __init__(self, message: dict[str, Any], headers: dict[str, str]) -> None:
        self._body = ("event: message\ndata: " + json.dumps(message) + "\n\n").encode()
        self.headers = {"Content-Type": "text/event-stream", **headers}

    def read(self) -> bytes:
        return self._body

    def __iter__(self) -> Iterator[bytes]:
        return iter(self._body.splitlines(keepends=True))

    def __enter__(self) -> "FakeMcpResponse":
        return self

    def __exit__(self, *args: object) -> bool:
        return False


class FakeMcpOpener:
    """Answers initialize / notifications / tools-list like Playwright MCP's streamable HTTP."""

    def __init__(self) -> None:
        self.methods: list[str] = []

    def __call__(self, request: object, timeout: float | None = None) -> FakeMcpResponse:
        data = getattr(request, "data", b"")
        method = json.loads(data).get("method") if isinstance(data, (bytes, str)) else None
        self.methods.append(str(method))
        if method == "initialize":
            return FakeMcpResponse(
                {"jsonrpc": "2.0", "id": 1, "result": {"serverInfo": {"name": "playwright"}}},
                {"Mcp-Session-Id": "s1"},
            )
        if method == "tools/list":
            return FakeMcpResponse(
                {"jsonrpc": "2.0", "id": 2, "result": {"tools": [{"name": "browser_navigate"}]}}, {}
            )
        return FakeMcpResponse(
            {"jsonrpc": "2.0", "id": 3, "result": {"content": [{"type": "text", "text": "ok"}]}}, {}
        )


def test_parse_endpoints_and_idle_timeout() -> None:
    argv = ["playwright-mcp", "--cdp-endpoint", "http://x:1", "--port", "9000", "--idle-timeout", "45000"]
    assert supervise.parse_endpoints(argv) == ("http://x:1", 9000)
    assert supervise.parse_endpoints(["playwright-mcp"]) == ("http://127.0.0.1:9223", 8000)
    assert supervise.idle_timeout_ms(argv) == 45000
    assert supervise.idle_timeout_ms(["playwright-mcp"]) == 600000


def test_cdp_ok() -> None:
    def ok(*args: object, **kwargs: object) -> FakeResponse:
        return FakeResponse(200)

    def bad(*args: object, **kwargs: object) -> FakeResponse:
        return FakeResponse(500)

    def boomer(*args: object, **kwargs: object) -> object:
        raise OSError("no route")

    assert supervise.cdp_ok("http://x", opener=ok) is True
    assert supervise.cdp_ok("http://x", opener=bad) is False
    assert supervise.cdp_ok("http://x", opener=boomer) is False


def test_smoke_test_walks_initialize_then_tools_list() -> None:
    opener = FakeMcpOpener()
    assert supervise.smoke_test("http://x/mcp", opener=opener) == ["browser_navigate"]
    assert opener.methods == ["initialize", "notifications/initialized", "tools/list"]


def test_result_text_joins_blocks() -> None:
    assert (
        supervise.result_text(
            {"result": {"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}}
        )
        == "a\nb"
    )
    assert supervise.result_text({}) == ""


def test_supervise_reports_health_then_fails_on_child_exit() -> None:
    events: list[object] = []

    def ok() -> None:
        events.append("ok")

    def failed(reason: str) -> None:
        events.append(reason)

    def always_healthy(url: str) -> bool:
        return True

    def no_sleep(seconds: float) -> None:
        return None

    code = supervise.supervise(
        FakeChild([None, None, 3]),
        cdp_url="http://x",
        interval=0,
        cdp_check=always_healthy,
        health_ok=ok,
        health_fail=failed,
        sleep=no_sleep,
    )
    assert code == 1
    assert events == ["ok", "ok", "playwright-mcp exited with code 3"]


def test_supervise_fails_when_cdp_stops_answering() -> None:
    checks = iter([True, False])
    events: list[object] = []

    def check(url: str) -> bool:
        return next(checks)

    def ok() -> None:
        events.append("ok")

    def failed(reason: str) -> None:
        events.append(reason)

    def no_sleep(seconds: float) -> None:
        return None

    code = supervise.supervise(
        FakeChild([None, None]),
        cdp_url="http://x",
        interval=0,
        cdp_check=check,
        health_ok=ok,
        health_fail=failed,
        sleep=no_sleep,
    )
    assert code == 1 and events == ["ok", "Chromium's CDP stopped answering"]


def test_supervise_survives_a_failed_health_report() -> None:
    """A hub restart refusing its socket must not kill the browser (incident 2026-10-31)."""
    events: list[object] = []
    reports: Iterator[object] = iter([RuntimeError("cannot reach the hub"), None])

    def ok() -> None:
        result = next(reports)
        if isinstance(result, Exception):
            raise result
        events.append("ok")

    def failed(reason: str) -> None:
        events.append(reason)

    def no_sleep(seconds: float) -> None:
        return None

    def always_healthy(url: str) -> bool:
        return True

    code = supervise.supervise(
        FakeChild([None, None, 3]),
        cdp_url="http://x",
        interval=0,
        cdp_check=always_healthy,
        health_ok=ok,
        health_fail=failed,
        sleep=no_sleep,
    )
    assert code == 1
    assert events == ["ok", "playwright-mcp exited with code 3"]


def test_supervise_retries_a_failed_health_report_sooner() -> None:
    """A failed report must be retried promptly, not a full interval later, so the heartbeat
    refreshes as soon as the hub returns (incident 2026-11-01)."""
    events: list[object] = []
    reports: Iterator[object] = iter([RuntimeError("cannot reach the hub"), None])
    sleeps: list[float] = []

    def ok() -> None:
        result = next(reports)
        if isinstance(result, Exception):
            raise result
        events.append("ok")

    def failed(reason: str) -> None:
        events.append(reason)

    def always_healthy(url: str) -> bool:
        return True

    code = supervise.supervise(
        FakeChild([None, None, 3]),
        cdp_url="http://x",
        interval=60.0,
        retry_interval=5.0,
        cdp_check=always_healthy,
        health_ok=ok,
        health_fail=failed,
        sleep=sleeps.append,
    )
    assert code == 1
    assert sleeps == [60.0, 5.0, 60.0]
    assert events == ["ok", "playwright-mcp exited with code 3"]


def test_blocklist_check_reads_the_pinned_list(tmp_path: Path) -> None:
    hosts = tmp_path / "hosts"
    hosts.write_text("0.0.0.0 doubleclick.net\n0.0.0.0 googleadservices.com\n")
    result = supervise.blocklist_check(str(hosts))
    assert result["present"] is True
    assert "doubleclick.net" in result["listed"]  # type: ignore[operator]


def test_blocklist_check_missing_file() -> None:
    assert supervise.blocklist_check("/nonexistent/hosts")["present"] is False
