import { resolve } from "node:path";
import { defineConfig } from "vite";

// The UI lives in ./ui; Tauri serves ui/dist in production and the dev server in development.
export default defineConfig({
  root: "ui",
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2021",
    sourcemap: false,
    // Two pages: the main window, and the pop-up search window (desktop app only).
    rollupOptions: { input: { main: resolve(__dirname, "ui/index.html"), quick: resolve(__dirname, "ui/quick.html") } },
  },
  test: {
    root: ".",
    include: ["ui/tests/**/*.test.ts"],
    environment: "jsdom",
  },
} as any);
