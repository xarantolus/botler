#!/usr/bin/env python3
"""Supervise Playwright MCP in the browser service.

`browser-session` starts Xvfb + Chromium and execs this script, which:

1. waits for Chromium's DevTools endpoint (`http://127.0.0.1:9223/json/version`);
2. starts the child command (the rest of our argv: `playwright-mcp ...`) and passes its stdio
   straight through;
3. smoke-tests the whole path over HTTP: `initialize` + `tools/list` on the MCP endpoint, then two
   sessions prove their contexts are isolated, and the hosts blocklist is checked;
4. every minute checks that the child is alive and that CDP still answers, and reports
   `botler.health.ok()`; on child exit or CDP loss it reports `botler.health.fail(reason)` and exits
   non-zero, so the hub restarts the service and opens an incident.

`jobs/browser` has no Python package (it is excluded from the uv workspace), so this is a
standalone stdlib script; the botler SDK is on PYTHONPATH at run time.
"""

import contextlib
import json
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from collections.abc import Callable, Iterable
from pathlib import Path
from types import FrameType
from typing import Any, cast

import botler

CHECK_SECONDS = 60.0
RETRY_SECONDS = 5.0
START_TIMEOUT = 90.0
Opener = Callable[..., Any]


def _open(url: str | urllib.request.Request, timeout: float = 5.0) -> object:
    return urllib.request.urlopen(url, timeout=timeout)  # noqa: S310


def parse_endpoints(argv: list[str]) -> tuple[str, int]:
    """The CDP URL and MCP port from the child command line (defaults match job.toml)."""
    cdp, port = "http://127.0.0.1:9223", 8000
    for index, arg in enumerate(argv):
        if arg == "--cdp-endpoint" and index + 1 < len(argv):
            cdp = argv[index + 1]
        elif arg == "--port" and index + 1 < len(argv):
            port = int(argv[index + 1])
    return cdp, port


def idle_timeout_ms(argv: list[str]) -> int:
    """Playwright MCP's `--idle-timeout` (ms); the smoke test shortens its own check for small ones."""
    for index, arg in enumerate(argv):
        if arg == "--idle-timeout" and index + 1 < len(argv):
            return int(argv[index + 1])
    return 600_000


def cdp_ok(cdp_url: str, *, opener: Opener = _open, timeout: float = 5.0) -> bool:
    """Whether Chromium's own DevTools endpoint answers."""
    try:
        with opener(f"{cdp_url}/json/version", timeout=timeout) as response:
            return bool(getattr(response, "status", 200) == 200)
    except Exception:
        return False


def wait_cdp(
    cdp_url: str,
    *,
    timeout: float = START_TIMEOUT,
    interval: float = 1.0,
    opener: Opener = _open,
    sleep: Callable[[float], None] = time.sleep,
    now: Callable[[], float] = time.monotonic,
) -> bool:
    deadline = now() + timeout
    while now() < deadline:
        if cdp_ok(cdp_url, opener=opener):
            return True
        sleep(interval)
    return False


def _read_sse(response: Iterable[bytes | str], limit: int = 1_000_000) -> object:
    """The first JSON-RPC response in a `text/event-stream` body (Playwright MCP's default).

    The stream stays open after the response, so read events until one carries a `result`/`error`
    and then let the caller close the connection.
    """
    data: list[str] = []
    total = 0
    for raw in response:
        line = raw.decode("utf-8", "replace") if isinstance(raw, bytes) else str(raw)
        total += len(line)
        if not line.strip():
            continue
        if line.startswith("data:"):
            data.append(line[5:].strip())
            try:
                message: Any = json.loads("\n".join(data))
            except ValueError:
                continue
            if isinstance(message, dict) and ("result" in message or "error" in message):
                return cast("dict[str, Any]", message)
            data = []
        if total > limit:
            break
    return None


