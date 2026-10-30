# botler web app

Vue 3 + TypeScript + [Naive UI](https://www.naiveui.com/), built with Vite. The hub serves it at
`/`; the old single-file page stays at `/legacy`.

## Layout

```
src/generated/   TypeScript types + typed route table, GENERATED from the Rust API types. Do not edit.
src/api/         the typed client: call("session", { params: { instance } }) → SessionView
src/live.ts      the /api/v1/live WebSocket (reconnects with backoff; polling while it is down)
src/store.ts     app state (one reactive object) and refreshes
src/lib/         pure logic with unit tests: envelopes, change summaries, phase badges, files
src/components/  chat list, header, dialog cards, change summary, secret inputs, ...
src/views/       one per tab: chat, events, requests, runs, incidents, proposals, secrets
dist/            the built app, committed (see below)
```

## API types are generated

Every JSON shape the app reads is a Rust type (API responses in
`crates/botler-hub/src/wire.rs`, WebSocket messages in `live.rs`, built-in event payloads in
`crates/botler-core/src/payloads.rs`, plus the store/core records they embed). `ts-rs` turns
them into `src/generated/`, together with `routes.ts`, the route table the client is typed
against. After changing any of them:

```sh
BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings   # regenerate src/generated/
cd frontend && npm run build                                     # then rebuild dist/
```

`cargo test` fails when `src/generated/` is stale.

## Building and the committed `dist/`

The hub embeds `dist/` at compile time (`include_dir`), so the `botler` binary stays a single
self-contained file and building it needs no Node. The price is that `dist/` is committed and
must be rebuilt with the sources:

```sh
npm ci
npm run build        # vue-tsc, vite build, then records the source hash in dist/.source-hash
npm run check-dist   # fails if dist/ is older than the sources
npm test             # vitest unit tests
npm run typecheck
```

`npm run build` writes a hash of the sources (`src/`, `public/`, `index.html`, the package and
config files; tests excluded) to `dist/.source-hash`. `cargo test` recomputes it
(`web::tests::embedded_frontend_is_current`) and fails when they differ, so a stale `dist/` cannot
be committed unnoticed. Vite's output is deterministic, so rebuilding unchanged sources gives an
identical `dist/`.

Dependencies are pinned to exact versions and `package-lock.json` is committed. Node 22+.

## Developing

```sh
BOTLER_DEV_HUB=http://127.0.0.1:7431 npm run dev
```

proxies `/api` (including the live WebSocket) to that hub. Log in with a token from
`botler token`.
