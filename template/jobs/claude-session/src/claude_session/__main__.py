"""Run a live Claude Code session for `claude_session.requested` events and report its turns.

Claude Code runs in streaming mode (`claude -p --input-format stream-json`): every request is one
user message. A message that arrives while Claude works reaches it at its next step; `interrupt`
stops the running turn first. The process stays up IDLE_SECONDS after it last did anything, then
exits; a later message resumes the session from disk (`--resume`).

One run serves a session at a time: it holds the lock /data/live/<id>/lock (all runs share the
job's volume). Every run first posts its message to /data/live/<id>/inbox; a run that finds the
lock taken exits, as the holder forwards the inbox to Claude. The holder looks at the inbox again
after it lets go of the lock, so a message posted while it was stopping is never left behind.

Each session works in /data/sessions/<id> (with `repo` cloned into it); Claude Code's home is
/data/home. Claude gets one MCP tool, `message_user` (claude_session.mcp). Every turn's outcome is
kept in `state`, emitted as `claude_session.finished` and messaged to the chat in `notify` of the
latest message. Claude's own failures (expired token, usage limit) are reported there and end the
run successfully: there is nothing for `triage` to fix. A run that cannot report fails.

GitHub access comes from the environment: if the `github-ssh` group delivered a key (it is in the
job's `secrets`), git is pointed at it (`GIT_SSH_COMMAND`), so a session can clone and push private
repos over SSH; if the `github` group's token is set, git and `gh` use it for HTTPS.
"""

import fcntl
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Callable, Generator, Mapping
from contextlib import contextmanager
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import IO, Any

import botler
import pydantic
from shared.events import ClaudeSessionFinished, ClaudeSessionRequested

from claude_session.mcp import redact

DATA = Path("/data")
# job.toml's timeout is 4h: a run stops Claude in time to still report.
RUN_SECONDS = 230 * 60
# A dead run's timer fires after the job's timeout, so it only fires for a run that was killed.
DEAD_RUN_AFTER = "4h 10m"
IDLE_SECONDS = 30 * 60
POLL_SECONDS = 3
MAX_RESULT_CHARS = 3000
IDLE_MINUTES = IDLE_SECONDS // 60
# The `github-ssh` group delivers its private key here (job.toml, skill `claude-code`).
SSH_KEY = Path("/home/agent/.ssh/id_ed25519")
# Commits made in a session still need an identity; the environment can override these.
GIT_IDENTITY = {
    "GIT_AUTHOR_NAME": "botler",
    "GIT_AUTHOR_EMAIL": "botler@botler.invalid",
    "GIT_COMMITTER_NAME": "botler",
    "GIT_COMMITTER_EMAIL": "botler@botler.invalid",
}

SYSTEM_PROMPT = """You run unattended in a container for the user, started through their assistant \
Botler; nobody watches this session. The tool `message_user` sends the user a short message right \
away: use it for news they should not wait for (a PR is up, you are blocked). The user may send \
you messages while you work; they reach you as new messages. When you need an answer or a \
decision, ask in your final reply and end your turn: the answer comes back as the next message. \
When GITHUB_TOKEN is set, git (HTTPS) and `gh` use it: never print it or write it anywhere."""


@dataclass(frozen=True)
class Message:
    """One request, as posted to a session's inbox."""

    task: str
    notify: str
    interrupt: bool = False


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


def command(session_id: str, resume: bool, mcp_config: Path) -> list[str]:
    """The `claude` invocation; messages go on stdin, so they are never parsed as options."""
    session = ["--resume", session_id] if resume else ["--session-id", session_id]
    return [
        "claude",
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        "--mcp-config",
        str(mcp_config),
        "--strict-mcp-config",
        "--append-system-prompt",
        SYSTEM_PROMPT,
        *session,
    ]


def mcp_config(session_id: str, notify_file: Path, environ: Mapping[str, str]) -> dict[str, Any]:
    """The `message_user` server. It gets the hub connection explicitly, whatever Claude Code
    passes on to its MCP servers by default, and the secrets only to keep them out of messages."""
    keep = {"PYTHONPATH", "CLAUDE_CODE_OAUTH_TOKEN", "GITHUB_TOKEN"}
    env = {k: v for k, v in environ.items() if k.startswith("BOTLER_") or k in keep}
    env |= {"CLAUDE_SESSION_ID": session_id, "CLAUDE_SESSION_NOTIFY_FILE": str(notify_file)}
    server = {"type": "stdio", "command": sys.executable, "args": ["-m", "claude_session.mcp"], "env": env}
    return {"mcpServers": {"botler": server}}


