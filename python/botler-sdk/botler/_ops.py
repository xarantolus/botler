"""Every hub operation as a function. GENERATED from the hub's `OPS` specs by
`BOTLER_BLESS=1 cargo test -p botler --test sdk`; do not edit. The hub decides who may call what."""

from __future__ import annotations

from typing import Any

from ._client import call


def send_message(*, text: str, to: str, reply_to: str | None = None, urgent: bool | None = None) -> dict[str, Any]:
    """Send a message to an agent instance (e.g. `botler`). Replies come back to you as messages. Set `reply_to` to the event you are answering to keep the thread.

    Args:
        to: Recipient instance name.
        reply_to: Event id this answers, to keep the conversation thread.
        urgent: Interrupt the recipient at its next turn instead of waiting until it is idle.
    """
    return call("send_message", {"text": text, "to": to, "reply_to": reply_to, "urgent": urgent})


def emit_event(*, type: str, payload: Any = None, provenance: Any = None) -> dict[str, Any]:
    """Emit a typed event. Triggers subscribed to its type react to it. Use for facts (`mail.received`), not for talking to someone (use send_message).

    Args:
        type: Dotted event type, e.g. `mail.received`.
        provenance: `external` (from a known outside source) or `untrusted` (arbitrary content). Defaults to `external`; events from agents and jobs are never `trusted`.
    """
    return call("emit_event", {"type": type, "payload": payload, "provenance": provenance})


def set_timer(*, prompt: str, at: str | None = None, in_: str | None = None, instance: str | None = None) -> dict[str, Any]:
    """Wake an agent later with a prompt: at an RFC 3339 time or after a delay like "2h". Defaults to waking you. One-off only; recurring work belongs in a trigger file.

    Args:
        prompt: What the woken agent is told.
        at: RFC 3339 time, e.g. `2026-11-02T07:30:00+01:00`.
        in_: Delay instead of a time, e.g. `20m`, `2h 30m`.
        instance: Instance to wake; defaults to the caller.
    """
    return call("set_timer", {"prompt": prompt, "at": at, "in": in_, "instance": instance})


def cancel_timer(*, id: str) -> dict[str, Any]:
    """Cancel a pending timer."""
    return call("cancel_timer", {"id": id})


def list_timers() -> list[dict[str, Any]]:
    """Pending timers you created (all timers for the user)."""
    return call("list_timers", {})


def set_chat_title(*, slug: str, title: str) -> dict[str, Any]:
    """Name your chat so the user can find it: a short title and a unique slug like `groundstation-antenna-fix` (lowercase words joined by dashes, project or topic first). Others can then message you by the slug. Do this after the first message of a new chat, and again if the topic changes.

    Args:
        slug: Lowercase words joined by dashes, 3 to 60 characters.
    """
    return call("set_chat_title", {"slug": slug, "title": title})


def spawn_agent(*, definition: str, task: str, env: Any = None) -> dict[str, Any]:
    """Start a new instance of an agent definition with a task. It reports back to you by message. `env = {secrets: [...], mounts: [...]}` gives it secret groups and shares beyond its definition: what you hold yourself or policy allows is granted at once, the rest is asked of the user. Give the child the access its task needs; never tell it to work around missing access.

    Args:
        definition: Agent definition under `agents/`, e.g. `dev`.
        env: What the new chat gets beyond its definition.
    """
    return call("spawn_agent", {"definition": definition, "task": task, "env": env})


def list_chats() -> dict[str, Any]:
    """Every chat (agent instance), running or asleep, with its title, slug and what it was doing, plus the agent definitions you can spawn. Messaging a sleeping chat wakes it with its conversation and workspace intact."""
    return call("list_chats", {})


def propose_change(*, branch: str, title: str) -> dict[str, Any]:
    """Propose your pushed branch (agents/<you>/...) for merging into main. Runs asynchronously: rebase, validation, a plain-language deployment summary (what will run when, what needs the user's approval), the gate (gate/run), a reviewer, maybe the user. Progress arrives as proposal.updated events; nothing takes effect until it is merged.

    Args:
        branch: Your pushed branch, e.g. `agents/botler/work`.
        title: One line saying what the change does.
    """
    return call("propose_change", {"branch": branch, "title": title})


