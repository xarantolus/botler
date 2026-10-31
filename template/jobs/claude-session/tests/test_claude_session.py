import io
import json
import subprocess
from pathlib import Path
from typing import Any
from uuid import UUID

import botler
import pytest
from claude_session import __main__ as job
from claude_session.mcp import handle, serve
from shared.events import ClaudeSessionFinished, ClaudeSessionRequested

SID = "0b9f3c62-3f0e-4a8e-9a39-6d3c7f0c1a11"


def event(payload: dict[str, Any], source: dict[str, str] | None = None) -> dict[str, Any]:
    return {
        "type": "claude_session.requested",
        "source": source or {"kind": "agent", "name": "chat-7"},
        "payload": payload,
    }


def test_request_defaults_to_notifying_the_emitter() -> None:
    req, notify = job.read_request(event({"task": "fix the tests"}))
    assert (req.task, req.repo, req.resume, notify) == ("fix the tests", None, None, "chat-7")
    _, notify = job.read_request(event({"task": "t", "notify": "botler"}))
    assert notify == "botler"
    _, notify = job.read_request(event({"task": "t"}, {"kind": "user"}))
    assert notify == "botler"


def test_bad_requests_are_refused() -> None:
    with pytest.raises(SystemExit, match=r"claude_session\.requested"):
        job.read_request({"type": "manual", "payload": {}})
    with pytest.raises(SystemExit):
        job.read_request(None)
    with pytest.raises(ValueError, match="resume"):
        job.read_request(event({"task": "t", "resume": "../../etc"}))


def test_command_starts_or_resumes_and_keeps_the_task_off_argv() -> None:
    new = job.command(ClaudeSessionRequested(task="--help"), SID, Path("m.json"))
    assert new[:2] == ["claude", "-p"] and new[-2:] == ["--session-id", SID] and "--help" not in new
    assert "bypassPermissions" in new and "m.json" in new
    again = job.command(ClaudeSessionRequested(task="go on", resume=UUID(SID)), SID, Path("m.json"))
    assert again[-2:] == ["--resume", SID]


def test_mcp_config_passes_the_hub_connection() -> None:
    env = {
        "BOTLER_TOKEN": "t",
        "BOTLER_SOCKET": "/s",
        "PYTHONPATH": "/opt/botler/sdk",
        "CLAUDE_CODE_OAUTH_TOKEN": "x",
    }
    server = job.mcp_config(SID, "chat-7", env)["mcpServers"]["botler"]
    assert server["args"] == ["-m", "claude_session.mcp"]
    assert server["env"] == {
        "BOTLER_TOKEN": "t",
        "BOTLER_SOCKET": "/s",
        "PYTHONPATH": "/opt/botler/sdk",
        "CLAUDE_SESSION_ID": SID,
        "CLAUDE_SESSION_NOTIFY": "chat-7",
    }


def test_parse_output_reads_claudes_json() -> None:
    out = json.dumps(
        {
            "type": "result",
            "subtype": "success",
            "is_error": False,
            "result": "Done: PR #4",
            "session_id": SID,
            "total_cost_usd": 0.12,
        }
    )
    assert job.parse_output("other", out, "", 0) == job.Outcome(True, SID, "Done: PR #4", 0.12)
    failed = json.dumps(
        {"is_error": True, "result": "Invalid API key · Please run /login", "session_id": SID}
    )
    assert job.parse_output(SID, failed, "", 1) == job.Outcome(
        False, SID, "Invalid API key · Please run /login"
    )
    long = job.parse_output(SID, json.dumps({"result": "x" * 5000}), "", 0)
    assert long.ok and long.session_id == SID and long.result.endswith(" [cut]") and len(long.result) < 3100


def test_parse_output_without_json_reports_stderr() -> None:
    o = job.parse_output(SID, "", "error: unknown option '--bogus'\n", 1)
    assert not o.ok and o.session_id == SID and "code 1: error: unknown option '--bogus'" in o.result


class FakeHub:
    def __init__(self) -> None:
        self.state: dict[str, str] = {}
        self.emitted: list[ClaudeSessionFinished] = []
        self.sent: list[tuple[str, str]] = []

    def hub(self) -> job.Hub:
        return job.Hub(
            self.state.__setitem__, self.emitted.append, lambda to, text: self.sent.append((to, text))
        )


