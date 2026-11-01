---
name: claude-code
description: Hand a coding task to a Claude Code session that runs in a container on the user's own Claude subscription, send it follow-ups and messages while it works, and relay what it says. Use when the user asks for Claude Code or Claude to do something for them, or to answer, steer or continue a Claude session.
---

# Claude Code sessions

`jobs/claude-session/` runs Claude Code (image `images/claude-code`) for `claude_session.requested`
events. A session is live: Claude works without permission prompts in its own container and
working directory, takes your messages while it works, and stays loaded 30 minutes after it last
did anything. You do not get the transcript. You get messages from it (its `message_user` tool)
and one at the end of every turn: its answer and the `session_id`.

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

## Botler's own monorepo (optional, the user decides)

A session can work on this monorepo itself, through the hub (no SSH key, no GitHub). It is off
until the user wants it: propose `monorepo = true` in `jobs/claude-session/job.toml`, which asks
them. Then emit `claude_session.requested` with `repo = "monorepo"`: the session clones main from
the hub onto a branch of yours, `agents/<your chat>/claude-<session>`, and pushes it when done.
You then `propose_change` that branch like your own work (review, gate, the user's approval), so
its proposal and its outcome are yours. Check the branch with `git fetch` before proposing.

## GitHub (optional: private repos, pushes, PRs)

Public repos clone without either. There are two ways in, and both can be set up. Each is a
group the job only gets once a proposal adds it to `secrets` in `jobs/claude-session/job.toml`.
Only add a group after its value is entered: a group without a value stops the job from starting.
Look in memory first. Note there which way the sessions have, and for which repositories.

### Over SSH (`github-ssh`)

The key arrives at `/home/agent/.ssh/id_ed25519`. The job points git at it (`GIT_SSH_COMMAND`),
so sessions clone and push private repos over SSH.

> Generate a key for botler (or reuse an existing one):
> `ssh-keygen -t ed25519 -C botler-github -f ~/.ssh/botler_github`. Add the `.pub` to GitHub →
> Settings → SSH and GPG keys → New SSH key, then paste the private key file's contents into the
> form I'm opening.

Then call `request_secrets(group="github-ssh", fill=["GITHUB_SSH_PRIVATE_KEY"], reason="Clone and
push GitHub repos from Claude Code sessions")`. Once it is provided, propose
`secrets = ["claude-code", "github-ssh"]`.

### Over HTTPS with a token (`github`, `GITHUB_TOKEN`)

Wider than SSH: git and the `gh` CLI in the session use the token, so pushes and PRs are easy.

1. Tell the user to create a fine-grained personal access token on github.com (Settings >
   Developer settings > Fine-grained tokens) for only the repositories Claude should work on, with
   **Contents** and **Pull requests** set to read and write. Then `request_secrets(group="github",
   fill=["GITHUB_TOKEN"], reason="Let Claude Code sessions push and open PRs")`.
2. When `dialog.answered` says it was provided, propose adding `"github"` to the job's `secrets`.

Every session then holds the key or token, so keep it narrow. To remove access, propose taking
the group out again.

## Start a session

```
emit_event(type="claude_session.requested", payload={
  "task": "In the repo, make the flaky test in tests/test_sync.py deterministic and open a PR.",
  "repo": "https://github.com/me/project.git",   # optional: cloned into the session's directory
})
```

- **The task:** a complete brief, since Claude sees nothing else. Say what done looks like.
- **`repo`:** only for a new session. Private repos need GitHub access (above): with the
  `github-ssh` key use an SSH URL (`git@github.com:me/project.git`); with `GITHUB_TOKEN` an
  `https://` URL works too and `gh` can open PRs. With neither, an `https://` URL is cloned
  anonymously and must be public.
- **`notify`:** the chat that gets the messages. It defaults to you; set `"botler"` when you are a
  short-lived chat and the session will outlast you.

Right away you get `Claude Code session <id> started`: from then on you can send it messages.
Tell the user it started. Note open sessions you care about in memory: the id and what it is for.

## Messages from the session

- `Claude Code session <id> says: …` is news while it works.
- `Claude Code session <id> finished its turn:` (or `failed:`) carries its answer.

Pass them on to the user in your words, with the session id when they may want to answer. The
same result is a `claude_session.finished` event.

## Send it a message: while it works, or later

Whenever the user has something for Claude ("tell Claude to use Postgres", an answer to its
question), send it to the session, whether it is working or not:

```
emit_event(type="claude_session.requested", payload={
  "resume": "<session_id>", "task": "The user says: use Postgres, not SQLite."})
```

- **While it works**, Claude reads the message at its next step and carries on with it in mind.
- **To stop it and do something else**, add `"interrupt": true`: its current work stops and it
  starts on your message.
- **After a turn**, the message starts the next turn. Claude has the whole conversation and its
  working directory, also after the session unloaded (30 minutes idle) or days later: the same
  event loads it again.

When Claude needs a decision, it asks in its answer and waits; relay the question and send the
user's answer the same way. Pass messages on faithfully and say they come from the user.

## Failures

- **`failed:` with a login error:** the token is wrong or expired. Ask for a new one (setup
  above) with `fill`.
- **A usage limit:** the user's subscription limits apply (shared with their own Claude use).
  Tell them when it should reset, if Claude said, and offer to continue then (`set_timer`).
- **"stopped at the run's limit":** a run lasts at most about 4 hours. Send a message ("continue")
  to go on; a message that arrived just then comes back to you to send again.
- **`stopped without reporting`:** a timer the job set fires when its run was killed (out of
  memory, a restart). Tell the user; a message to the session continues from its last saved step.

## Limits

- **No permission prompts.** Claude runs every command it chooses. The container is the boundary:
  it holds only `/data` (Claude's home and the session directories), the network, and the
  tokens. It cannot reach the user's machine or other containers.
- **Grant only what a task needs.** Every session gets the job's secret groups. Claude is told to
  keep tokens out of what it writes, and the job removes them from messages, but Claude can read
  them: give it only tokens limited to what it may touch.
- **Usage:** runs use the user's subscription, not botler's budget. Do not start sessions the
  user did not ask for, and do not loop on messages to a session.