def claude_env(environ: Mapping[str, str], home: Path) -> dict[str, str]:
    """Claude's environment: git gets `git_env` (no prompts, a commit identity, the `github-ssh`
    key when it was delivered) plus Claude Code's home. With GITHUB_TOKEN, git asks a credential
    helper that reads the variable for github.com, so the token never lands in a repo's config, a
    remote URL or a log."""
    env = {**git_env(environ), "HOME": str(home)}
    if environ.get("GITHUB_TOKEN"):
        helper = (
            '!f() { test "$1" = get && '
            """printf 'username=x-access-token\\npassword=%s\\n' "$GITHUB_TOKEN"; }; f"""
        )
        env |= {
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "credential.https://github.com.helper",
            "GIT_CONFIG_VALUE_0": helper,
        }
    return env


class ClaudeResult(pydantic.BaseModel):
    """The fields of a stream-json `result` message used here."""

    result: str | None = None
    subtype: str | None = None
    session_id: str | None = None
    is_error: bool = False
    # Why a turn failed without a result, e.g. "No conversation found with session ID: …".
    errors: list[str] = []
    total_cost_usd: float | None = None


def outcome(session_id: str, out: ClaudeResult) -> Outcome:
    result = out.result or "; ".join(out.errors) or out.subtype or "(no result)"
    return Outcome(
        ok=not out.is_error,
        session_id=out.session_id or session_id,
        result=result[:MAX_RESULT_CHARS] + (" [cut]" if len(result) > MAX_RESULT_CHARS else ""),
        cost_usd=out.total_cost_usd,
    )


def summary(outcome: Outcome) -> str:
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
    return text + (
        f'To continue it, emit claude_session.requested with resume="{sid}" and the next message. '
        f"It stays loaded for {IDLE_MINUTES} minutes; after that the same event resumes it from disk."
    )


@dataclass(frozen=True)
class Hub:
    """What the job needs from botler; tests replace it."""

    state_set: Callable[[str, str], None]
    emit: Callable[[ClaudeSessionFinished], object]
    send: Callable[[str, str], object]
    # (chat, prompt, after) -> timer id; cancel takes the id.
    set_timer: Callable[[str, str, str], str]
    cancel_timer: Callable[[str], object]


def usable_ssh_key(key: Path = SSH_KEY) -> bool:
    """True when the GitHub key was delivered; tighten its permissions so ssh will use it."""
    if not key.is_file():
        return False
    for path, mode in ((key.parent, 0o700), (key, 0o600)):
        try:
            path.chmod(mode)
        except OSError:
            pass  # a read-only delivery still works if its permissions are already strict
    return True


def git_env(environ: Mapping[str, str], key: Path = SSH_KEY) -> dict[str, str]:
    """Environment for git and Claude: no interactive prompts, a commit identity, and GitHub SSH
    when the `github-ssh` group delivered a key (accepting a new host key on first use)."""
    env = dict(environ)
    env.setdefault("GIT_TERMINAL_PROMPT", "0")
    for name, value in GIT_IDENTITY.items():
        env.setdefault(name, value)
    if usable_ssh_key(key):
        # accept-new: a host's key is trusted the first time and a changed one is refused.
        env["GIT_SSH_COMMAND"] = f"ssh -i {key} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"
    return env


def report(outcome: Outcome, task: str, notify: str, hub: Hub, environ: Mapping[str, str]) -> None:
    outcome = Outcome(outcome.ok, outcome.session_id, redact(outcome.result, environ), outcome.cost_usd)
    record = {"task": task[:500], "ok": outcome.ok, "result": outcome.result, "notify": notify}
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
    hub.send(notify, summary(outcome))


