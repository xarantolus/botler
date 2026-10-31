---
name: claude-code
description: Hand a coding task to a Claude Code session that runs in a container on the user's own Claude subscription, and relay its messages. Use when the user asks for Claude Code or Claude to do something for them, or to answer or continue a Claude session.
---

# Claude Code sessions

`jobs/claude-session/` runs Claude Code (`claude -p`, image `images/claude-code`) for every
`claude_session.requested` event. Each request is one turn of a session. Claude works without
permission prompts in its own container, with its own working directory per session. You do not
get the transcript. You get messages from it while it works (its `message_user` tool) and one
when the turn ends: its final answer and the `session_id`.

## Setup (once)

The job needs the secret group `claude-code` (`CLAUDE_CODE_OAUTH_TOKEN`). Look in memory first:
if it says the token is set up, skip this. Otherwise tell the user:

> On a computer where Claude Code is installed (`npm install -g @anthropic-ai/claude-code`, or see
> https://code.claude.com), run `claude setup-token`. Approve in the browser, then paste the token
> it prints into the form I'm opening. It is valid for one year and only makes model requests on
> your subscription (Pro, Max, Team or Enterprise).

Then call `request_secrets(group="claude-code", fill=["CLAUDE_CODE_OAUTH_TOKEN"], reason="Run
Claude Code sessions on your subscription")`. When `dialog.answered` says it was provided, write
to memory that the Claude Code token is set up (and the date: it expires after a year). Without
the token the job cannot start, and the failure goes to `triage`, not to you.

## Start a session

```
emit_event(type="claude_session.requested", payload={
  "task": "In the repo, make the flaky test in tests/test_sync.py deterministic and open a PR.",
  "repo": "https://github.com/me/project.git",   # optional: cloned into the session's directory
})
```

- **The task:** a complete brief, since Claude sees nothing else. Say what done looks like.
- **`repo`:** only for a new session. It is cloned anonymously, so it must be public unless the
  user adds a credential (below).
- **`notify`:** the chat that gets the messages. It defaults to you; set `"botler"` when you are a
  short-lived chat and the session will outlast you.

Tell the user it started. Each run takes up to about 2 hours; several sessions may run at once.

## Messages and the end of a turn

They arrive as messages from the job:
- `Claude Code session <id> says: …` is news while it works.
- `Claude Code session <id> finished its turn:` (or `failed:`) carries the result.

Pass them on to the user in your words, with the session id when they may want to continue. The
same result is a `claude_session.finished` event, and the job keeps it in its state under
`session:<id>`. Note open sessions you care about in memory: the session id and what it is for.

## Continue a session (the user replies)

When the user answers Claude ("tell the Claude session to use Postgres"), continue it:

```
emit_event(type="claude_session.requested", payload={
  "resume": "<session_id>", "task": "The user says: use Postgres, not SQLite."})
```

Claude has the whole earlier conversation and its working directory. When it needs a decision,
it asks in its final answer and stops; relay the question, and resume with the user's answer.

## Failures

- **"Not logged in" or a login error:** the token is wrong or expired. Ask for a new one (setup
  above) with `fill`.
- **A usage limit:** the user's subscription limits apply (shared with their own Claude use).
  Tell them when it should reset, if Claude said, and offer to resume then (`set_timer`).
- **"stopped after 110 minutes":** resume it with "continue".

## Limits

- **No permission prompts.** Claude runs every command it chooses. The container is the boundary:
  it holds only `/data` (Claude's home and the session directories), the network, and the token.
  It cannot reach the user's machine or other containers.
- **Grant only what a task needs.** For pushing to private repos, add a group (e.g. a GitHub
  token) to `secrets` in `jobs/claude-session/job.toml` through a proposal. Every session can
  then use it, so suggest a narrowly scoped token.
- **Usage:** runs use the user's subscription, not botler's budget. Do not start sessions the
  user did not ask for, and do not loop on resume.
