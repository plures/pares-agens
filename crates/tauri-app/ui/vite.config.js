import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  // Allow Tauri's webview to access the app in development
  server: {
    port: 5173,
    strictPort: true,
  },
});
