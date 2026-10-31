import io
import json
import os
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any
from uuid import UUID

import botler
import pytest
from claude_session import __main__ as job
from claude_session.mcp import handle, redact, serve
from shared.events import ClaudeSessionFinished, ClaudeSessionRequested

SID = "0b9f3c62-3f0e-4a8e-9a39-6d3c7f0c1a11"
FAKE_CLAUDE = Path(__file__).with_name("fake_claude.py")


def event(payload: dict[str, Any], source: dict[str, str] | None = None) -> dict[str, Any]:
    return {
        "id": "e1",
        "type": "claude_session.requested",
        "source": source or {"kind": "agent", "name": "chat-7"},
        "payload": payload,
    }


def test_request_defaults_to_notifying_the_emitter() -> None:
    req, notify = job.read_request(event({"task": "fix the tests"}))
    assert (req.task, req.repo, req.resume, req.interrupt, notify) == (
        "fix the tests",
        None,
        None,
        False,
        "chat-7",
    )
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


def test_command_streams_and_starts_or_resumes() -> None:
    new = job.command(SID, False, Path("m.json"))
    assert new[:2] == ["claude", "-p"] and new[-2:] == ["--session-id", SID]
    assert {"stream-json", "--verbose", "bypassPermissions", "m.json"} <= set(new)
    assert job.command(SID, True, Path("m.json"))[-2:] == ["--resume", SID]


def test_mcp_config_passes_the_hub_connection_and_the_secrets_to_redact() -> None:
    env = {"BOTLER_TOKEN": "t", "PYTHONPATH": "/sdk", "GITHUB_TOKEN": "g", "HOME": "/h"}
    server = job.mcp_config(SID, Path("/data/live/x/notify"), env)["mcpServers"]["botler"]
    assert server["args"] == ["-m", "claude_session.mcp"]
    assert server["env"] == {
        "BOTLER_TOKEN": "t",
        "PYTHONPATH": "/sdk",
        "GITHUB_TOKEN": "g",
        "CLAUDE_SESSION_ID": SID,
        "CLAUDE_SESSION_NOTIFY_FILE": "/data/live/x/notify",
    }


