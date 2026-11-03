"""A minimal MCP server (streamable HTTP) exposing `send_dm`.

Pi reaches a service listed in its `mcp = [...]` at `http://<name>:<port>/mcp`. This speaks the
subset of MCP a tools-only server needs over the streamable-HTTP transport: JSON-RPC `initialize`,
`ping`, `tools/list` and `tools/call`, answered with `application/json`. `GET /mcp` is answered as
an (optional) SSE keepalive stream, and `DELETE /mcp` closes a session — both so any MCP client,
including the one pi uses, is satisfied without a session store (the server is stateless).

No MCP SDK: stateless, one tool.
"""

from __future__ import annotations

import json
import sys
import threading
from collections.abc import Callable
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, cast

from pydantic import BaseModel, ConfigDict, Field, ValidationError

PROTOCOL_VERSION = "2025-06-18"
MAX_SSE_STREAMS = 8
SSE_KEEPALIVE_SECONDS = 15.0


class JsonRpcParams(BaseModel):
    """The params we read from a JSON-RPC message; every other field is ignored."""

    model_config = ConfigDict(extra="ignore")

    name: str = ""
    arguments: dict[str, Any] = {}
    # The wire name of the JSON-RPC protocol version field.
    protocolVersion: str = ""


class JsonRpcMessage(BaseModel):
    """One JSON-RPC request/notification. `id` is `object` because it may be int or string."""

    model_config = ConfigDict(extra="ignore")

    id: object = None
    method: str = ""
    params: JsonRpcParams = Field(default_factory=JsonRpcParams)


INSTRUCTIONS = "Send the paired user a Discord direct message as the bot."


@dataclass(frozen=True)
class Tool:
    name: str
    description: str
    input_schema: dict[str, Any]
    call: Callable[[dict[str, Any]], str]


class SendDmArgs(BaseModel):
    model_config = ConfigDict(extra="forbid")

    user_id: str = Field(description="The Discord user id (digits) to message, e.g. the paired user's.")
    text: str = Field(description="The message. Longer than 2000 characters is split into several messages.")


def build_tools(send_dm: Callable[[str, str], str]) -> list[Tool]:
    """`send_dm(user_id, text)` posts the message and returns its id."""

    def call(arguments: dict[str, Any]) -> str:
        args = SendDmArgs.model_validate(arguments)
        return f"sent (message id {send_dm(args.user_id, args.text)})"

    schema = SendDmArgs.model_json_schema()
    schema.pop("title", None)
    return [
        Tool(
            name="send_dm",
            description=(
                "Send a Discord direct message to a user as the bot, without waiting for them to write "
                "first. Use it to tell the user something on your own initiative, e.g. a reminder that is "
                "due. The user must have opened a DM with the bot once (Discord refuses otherwise). "
                "Returns the message id."
            ),
            input_schema=schema,
            call=call,
        )
    ]


