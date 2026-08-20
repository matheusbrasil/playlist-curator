import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri sets this when serving to a physical device on the LAN.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],

  // Tauri prints its own build output; wiping the screen hides it.
  clearScreen: false,

  server: {
    port: 1420,
    strictPort: true,
    host: host ?? false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      // Rust rebuilds are driven by cargo, not by Vite.
      ignored: ["**/src-tauri/**", "**/core/**", "**/target/**"],
    },
  },

  envPrefix: ["VITE_", "TAURI_ENV_"],

  build: {
    // The webview versions Tauri ships against, not the browsers on this host.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: process.env.TAURI_ENV_DEBUG ? false : "esbuild",
    sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
  },
});