def test_github_token_reaches_git_through_a_credential_helper_only(tmp_path: Path) -> None:
    assert "GIT_CONFIG_COUNT" not in job.claude_env({"PATH": os.environ["PATH"]}, tmp_path)
    env = job.claude_env({"PATH": os.environ["PATH"], "GITHUB_TOKEN": "ghp_secret"}, tmp_path)
    assert all("ghp_secret" not in v for k, v in env.items() if k != "GITHUB_TOKEN")
    fill = subprocess.run(
        ["git", "credential", "fill"],  # noqa: S607
        input="protocol=https\nhost=github.com\n\n",
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    assert "username=x-access-token\npassword=ghp_secret\n" in fill.stdout


def test_claude_env_carries_the_git_defaults(tmp_path: Path) -> None:
    env = job.claude_env({"PATH": os.environ["PATH"]}, tmp_path / "home")
    assert env["HOME"] == str(tmp_path / "home")
    assert env["GIT_TERMINAL_PROMPT"] == "0" and env["GIT_AUTHOR_EMAIL"] == "botler@botler.invalid"


def test_git_env_uses_the_delivered_github_key(tmp_path: Path) -> None:
    key = tmp_path / "id_ed25519"
    key.write_text("PRIVATE KEY")
    env = job.git_env({"KEEP": "yes"}, key)
    assert env["KEEP"] == "yes" and env["GIT_TERMINAL_PROMPT"] == "0"
    assert env["GIT_SSH_COMMAND"] == (
        f"ssh -i {key} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"
    )
    assert env["GIT_AUTHOR_NAME"] == "botler"
    assert env["GIT_COMMITTER_EMAIL"] == "botler@botler.invalid"
    assert key.stat().st_mode & 0o777 == 0o600 and key.parent.stat().st_mode & 0o777 == 0o700


def test_git_env_without_a_key_still_sets_defaults() -> None:
    env = job.git_env({}, Path("/nonexistent/id_ed25519"))
    assert "GIT_SSH_COMMAND" not in env
    assert env["GIT_TERMINAL_PROMPT"] == "0" and env["GIT_AUTHOR_EMAIL"] == "botler@botler.invalid"


def test_git_env_keeps_the_environment_it_was_given() -> None:
    env = job.git_env({"GIT_AUTHOR_NAME": "Ada", "GIT_TERMINAL_PROMPT": "1"}, Path("/nonexistent/k"))
    assert env["GIT_AUTHOR_NAME"] == "Ada" and env["GIT_TERMINAL_PROMPT"] == "1"


def test_outcome_reads_a_result_message() -> None:
    msg = {
        "type": "result",
        "subtype": "success",
        "result": "PR #4",
        "session_id": SID,
        "total_cost_usd": 0.1,
    }
    assert job.outcome("x", job.ClaudeResult.model_validate(msg)) == job.Outcome(True, SID, "PR #4", 0.1)
    failed = job.ClaudeResult(is_error=True, result="Invalid API key · Please run /login")
    assert job.outcome(SID, failed) == job.Outcome(False, SID, "Invalid API key · Please run /login")
    unknown = job.ClaudeResult(
        subtype="error_during_execution", is_error=True, errors=["No conversation found"]
    )
    assert job.outcome(SID, unknown).result == "No conversation found"
    long = job.outcome(SID, job.ClaudeResult(result="x" * 5000))
    assert long.ok and long.result.endswith(" [cut]") and len(long.result) < 3100


class FakeHub:
    def __init__(self) -> None:
        self.state: dict[str, str] = {}
        self.emitted: list[ClaudeSessionFinished] = []
        self.sent: list[tuple[str, str]] = []
        self.timers: dict[str, str] = {}

    def set_timer(self, chat: str, prompt: str, after: str) -> str:
        self.timers[f"t{len(self.timers)}"] = f"{chat} {after}: {prompt}"
        return f"t{len(self.timers) - 1}"

    def hub(self) -> job.Hub:
        return job.Hub(
            self.state.__setitem__,
            self.emitted.append,
            lambda to, text: self.sent.append((to, text)),
            self.set_timer,
            self.timers.pop,
        )

    def texts(self) -> list[str]:
        return [text for _, text in self.sent]


def test_report_records_emits_messages_and_redacts() -> None:
    fake = FakeHub()
    out = job.Outcome(True, SID, "PR is up, token ghp_x", 0.5)
    job.report(out, "fix it", "chat-7", fake.hub(), {"GITHUB_TOKEN": "ghp_x"})
    assert json.loads(fake.state[f"session:{SID}"])["result"] == "PR is up, token [GITHUB_TOKEN]"
    assert fake.emitted == [
        ClaudeSessionFinished(
            session_id=SID, ok=True, result="PR is up, token [GITHUB_TOKEN]", notify="chat-7", cost_usd=0.5
        )
    ]
    [(to, text)] = fake.sent
    assert to == "chat-7" and "PR is up" in text and f'resume="{SID}"' in text and "ghp_x" not in text


def test_login_failures_say_how_to_fix_them() -> None:
    fake = FakeHub()
    job.report(job.Outcome(False, SID, "Invalid API key · Please run /login"), "t", "c", fake.hub(), {})
    assert "failed" in fake.sent[0][1] and "claude setup-token" in fake.sent[0][1]
    assert not fake.emitted[0].ok


def test_live_inbox_keeps_order_and_the_lock_is_exclusive(tmp_path: Path) -> None:
    live, other = job.Live(tmp_path), job.Live(tmp_path)
    live.post("b", job.Message("first", "c"))
    time.sleep(0.01)
    live.post("a", job.Message("second", "c", interrupt=True))
    assert other.waiting()
    with live.locked() as held, other.locked() as also:
        assert held and not also
    with other.locked() as held:
        assert held
    assert live.take() == [job.Message("first", "c"), job.Message("second", "c", True)]
    assert not live.waiting()


@pytest.fixture
def claude(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, str]:
    """A fake `claude` on PATH; quick polls and a short idle time."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "claude").write_text(f'#!/bin/sh\nexec {sys.executable} {FAKE_CLAUDE} "$@"\n')
    (bin_dir / "claude").chmod(0o755)
    monkeypatch.setattr(job, "POLL_SECONDS", 0.05)
    monkeypatch.setattr(job, "IDLE_SECONDS", 0.5)
    return {"PATH": f"{bin_dir}:{os.environ['PATH']}", "FAKE_CLAUDE_LOG": str(tmp_path / "claude.log")}


def runs(env: dict[str, str]) -> list[list[str]]:
    return [json.loads(line) for line in Path(env["FAKE_CLAUDE_LOG"]).read_text().splitlines()]


def test_a_session_takes_messages_while_it_works_then_idles_out(
    tmp_path: Path, claude: dict[str, str]
) -> None:
    fake = FakeHub()
    data = tmp_path / "data"
    first = ClaudeSessionRequested(task="slow 1")
    holder = threading.Thread(target=job.serve, args=("e1", SID, first, "chat-7", data, claude, fake.hub()))
    holder.start()
    time.sleep(0.4)
    # A second run for the same session posts its message and leaves it to the holder.
    more = ClaudeSessionRequested(task="also this", resume=UUID(SID), notify="chat-8")
    job.serve("e2", SID, more, "chat-8", data, claude, fake.hub())
    holder.join(10)
    assert not holder.is_alive()
    assert fake.texts()[0].startswith(f"Claude Code session {SID} started")
    assert [(e.ok, e.result, e.notify) for e in fake.emitted] == [(True, "did: slow 1 + also this", "chat-8")]
    assert [r[-2:] for r in runs(claude)] == [["--session-id", SID]]
    assert fake.timers == {} and (data / "sessions" / SID).is_dir()


def test_after_idling_out_a_message_resumes_the_session(tmp_path: Path, claude: dict[str, str]) -> None:
    fake = FakeHub()
    data = tmp_path / "data"
    job.serve("e1", SID, ClaudeSessionRequested(task="one"), "c", data, claude, fake.hub())
    job.serve("e2", SID, ClaudeSessionRequested(task="two", resume=UUID(SID)), "c", data, claude, fake.hub())
    assert [e.result for e in fake.emitted] == ["did: one", "did: two"]
    assert [r[-2:] for r in runs(claude)] == [["--session-id", SID], ["--resume", SID]]


def test_an_interrupt_stops_the_turn_without_reporting_it(tmp_path: Path, claude: dict[str, str]) -> None:
    fake = FakeHub()
    data = tmp_path / "data"
    holder = threading.Thread(
        target=job.serve,
        args=("e1", SID, ClaudeSessionRequested(task="slow 5"), "c", data, claude, fake.hub()),
    )
    holder.start()
    time.sleep(0.4)
    stop = ClaudeSessionRequested(task="do Y instead", resume=UUID(SID), interrupt=True)
    job.serve("e2", SID, stop, "c", data, claude, fake.hub())
    holder.join(10)
    assert [(e.ok, e.result) for e in fake.emitted] == [(True, "did: do Y instead")]


def test_a_message_posted_while_claude_stops_starts_it_again(
    tmp_path: Path, claude: dict[str, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeHub()
    data = tmp_path / "data"
    real_converse = job.converse

    def converse(
        claude_: job.Claude,
        live: job.Live,
        sid: str,
        deadline: float,
        on_turn: Callable[[job.Outcome, job.Message], None],
    ) -> int:
        left = real_converse(claude_, live, sid, deadline, on_turn)
        if len(runs(claude)) == 1:  # as if another run posted it just as Claude was exiting
            live.post("late", job.Message("late one", "c"))
        return left

    monkeypatch.setattr(job, "converse", converse)
    job.serve("e1", SID, ClaudeSessionRequested(task="one"), "c", data, claude, fake.hub())
    assert [e.result for e in fake.emitted] == ["did: one", "did: late one"]
    assert [r[-2:] for r in runs(claude)] == [["--session-id", SID], ["--resume", SID]]


def test_a_crash_is_reported_with_what_claude_said(tmp_path: Path, claude: dict[str, str]) -> None:
    fake = FakeHub()
    job.serve("e1", SID, ClaudeSessionRequested(task="crash"), "c", tmp_path / "data", claude, fake.hub())
    [finished] = fake.emitted
    assert not finished.ok and finished.result == "claude exited with code 3: boom"


def test_messages_after_the_runs_time_limit_are_handed_back(
    tmp_path: Path, claude: dict[str, str], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(job, "RUN_SECONDS", 0)
    fake = FakeHub()
    job.serve(
        "e1",
        SID,
        ClaudeSessionRequested(task="t", resume=UUID(SID)),
        "c",
        tmp_path / "data",
        claude,
        fake.hub(),
    )
    assert fake.emitted == [] and "time limit before it got this message; send it again: t" in fake.sent[0][1]


def test_failed_clone_is_reported_without_running_claude(tmp_path: Path, claude: dict[str, str]) -> None:
    fake = FakeHub()
    req = ClaudeSessionRequested(task="t", repo=str(tmp_path / "missing"))
    job.serve("e1", SID, req, "c", tmp_path / "data", claude, fake.hub())
    [finished] = fake.emitted
    assert not finished.ok and "git clone" in finished.result
    assert not Path(claude["FAKE_CLAUDE_LOG"]).exists()


def test_a_bug_after_the_request_is_parsed_still_tells_the_chat(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake = FakeHub()
    (tmp_path / "event.json").write_text(json.dumps(event({"task": "t", "resume": SID})))
    monkeypatch.setenv("BOTLER_EVENT_FILE", str(tmp_path / "event.json"))
    monkeypatch.setattr(job, "botler_hub", fake.hub)

    def broken(*_: object) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(job, "serve", broken)
    with pytest.raises(OSError, match="disk full"):
        job.main()
    assert fake.sent == [("chat-7", f"Claude Code session {SID} failed: OSError('disk full')")]


def test_redact_hides_the_session_secrets() -> None:
    env = {"GITHUB_TOKEN": "ghp_1", "CLAUDE_CODE_OAUTH_TOKEN": "sk-ant-2", "OTHER": "x"}
    assert redact("a ghp_1 b sk-ant-2 x", env) == "a [GITHUB_TOKEN] b [CLAUDE_CODE_OAUTH_TOKEN] x"


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