class McpServer:
    """The HTTP server. Create it (this binds the port), then `serve_forever()` in a thread."""

    def __init__(self, tools: list[Tool], port: int, host: str = "0.0.0.0") -> None:  # noqa: S104
        self.tools = {tool.name: tool for tool in tools}
        self._listing = [
            {"name": tool.name, "description": tool.description, "inputSchema": tool.input_schema}
            for tool in tools
        ]
        self._streams = 0
        self._streams_lock = threading.Lock()
        self._serving = threading.Event()
        self._httpd = ThreadingHTTPServer((host, port), self._make_handler())
        self.port = int(self._httpd.server_address[1])

    def serve_forever(self) -> None:
        self._serving.set()
        self._httpd.serve_forever()

    def shutdown(self) -> None:
        # `shutdown()` waits for a running `serve_forever`; it would block if it was never started.
        if self._serving.is_set():
            self._httpd.shutdown()
        self._httpd.server_close()

    # -- JSON-RPC --------------------------------------------------------------------------
    def handle(self, message: dict[str, Any]) -> dict[str, Any] | None:
        """The response to one JSON-RPC message, or None for a notification."""
        rpc = JsonRpcMessage.model_validate(message)
        if "id" not in rpc.model_fields_set:
            return None
        params = rpc.params
        if rpc.method == "initialize":
            result: dict[str, Any] = {
                "protocolVersion": params.protocolVersion or PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "discord", "version": "1"},
                "instructions": INSTRUCTIONS,
            }
        elif rpc.method == "ping":
            result = {}
        elif rpc.method == "tools/list":
            result = {"tools": self._listing}
        elif rpc.method == "tools/call":
            result = self._call(params.name, params.arguments)
        else:
            return _error(rpc.id, -32601, f"unknown method: {rpc.method}")
        return {"jsonrpc": "2.0", "id": rpc.id, "result": result}

    def _call(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        tool = self.tools.get(name)
        if tool is None:
            return {
                "content": [{"type": "text", "text": f"unknown tool: {name}"}],
                "isError": True,
            }
        try:
            text = tool.call(arguments)
        except Exception as e:  # reported to the caller, never dropped
            return {
                "content": [{"type": "text", "text": f"{type(e).__name__}: {e}"}],
                "isError": True,
            }
        return {"content": [{"type": "text", "text": text}], "isError": False}

    # -- HTTP ------------------------------------------------------------------------------
    def _make_handler(self) -> type[BaseHTTPRequestHandler]:
        server = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: Any) -> None:  # noqa: ANN401
                print(f"mcp {self.address_string()} {format % args}", file=sys.stderr, flush=True)

            def _send(self, status: int, body: bytes, content_type: str | None = None) -> None:
                self.send_response(status)
                if content_type:
                    self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                if body:
                    self.wfile.write(body)

            def _json(self, status: int, payload: Any) -> None:  # noqa: ANN401
                self._send(status, json.dumps(payload).encode(), "application/json")

            def _is_mcp(self) -> bool:
                return self.path.rstrip("/") == "/mcp"

            def do_POST(self) -> None:
                if not self._is_mcp():
                    self._json(404, {"error": "not found; the MCP endpoint is /mcp"})
                    return
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length)
                try:
                    payload: object = json.loads(raw or b"null")
                except ValueError:
                    self._json(400, {"error": "the request body is not JSON"})
                    return
                raw_items = cast("list[object]", payload) if isinstance(payload, list) else [payload]
                batch = isinstance(payload, list)
                messages: list[dict[str, Any]] = [item for item in raw_items if isinstance(item, dict)]
                if len(messages) != len(raw_items) or not messages:
                    self._json(400, {"error": "expected a JSON-RPC object or array"})
                    return
                try:
                    responses = [r for r in (server.handle(m) for m in messages) if r is not None]
                except ValidationError:
                    self._json(400, {"error": "expected a JSON-RPC object or array"})
                    return
                if not responses:
                    self._send(202, b"")
                    return
                self._json(200, responses if batch else responses[0])

            def do_GET(self) -> None:
                if not self._is_mcp():
                    self._json(404, {"error": "not found"})
                    return
                if "text/event-stream" not in (self.headers.get("Accept") or ""):
                    self.send_response(405)
                    self.send_header("Allow", "POST, DELETE")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                with server._streams_lock:
                    if server._streams >= MAX_SSE_STREAMS:
                        self._send(503, b"too many event streams")
                        return
                    server._streams += 1
                try:
                    self.close_connection = True
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    while True:
                        self.wfile.write(b": keepalive\n\n")
                        self.wfile.flush()
                        threading.Event().wait(SSE_KEEPALIVE_SECONDS)
                except (BrokenPipeError, ConnectionResetError, OSError):
                    pass
                finally:
                    with server._streams_lock:
                        server._streams -= 1

            def do_DELETE(self) -> None:
                self._send(204, b"")

            def do_OPTIONS(self) -> None:
                self.send_response(204)
                self.send_header("Allow", "GET, POST, DELETE, OPTIONS")
                self.send_header("Content-Length", "0")
                self.end_headers()

        return Handler


def _error(request_id: object, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}
