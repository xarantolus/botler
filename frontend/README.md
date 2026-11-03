# botler web app

Vue 3 + TypeScript + [Nuxt UI](https://ui.nuxt.com/) v4, used as a plain Vue + Vite library (no
Nuxt), built with Vite. The hub serves it at `/`; the old single-file page stays at `/legacy`.

## Layout

```
src/generated/         TypeScript types + typed route table, GENERATED from the Rust API types. Do not edit.
src/api/               the typed client: call("session", { params: { instance } }) → SessionView
src/live.ts            the /api/v1/live WebSocket (reconnects with backoff; polling while it is down)
src/store.ts           app data (one reactive object) and refreshes
src/shell.ts           UI state of the shell (sidebar, new-chat modal)
src/notify.ts          toasts (usable outside components); src/confirm.ts: confirmation modal
src/lib/               pure logic with unit tests: the transcript model, envelopes, change summaries,
                       phase badges, tools, statuses, files
src/components/shell/  dashboard shell: sidebar, chat list, hub menu, command palette, login
src/components/chat/   the chat: title bar, banners, transcript, message parts, tool calls, composer
src/components/common/ cards and pieces used across views (dialog, envelope, change summary, ...)
src/views/             one per section: chat, events, requests, runs, incidents, proposals, secrets, memory
tests/                 component tests (the real kit in happy-dom)
dist/                  the built app (not committed; see below)
```

## The UI kit

Everything visible is built from Nuxt UI components: the dashboard shell (`UDashboardGroup`,
`UDashboardSidebar`, `UDashboardPanel`, `UDashboardNavbar`, `UDashboardSearch` as the Cmd/Ctrl+K
palette), the chat (`UChatMessages`, `UChatMessage`, `UChatReasoning`, `UChatTool`, `UChatShimmer`,
`UChatPrompt`, `UChatPromptSubmit`) and everything else (`UTable`, `USlideover`, `UModal` through
`useOverlay`, toasts through `useToast`, `UFileUpload`, `UStepper`, `UTree`, `UAlert`, `UCard`, ...).
Don't add a second kit or hand-rolled widgets where the kit has one.

- **Transcripts are AI SDK `UIMessage`s.** `src/lib/transcript.ts` maps a session (plus the outbox and
  what is streaming in) to the messages the chat components render; it is the only module that knows
  the AI SDK's types, and `ai` is a type-only dev dependency (nothing of it is bundled).
- **Offline.** The hub may run without internet. Icons come from the bundled Lucide set
  (`@iconify-json/lucide`; only the icons the sources name are bundled), and `@iconify/vue` is aliased
  to its offline build (`src/lib/iconify-offline.ts`), so nothing is ever fetched; no web fonts.
  `scripts/check-offline.mjs` (part of `build` and `check-dist`) fails on any external URL in `dist/`,
  and `src/lib/icons.test.ts` on an icon name that is not in the set.
- **Markdown** from agents is rendered with `marked` and sanitized with DOMPurify (`src/lib/markdown.ts`),
  the streaming tail included.
- **Generated files.** Nuxt UI's Vite plugin writes `components.d.ts` (types of the auto-imported `U*`
  components; committed, so `npm run typecheck` works before a build) and its theme templates under
  `node_modules/.nuxt-ui/`. The app's own components and all composables are imported explicitly.
- Theme: primary `indigo`, neutral `zinc` (`vite.config.ts`); color mode follows the system until the
  user picks one.

## API types are generated

Every JSON shape the app reads is a Rust type (API responses in
`crates/botler-hub/src/wire.rs`, WebSocket messages in `live.rs`, built-in event payloads in
`crates/botler-core/src/payloads.rs`, plus the store/core records they embed). `ts-rs` turns
them into `src/generated/`, together with `routes.ts`, the route table the client is typed
against. After changing any of them:

```sh
BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings   # regenerate src/generated/
```

`cargo test` fails when `src/generated/` is stale.

## Building `dist/`

The hub embeds `dist/` at compile time (`include_dir`), so the `botler` binary stays a single
self-contained file. `dist/` is not committed: the hub's `build.rs` runs `npm ci` (once) and
`npm run build` whenever `dist/` is missing or older than the sources, so building the hub needs
Node 22+. By hand:

```sh
npm ci
npm run build        # vue-tsc, vite build, offline check, then records the source hash in dist/.source-hash
npm run check-dist   # fails if dist/ is older than the sources or references external URLs
npm test             # vitest: unit tests (src/**/*.test.ts) and component tests (tests/)
npm run typecheck
npm run e2e          # end-to-end smoke test (local only: needs Podman and a built botler binary)
```

`npm run e2e` (`e2e/smoke.mjs`) runs a throwaway hub with real agents on port 7437, its models
pointed at a scripted stub in the same file, and drives the app in headless Chromium: sign in,
send, answer a question inline, reject a proposal, and run a job from the System view when there
is one. It cleans up after itself; the first run builds the agent image. Its header lists the
environment variables (Chromium binary, screenshots, where the data directory goes).

`npm run build` writes a hash of the sources (`src/`, `public/`, `index.html`, the package and
config files; tests excluded) to `dist/.source-hash`. `cargo test` recomputes it
(`web::tests::embedded_frontend_is_current`) and fails when they differ; `build.rs` uses the same
check to decide whether to rebuild. The build reads nothing else: Tailwind scans only `src/` and
`index.html` (never `dist/`, never tests), and the icon scan the same sources, so rebuilding
unchanged sources gives an identical `dist/`.

Dependencies are pinned to exact versions and `package-lock.json` is committed. Node 22+.

## Developing

```sh
BOTLER_DEV_HUB=http://127.0.0.1:7431 npm run dev
```

proxies `/api` (including the live WebSocket) to that hub. Log in with a token from
`botler token`.
