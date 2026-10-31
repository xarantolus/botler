"""A stdio MCP server with one tool, `message_user`, that Claude Code starts for the session
(`--mcp-config`). It sends the text through the botler SDK to the chat named in the file
CLAUDE_SESSION_NOTIFY_FILE (the chat of the session's latest message). The protocol is
newline-delimited JSON-RPC 2.0; only what a tools-only server needs is here."""

import json
import os
import sys
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import IO, Any

import botler

# The session's secrets: never in a message, whatever Claude writes.
SECRETS = ("CLAUDE_CODE_OAUTH_TOKEN", "GITHUB_TOKEN")

TOOL = {
    "name": "message_user",
    "description": (
        "Send the user a short message now, while you keep working: progress worth knowing "
        "(a PR is up), or that you are blocked. For questions, ask in your final reply instead: "
        "the answer comes back as a new message."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {"text": {"type": "string", "description": "The message, plain text."}},
        "required": ["text"],
    },
}


def redact(text: str, environ: Mapping[str, str]) -> str:
    for name in SECRETS:
        if value := environ.get(name):
            text = text.replace(value, f"[{name}]")
    return text


def handle(request: dict[str, Any], send: Callable[[str], object]) -> dict[str, Any] | None:
    """The response to one JSON-RPC message, or None for notifications."""
    if "id" not in request:
        return None
    method = request.get("method")
    params: dict[str, Any] = request.get("params") or {}
    result: dict[str, Any]
    if method == "initialize":
        result = {
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "botler", "version": "1"},
        }
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {"tools": [TOOL]}
    elif method == "tools/call" and params.get("name") == TOOL["name"]:
        arguments: dict[str, Any] = params.get("arguments") or {}
        text = str(arguments.get("text", "")).strip()
        try:
            if not text:
                raise ValueError("text is empty")
            send(text)
            result = {"content": [{"type": "text", "text": "Sent."}]}
        except (botler.BotlerError, ValueError) as e:
            result = {"content": [{"type": "text", "text": f"Not sent: {e}"}], "isError": True}
    else:
        error = {"code": -32601, "message": f"unknown method or tool: {method}"}
        return {"jsonrpc": "2.0", "id": request["id"], "error": error}
    return {"jsonrpc": "2.0", "id": request["id"], "result": result}


def serve(stdin: IO[str], stdout: IO[str], send: Callable[[str], object]) -> None:
    for line in stdin:
        if not line.strip():
            continue
        response = handle(json.loads(line), send)
        if response is not None:
            stdout.write(json.dumps(response) + "\n")
            stdout.flush()


def main() -> None:
    notify, session = Path(os.environ["CLAUDE_SESSION_NOTIFY_FILE"]), os.environ["CLAUDE_SESSION_ID"]

    def send(text: str) -> object:
        text = redact(text, os.environ)
        return botler.send_message(notify.read_text(), f"Claude Code session {session} says: {text}")

    serve(sys.stdin, sys.stdout, send)


if __name__ == "__main__":
    main()