def mcp_call(
    mcp_url: str,
    payload: dict[str, Any],
    *,
    session_id: str | None = None,
    host_header: str = "browser:8000",
    opener: Opener = _open,
    timeout: float = 15.0,
) -> tuple[str | None, Any]:
    """One MCP JSON-RPC POST; returns the (possibly new) session id and the parsed response."""
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        # Match the name published in `--allowed-hosts`; Playwright MCP refuses other Host headers.
        "Host": host_header,
    }
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    request = urllib.request.Request(  # noqa: S310
        mcp_url, data=json.dumps(payload).encode(), headers=headers, method="POST"
    )
    with opener(request, timeout=timeout) as response:
        new_session: str | None = response.headers.get("Mcp-Session-Id")
        content_type = str(response.headers.get("Content-Type") or "")
        if "text/event-stream" in content_type:
            return new_session, _read_sse(response)
        body = response.read()
        return new_session, json.loads(body) if body else None


def result_text(response: object) -> str:
    """Join the text blocks of a tools/call result."""
    payload = cast("dict[str, Any]", response) if isinstance(response, dict) else {}
    result = cast("dict[str, Any]", payload.get("result") or {})
    content = cast("list[dict[str, Any]]", result.get("content") or [])
    return "\n".join(str(block.get("text", "")) for block in content)


def _initialize(mcp_url: str, opener: Opener) -> str | None:
    session, _ = mcp_call(
        mcp_url,
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "browser-supervisor", "version": "1"},
            },
        },
        opener=opener,
    )
    with contextlib.suppress(Exception):
        mcp_call(
            mcp_url,
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            session_id=session,
            opener=opener,
        )
    return session


def _call(mcp_url: str, session_id: str | None, name: str, arguments: dict[str, Any], opener: Opener) -> str:
    _, response = mcp_call(
        mcp_url,
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": name, "arguments": arguments}},
        session_id=session_id,
        opener=opener,
    )
    return result_text(response)


def tools_list(mcp_url: str, session_id: str | None, *, opener: Opener = _open) -> list[str]:
    _, listing = mcp_call(
        mcp_url, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, session_id=session_id, opener=opener
    )
    return [str(tool["name"]) for tool in listing["result"]["tools"]]


def smoke_test(mcp_url: str, *, opener: Opener = _open) -> list[str]:
    """initialize + tools/list; raises if the MCP endpoint does not answer."""
    session = _initialize(mcp_url, opener)
    return tools_list(mcp_url, session, opener=opener)


def verify_isolation(mcp_url: str, *, opener: Opener = _open) -> dict[str, object]:
    """Two MCP sessions must not share cookies: A sets one, B must not see it."""
    a = _initialize(mcp_url, opener)
    b = _initialize(mcp_url, opener)
    _call(mcp_url, a, "browser_navigate", {"url": "https://example.com/"}, opener)
    a_cookie = _call(
        mcp_url,
        a,
        "browser_evaluate",
        {"function": "() => { document.cookie = 'isolation=alpha; path=/'; return document.cookie; }"},
        opener,
    )
    _call(mcp_url, b, "browser_navigate", {"url": "https://example.com/"}, opener)
    b_cookie = _call(mcp_url, b, "browser_evaluate", {"function": "() => document.cookie"}, opener)
    return {
        "isolated": "alpha" in a_cookie and "alpha" not in b_cookie,
        "a_has_cookie": "alpha" in a_cookie,
        "b_has_cookie": "alpha" in b_cookie,
    }


def verify_idle(
    mcp_url: str,
    idle_ms: int,
    *,
    opener: Opener = _open,
    sleep: Callable[[float], None] = time.sleep,
) -> dict[str, object]:
    """With a short idle timeout, a context must be gone after it: set a cookie, wait, read it back."""
    if idle_ms > 60_000:
        return {"checked": False, "idle_ms": idle_ms}
    session = _initialize(mcp_url, opener)
    _call(mcp_url, session, "browser_navigate", {"url": "https://example.com/"}, opener)
    _call(
        mcp_url,
        session,
        "browser_evaluate",
        {"function": "() => { document.cookie = 'idle=beta; path=/'; return document.cookie; }"},
        opener,
    )
    sleep(idle_ms / 1000 + 5)
    after = _call(mcp_url, session, "browser_evaluate", {"function": "() => document.cookie"}, opener)
    return {"checked": True, "idle_ms": idle_ms, "cookie_gone": "beta" not in after}


