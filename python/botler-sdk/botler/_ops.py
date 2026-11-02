"""Every hub operation as a function. GENERATED from the hub's `OPS` specs by
`BOTLER_BLESS=1 cargo test -p botler --test sdk`; do not edit. The hub decides who may call what."""

from __future__ import annotations

from typing import Any

from ._client import call


def send_message(*, to: str, text: str, urgent: bool | None = None, reply_to: str | None = None, expect_reply_within: str | None = None, files: list[str] | None = None) -> dict[str, Any]:
    """Send a message to an agent instance (e.g. `botler`). Replies come back to you as messages. Set `reply_to` to the event you are answering to keep the thread.

    Args:
        to: Recipient instance name.
        urgent: Interrupt the recipient at its next turn instead of waiting until it is idle.
        reply_to: Event id this answers, to keep the conversation thread.
        expect_reply_within: You need an answer within this long, e.g. `20m`: if the recipient neither replies to you nor ends its turn by then, you get a `reply.overdue` event.
        files: Files from your container to send along (any type): paths in your home, your monorepo clone or `/inbox/files`. The recipient finds them under `/inbox/files`.
    """
    return call("send_message", {"to": to, "text": text, "urgent": urgent, "reply_to": reply_to, "expect_reply_within": expect_reply_within, "files": files})


def attach_files(*, files: list[str]) -> dict[str, Any]:
    """Show files from your container to the user in your chat, where your reply is: a screenshot, a picture, a video, audio, a PDF, any file (at most 100 MiB each). The paths must be in your home, your monorepo clone or /inbox/files (copy a file from /tmp into your home first). To give files to another chat, use send_message's `files`.

    Args:
        files: Paths in your container: your home (`/home/agent`, `~/...` or relative), your monorepo clone or `/inbox/files`. Any type (images, videos, audio, PDFs, archives, ...).
    """
    return call("attach_files", {"files": files})


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


def wait(*, in_: str, reason: str) -> dict[str, Any]:
    """Wait for something nothing will tell you about (an outside page or process), instead of sleeping or polling in a shell: your turn ends now, and you wake when anything arrives for you or at the latest after `in`, reminded of `reason`. Not for answers, approvals, grants, proposals, trials or children: those wake you by themselves, so just end your turn. A new wait replaces the last one, and anything arriving ends it.

    Args:
        in_: At most how long, e.g. `10m`, `2h` (up to a day).
        reason: What you are waiting for, e.g. "the trial of mail-watch"; you are reminded of it.
    """
    return call("wait", {"in": in_, "reason": reason})


def write_document(*, name: str, title: str, body: str) -> dict[str, Any]:
    """Write a document for the user to read in the app (Documents), in Markdown: a report, a shortlist you keep adding to, a plan. Writing the same name again replaces it, so read_document first to add to it. Prefer this over a memory note or a file when the user is the reader.

    Args:
        name: Its id: lowercase words joined by dashes, e.g. `flat-shortlist`. Writing an existing one replaces it, so read it first to add to it.
        body: The whole document, in Markdown.
    """
    return call("write_document", {"name": name, "title": title, "body": body})


def read_document(*, name: str) -> dict[str, Any]:
    """Read a document (its title, Markdown body and who wrote it last), by name or by its link: a `/documents/<name>` link (also as a full URL) is a document, read here, not a file.

    Args:
        name: Its name, or its link as the app shows it (`/documents/<name>`, also a full URL).
    """
    return call("read_document", {"name": name})


def list_documents() -> list[dict[str, Any]]:
    """The documents there are, most recently updated first (without their bodies)."""
    return call("list_documents", {})


def delete_document(*, name: str) -> dict[str, Any]:
    """Delete a document."""
    return call("delete_document", {"name": name})


def cancel_timer(*, id: str) -> dict[str, Any]:
    """Cancel a pending timer."""
    return call("cancel_timer", {"id": id})


def list_timers() -> list[dict[str, Any]]:
    """Pending timers you created (all timers for the user)."""
    return call("list_timers", {})


