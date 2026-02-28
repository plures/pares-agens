import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [svelte()],
  // Prevent Vite from obscuring Rust errors.
  clearScreen: false,
  // Tauri expects a fixed port; fail if unavailable.
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // Tell Vite to ignore watching the Rust source files.
      ignored: ["**/src-tauri/**"],
    },
  },
});
