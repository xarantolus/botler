---
name: compose-apps
description: Deploy a self-hosted app that ships as a docker-compose file or upstream images (Mattermost, Gitea, Nextcloud, a database). Use when the user wants such an app running for them.
---

# Apps from a docker-compose file

There is no `docker compose` here: each compose service becomes one botler service
(`jobs/<name>/job.toml`, `lifetime = "service"`), so each gets the hub's supervision, restarts,
secrets and approvals. Use the upstream images as they are; write a Dockerfile only when the image
really needs changing.

| compose | job.toml |
|---|---|
| `image: postgres:16-alpine` | `from = "docker.io/library/postgres:16-alpine"` (fully qualified, a version tag or digest, never `latest`) |
| `command:` / `entrypoint:` | `command = [...]` (omit for the image's own) |
| `environment:` (plain settings) | `environment = { POSTGRES_DB = "app" }` |
| `environment:` (passwords, tokens) | a secret group: `secrets = ["app-db"]` (ask with `request_secrets`); its keys arrive as environment variables |
| `depends_on:` / reaching `db:5432` | `services = ["app-db"]` in the dependent; it reaches `http://app-db:5432` (or plain TCP) by name |
| `expose:` | `port = 5432` |
| `ports: "8080:80"` | `port = 80` plus `[[publish]] port = 80, host_port = 8080` (asks the user) |
| `volumes: data:/var/lib/x` | `volume = true` mounts one volume at `/data`: point the app there (`PGDATA = "/data/pgdata"`, a config option, or the app's data-dir variable) |
| `healthcheck: test: [...]` | `[health] heartbeat = "1m"`, `grace = "3m"`, `command = [...]` (the hub runs it in the container) |
| `restart: always` | nothing: services always restart |

Name the jobs `<app>` and `<app>-db` and so on. The volume belongs to the job's name
(`botler-job-<name>`): **renaming or removing a job with `volume = true` loses its data**, so keep
the names of anything already deployed. To move a deployed service from a Dockerfile to `from`,
keep its name and check that the image reads its data from the same place under `/data`.

The image's user must be able to write `/data` (rootless Podman: the volume is owned by the
container's root). Images that start as root and drop privileges (postgres, most official ones)
work; for others, set the app's data path to a directory the image creates for its user.

`trial_run` each service before proposing (its `/data` is empty and thrown away); start the
database first. The first deployment of each service asks the user once.