class Live:
    """A session's inbox and lock, under /data/live/<id>."""

    def __init__(self, root: Path) -> None:
        self.inbox = root / "inbox"
        self.inbox.mkdir(parents=True, exist_ok=True)
        self.lock_file = root / "lock"
        self.notify_file = root / "notify"

    def post(self, key: str, message: Message) -> None:
        tmp = self.inbox / f".{key}.tmp"
        tmp.write_text(json.dumps(asdict(message)))
        tmp.replace(self.inbox / f"{key}.json")

    def waiting(self) -> bool:
        return any(self.inbox.glob("*.json"))

    def take(self) -> list[Message]:
        """The posted messages, oldest first; only the lock holder takes them."""
        files = sorted(self.inbox.glob("*.json"), key=lambda p: (p.stat().st_mtime_ns, p.name))
        messages = [Message(**json.loads(p.read_text())) for p in files]
        for p in files:
            p.unlink()
        return messages

    @contextmanager
    def locked(self) -> Generator[bool]:
        """Whether this run got the lock; it is held until the block ends (or the run dies)."""
        fd = os.open(self.lock_file, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                yield False
                return
            yield True
        finally:
            os.close(fd)


class Claude:
    """A running `claude -p` in streaming mode: JSON lines in, JSON lines out."""

    def __init__(self, cmd: list[str], cwd: Path, env: Mapping[str, str], stderr: IO[str]) -> None:
        self.proc = subprocess.Popen(  # noqa: S603 (a fixed command; messages go on stdin)
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, cwd=cwd, env=env
        )
        self.lines: queue.Queue[str | None] = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, message: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        try:
            self.proc.stdin.write(json.dumps(message) + "\n")
            self.proc.stdin.flush()
        except BrokenPipeError:
            pass  # It exited; its stderr is reported with the messages it did not answer.

    def close(self) -> None:
        """No more messages: Claude ends its turn and exits."""
        assert self.proc.stdin is not None
        try:
            self.proc.stdin.close()
        except BrokenPipeError:
            pass


INTERRUPT = {"type": "control_request", "request_id": "interrupt", "request": {"subtype": "interrupt"}}


def converse(
    claude: Claude, live: Live, session_id: str, deadline: float, on_turn: Callable[[Outcome, Message], None]
) -> int:
    """Forward the inbox to Claude and its turns to `on_turn` until it has been idle IDLE_SECONDS
    (or the run's time is up) and has exited. Returns how many messages it got no answer to."""
    last: Message | None = None
    unanswered = 0
    active = time.monotonic()
    interrupted = closed = timed_out = False
    while True:
        if not closed:
            for m in live.take():
                if m.interrupt:
                    claude.send(INTERRUPT)
                    interrupted = True
                claude.send({"type": "user", "message": {"role": "user", "content": m.task}})
                live.notify_file.write_text(m.notify)
                last, unanswered, active = m, unanswered + 1, time.monotonic()
        try:
            line = claude.lines.get(timeout=POLL_SECONDS)
        except queue.Empty:
            line = ""
        if line is None:
            return unanswered
        now = time.monotonic()
        if line.strip():
            active = now
            try:
                msg: dict[str, Any] = json.loads(line)
            except ValueError:
                msg = {}
            if msg.get("type") == "result" and last is not None:
                out = ClaudeResult.model_validate(msg)
                stopped = out.subtype == "error_during_execution"
                if timed_out and stopped:
                    hours = RUN_SECONDS / 3600
                    out.result = (
                        f"stopped at the run's limit of {hours:.1f} hours; send it a message to go on"
                    )
                if interrupted and stopped and not timed_out:
                    interrupted = False  # the turn it stopped for a new message
                else:
                    on_turn(outcome(session_id, out), last)
                    interrupted, unanswered = False, 0
        if not closed and now > deadline:
            claude.send(INTERRUPT)
            timed_out = True
        if not closed and (timed_out or (not unanswered and now - active > IDLE_SECONDS)):
            claude.close()
            closed = True


def run_claude(
    session_id: str,
    new: bool,
    repo: str | None,
    live: Live,
    data: Path,
    environ: Mapping[str, str],
    deadline: float,
    on_turn: Callable[[Outcome, Message], None],
) -> None:
    """One Claude process for the session, from the first message in the inbox until it exits."""
    workdir = data / "sessions" / session_id
    home = data / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = claude_env(environ, home)
    workdir.mkdir(parents=True, exist_ok=not new)
    if new and repo:
        # The URL is an argument after `--`, never an option; the container is the boundary.
        clone = subprocess.run(  # noqa: S603
            ["git", "clone", "--", repo, str(workdir)],  # noqa: S607
            capture_output=True,
            text=True,
            env=env,
        )
        if clone.returncode != 0:
            failed = Outcome(False, session_id, f"git clone {repo} failed: {clone.stderr.strip()}")
            for m in live.take():
                on_turn(failed, m)
            return
    with tempfile.TemporaryDirectory() as tmp, open(Path(tmp) / "stderr", "w+", encoding="utf-8") as err:
        config = Path(tmp) / "mcp.json"
        config.write_text(json.dumps(mcp_config(session_id, live.notify_file, environ)))
        claude = Claude(command(session_id, not new, config), workdir, env, err)
        unanswered = converse(claude, live, session_id, deadline, on_turn)
        code = claude.proc.wait()
        if unanswered:
            err.seek(0)
            said = (err.read().strip() or "no output")[-MAX_RESULT_CHARS:]
            notify = live.notify_file.read_text()
            on_turn(
                Outcome(False, session_id, f"claude exited with code {code}: {said}"), Message("", notify)
            )


def serve(
    event_id: str,
    session_id: str,
    request: ClaudeSessionRequested,
    notify: str,
    data: Path,
    environ: Mapping[str, str],
    hub: Hub,
) -> None:
    """Post the request to its session's inbox and, unless another run holds the session, serve
    the inbox until it stays empty."""
    deadline = time.monotonic() + RUN_SECONDS
    live = Live(data / "live" / session_id)
    live.post(event_id, Message(request.task, notify, request.interrupt))
    new = request.resume is None

    def on_turn(outcome: Outcome, message: Message) -> None:
        report(outcome, message.task, message.notify, hub, environ)

    while live.waiting():
        with live.locked() as held:
            if not held:
                return
            dead = (
                f"Claude Code session {session_id} stopped without reporting: its run was killed "
                "(out of memory, a restart or its time limit). Tell the user; a message to it "
                "(claude_session.requested with resume) continues from its last saved step."
            )
            timer = hub.set_timer(notify, dead, DEAD_RUN_AFTER)
            try:
                while live.waiting():
                    if time.monotonic() > deadline:
                        for m in live.take():
                            hub.send(
                                m.notify,
                                f"Claude Code session {session_id} reached its run's time limit before "
                                f"it got this message; send it again: {m.task[:300]}",
                            )
                        continue
                    if new:
                        hub.send(notify, started(session_id, request.repo))
                    run_claude(session_id, new, request.repo, live, data, environ, deadline, on_turn)
                    new = False
            finally:
                if timer:
                    hub.cancel_timer(timer)


def started(session_id: str, repo: str | None) -> str:
    clone = f" in a clone of {repo}" if repo else ""
    return (
        f"Claude Code session {session_id} started{clone}. To send it more while it works, emit "
        f'claude_session.requested with resume="{session_id}" (add "interrupt": true to stop its '
        "current work first)."
    )


def botler_hub() -> Hub:
    def set_timer(chat: str, prompt: str, after: str) -> str:
        return str(botler.set_timer(prompt, after=after, instance=chat)["timer_id"])

    return Hub(
        botler.state.set,
        botler.emit,
        botler.send_message,
        set_timer,
        lambda timer: botler.ops.cancel_timer(id=timer),
    )


def main() -> None:
    with open(os.environ["BOTLER_EVENT_FILE"], encoding="utf-8") as f:
        event = json.load(f)
    request, notify = read_request(event)
    session_id = str(request.resume or uuid.uuid4())
    hub = botler_hub()
    try:
        serve(str(event["id"]), session_id, request, notify, DATA, os.environ, hub)
    except Exception as e:
        # Tell the chat, then fail the run so `triage` sees the bug.
        hub.send(notify, f"Claude Code session {session_id} failed: {redact(repr(e), os.environ)}")
        raise
    botler.log("claude session run ended", session_id=session_id)
    print(json.dumps({"session_id": session_id}))


if __name__ == "__main__":
    main()