def set_chat_title(*, title: str, slug: str) -> dict[str, Any]:
    """Name your chat so the user can find it: a short title and a unique slug like `groundstation-antenna-fix` (lowercase words joined by dashes, project or topic first). The slug is the chat's name in its link and for messages. Do this after the first message of a new chat (unless your spawner named it), again if the topic changes, and when the user asks you to rename yourself. A title the user gave stays; the slug still changes.

    Args:
        slug: Lowercase words joined by dashes, 3 to 60 characters.
    """
    return call("set_chat_title", {"title": title, "slug": slug})


def spawn_agent(*, definition: str, task: str, slug: str | None = None, title: str | None = None, env: Any = None) -> dict[str, Any]:
    """Start a new instance of an agent definition with a task. It reports back to you by message; if it has not by `reply_within` (botler.toml, default 20m), you get a `reply.overdue` event. Name it with `slug` (e.g. `mattermost-websocket-bridge`) so the user can tell chats apart. `env = {secrets: [...], mounts: [...]}` gives it secret groups and shares beyond its definition: what you hold yourself or policy allows is granted at once, the rest is asked of the user. Give the child the access its task needs; never tell it to work around missing access.

    Args:
        definition: Agent definition under `agents/`, e.g. `dev`.
        slug: The new chat's name in links and messages, like `mattermost-websocket-bridge` (see `set_chat_title`); without one it is `<definition>-<random>` until the chat names itself.
        title: Its title (default: from the slug).
        env: What the new chat gets beyond its definition.
    """
    return call("spawn_agent", {"definition": definition, "task": task, "slug": slug, "title": title, "env": env})


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


def trial_run(*, job: str, branch: str, minutes: int | None = None) -> dict[str, Any]:
    """Try a job or service from your pushed branch (agents/<you>/...) for a few minutes before proposing it: its definition and image from the branch, only secret groups you hold, its own empty `state`. emit_event, send_message and set_timer are recorded, not carried out (they return dry_run: true). When it exits or its minutes are up, a trial.finished event brings the outcome, logs tail and the events it would have emitted.

    Args:
        job: The job or service (`jobs/<job>/` on the branch).
        branch: Your pushed branch, e.g. `agents/botler/mail-watch`.
        minutes: How long it may run, 1 to 10 (default 3). Then it is stopped: the normal end of a service.
    """
    return call("trial_run", {"job": job, "branch": branch, "minutes": minutes})


def stop_trial(*, run_id: str | None = None) -> dict[str, Any]:
    """Stop your running trial (from trial_run or replay_event), e.g. when it hangs or you have seen enough. Its trial.finished, with the logs tail and recorded effects, follows.

    Args:
        run_id: The trial's run id (from trial_run); your running trial when unset.
    """
    return call("stop_trial", {"run_id": run_id})


def replay_event(*, event_id: str, job: str, branch: str | None = None, minutes: int | None = None) -> dict[str, Any]:
    """Re-run a job against a past event, as a dry run: main's definition and image, or your pushed branch's to test a fix. Like trial_run: only secret groups you hold, its own empty `state`, and emit_event, send_message and set_timer are recorded, not carried out. The outcome arrives as trial.finished.

    Args:
        event_id: The past event to feed the job (`/botler/event.json`, as when it first ran).
        branch: Your pushed branch to take the job from (to test a fix); main when unset.
        minutes: How long it may run, 1 to 10 (default 3).
    """
    return call("replay_event", {"event_id": event_id, "job": job, "branch": branch, "minutes": minutes})


def get_proposal(*, id: str) -> dict[str, Any]:
    """A proposal's status and stage, deployment summary, gate report, review notes, and `process`: how proposals move and what happens next with this one (tell the user no more than that)."""
    return call("get_proposal", {"id": id})


def submit_review(*, proposal_id: str, verdict: Any, notes: str) -> dict[str, Any]:
    """Your verdict on the proposal you were asked to review: approve, escalate (fine, but the user should look) or reject, with notes for a busy reader."""
    return call("submit_review", {"proposal_id": proposal_id, "verdict": verdict, "notes": notes})


