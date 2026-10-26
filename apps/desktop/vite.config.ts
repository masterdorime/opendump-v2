import { defineConfig } from 'vite'

// Tauri serves the dev server on a fixed port and builds to ../dist.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { outDir: 'dist', emptyOutDir: true }
})
