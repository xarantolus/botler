---
name: background-service
description: Build an always-running watcher (inbox, feed, folder, API poller) that emits typed events which triggers route to agents. Use when the user wants something watched or polled continuously, rather than checked on a schedule.
---

# Background services (watchers)

A watcher is a job with `lifetime = "service"`: the hub keeps it running, restarts it with
backoff, and opens an incident when it crash-loops or misses its heartbeat. It calls no LLM. It
notices things and emits **typed events**; trigger files decide which agent hears about them.
For "check once a day", use a oneshot job with a `cron` trigger instead.

`jobs/example-watcher/` is a working template: copy it.

## 1. The event (`packages/`)

```python
# packages/shared/src/shared/events.py (or your own package)
class MailReceived(botler.Event):
    """A new mail arrived in a watched folder."""

    type: ClassVar[str] = "mail.received"
    folder: str
    sender: str
    subject: str
    uid: int
```

Regenerate the manifest with `uv run python -m botler.events manifest`. The gate checks that it is
current. Keep payloads small: a summary and an id to fetch the rest, never whole bodies or
attachments.

## 2. The job (`jobs/<name>/`)

```toml
# jobs/mail-watch/job.toml
version = 1
description = "Watches the INBOX over IMAP and emits mail.received for each new mail."
lifetime = "service"
command = ["/app/.venv/bin/python", "-m", "mail_watch"]
secrets = ["imap"]            # a secret group; request_secrets if it does not exist yet
# emits = ["mail.received"]   # once job.toml supports it: declares what this service produces

[health]
heartbeat = "5m"              # call botler.health.ok() at least this often
```

Add a `Dockerfile`, a `pyproject.toml`, `src/` and `tests/`, as in `jobs/example-watcher/`.

## 3. The loop

- Keep the **cursor** (last seen id, timestamp, ETag) in `botler.state`. It survives restarts.
- Emit, *then* move the cursor: a crash then repeats an event instead of losing one.
- Call `botler.health.ok()` once per loop. Call `botler.health.fail(reason)` for a condition a
  restart might fix. Let unexpected exceptions crash the service: the hub restarts it and reports
  the failure.
- Give the loop's step injectable `state`, `emit` and `healthy` parameters so tests run without a
  hub (see `jobs/example-watcher/tests/`).

## 3b. Settings the user can change

What the user may want to adjust (folders, intervals, thresholds, where to notify) is a
**settings model**, not a constant. Then the user edits it under Settings → Jobs & Services
without a proposal.

```python
# jobs/mail-watch/src/mail_watch/settings.py  (this path and the class name are how it is found)
from pydantic import Field

import botler


class Settings(botler.Settings):
    """Which folders to watch and how often."""

    folders: list[str] = Field(["INBOX"], description="IMAP folders to watch")
    every_minutes: int = Field(5, ge=1, description="How often to look")
```

- Read the values with `Settings.load()` at start. A change restarts a service.
- Give every field a default, so the service runs before anyone sets a value.
- Use `Literal[...]` for choices (they render as a dropdown) and `list[Literal[...]]` for
  multi-select.
- `python -m botler.settings manifest` regenerates `settings.manifest.json`; the gate checks it.
- The values live in `jobs/<name>/settings.toml`, which the page commits as the user. Agents may
  propose changes to that file too.
- Never put secrets in settings: those belong in a secret group.

## 4. The trigger (`triggers/<name>.toml`)

```toml
version = 1
[[trigger]]
name = "new-mail"
on = "mail.received"
filter = { folder = "INBOX" }
target = { resume = "botler" }        # or { spawn = "triage" } for a fresh chat per event
prompt = "New mail arrived. Decide whether the user needs to know."
```

## 5. Try it, then propose it

1. Run `gate/run`, commit, `git push origin HEAD`.
2. `trial_run(job="mail-watch", branch="agents/<you>/...", minutes=3)` runs it from your branch,
   with only the secret groups you hold and its own empty `state`. `emit_event`, `send_message`
   and `set_timer` are recorded, not carried out. A `trial.finished` event brings the outcome, the
   logs tail and the events it would have emitted. Check them, fix, and try again.
3. `propose_change`. **The first deployment of a service asks the user**, so say in the title
   what it watches. Later code-only changes merge after review; new secrets, ports or network
   access ask again.

To keep a service defined but not running, set `enabled = false` (as the example does).

## Reference: IMAP inbox watcher (sketch, not deployed)

```python
"""Watch an IMAP folder; emit mail.received per new mail. Secrets (group `imap`): IMAP_HOST,
IMAP_USER, IMAP_PASSWORD."""

import imaplib, os, time
from email.parser import BytesHeaderParser
from email.utils import parseaddr
import botler
from shared.events import MailReceived

FOLDER = "INBOX"


def poll(conn: imaplib.IMAP4_SSL) -> None:
    last = int(botler.state.get("uid") or "0")
    conn.select(FOLDER, readonly=True)
    _, data = conn.uid("SEARCH", None, f"UID {last + 1}:*")
    for uid in sorted(int(u) for u in data[0].split() if int(u) > last):
        _, msg = conn.uid("FETCH", str(uid), "(BODY.PEEK[HEADER.FIELDS (FROM SUBJECT)])")
        h = BytesHeaderParser().parsebytes(msg[0][1])
        botler.emit(
            MailReceived(folder=FOLDER, sender=parseaddr(h["From"])[1], subject=h["Subject"] or "", uid=uid),
            provenance="untrusted",
        )
        botler.state.set("uid", str(uid))  # after the emit
    botler.health.ok()


def main() -> None:
    conn = imaplib.IMAP4_SSL(os.environ["IMAP_HOST"])
    conn.login(os.environ["IMAP_USER"], os.environ["IMAP_PASSWORD"])
    while True:
        poll(conn)
        time.sleep(60)  # or IMAP IDLE: send "IDLE", wait up to ~29 min for "* n EXISTS", "DONE"
```

Notes: on the first run, start the cursor at the current highest UID so years of old mail are not
emitted. If the folder's `UIDVALIDITY` changes, reset the cursor. Mail content is `untrusted`.
