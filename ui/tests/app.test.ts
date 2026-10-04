import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app";
import { FakeApi } from "./fake";

const flush = async (ms = 250) => {
  await vi.advanceTimersByTimeAsync(ms);
};

function input(): HTMLInputElement {
  return document.querySelector(".search-input") as HTMLInputElement;
}

async function type(text: string) {
  const el = input();
  el.value = text;
  el.dispatchEvent(new Event("input"));
  await flush();
}

function key(target: EventTarget, k: string, opts: KeyboardEventInit = {}) {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...opts }));
}

describe("FastFind UI", () => {
  let api: FakeApi;
  let app: App;

  beforeEach(async () => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="app"></div>';
    api = new FakeApi();
    app = new App(document.getElementById("app")!, api);
    await app.init();
    await flush(0);
  });

  afterEach(() => {
    app.destroy();
    vi.useRealTimers();
  });

  it("shows index status in the header", () => {
    const pill = document.querySelector(".status-pill")!.textContent!;
    expect(pill).toContain("Indexed: 4 files");
    expect(pill).toContain("Updated 3 min ago");
  });

  it("searches as you type and renders results with count and timing", async () => {
    await type("invoice");
    expect(api.requests.at(-1)!.query).toBe("invoice");
    expect(document.querySelectorAll(".row").length).toBe(1);
    expect(document.querySelector(".summary-text")!.textContent).toContain("1 result");
    expect(document.querySelector(".row .name")!.textContent).toBe("invoice-2026.txt");
  });

  it("debounces typing into a single request", async () => {
    const el = input();
    for (const t of ["i", "in", "inv", "invo"]) {
      el.value = t;
      el.dispatchEvent(new Event("input"));
      await vi.advanceTimersByTimeAsync(20);
    }
    await flush();
    expect(api.requests.map((r) => r.query)).toEqual(["invo"]);
  });

  it("loads snippets lazily and highlights matches without interpreting HTML", async () => {
    await type(".txt");
    await flush(100);
    const snip = document.querySelector(".row .snippet")!;
    expect(snip.querySelector("mark")!.textContent).toBe("invoice");
    // "<b>" from the document is shown as text, not markup.
    expect(snip.querySelector("b")).toBeNull();
    expect(snip.textContent).toContain("<b>invoice</b>");
    // A file literally named like an HTML payload is rendered as text.
    expect(document.querySelector("img")).toBeNull();
    expect(document.querySelector(".row .count")!.textContent).toBe("2 matches");
  });

  it("selects the first result and shows its preview with match navigation", async () => {
    await type("invoice");
    await flush(100);
    const pv = document.querySelector(".preview")!;
    expect(pv.querySelector(".pv-name")!.textContent).toBe("invoice-2026.txt");
    expect(pv.querySelectorAll("mark").length).toBe(3);
    expect(pv.querySelector(".pv-counter")!.textContent).toBe("1 / 3");
    key(document, "F3");
    expect(pv.querySelector(".pv-counter")!.textContent).toBe("2 / 3");
    expect(pv.querySelector("mark.current")!.getAttribute("data-match")).toBe("1");
    expect(pv.querySelector(".pv-loc")!.textContent).toBe("Line 9");
    key(document, "F3", { shiftKey: true });
    key(document, "F3", { shiftKey: true });
    expect(pv.querySelector(".pv-counter")!.textContent).toBe("3 / 3");
  });

  it("navigates results with the keyboard and opens / reveals the selection", async () => {
    await type(".");
    await flush(100);
    key(input(), "ArrowDown");
    await flush(100);
    expect(document.querySelector(".row.selected .name")!.textContent).toBe("report.pdf");
    key(input(), "Enter");
    await flush(0);
    expect(api.opened).toEqual(["/docs/report.pdf"]);
    key(document, "Enter", { ctrlKey: true, metaKey: true });
    await flush(0);
    expect(api.revealed).toEqual(["/docs/report.pdf"]);
  });

  it("Escape clears the query and results", async () => {
    await type("invoice");
    key(input(), "Escape");
    await flush();
    expect(input().value).toBe("");
    expect(document.querySelectorAll(".row").length).toBe(0);
  });

  it("switches search modes and shows the regex banner", async () => {
    await type("inv.*");
    (document.querySelector('.mode[data-mode="regex"]') as HTMLButtonElement).click();
    await flush();
    expect(api.requests.at(-1)!.mode).toBe("regex");
    expect((document.querySelector(".banner.regex") as HTMLElement).hidden).toBe(false);
    key(document, "1", { ctrlKey: true, metaKey: true });
    await flush();
    expect(api.requests.at(-1)!.mode).toBe("smart");
  });

  it("case and whole-word toggles are sent with the request", async () => {
    await type("invoice");
    (document.querySelector('.toggle[aria-label="Match case"]') as HTMLButtonElement).click();
    await flush();
    expect(api.requests.at(-1)!.caseSensitive).toBe(true);
    (document.querySelector('.toggle[aria-label="Whole words only"]') as HTMLButtonElement).click();
    await flush();
    expect(api.requests.at(-1)!.wholeWord).toBe(true);
  });

  it("applies type filters and shows the active filter count", async () => {
    await type(".");
    (document.querySelector('button[aria-expanded]') as HTMLButtonElement).click();
    const pdfChip = [...document.querySelectorAll(".chip")].find((c) => c.textContent === "PDF") as HTMLButtonElement;
    pdfChip.click();
    await flush();
    expect(api.requests.at(-1)!.filters.kinds).toEqual(["pdf"]);
    expect(document.querySelectorAll(".row").length).toBe(1);
    expect(document.querySelector('button[aria-expanded]')!.textContent).toContain("Filters (1)");
  });

  it("adds folders through the picker", async () => {
    const add = [...document.querySelectorAll(".rootsbar .btn")].find((b) => b.textContent?.includes("Add folder")) as HTMLButtonElement;
    add.click();
    await flush(0);
    expect(api.added).toEqual(["/new/folder"]);
    await flush(0);
    expect([...document.querySelectorAll(".root-chip .rc-name")].map((e) => e.textContent)).toEqual(["docs", "folder"]);
  });

  it("opens settings, edits a value and saves it", async () => {
    key(document, ",", { ctrlKey: true, metaKey: true });
    await flush(0);
    const dlg = document.querySelector("dialog")!;
    expect(dlg.querySelector("h2")!.textContent).toBe("Settings");
    const appearanceTab = [...dlg.querySelectorAll(".tab")].find((t) => t.textContent === "Appearance") as HTMLButtonElement;
    appearanceTab.click();
    const theme = dlg.querySelector("select") as HTMLSelectElement;
    theme.value = "dark";
    theme.dispatchEvent(new Event("change"));
    const save = [...dlg.querySelectorAll(".dlg-foot .btn")].find((b) => b.textContent === "Save") as HTMLButtonElement;
    save.click();
    await flush(0);
    expect(api.saved.at(-1)!.appearance.theme).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("shows help with the search syntax", () => {
    key(document, "F1");
    const dlg = document.querySelector("dialog")!;
    expect(dlg.textContent).toContain("filename:report");
    expect(dlg.textContent).toContain("modified:2026-01-01");
    app.closeAllDialogs();
  });

  it("offers a refresh when new files are indexed while results are shown", async () => {
    await type("invoice");
    api.indexListener!(5);
    await flush(0);
    expect(document.querySelector(".refresh-link")).not.toBeNull();
  });

  it("shows an onboarding state when no folders are indexed", async () => {
    api.roots = [];
    document.body.innerHTML = '<div id="app"></div>';
    app.destroy();
    const fresh = new App(document.getElementById("app")!, api);
    await fresh.init();
    expect(document.querySelector(".empty-card h2")!.textContent).toBe("Choose folders to search");
    app = fresh;
  });

  it("draws its own title bar with window controls in the frameless desktop window", async () => {
    // Browser/demo mode has no window object: no controls.
    expect(document.querySelector(".win-controls")).toBeNull();
    const calls: string[] = [];
    let maximized = false;
    let resized: (() => void) | undefined;
    api.window = {
      nativeButtons: false,
      minimize: async () => void calls.push("min"),
      toggleMaximize: async () => { calls.push("max"); maximized = !maximized; },
      close: async () => void calls.push("close"),
      isMaximized: async () => maximized,
      onResized: (cb) => { resized = cb; },
    };
    app.destroy();
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    await flush(0);

    const header = document.querySelector(".topbar") as HTMLElement;
    expect(header.hasAttribute("data-tauri-drag-region")).toBe(true);
    expect(document.querySelector(".brand")!.hasAttribute("data-tauri-drag-region")).toBe(true);
    const btn = (label: string) => header.querySelector(`.win-controls [aria-label="${label}"]`) as HTMLButtonElement;
    btn("Minimize").click();
    btn("Maximize").click();
    resized!();
    await flush(0);
    expect(btn("Restore")).not.toBeNull();
    btn("Close").click();
    expect(calls).toEqual(["min", "max", "close"]);

    // macOS keeps its native traffic lights: no custom buttons, room left for them.
    api.window = { ...api.window, nativeButtons: true };
    app.destroy();
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    expect(document.querySelector(".win-controls")).toBeNull();
    expect(document.querySelector(".topbar")!.classList.contains("native-controls")).toBe(true);
  });
});
