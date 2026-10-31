"""Run one Claude Code turn for a `claude_session.requested` event and report it.

`claude -p` reads the task on stdin and runs without permission prompts in a working directory per
session under the job's volume (`/data/sessions/<id>`, with `repo` cloned into it); Claude Code's
own home is `/data/home`, so `--resume` finds earlier turns. Claude gets one MCP tool,
`message_user` (claude_session.mcp), for news while it works. The outcome is kept in `state`,
emitted as `claude_session.finished` and messaged to the chat in `notify`.

Claude's own failures (expired token, usage limit, unknown session) are reported to that chat and
end the run successfully: there is nothing for `triage` to fix. A run that cannot report fails.
"""

import json
import os
import subprocess
import sys
import tempfile
import uuid
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import botler
import pydantic
from shared.events import ClaudeSessionFinished, ClaudeSessionRequested

DATA = Path("/data")
# job.toml's timeout is 2h: stop Claude in time to still report.
CLAUDE_TIMEOUT_SECONDS = 110 * 60
MAX_RESULT_CHARS = 3000

SYSTEM_PROMPT = """You run unattended in a container for the user, started through their assistant \
Botler; nobody watches this session. The tool `message_user` sends the user a short message right \
away: use it for news they should not wait for (a PR is up, you are blocked). When you need an \
answer or a decision, ask in your final reply and end your turn: the user's answer comes back as \
the next turn of this session."""


@dataclass(frozen=True)
class Outcome:
    ok: bool
    session_id: str
    result: str
    cost_usd: float | None = None


def read_request(event: Mapping[str, Any] | None) -> tuple[ClaudeSessionRequested, str]:
    """The request in a trigger's event file, and the chat to notify (default: whoever emitted it)."""
    if not event or event.get("type") != ClaudeSessionRequested.type:
        raise SystemExit(
            f"claude-session runs for {ClaudeSessionRequested.type} events (skill `claude-code`), "
            f"not for {event.get('type') if event else 'no event'}"
        )
    request = ClaudeSessionRequested.model_validate(event.get("payload") or {})
    source: Mapping[str, Any] = event.get("source") or {}
    emitter = source.get("name") if source.get("kind") == "agent" else None
    return request, request.notify or emitter or "botler"


def command(request: ClaudeSessionRequested, session_id: str, mcp_config: Path) -> list[str]:
    """The `claude` invocation; the task goes on stdin, so it is never parsed as an option."""
    session = ["--resume", session_id] if request.resume else ["--session-id", session_id]
    return [
        "claude",
        "-p",
        "--output-format",
        "json",
        "--permission-mode",
        "bypassPermissions",
        "--mcp-config",
        str(mcp_config),
        "--strict-mcp-config",
        "--append-system-prompt",
        SYSTEM_PROMPT,
        *session,
    ]


def mcp_config(session_id: str, notify: str, environ: Mapping[str, str]) -> dict[str, Any]:
    """The `message_user` server. It gets the hub connection explicitly, whatever Claude Code
    passes on to its MCP servers by default."""
    env = {k: v for k, v in environ.items() if k.startswith("BOTLER_") or k == "PYTHONPATH"}
    env |= {"CLAUDE_SESSION_ID": session_id, "CLAUDE_SESSION_NOTIFY": notify}
    server = {"type": "stdio", "command": sys.executable, "args": ["-m", "claude_session.mcp"], "env": env}
    return {"mcpServers": {"botler": server}}


class ClaudeResult(pydantic.BaseModel):
    """The fields of `claude -p --output-format json` used here."""

    result: str | None = None
    subtype: str | None = None
    session_id: str | None = None
    is_error: bool = False
    total_cost_usd: float | None = None


def parse_output(session_id: str, stdout: str, stderr: str, code: int) -> Outcome:
    """Claude's JSON result, or whatever it said when it printed none (a bad flag, a crash)."""
    try:
        out = ClaudeResult.model_validate_json(stdout)
    except pydantic.ValidationError:
        said = (stderr.strip() or stdout.strip() or "no output")[-MAX_RESULT_CHARS:]
        return Outcome(False, session_id, f"claude exited with code {code}: {said}")
    result = out.result or out.subtype or "(no result)"
    return Outcome(
        ok=code == 0 and not out.is_error,
        session_id=out.session_id or session_id,
        result=result[:MAX_RESULT_CHARS] + (" [cut]" if len(result) > MAX_RESULT_CHARS else ""),
        cost_usd=out.total_cost_usd,
    )