def blocklist_check(hosts_path: str = "/opt/blocklist/hosts") -> dict[str, object]:
    """Whether known ad/tracker domains are in the pinned list and resolve to the block address."""
    domains = ["doubleclick.net", "googleadservices.com", "adservice.google.com", "googlesyndication.com"]
    try:
        text = Path(hosts_path).read_text(encoding="utf-8", errors="replace")
    except OSError as e:
        return {"present": False, "error": type(e).__name__}
    resolved: dict[str, str] = {}
    for domain in domains:
        with contextlib.suppress(OSError):
            resolved[domain] = socket.gethostbyname(domain)
    return {
        "present": True,
        "listed": [d for d in domains if f"\t{d}\n" in text or f" {d}\n" in text],
        "resolved": resolved,
    }


def supervise(
    child: subprocess.Popen[bytes],
    *,
    cdp_url: str,
    health_ok: Callable[[], object],
    health_fail: Callable[[str], object],
    interval: float = CHECK_SECONDS,
    retry_interval: float = RETRY_SECONDS,
    cdp_check: Callable[..., bool] = cdp_ok,
    sleep: Callable[[float], None] = time.sleep,
) -> int:
    """Report health until the child dies or CDP stops answering; then fail so the hub restarts.

    A failed health report (e.g. the hub restarting and briefly refusing its socket) is logged and
    retried after `retry_interval`, not a full `interval`. The SDK already waits out a short hub
    restart inside one call; reporting again promptly afterwards keeps the heartbeat fresh, so the
    hub does not mark an otherwise healthy browser unhealthy for a restart it caused itself
    (incident 2026-11-01).
    """
    delay = interval
    while True:
        sleep(delay)
        delay = interval
        code = child.poll()
        if code is not None:
            health_fail(f"playwright-mcp exited with code {code}")
            return 1
        if not cdp_check(cdp_url):
            health_fail("Chromium's CDP stopped answering")
            return 1
        try:
            health_ok()
        except Exception as e:
            botler.log(
                "browser supervisor: health report failed; will retry shortly",
                error=f"{type(e).__name__}: {e}",
            )
            delay = retry_interval


def fail(reason: str) -> int:
    botler.log("browser supervisor: " + reason)
    with contextlib.suppress(Exception):
        botler.health.fail(reason)
    return 1


def run_checks(mcp_url: str, idle_ms: int) -> None:
    """The startup smoke checks; diagnostics only, never fatal to the service."""
    try:
        with contextlib.suppress(Exception):
            botler.log("browser isolation", **verify_isolation(mcp_url))
        botler.log("browser blocklist", **blocklist_check())
        if idle_ms <= 60_000:
            with contextlib.suppress(Exception):
                botler.log("browser idle", **verify_idle(mcp_url, idle_ms))
    except Exception as e:  # the checks must never take the service down
        botler.log("browser supervisor: smoke checks failed", error=f"{type(e).__name__}: {e}")


def main(argv: list[str]) -> int:
    cdp_url, port = parse_endpoints(argv)
    mcp_url = f"http://127.0.0.1:{port}/mcp"
    if not wait_cdp(cdp_url):
        return fail(f"Chromium's CDP never came up at {cdp_url}")

    child = subprocess.Popen(argv)  # noqa: S603 - argv is our own job command line

    def on_signal(signum: int, frame: FrameType | None) -> None:
        with contextlib.suppress(Exception):
            child.terminate()
        raise SystemExit(0)

    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)

    tools: list[str] | None = None
    deadline = time.monotonic() + START_TIMEOUT
    while time.monotonic() < deadline:
        if child.poll() is not None:
            return fail(f"playwright-mcp exited with code {child.returncode} during startup")
        try:
            tools = smoke_test(mcp_url)
            break
        except Exception:
            time.sleep(1.0)
    if tools is None:
        with contextlib.suppress(Exception):
            child.terminate()
        return fail("MCP smoke test never answered")

    botler.log("browser MCP smoke test ok", tools=tools)
    run_checks(mcp_url, idle_timeout_ms(argv))
    return supervise(child, cdp_url=cdp_url, health_ok=botler.health.ok, health_fail=botler.health.fail)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