def request_secrets(*, group: str, reason: str, description: str | None = None, class_: Any = None, add: list[Any] | None = None, fill: list[str] | None = None, remove: list[str] | None = None) -> dict[str, Any]:
    """Ask the user for secrets, asynchronously: create a group, add fields to one, re-enter values (`fill`) or remove fields. The user gets a form; you get a request id now and a `dialog.answered` event later saying what was provided (never the values). Do not continue work that needs the secrets until that event arrives. Using a group in a container still needs a grant: `request_access("secret:<group>")` for this chat, or `secrets = ["<group>"]` in a definition, proposed as a change.

    Args:
        group: The group (existing, or new: then `description` and `add` are required).
        reason: Why you need this, shown to the user.
        description: For a new group: what it is for.
        class_: For a new group: `container` (default) or `hub` (never enters a container).
        add: Fields to add (the user enters their values).
        fill: Existing fields whose values the user should (re-)enter.
        remove: Fields to remove from the group (their values are deleted).
    """
    return call("request_secrets", {"group": group, "reason": reason, "description": description, "class": class_, "add": add, "fill": fill, "remove": remove})


def request_access(*, resource: str, reason: str, mode: Any = None) -> dict[str, Any]:
    """Ask for a resource your definition does not give you, for this chat: `secret:<group>`, `service:<name>`, `mount:<name>` or `event:<type>`. Policy may grant it at once; otherwise the user decides (allow for this chat, always allow, or deny) and the answer wakes you as a `dialog.answered` event, with the grant applied to your container. Memory notes say which group fits which work.

    Args:
        resource: What you need, as `<kind>:<name>`: `secret:<group>` (a secret group's values in your container), `service:<name>` (a service's MCP server), `mount:<name>` (a network share) or `event:<type>` (subscribing to a restricted event type).
        reason: Why, for the user deciding.
        mode: Mounts: `rw` to write (default `ro`).
    """
    return call("request_access", {"resource": resource, "reason": reason, "mode": mode})


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
    """Withdraw one of your outstanding requests by id when it no longer matters, e.g. the user answered it in the chat (it disappears for the user; a late answer is rejected). Withdrawing a proposal's approval request withdraws the proposal.

    Args:
        reason: Why it is no longer needed.
    """
    return call("cancel_request", {"id": id, "reason": reason})


def handover_ready(*, summary: str, outstanding: list[str] | None = None, state: list[str] | None = None, handoff: Any = None) -> dict[str, Any]:
    """Hand over: after an archive.requested or handover.requested event, or (not in a persistent chat) by yourself once your task is done and the user has what they need, which archives this chat. Give what you did, what is outstanding, where your state lives, and optionally who should continue. For a handover to yourself you continue on a fresh session that starts from this report.

    Args:
        summary: Everything you did in this chat, for someone who has not read it.
        outstanding: Work you did not finish.
        state: Where your state lives: branches, files, memory notes, external systems.
        handoff: Who should continue the outstanding work, if anyone.
    """
    return call("handover_ready", {"summary": summary, "outstanding": outstanding, "state": state, "handoff": handoff})


def handover(*, chat: str, to: str | None = None, reason: str | None = None) -> dict[str, Any]:
    """Ask a chat to hand over now: to a fresh session of itself (`to = self`, the default) or to another chat or agent (then it is archived). The chat writes its report with `handover_ready`.

    Args:
        chat: The chat (name or slug).
        to: `self` (default): continue on a fresh session seeded with its report. Otherwise a chat or an agent definition that takes over; the chat is then archived.
    """
    return call("handover", {"chat": chat, "to": to, "reason": reason})


def compact_chat(*, chat: str) -> dict[str, Any]:
    """Compact a chat's conversation now: pi summarizes the older part and the chat goes on from the summary (`/compact`).

    Args:
        chat: The chat (name or slug).
    """
    return call("compact_chat", {"chat": chat})


def clear_chat(*, chat: str) -> dict[str, Any]:
    """Start a chat over on an empty session, carrying nothing over (`/clear`). Its home, memory and the old session stay; `handover` instead carries a report over.

    Args:
        chat: The chat (name or slug).
    """
    return call("clear_chat", {"chat": chat})


def raise_budget(*, chat: str, usd: float | None = None, tokens: int | None = None) -> dict[str, Any]:
    """Allow a chat more than its daily budget, for today only (the definition is unchanged), and resume it if the budget had stopped it.

    Args:
        chat: The chat (name or slug).
        usd: Dollars more today.
        tokens: Tokens more today (for a budget in tokens).
    """
    return call("raise_budget", {"chat": chat, "usd": usd, "tokens": tokens})