def summary(outcome: Outcome, request: ClaudeSessionRequested) -> str:
    """The message to the chat: what Claude said and how to continue."""
    sid = outcome.session_id
    if outcome.ok:
        head = f"Claude Code session {sid} finished its turn:"
    else:
        head = f"Claude Code session {sid} failed:"
    text = f"{head}\n\n{outcome.result}\n\n"
    words = outcome.result.lower()
    if not outcome.ok and any(w in words for w in ("login", "oauth", "token", "401", "authenticat")):
        text += (
            "This looks like a login problem: the CLAUDE_CODE_OAUTH_TOKEN in the secret group "
            "`claude-code` may be wrong or expired. The user makes a new one with `claude setup-token` "
            "(skill `claude-code`).\n"
        )
    text += f'To continue it, emit claude_session.requested with resume="{sid}" and the next task.'
    if request.resume is None and request.repo:
        text += f" Its working directory holds a clone of {request.repo}."
    return text


@dataclass(frozen=True)
class Hub:
    """What the job needs from botler; tests replace it."""

    state_set: Callable[[str, str], None]
    emit: Callable[[ClaudeSessionFinished], object]
    send: Callable[[str, str], object]


def run_claude(
    request: ClaudeSessionRequested, notify: str, data: Path, environ: Mapping[str, str]
) -> Outcome:
    session_id = str(request.resume or uuid.uuid4())
    workdir = data / "sessions" / session_id
    home = data / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = {**environ, "HOME": str(home)}
    if request.resume is None:
        workdir.mkdir(parents=True)
        if request.repo:
            # The URL is an argument after `--`, never an option; the container is the boundary.
            clone = subprocess.run(  # noqa: S603
                ["git", "clone", "--", request.repo, str(workdir)],  # noqa: S607
                capture_output=True,
                text=True,
                env=env,
            )
            if clone.returncode != 0:
                return Outcome(False, session_id, f"git clone {request.repo} failed: {clone.stderr.strip()}")
    else:
        workdir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        config = Path(tmp) / "mcp.json"
        config.write_text(json.dumps(mcp_config(session_id, notify, environ)))
        try:
            done = subprocess.run(  # noqa: S603 (a fixed command; the task goes on stdin)
                command(request, session_id, config),
                input=request.task,
                capture_output=True,
                text=True,
                cwd=workdir,
                env=env,
                timeout=CLAUDE_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            minutes = CLAUDE_TIMEOUT_SECONDS // 60
            return Outcome(False, session_id, f"stopped after {minutes} minutes; resume it to go on")
    return parse_output(session_id, done.stdout, done.stderr, done.returncode)


def report(outcome: Outcome, request: ClaudeSessionRequested, notify: str, hub: Hub) -> None:
    record = {"task": request.task[:500], "ok": outcome.ok, "result": outcome.result, "notify": notify}
    hub.state_set(f"session:{outcome.session_id}", json.dumps(record))
    hub.emit(
        ClaudeSessionFinished(
            session_id=outcome.session_id,
            ok=outcome.ok,
            result=outcome.result,
            notify=notify,
            cost_usd=outcome.cost_usd,
        )
    )
    hub.send(notify, summary(outcome, request))


def main() -> None:
    with open(os.environ["BOTLER_EVENT_FILE"], encoding="utf-8") as f:
        request, notify = read_request(json.load(f))
    outcome = run_claude(request, notify, DATA, os.environ)
    botler.log("claude session ended", session_id=outcome.session_id, ok=outcome.ok)
    report(outcome, request, notify, Hub(botler.state.set, botler.emit, botler.send_message))
    print(json.dumps({"session_id": outcome.session_id, "ok": outcome.ok}))


if __name__ == "__main__":
    main()
