import { fileURLToPath } from "node:url";
import ui from "@nuxt/ui/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

// `npm run dev` proxies the API (and the live WebSocket) to a hub on BOTLER_DEV_HUB.
const hub = process.env.BOTLER_DEV_HUB ?? "http://127.0.0.1:7420";

export default defineConfig({
  plugins: [
    vue(),
    ui({
      ui: {
        colors: { primary: "indigo", neutral: "zinc" },
      },
      // The hub may run without internet: every icon the sources name is bundled at build time
      // (src/lib/iconify-offline.ts keeps the runtime from ever fetching one). Only the hashed
      // sources are scanned, so the build depends on nothing else (see scripts/source-hash.mjs).
      icon: {
        clientBundle: {
          scan: { globInclude: ["src/**/*.{vue,ts}"], globExclude: ["**/*.test.ts", "src/generated/**"] },
        },
      },
      // Nuxt UI's components (U*) are auto-imported; their types land in components.d.ts
      // (committed). The app's own components and all composables are imported explicitly.
      components: { dirs: [] },
      autoImport: false,
    }),
  ],
  resolve: {
    alias: [
      // Nuxt UI renders icons with @iconify/vue, which fetches unknown icons from the Iconify API.
      // Its offline build never does; the shim adapts it to what Nuxt UI imports.
      {
        find: /^@iconify\/vue$/,
        replacement: fileURLToPath(new URL("./src/lib/iconify-offline.ts", import.meta.url)),
      },
    ],
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // One app chunk, embedded in the binary and cached forever by its hash.
    chunkSizeWarningLimit: 2000,
  },
  server: {
    proxy: {
      "/api/v1/live": { target: hub.replace(/^http/, "ws"), ws: true },
      "/api": hub,
      "/legacy": hub,
    },
  },
  test: {
    include: ["src/**/*.test.ts", "tests/**/*.test.ts"],
  },
});
