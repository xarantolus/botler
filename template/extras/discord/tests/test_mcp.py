"""MCP server tests: the JSON-RPC surface and a real HTTP round trip."""

from __future__ import annotations

import json
import threading
import urllib.request
from typing import Any

from discord_bot.mcp import McpServer, build_tools


def make_server(sent: list[tuple[str, str]]) -> McpServer:
    def send_dm(user_id: str, text: str) -> str:
        sent.append((user_id, text))
        return "42"

    return McpServer(build_tools(send_dm), port=0, host="127.0.0.1")


def rpc(server: McpServer, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
    response = server.handle({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}})
    assert response is not None
    return response


def test_lists_and_calls_send_dm() -> None:
    sent: list[tuple[str, str]] = []
    server = make_server(sent)
    try:
        tools = rpc(server, "tools/list")["result"]["tools"]
        assert [t["name"] for t in tools] == ["send_dm"]
        assert set(tools[0]["inputSchema"]["required"]) == {"user_id", "text"}
        result = rpc(server, "tools/call", {"name": "send_dm", "arguments": {"user_id": "1", "text": "hi"}})
        assert result["result"]["isError"] is False and sent == [("1", "hi")]
    finally:
        server.shutdown()


def test_bad_arguments_are_an_error_result_not_a_crash() -> None:
    server = make_server([])
    try:
        result = rpc(server, "tools/call", {"name": "send_dm", "arguments": {"user_id": "1"}})["result"]
        assert result["isError"] is True
        assert "unknown tool" in rpc(server, "tools/call", {"name": "x"})["result"]["content"][0]["text"]
    finally:
        server.shutdown()


def test_http_round_trip() -> None:
    server = make_server([])
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        request = urllib.request.Request(
            f"http://127.0.0.1:{server.port}/mcp",
            data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=5) as response:  # noqa: S310
            body: Any = json.loads(response.read())
        assert body["result"]["tools"]
    finally:
        server.shutdown()