def test_report_records_emits_and_messages() -> None:
    fake = FakeHub()
    req = ClaudeSessionRequested(task="fix it", repo="https://example.com/r.git")
    job.report(job.Outcome(True, SID, "PR is up", 0.5), req, "chat-7", fake.hub())
    assert json.loads(fake.state[f"session:{SID}"])["result"] == "PR is up"
    assert fake.emitted == [
        ClaudeSessionFinished(session_id=SID, ok=True, result="PR is up", notify="chat-7", cost_usd=0.5)
    ]
    [(to, text)] = fake.sent
    assert to == "chat-7" and "PR is up" in text and f'resume="{SID}"' in text and "r.git" in text


def test_login_failures_say_how_to_fix_them() -> None:
    fake = FakeHub()
    job.report(
        job.Outcome(False, SID, "Invalid API key · Please run /login"),
        ClaudeSessionRequested(task="t"),
        "c",
        fake.hub(),
    )
    assert "failed" in fake.sent[0][1] and "claude setup-token" in fake.sent[0][1]
    assert not fake.emitted[0].ok


def test_run_claude_clones_then_runs_in_the_session_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls: list[tuple[list[str], dict[str, object]]] = []

    def fake_run(cmd: list[str], **kw: object) -> subprocess.CompletedProcess[str]:
        calls.append((cmd, kw))
        if cmd[0] == "git":
            Path(cmd[-1]).mkdir(parents=True, exist_ok=True)
            return subprocess.CompletedProcess(cmd, 0, "", "")
        config = json.loads(Path(cmd[cmd.index("--mcp-config") + 1]).read_text())
        assert config["mcpServers"]["botler"]["env"]["CLAUDE_SESSION_NOTIFY"] == "chat-7"
        sid = cmd[-1]
        return subprocess.CompletedProcess(cmd, 0, json.dumps({"result": "ok", "session_id": sid}), "")

    monkeypatch.setattr(job.subprocess, "run", fake_run)
    req = ClaudeSessionRequested(task="do it", repo="https://example.com/r.git")
    o = job.run_claude(req, "chat-7", tmp_path, {"BOTLER_TOKEN": "t"})
    assert o.ok and o.result == "ok"
    (git, git_kw), (_, kw) = calls
    assert git[:3] == ["git", "clone", "--"] and git_kw["env"] == {
        "BOTLER_TOKEN": "t",
        "HOME": str(tmp_path / "home"),
    }
    assert kw["cwd"] == tmp_path / "sessions" / o.session_id and kw["input"] == "do it"


def test_failed_clone_is_reported_without_running_claude(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def fake_run(cmd: list[str], **_: object) -> subprocess.CompletedProcess[str]:
        assert cmd[0] == "git"
        return subprocess.CompletedProcess(cmd, 128, "", "fatal: repository not found")

    monkeypatch.setattr(job.subprocess, "run", fake_run)
    o = job.run_claude(ClaudeSessionRequested(task="t", repo="https://x/y.git"), "c", tmp_path, {})
    assert not o.ok and "repository not found" in o.result


def test_mcp_message_user_sends() -> None:
    sent: list[str] = []
    init = handle(
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
        sent.append,
    )
    assert init and init["result"]["protocolVersion"] == "2025-06-18"
    assert handle({"jsonrpc": "2.0", "method": "notifications/initialized"}, sent.append) is None
    tools = handle({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, sent.append)
    assert tools and [t["name"] for t in tools["result"]["tools"]] == ["message_user"]
    call = {"name": "message_user", "arguments": {"text": "PR is up"}}
    r = handle({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": call}, sent.append)
    assert r and r["result"]["content"][0]["text"] == "Sent." and sent == ["PR is up"]


def test_mcp_errors_are_tool_results() -> None:
    def broken(_: str) -> object:
        raise botler.BotlerError("send_message", "bad_request", "no agent instance or chat named x")

    call = {"name": "message_user", "arguments": {"text": "hi"}}
    r = handle({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": call}, broken)
    assert r and r["result"]["isError"] and "no agent instance" in r["result"]["content"][0]["text"]
    unknown = handle({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}, broken)
    assert unknown and unknown["error"]["code"] == -32601


def test_mcp_serve_speaks_line_delimited_json() -> None:
    out = io.StringIO()
    lines = [
        {"jsonrpc": "2.0", "id": 1, "method": "ping"},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
    ]
    serve(io.StringIO("\n".join(json.dumps(m) for m in lines) + "\n\n"), out, lambda _: None)
    assert [json.loads(line) for line in out.getvalue().splitlines()] == [
        {"jsonrpc": "2.0", "id": 1, "result": {}}
    ]
