# mounts/

Network shares (SMB, SFTP, WebDAV) that chats, jobs and services can see as a directory. Each
`mounts/<name>.toml` defines one share. The hub mounts it with rclone inside the container that
holds it, never on the host, and only that container sees it. Adding or changing a share needs the
user's approval.

```toml
# mounts/nas-photos.toml
version = 1
type = "smb"                 # smb | sftp | webdav
secrets = "nas"              # the secret group with the connection details (see below)
remote_path = "photos"       # smb: the share, then a folder in it ("photos/2024")
path = "/mnt/nas-photos"     # where it appears in the container
read_only = true             # the default; false lets the user grant "rw"
# options = { vfs_cache_max_size = "2G" }   # extra `rclone mount` flags, without the dashes
```

The secret group's secret names end in the rclone option they fill:

| type | required | optional |
|---|---|---|
| `smb` | `_HOST` | `_USER`, `_PASS`, `_PORT`, `_DOMAIN`, `_SPN` |
| `sftp` | `_HOST` | `_USER`, `_PASS`, `_PORT`, `_KEY_PEM`, `_PUBKEY`, `_KNOWN_HOSTS_FILE` |
| `webdav` | `_URL` | `_USER`, `_PASS`, `_VENDOR`, `_BEARER_TOKEN` |

For example, group `nas` with `NAS_HOST`, `NAS_USER` and `NAS_PASS`. The password is entered in
plain text; the container obscures it for rclone.

Who gets a share:

- A chat asks with `request_access(resource="mount:nas-photos", reason=..., mode="ro")`, or a
  parent passes it on with `spawn_agent(env={mounts: ["nas-photos"]})`. The user decides, unless
  `policies/grants.toml` says `"mount:nas-photos" = "auto"`. `rw` always asks.
- An agent or job definition lists it: `mounts = { nas-photos = "ro" }` in `agent.toml` or
  `job.toml` (this needs the user's approval). A job's image needs rclone and fuse3: build it
  `FROM localhost/botler/agent-base:latest`, which has both.

If a share cannot be mounted (wrong password, host unreachable), the chat still starts. It is
told `mount nas-photos unavailable: <why>` with its next message, and an incident is opened.
A job fails instead. Writes to an `rw` share are uploaded when the file is closed, and at the
latest when the container stops.
