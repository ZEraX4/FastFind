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
  },
  test: {
    root: ".",
    include: ["ui/tests/**/*.test.ts"],
    environment: "jsdom",
  },
} as any);
