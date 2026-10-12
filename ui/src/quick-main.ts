import "./styles.css";
import "./quick.css";
import { QuickSearch, type QuickApi } from "./quick";

async function tauriQuickApi(): Promise<QuickApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  return {
    search: (req) => invoke("search", { req }),
    openFile: (path) => invoke("open_file", { path }),
    revealFile: (path) => invoke("reveal_file", { path }),
    getSettings: () => invoke("get_settings"),
    focus: () => invoke("quick_focus"),
    hide: () => invoke("quick_hide"),
    resize: (height) => invoke("quick_resize", { height }),
    showAll: (query) => invoke("quick_show_all", { query }),
    onShow: (cb) => {
      void listen("quick-show", () => cb());
    },
  };
}

/** In a plain browser (npm run dev, the website demo): demo data, nothing to hide or resize. */
async function demoQuickApi(): Promise<QuickApi> {
  const { demoApi } = await import("./demo");
  const api = demoApi();
  return {
    search: (req) => api.search(req),
    openFile: async () => {},
    revealFile: async () => {},
    getSettings: () => api.getSettings(),
    focus: async () => {},
    hide: async () => {},
    resize: async () => {},
    showAll: async () => {},
    onShow: () => {},
  };
}

async function start() {
  const root = document.getElementById("quick")!;
  try {
    const api = "__TAURI_INTERNALS__" in window ? await tauriQuickApi() : await demoQuickApi();
    await new QuickSearch(root, api).init();
  } catch (e) {
    root.textContent = `Quick search failed to start: ${e}`;
  }
}

document.addEventListener("contextmenu", (e) => {
  if (!(e.target as HTMLElement).closest("input")) e.preventDefault();
});

void start();