def pin_chat(*, chat: str, pinned: bool) -> dict[str, Any]:
    """Pin a chat to the top of the list (right after the front door), or unpin it.

    Args:
        chat: The chat (name or slug).
    """
    return call("pin_chat", {"chat": chat, "pinned": pinned})


def rename_chat(*, chat: str, title: str) -> dict[str, Any]:
    """Give a chat the user's own title; its agent keeps it from then on.

    Args:
        chat: The chat (name or slug).
    """
    return call("rename_chat", {"chat": chat, "title": title})


def session_tree(*, chat: str) -> dict[str, Any]:
    """A chat's current session as a tree of entries (ids, parents, labels, the active branch)."""
    return call("session_tree", {"chat": chat})


def goto_entry(*, chat: str, entry: str) -> dict[str, Any]:
    """Rewind a chat (or switch branch): it continues from this entry of its session tree. Only while it is not working.

    Args:
        entry: The entry to continue from: later entries stay in the tree on another branch.
    """
    return call("goto_entry", {"chat": chat, "entry": entry})


def label_entry(*, chat: str, entry: str, label: str | None = None) -> dict[str, Any]:
    """Label an entry of a chat's session tree (none: remove the label). Only while it is not working.

    Args:
        label: The label; none removes it.
    """
    return call("label_entry", {"chat": chat, "entry": entry, "label": label})


def fork_chat(*, chat: str, entry: str) -> dict[str, Any]:
    """Start a new chat of the same agent definition from a chat's conversation up to one of its assistant entries.

    Args:
        entry: An assistant entry: the new chat holds the conversation up to and including it.
    """
    return call("fork_chat", {"chat": chat, "entry": entry})


def stop_chat(*, chat: str, cascade: bool | None = None, reason: str | None = None) -> dict[str, Any]:
    """Stop a chat: abort its running turn and cancel what is queued for it; with `cascade`, also every chat it started (recursively). The chat and its parent are told with a `chat.stopped` note.

    Args:
        chat: The chat (name or slug).
        cascade: Also stop every chat it started, and theirs.
        reason: Why; the chat reads it when it next wakes.
    """
    return call("stop_chat", {"chat": chat, "cascade": cascade, "reason": reason})


def list_event_types(*, query: str | None = None) -> dict[str, Any]:
    """Event types (built-in and declared) with their description, payload schema, producers and current subscribers; `query` narrows by words or a glob. `emit_event` validates payloads against these; `subscribe` takes their names.

    Args:
        query: Words or a glob (`mail.*`) to narrow the list by type or description.
    """
    return call("list_event_types", {"query": query})


def subscribe(*, type: str, filter: dict[str, Any] | None = None, wake: bool | None = None, note: str | None = None) -> dict[str, Any]:
    """Get events of a type (or glob like `mail.*`) delivered to this chat, optionally only those whose payload fields equal `filter`. They wake you even while you sleep (`wake=false`: quiet notes instead). Lasts until `unsubscribe`; find types with `list_event_types` or `search(sources=["event_type"])`. Restricted types need `request_access("event:<type>")` first.

    Args:
        type: Event type (`mail.received`) or glob (`mail.*`); see `list_event_types`.
        filter: Payload fields that must have these values, e.g. `{"folder": "INBOX"}`.
        wake: `false`: deliver as a quiet note that waits for whatever wakes you next (default `true`: each event wakes you).
        note: Why you subscribed; shown with every delivery.
    """
    return call("subscribe", {"type": type, "filter": filter, "wake": wake, "note": note})


def unsubscribe(*, id: str) -> dict[str, Any]:
    """Stop a subscription by id (from `subscribe` or `list_subscriptions`).

    Args:
        id: The subscription id.
    """
    return call("unsubscribe", {"id": id})


def list_subscriptions() -> dict[str, Any]:
    """Your event subscriptions with their ids."""
    return call("list_subscriptions", {})


def read_session(*, instance: str, last: int | None = None, around: str | None = None, n: int | None = None) -> dict[str, Any]:
    """Read the recent messages of a session: your own, or another instance's (what it has been doing) if you may. `around` (an entry id from `search`) reads the messages around that entry instead.

    Args:
        last: How many of the most recent messages (default 20, max 200).
        around: An entry id (from `search`): read the messages around it instead of the latest.
        n: With `around`: how many messages before and after it (default 5, max 50).
    """
    return call("read_session", {"instance": instance, "last": last, "around": around, "n": n})


