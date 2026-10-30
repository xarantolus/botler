import { defineConfig } from "vitest/config";
import vue from "@vitejs/plugin-vue";

// `npm run dev` proxies the API (and the live WebSocket) to a hub on BOTLER_DEV_HUB.
const hub = process.env.BOTLER_DEV_HUB ?? "http://127.0.0.1:7420";

export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Naive UI is one large chunk; it is embedded in the binary and cached forever by hash.
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
    include: ["src/**/*.test.ts"],
  },
});
