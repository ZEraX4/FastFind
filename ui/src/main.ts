import "./styles.css";
import { tauriApi, type Api } from "./api";
import { App } from "./app";

async function start() {
  const root = document.getElementById("app")!;
  try {
    // Inside the desktop app Tauri injects its IPC bridge; in a plain browser (npm run dev)
    // the UI runs against demo data instead.
    const inTauri = "__TAURI_INTERNALS__" in window;
    const api: Api = inTauri ? await tauriApi() : (await import("./demo")).demoApi();
    await new App(root, api).init();
  } catch (e) {
    root.textContent = `FastFind failed to start: ${e}`;
  }
}

// Block the webview's own find/reload/print shortcuts that make no sense here.
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && ["r", "p", "g"].includes(e.key.toLowerCase())) e.preventDefault();
  if (e.key === "F5") e.preventDefault();
});
document.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement;
  if (!t.closest("input, textarea, .pv-text, .snippet")) e.preventDefault();
});

void start();