def preview_change(*, branch: str) -> dict[str, Any]:
    """The deployment summary of your pushed branch against main without proposing it: validation problems, triggers in plain words with next fire times, new capabilities, and whether the user must approve."""
    return call("preview_change", {"branch": branch})


def get_proposal(*, id: str) -> dict[str, Any]:
    """A proposal's status, deployment summary, gate report and review notes."""
    return call("get_proposal", {"id": id})


def submit_review(*, notes: str, proposal_id: str, verdict: Any) -> dict[str, Any]:
    """Your verdict on the proposal you were asked to review: approve, escalate (fine, but the user should look) or reject, with notes for a busy reader."""
    return call("submit_review", {"notes": notes, "proposal_id": proposal_id, "verdict": verdict})


def request_secrets(*, group: str, reason: str, add: list[Any] | None = None, class_: Any = None, description: str | None = None, fill: list[str] | None = None, remove: list[str] | None = None) -> dict[str, Any]:
    """Ask the user for secrets, asynchronously: create a group, add fields to one, re-enter values (`fill`) or remove fields. The user gets a form; you get a request id now and a `dialog.answered` event later saying what was provided (never the values). Do not continue work that needs the secrets until that event arrives. Using a group in a container still needs a grant: `request_access("secret:<group>")` for this chat, or `secrets = ["<group>"]` in a definition, proposed as a change.

    Args:
        group: The group (existing, or new: then `description` and `add` are required).
        reason: Why you need this, shown to the user.
        add: Fields to add (the user enters their values).
        class_: For a new group: `container` (default) or `hub` (never enters a container).
        description: For a new group: what it is for.
        fill: Existing fields whose values the user should (re-)enter.
        remove: Fields to remove from the group (their values are deleted).
    """
    return call("request_secrets", {"group": group, "reason": reason, "add": add, "class": class_, "description": description, "fill": fill, "remove": remove})


def request_access(*, reason: str, resource: str, mode: Any = None) -> dict[str, Any]:
    """Ask for a resource your definition does not give you, for this chat: `secret:<group>`, `service:<name>`, `mount:<name>` or `event:<type>`. Policy may grant it at once; otherwise the user decides (allow for this chat, always allow, or deny) and the answer wakes you as a `dialog.answered` event, with the grant applied to your container. Memory notes say which group fits which work.

    Args:
        reason: Why, for the user deciding.
        resource: What you need, as `<kind>:<name>`: `secret:<group>` (a secret group's values in your container), `service:<name>` (a service's MCP server), `mount:<name>` (a network share) or `event:<type>` (subscribing to a restricted event type).
        mode: Mounts: `rw` to write (default `ro`).
    """
    return call("request_access", {"reason": reason, "resource": resource, "mode": mode})


def revoke_access(*, grant: str) -> dict[str, Any]:
    """Give up one of your grants (the user can revoke any). It is gone from your next wake on.

    Args:
        grant: The grant id (from `list_connections`).
    """
    return call("revoke_access", {"grant": grant})


def list_connections(*, chat: str | None = None) -> dict[str, Any]:
    """What a chat (default: you) is connected to: its parent and children, secret groups and services from its definition, grants it holds (with ids, for `revoke_access`), event subscriptions and pending timers.

    Args:
        chat: The chat (name or slug); defaults to you.
    """
    return call("list_connections", {"chat": chat})


def list_requests() -> dict[str, Any]:
    """Your outstanding requests to the user (questions, secret requests) with their ids."""
    return call("list_requests", {})


def cancel_request(*, id: str, reason: str) -> dict[str, Any]:
    """Withdraw one of your outstanding requests by id (it disappears for the user; a late answer is rejected).

    Args:
        reason: Why it is no longer needed.
    """
    return call("cancel_request", {"id": id, "reason": reason})


def archive_ready(*, summary: str, handoff: Any = None, outstanding: list[str] | None = None, state: list[str] | None = None) -> dict[str, Any]:
    """Hand over before this chat is archived (only after an archive.requested event): what you did, what is outstanding, where your state lives, and optionally who should continue.

    Args:
        summary: Everything you did in this chat, for someone who has not read it.
        handoff: Who should continue the outstanding work, if anyone.
        outstanding: Work you did not finish.
        state: Where your state lives: branches, files, memory notes, external systems.
    """
    return call("archive_ready", {"summary": summary, "handoff": handoff, "outstanding": outstanding, "state": state})


