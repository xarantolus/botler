---
name: network-shares
description: Mount a network share (SMB/Windows/NAS, SFTP, WebDAV/Nextcloud) into a chat, job or service. Use when the user wants files on a NAS, server or cloud drive read or written.
---

# Network shares

A share is a directory in your container (e.g. `/mnt/nas-photos`), mounted with rclone. It is
defined once in `mounts/<name>.toml` and then granted to chats. See `mounts/README.md`.

## 1. Credentials: a secret group whose names end in the rclone option

For an SMB share (a NAS, a Windows share), ask the user for exactly these fields:

```
request_secrets(
  group="nas",
  description="Login for the NAS at home (SMB)",
  reason="To mount the photos share",
  add=[
    {name: "NAS_HOST", description: "The NAS's host name or IP, e.g. nas.local or 192.168.1.20"},
    {name: "NAS_USER", description: "Your NAS user name"},
    {name: "NAS_PASS", description: "Its password (entered here; it never reaches the chat)"},
  ],
)
```

SFTP uses `_HOST`, `_USER`, `_PASS` (or `_KEY_PEM`), optionally `_PORT`. WebDAV uses `_URL`
(e.g. `https://cloud.example.com/remote.php/dav/files/<user>/`), `_USER`, `_PASS`, and optionally
`_VENDOR` (`nextcloud`, `owncloud`, `sharepoint`, `other`). Never put a credential in a file in
the repo.

## 2. The definition

```toml
# mounts/nas-photos.toml
version = 1
type = "smb"
secrets = "nas"
remote_path = "photos"        # the SMB share name, then optionally a folder
path = "/mnt/nas-photos"
read_only = true              # false only if writing is part of the task
```

Run `gate/run`, commit and `propose_change`. A new share needs the user's approval.

## 3. Getting it into a chat

- Yourself: `request_access(resource="mount:nas-photos", reason="...", mode="ro")`. Use `mode="rw"`
  only when you must write. You are restarted with the share mounted.
- A chat you spawn: `spawn_agent(definition=..., task=..., env={mounts: ["nas-photos"]})`.
- A job or service: `mounts = { nas-photos = "ro" }` in `job.toml`. Its image must have rclone and
  fuse3, so build it `FROM localhost/botler/agent-base:latest`.

## 4. When it fails

If you are told `mount nas-photos unavailable: <why>` (a wrong password, an unreachable host), the
directory is not there. The user already has an incident. If your task needs the share, tell the
user what failed. Ask for corrected credentials with `request_secrets(group="nas", fill=["NAS_PASS"])`
if the password was wrong. Never copy the files some other way.

A share whose server goes away while it is mounted makes reads fail with `Input/output error` and
can make a command hang. Use timeouts for big copies (`timeout 600 cp ...`).