def search(*, query: str, sources: list[Any] | None = None, scope: Any = None, limit: int | None = None, raw: bool | None = None) -> dict[str, Any]:
    """Full-text search over sessions, memory notes, the event journal, the monorepo and event types (`event_type`: what can happen, who emits it), best matches first. `scope="self"` searches only your own conversation (including what was compacted away) and your memory. Session results: read the context with `read_session(instance=owner, around=ref)`.

    Args:
        query: Words to find; a result contains all of them (stemmed, so `deploy` finds `deployed`).
        sources: Restrict to these sources (`session`, `memory`, `event`, `repo`, `event_type`); default all.
        scope: `self` searches only your own conversation (also from before compaction) and your memory.
        limit: How many results (default 10, max 30).
        raw: Treat `query` as SQLite FTS5 syntax: `"exact phrase"`, `a OR b`, `a NOT b`, `prefix*`, `NEAR(a b)`.
    """
    return call("search", {"query": query, "sources": sources, "scope": scope, "limit": limit, "raw": raw})


def memory_write(*, path: str, content: str, expected_sha256: str | None = None) -> dict[str, Any]:
    """Write a memory file (Markdown) in one of your namespaces: `agents/<definition>/...` (mounted at /memory/self), `projects/<home>/...` (/memory/project) or `user/...` (/memory/user). Pass the sha256 of the version you read as `expected_sha256` (`sha256sum`), or omit it to create a new file. If someone changed it meanwhile you get a conflict with the current content: merge and write again.

    Args:
        path: e.g. `agents/dev/github.md`, `projects/groundstation/INDEX.md`, `user/preferences.md`.
        content: The whole new content (Markdown).
        expected_sha256: sha256 of the version you are replacing (`sha256sum <file>`); omit to create a new file.
    """
    return call("memory_write", {"path": path, "content": content, "expected_sha256": expected_sha256})


def memory_append(*, path: str, text: str) -> dict[str, Any]:
    """Append line(s) to a memory file, creating it if needed. No version needed: use it for logs and lists.

    Args:
        text: Added at the end, on its own line(s). Creates the file if needed.
    """
    return call("memory_append", {"path": path, "text": text})


def memory_delete(*, path: str, expected_sha256: str) -> dict[str, Any]:
    """Delete a memory file; `expected_sha256` is the sha256 of the version you are deleting.

    Args:
        expected_sha256: sha256 of the version you are deleting.
    """
    return call("memory_delete", {"path": path, "expected_sha256": expected_sha256})


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


def get_events(*, type: str | None = None, correlation: str | None = None, limit: int | None = None) -> list[dict[str, Any]]:
    """Read the event journal, newest first: filter by type (`mail.*`) or conversation (`correlation`)."""
    return call("get_events", {"type": type, "correlation": correlation, "limit": limit})


def ask_user(*, question: str, options: list[str] | None = None, detail: str | None = None, about: list[str] | None = None, priority: str | None = None, expires_in: str | None = None, form: Any = None) -> dict[str, Any]:
    """Ask the user a question and get the answer later as a `dialog.answered` event. `options` are the allowed answers; `form` (a JSON Schema object) asks for several typed fields at once (choices, multi-select, numbers, dates, files) and the answer's `values` match it; `about` lists objects (e.g. `proposal:<id>`) whose change withdraws the question.

    Args:
        options: Allowed answers, e.g. ["yes", "no"]. Empty means free text.
        detail: Background the user needs to answer.
        about: Objects whose change withdraws the question, e.g. `proposal:<id>`.
        priority: `blocking` (you cannot continue; notifies the user), `normal`, or `info`.
        expires_in: Withdraw the question after this long, e.g. `1d`.
        form: A form instead of free text or `options`: a JSON Schema object whose `properties` are the fields (`title`, `description`, `default`, `required`). Strings (with `enum`, or `format` date / date-time / time / file, or `"x-multiline": true`), numbers, integers, booleans, and arrays of `enum` strings (multi-select) or of files. Never ask for secrets in a form; use `request_secrets`.
    """
    return call("ask_user", {"question": question, "options": options, "detail": detail, "about": about, "priority": priority, "expires_in": expires_in, "form": form})