def list_event_types(*, query: str | None = None) -> dict[str, Any]:
    """Event types (built-in and declared) with their description, payload schema, producers and current subscribers; `query` narrows by words or a glob. `emit_event` validates payloads against these; `subscribe` takes their names.

    Args:
        query: Words or a glob (`mail.*`) to narrow the list by type or description.
    """
    return call("list_event_types", {"query": query})


def subscribe(*, type: str, filter: dict[str, Any] | None = None, note: str | None = None, wake: bool | None = None) -> dict[str, Any]:
    """Get events of a type (or glob like `mail.*`) delivered to this chat, optionally only those whose payload fields equal `filter`. They wake you even while you sleep (`wake=false`: quiet notes instead). Lasts until `unsubscribe`; find types with `list_event_types` or `search(sources=["event_type"])`. Restricted types need `request_access("event:<type>")` first.

    Args:
        type: Event type (`mail.received`) or glob (`mail.*`); see `list_event_types`.
        filter: Payload fields that must have these values, e.g. `{"folder": "INBOX"}`.
        note: Why you subscribed; shown with every delivery.
        wake: `false`: deliver as a quiet note that waits for whatever wakes you next (default `true`: each event wakes you).
    """
    return call("subscribe", {"type": type, "filter": filter, "note": note, "wake": wake})


def unsubscribe(*, id: str) -> dict[str, Any]:
    """Stop a subscription by id (from `subscribe` or `list_subscriptions`).

    Args:
        id: The subscription id.
    """
    return call("unsubscribe", {"id": id})


def list_subscriptions() -> dict[str, Any]:
    """Your event subscriptions with their ids."""
    return call("list_subscriptions", {})


def read_session(*, instance: str, around: str | None = None, last: int | None = None, n: int | None = None) -> dict[str, Any]:
    """Read the recent messages of a session: your own, or another instance's (what it has been doing) if you may. `around` (an entry id from `search`) reads the messages around that entry instead.

    Args:
        around: An entry id (from `search`): read the messages around it instead of the latest.
        last: How many of the most recent messages (default 20, max 200).
        n: With `around`: how many messages before and after it (default 5, max 50).
    """
    return call("read_session", {"instance": instance, "around": around, "last": last, "n": n})


def search(*, query: str, limit: int | None = None, raw: bool | None = None, scope: Any = None, sources: list[Any] | None = None) -> dict[str, Any]:
    """Full-text search over sessions, memory notes, the event journal, the monorepo and event types (`event_type`: what can happen, who emits it), best matches first. `scope="self"` searches only your own conversation (including what was compacted away) and your memory. Session results: read the context with `read_session(instance=owner, around=ref)`.

    Args:
        query: Words to find; a result contains all of them (stemmed, so `deploy` finds `deployed`).
        limit: How many results (default 10, max 30).
        raw: Treat `query` as SQLite FTS5 syntax: `"exact phrase"`, `a OR b`, `a NOT b`, `prefix*`, `NEAR(a b)`.
        scope: `self` searches only your own conversation (also from before compaction) and your memory.
        sources: Restrict to these sources (`session`, `memory`, `event`, `repo`, `event_type`); default all.
    """
    return call("search", {"query": query, "limit": limit, "raw": raw, "scope": scope, "sources": sources})


def memory_write(*, content: str, path: str, expected_sha256: str | None = None) -> dict[str, Any]:
    """Write a memory file (Markdown) in one of your namespaces: `agents/<definition>/...` (mounted at /memory/self), `projects/<home>/...` (/memory/project) or `user/...` (/memory/user). Pass the sha256 of the version you read as `expected_sha256` (`sha256sum`), or omit it to create a new file. If someone changed it meanwhile you get a conflict with the current content: merge and write again.

    Args:
        content: The whole new content (Markdown).
        path: e.g. `agents/dev/github.md`, `projects/groundstation/INDEX.md`, `user/preferences.md`.
        expected_sha256: sha256 of the version you are replacing (`sha256sum <file>`); omit to create a new file.
    """
    return call("memory_write", {"content": content, "path": path, "expected_sha256": expected_sha256})


