import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Dev server proxies the WebSocket to the hub so the origin check passes and
// no CORS/token-in-URL concerns arise (SPEC §9).
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/ws": {
        target: "ws://127.0.0.1:8787",
        ws: true,
      },
    },
  },
  build: {
    outDir: "../hub/web-dist",
    emptyOutDir: true,
  },
});