def request_visit(*, host_port: int, title: str, instructions: str, path: str | None = None, expires_in: str | None = None) -> dict[str, Any]:
    """Ask the user to open a page of a service in their browser (e.g. to log in) and tell you when done. `host_port` must be a port published by a service this chat may use. The answer (`done` or `cancel`) arrives as a `dialog.answered` event.

    Args:
        host_port: A host port published by a service this chat may use.
        title: What the visit is for, e.g. "Log in to Home Assistant".
        instructions: What the user should do there, step by step.
        path: Path to open, e.g. `/login`. Defaults to `/`.
        expires_in: Withdraw the request after this long, e.g. `1d`.
    """
    return call("request_visit", {"host_port": host_port, "title": title, "instructions": instructions, "path": path, "expires_in": expires_in})


def invoke_action(*, name: str, args: Any) -> dict[str, Any]:
    """Run an approval-gated action (something that reaches other people or uses hub-held secrets) with arguments. The user approves exactly these arguments; the outcome arrives as an `action.result` event.

    Args:
        name: The action (`jobs/<name>/job.toml` with `lifetime = "action"`).
        args: Its arguments, matching its `args` schema. The user approves exactly these.
    """
    return call("invoke_action", {"name": name, "args": args})


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


def storage_usage(*, refresh: bool | None = None) -> dict[str, Any]:
    """Disk use of the data directory by chat (home, workspace, session) and by kind (shared homes, images, exports, logs, search index, memory), largest first.

    Args:
        refresh: Measure again instead of returning the last overview.
    """
    return call("storage_usage", {"refresh": refresh})


def clear_chat_caches(*, chat: str) -> dict[str, Any]:
    """Delete a sleeping chat's rebuildable files: gitignored files in its git checkouts and `~/.cache`."""
    return call("clear_chat_caches", {"chat": chat})


def delete_chat(*, chat: str) -> dict[str, Any]:
    """Delete an archived chat for good: its home, workspace, session and pending timers and deliveries. Its events stay in the journal.

    Args:
        chat: An archived chat.
    """
    return call("delete_chat", {"chat": chat})


def collect_garbage(*, logs_older_than: str | None = None) -> dict[str, Any]:
    """Remove unused images and exports now, and run logs older than `logs_older_than` (default 30d).

    Args:
        logs_older_than: Delete run logs older than this (default `30d`).
    """
    return call("collect_garbage", {"logs_older_than": logs_older_than})


def run_job(*, job: str) -> dict[str, Any]:
    """Start a oneshot job (main's code) now, with a `manual` event: e.g. to check a fix that was merged. Honours pause and the overlap policy of the job's trigger (skip by default). Its outcome arrives as runnable.finished or runnable.failed."""
    return call("run_job", {"job": job})


def restart_service(*, service: str) -> dict[str, Any]:
    """Restart a service (main's code): it is stopped and started again right away, e.g. when it hangs or its tools are missing. Chats that use it reconnect by themselves once it is back. Not needed after a merge: the hub moves a service whose code changed to main's code by itself (it keeps serving while its new image builds, then swaps), and leaves one that is doing so alone."""
    return call("restart_service", {"service": service})


def channel_inbound(*, sender: str, text: str, files: list[Any] | None = None, reply_ref: str | None = None) -> dict[str, Any]:
    """Channel services: a message arrived from `sender`. The hub decides whether that is the user (paired with `pair <code>`) and where it goes; unpaired senders are ignored and you send them nothing.

    Args:
        sender: The outside account, stable across messages (a Telegram chat id, an email address, ...).
        reply_ref: The outbox item this message replies to, if the protocol says (answers that question).
    """
    return call("channel_inbound", {"sender": sender, "text": text, "files": files, "reply_ref": reply_ref})


def channel_outbox(*, wait_seconds: int | None = None) -> dict[str, Any]:
    """Channel services: what to send, waiting up to `wait_seconds` for something (a long poll; no open port needed). Send each message, then `channel_ack(id)`; unacknowledged messages come again.

    Args:
        wait_seconds: Wait up to this many seconds (at most 50) for something to send. 0: return at once.
    """
    return call("channel_outbox", {"wait_seconds": wait_seconds})


def channel_ack(*, id: str) -> dict[str, Any]:
    """Channel services: an outbox message was sent; it will not come again."""
    return call("channel_ack", {"id": id})