def memory_append(*, path: str, text: str) -> dict[str, Any]:
    """Append line(s) to a memory file, creating it if needed. No version needed: use it for logs and lists.

    Args:
        text: Added at the end, on its own line(s). Creates the file if needed.
    """
    return call("memory_append", {"path": path, "text": text})


def memory_delete(*, expected_sha256: str, path: str) -> dict[str, Any]:
    """Delete a memory file; `expected_sha256` is the sha256 of the version you are deleting.

    Args:
        expected_sha256: sha256 of the version you are deleting.
    """
    return call("memory_delete", {"expected_sha256": expected_sha256, "path": path})


def memory_read(*, path: str) -> dict[str, Any]:
    """Read a memory file with its sha256."""
    return call("memory_read", {"path": path})


def memory_list(*, prefix: str | None = None) -> list[dict[str, Any]]:
    """Memory files you may read, optionally under a directory such as `user` or `agents/dev`.

    Args:
        prefix: A directory such as `user` or `agents/dev`; omit for every file you may read.
    """
    return call("memory_list", {"prefix": prefix})


def memory_history(*, path: str) -> list[dict[str, Any]]:
    """The commits that changed a memory file, newest first, with their authors."""
    return call("memory_history", {"path": path})


def get_events(*, correlation: str | None = None, limit: int | None = None, type: str | None = None) -> list[dict[str, Any]]:
    """Read the event journal, newest first: filter by type (`mail.*`) or conversation (`correlation`)."""
    return call("get_events", {"correlation": correlation, "limit": limit, "type": type})


def ask_user(*, question: str, about: list[str] | None = None, detail: str | None = None, expires_in: str | None = None, options: list[str] | None = None, priority: str | None = None) -> dict[str, Any]:
    """Ask the user a question and get the answer later as a `dialog.answered` event. `options` are the allowed answers; `about` lists objects (e.g. `proposal:<id>`) whose change withdraws the question.

    Args:
        about: Objects whose change withdraws the question, e.g. `proposal:<id>`.
        detail: Background the user needs to answer.
        expires_in: Withdraw the question after this long, e.g. `1d`.
        options: Allowed answers, e.g. ["yes", "no"]. Empty means free text.
        priority: `blocking` (you cannot continue; notifies the user), `normal`, or `info`.
    """
    return call("ask_user", {"question": question, "about": about, "detail": detail, "expires_in": expires_in, "options": options, "priority": priority})


def get_run_logs(*, run_id: str, tail: int | None = None) -> dict[str, Any]:
    """Output of a job run.

    Args:
        tail: Last N lines (default 200).
    """
    return call("get_run_logs", {"run_id": run_id, "tail": tail})


def get_incident(*, id: str) -> dict[str, Any]:
    """One incident: what failed, how often, how it was resolved."""
    return call("get_incident", {"id": id})


def get_incident_history(*, runnable: str) -> list[dict[str, Any]]:
    """Past incidents of a job or service with their resolutions, newest first. Check this before fixing anything."""
    return call("get_incident_history", {"runnable": runnable})


def resolve_incident(*, id: str, resolution: str) -> dict[str, Any]:
    """Close an incident with what caused it and what you did (or why nothing needs doing). Required at the end of every triage.

    Args:
        resolution: Cause and what you did. Future triage reads this.
    """
    return call("resolve_incident", {"id": id, "resolution": resolution})


def state_get(*, key: str) -> dict[str, Any]:
    """Read this job's persistent key-value state."""
    return call("state_get", {"key": key})


def state_set(*, key: str, value: str) -> dict[str, Any]:
    """Write this job's persistent key-value state (cursors, dedup keys; up to 64 KiB per value)."""
    return call("state_set", {"key": key, "value": value})


def health(*, ok: bool, reason: str | None = None) -> dict[str, Any]:
    """Report this service's health: `ok=true` is the heartbeat its job.toml `[health]` asks for; `ok=false` with a reason stops and restarts it and reports an unhealthy incident.

    Args:
        reason: Why it is unhealthy (with `ok = false`).
    """
    return call("health", {"ok": ok, "reason": reason})
