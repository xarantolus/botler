from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
import socketserver
import tempfile
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any

import pytest

SDK = Path(__file__).resolve().parents[1]


class FakeHub:
    """Records ops calls; answers with `responses[op]` = (status, body)."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, Any]] = []
        self.responses: dict[str, tuple[int, Any]] = {}
        hub = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                op = self.path.removeprefix("/api/v1/ops/")
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                hub.calls.append((op, self.headers["Authorization"], body))
                status, reply = hub.responses.get(op, (200, {"ok": True}))
                data = json.dumps(reply).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *_: Any) -> None:
                pass

        self.handler = Handler
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()


class _UnixHTTPServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


@pytest.fixture
def hub(monkeypatch: pytest.MonkeyPatch) -> Iterator[FakeHub]:
    h = FakeHub()
    monkeypatch.delenv("BOTLER_SOCKET", raising=False)
    monkeypatch.setenv("BOTLER_URL", h.url)
    monkeypatch.setenv("BOTLER_TOKEN", "tok")
    yield h
    h.server.shutdown()


def run_py(code: str, *args: str, cwd: Path | None = None, stdin: str = "", **env: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, *(["-c", code] if code else []), *args],
        cwd=cwd,
        input=stdin,
        capture_output=True,
        text=True,
        env={**os.environ, "PYTHONPATH": str(SDK), **env},
        check=False,
    )


def test_runtime_imports_without_pydantic(hub: FakeHub) -> None:
    # `None` in sys.modules makes `import pydantic` fail, as in an image without it.
    code = textwrap.dedent(
        """
        import sys
        sys.modules["pydantic"] = None
        import botler
        botler.send_message("main", "hi")
        botler.emit("mail.received", {"a": 1})
        print(botler.state.get("cursor"))
        assert "botler.events" not in sys.modules and "botler.tools" not in sys.modules
        try:
            botler.Event
        except ImportError:
            print("lazy")
        """
    )
    hub.responses["state_get"] = (200, {"value": None})
    p = run_py(code, BOTLER_URL=hub.url, BOTLER_TOKEN="tok")
    assert p.returncode == 0, p.stderr
    assert p.stdout.split() == ["None", "lazy"]
    assert hub.calls == [
        ("send_message", "Bearer tok", {"to": "main", "text": "hi", "urgent": False}),
        ("emit_event", "Bearer tok", {"type": "mail.received", "payload": {"a": 1}}),
        ("state_get", "Bearer tok", {"key": "cursor"}),
    ]


def test_hub_errors_raise_botler_error(hub: FakeHub) -> None:
    import botler

    hub.responses["emit_event"] = (422, {"error": "payload does not match"})
    with pytest.raises(botler.BotlerError) as e:
        botler.emit("mail.received", {})
    assert (e.value.code, e.value.status, e.value.message) == ("invalid_event", 422, "payload does not match")
    hub.responses["state_set"] = (500, {"error": "disk full"})
    with pytest.raises(botler.BotlerError) as e:
        botler.state.set("k", "v")
    assert (e.value.code, e.value.status) == ("internal", 500)


def test_the_socket_is_used_when_set(monkeypatch: pytest.MonkeyPatch) -> None:
    """In containers the hub's Unix socket needs no network; it wins over BOTLER_URL."""
    import botler

    h = FakeHub()
    with tempfile.TemporaryDirectory() as d:
        path = f"{d}/hub.sock"
        server = _UnixHTTPServer(path, h.handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        monkeypatch.setenv("BOTLER_SOCKET", path)
        monkeypatch.setenv("BOTLER_URL", "http://127.0.0.1:9")
        monkeypatch.setenv("BOTLER_TOKEN", "tok")
        assert botler.ops.list_timers() == {"ok": True}
        assert h.calls == [("list_timers", "Bearer tok", {})]
        h.responses["list_timers"] = (403, {"error": "no"})
        with pytest.raises(botler.BotlerError) as e:
            botler.ops.list_timers()
        assert (e.value.code, e.value.status, e.value.message) == ("forbidden", 403, "no")
        server.shutdown()
        server.server_close()
        monkeypatch.setenv("BOTLER_SOCKET", f"{d}/gone.sock")
        with pytest.raises(botler.BotlerError) as e:
            botler.ops.list_timers()
        assert e.value.code == "unreachable" and e.value.status is None
    h.server.shutdown()


def test_action_args(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    import botler

    monkeypatch.delenv("BOTLER_ACTION_ARGS", raising=False)
    with pytest.raises(botler.BotlerError) as e:
        botler.action_args()
    assert e.value.code == "not_configured"
    args = tmp_path / "args.json"
    args.write_text('{"to": "a@b.c", "subject": "Hi"}')
    monkeypatch.setenv("BOTLER_ACTION_ARGS", str(args))
    assert botler.action_args() == {"to": "a@b.c", "subject": "Hi"}


def test_unreachable_and_unconfigured(monkeypatch: pytest.MonkeyPatch) -> None:
    import botler

    monkeypatch.delenv("BOTLER_SOCKET", raising=False)
    monkeypatch.delenv("BOTLER_URL", raising=False)
    with pytest.raises(botler.BotlerError) as e:
        botler.ops.list_timers()
    assert e.value.code == "not_configured"
    monkeypatch.setenv("BOTLER_URL", "http://127.0.0.1:9")
    monkeypatch.setenv("BOTLER_TOKEN", "t")
    with pytest.raises(botler.BotlerError) as e:
        botler.ops.list_timers()
    assert e.value.code == "unreachable" and e.value.status is None


def test_emit_event_instance(hub: FakeHub) -> None:
    from typing import ClassVar

    import botler

    class Ping(botler.Event):
        type: ClassVar[str] = "test.ping"
        n: int

    botler.emit(Ping(n=3), provenance="untrusted")
    assert hub.calls[-1][2] == {"type": "test.ping", "payload": {"n": 3}, "provenance": "untrusted"}


def monorepo(tmp_path: Path) -> Path:
    pkg = tmp_path / "tools" / "demo" / "src" / "demo"
    pkg.mkdir(parents=True)
    (pkg / "__init__.py").write_text(
        textwrap.dedent(
            '''
            from typing import ClassVar
            import botler

            class Greeted(botler.Event):
                """Someone was greeted."""
                type: ClassVar[str] = "demo.greeted"
                name: str

            class Whispered(botler.Event):
                """A secret was whispered."""
                type: ClassVar[str] = "demo.whispered"
                restricted: ClassVar[bool] = True
                text: str

            @botler.tool(effects=["network"])
            def greet(name: str, times: int = 1) -> dict[str, str]:
                """Greet someone."""
                if name == "boom":
                    raise RuntimeError("exploded")
                return {"text": " ".join(["hi " + name] * times)}
            '''
        )
    )
    return tmp_path


def test_manifests(tmp_path: Path) -> None:
    root = monorepo(tmp_path)
    for mod, file in [("botler.events", "events.manifest.json"), ("botler.tools", "tools.manifest.json")]:
        assert run_py("", "-m", mod, "manifest", "--check", "--root", str(root)).returncode == 1
        p = run_py("", "-m", mod, "manifest", "--root", str(root))
        assert p.returncode == 0, p.stderr
        p = run_py("", "-m", mod, "manifest", "--check", "--root", str(root))
        assert p.returncode == 0, p.stderr
    events = json.loads((root / "events.manifest.json").read_text())
    assert events == [
        {
            "type": "demo.greeted",
            "description": "Someone was greeted.",
            "schema": {
                "description": "Someone was greeted.",
                "properties": {"name": {"title": "Name", "type": "string"}},
                "required": ["name"],
                "title": "Greeted",
                "type": "object",
            },
        },
        {
            "type": "demo.whispered",
            "description": "A secret was whispered.",
            "schema": {
                "description": "A secret was whispered.",
                "properties": {"text": {"title": "Text", "type": "string"}},
                "required": ["text"],
                "title": "Whispered",
                "type": "object",
            },
            "restricted": True,
        },
    ]
    tools = json.loads((root / "tools.manifest.json").read_text())
    assert tools == [
        {
            "name": "demo.greet",
            "description": "Greet someone.",
            "parameters": {
                "additionalProperties": False,
                "properties": {
                    "name": {"title": "Name", "type": "string"},
                    "times": {"default": 1, "title": "Times", "type": "integer"},
                },
                "required": ["name"],
                "type": "object",
            },
            "effects": ["network"],
            "approval": False,
            "module": "demo",
            "function": "greet",
        }
    ]


def test_call_protocol(tmp_path: Path) -> None:
    root = monorepo(tmp_path)
    assert run_py("", "-m", "botler.tools", "manifest", "--root", str(root)).returncode == 0
    path = f"{SDK}{os.pathsep}{root / 'tools/demo/src'}"

    def call(name: str, args: str) -> tuple[int, Any]:
        p = run_py("", "-m", "botler.tools", "call", name, cwd=root, stdin=args, PYTHONPATH=path)
        return p.returncode, json.loads(p.stdout)

    assert call("demo.greet", '{"name": "ada", "times": 2}') == (0, {"ok": True, "result": {"text": "hi ada hi ada"}})
    code, out = call("demo.greet", '{"times": "x"}')
    assert code == 1 and out["error"]["type"] == "ValidationError" and "name" in out["error"]["message"]
    assert call("demo.greet", '{"name": "boom"}') == (1, {"ok": False, "error": {"type": "RuntimeError", "message": "exploded"}})
    code, out = call("demo.nope", "{}")
    assert code == 1 and out["error"]["type"] == "LookupError"


def test_render_hints(monkeypatch: pytest.MonkeyPatch) -> None:
    from pydantic import BaseModel

    from botler import tools
    from botler.tools import Image, Json, KeyValue, Links, Markdown, Table

    monkeypatch.setattr(tools, "_REGISTRY", {})

    class Hit(BaseModel):
        title: str
        url: str

    class Found(BaseModel):
        backend: str
        results: list[Hit] | None

    def entry(render: Any, returns: Any) -> dict[str, Any]:
        tools._REGISTRY.clear()

        def fn() -> None:
            """A tool."""

        fn.__annotations__ = {"return": returns}
        fn.__qualname__ = "fn"
        tools.tool(name="t.fn", render=render)(fn)
        return tools.manifest()[0]

    assert entry(Links(items="results", url="url"), Found)["render"] == {"kind": "links", "items": "results", "url": "url"}
    assert entry(Table(columns=["title"]), list[Hit])["render"] == {"kind": "table", "columns": ["title"]}
    assert entry(Markdown(), str)["render"] == {"kind": "markdown"}
    assert "render" not in entry(None, str)
    # Without a pydantic return type a hint cannot be checked; it is kept as declared.
    assert entry(Links(items="anything"), dict[str, Any])["render"]["items"] == "anything"
    for bad, returns, name in [
        (Links(items="hits"), Found, "hits"),
        (Links(items="results", snippet="snippet"), Found, "snippet"),
        (Table(columns=["title", "rank"]), list[Hit], "rank"),
        (Image(field="png"), Found, "png"),
    ]:
        with pytest.raises(ValueError, match=f"names {name}, which"):
            entry(bad, returns)
    with pytest.raises(TypeError, match="render must be one of"):
        tools.tool(name="t.x", render={"kind": "links"})  # type: ignore[arg-type]
    with pytest.raises(ValueError):
        Links(itmes="results")  # type: ignore[call-arg]
    assert KeyValue().kind == "keyvalue" and Json().kind == "json"


def test_render_hints_match_the_hub() -> None:
    """The hub's `ToolRender` (its JSON Schema, written by `BOTLER_BLESS=1 cargo test -p botler
    --test sdk`) and these models have the same kinds and fields."""
    from botler.tools import Image, Json, KeyValue, Links, Markdown, Table

    schema = json.loads((SDK / "tests" / "tool_render.schema.json").read_text())
    hub: dict[str, set[str]] = {}
    for variant in schema["oneOf"]:
        props = variant["properties"]
        hub[props["kind"].get("const") or props["kind"]["enum"][0]] = set(props) - {"kind"}
    models = (Links, Table, Markdown, KeyValue, Image, Json)
    assert {m.model_fields["kind"].default: set(m.model_fields) - {"kind"} for m in models} == hub


def test_settings_manifest_and_load(tmp_path: Path) -> None:
    pkg = tmp_path / "jobs" / "mail-watch" / "src" / "mail_watch_settings_demo"
    pkg.mkdir(parents=True)
    (pkg / "__init__.py").write_text("")
    (pkg / "settings.py").write_text(
        textwrap.dedent(
            '''
            from pydantic import Field

            import botler


            class Settings(botler.Settings):
                """Which folders to watch."""

                folders: list[str] = ["INBOX"]
                every_minutes: int = Field(5, ge=1, description="How often to look")
            '''
        )
    )
    p = run_py("", "-m", "botler.settings", "manifest", "--root", str(tmp_path))
    assert p.returncode == 0, p.stderr
    [entry] = json.loads((tmp_path / "settings.manifest.json").read_text())
    assert (entry["job"], entry["description"]) == ("mail-watch", "Which folders to watch.")
    assert entry["schema"]["properties"]["every_minutes"]["minimum"] == 1
    assert entry["schema"]["additionalProperties"] is False

    values = tmp_path / "settings.json"
    values.write_text(json.dumps({"every_minutes": 15}))
    code = (
        f"import sys; sys.path.insert(0, {str(pkg.parent)!r}); from mail_watch_settings_demo.settings import Settings; "
        "s = Settings.load(); print(s.every_minutes, s.folders)"
    )
    p = run_py(code, BOTLER_SETTINGS_FILE=str(values))
    assert p.stdout.split() == ["15", "['INBOX']"], p.stderr
    p = run_py(code, BOTLER_SETTINGS_FILE=str(tmp_path / "missing.json"))
    assert p.stdout.split() == ["5", "['INBOX']"], "defaults without a file"
