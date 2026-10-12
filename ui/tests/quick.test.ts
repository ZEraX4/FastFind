import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SearchRequest } from "../src/api";
import { QuickSearch, type QuickApi } from "../src/quick";
import { FakeApi } from "./fake";

const flush = (ms = 0) => vi.advanceTimersByTimeAsync(ms);

describe("pop-up search", () => {
  let fake: FakeApi;
  let api: QuickApi & { hidden: number; shownAll: string[]; heights: number[]; show?: () => void; failOpen?: string };
  let qs: QuickSearch;

  const rows = () => [...document.querySelectorAll(".qs-row")].map((r) => r.querySelector(".qs-name")!.textContent);
  const selected = () => document.querySelector(".qs-row.selected .qs-name")?.textContent;
  const press = (key: string, mods: Partial<KeyboardEventInit> = {}) => qs.input.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...mods }));
  const type = async (text: string) => {
    qs.input.value = text;
    qs.input.dispatchEvent(new Event("input"));
    await flush(100);
  };

  beforeEach(async () => {
    vi.useFakeTimers();
    fake = new FakeApi();
    api = {
      search: (req: SearchRequest) => fake.search(req),
      openFile: async (p) => {
        if (api.failOpen) throw new Error(api.failOpen);
        fake.opened.push(p);
      },
      revealFile: async (p) => { fake.revealed.push(p); },
      getSettings: () => fake.getSettings(),
      focus: async () => {},
      hide: async () => { api.hidden++; },
      resize: async (h) => { api.heights.push(h); },
      showAll: async (q) => { api.shownAll.push(q); },
      onShow: (cb) => { api.show = cb; },
      hidden: 0,
      shownAll: [],
      heights: [],
    };
    document.body.innerHTML = '<div id="quick"></div>';
    qs = new QuickSearch(document.getElementById("quick")!, api);
    await qs.init();
  });

  afterEach(() => vi.useRealTimers());

  it("searches as you type with a short result list", async () => {
    await type("invoice");
    expect(rows()).toEqual(["invoice-2026.txt"]);
    const req = fake.requests.at(-1)!;
    expect(req.limit).toBe(8);
    expect(req.mode).toBe("smart");
    expect(api.heights.length).toBeGreaterThan(1);
  });

  it("opens the selected file with Enter and closes", async () => {
    await type("o");
    expect(rows().length).toBeGreaterThan(1);
    press("ArrowDown");
    const second = selected();
    press("Enter");
    await flush();
    expect(fake.opened).toEqual([fake.docs.find((d) => d.name === second)!.path]);
    expect(api.hidden).toBe(1);
  });

  it("shows the file in its folder with Ctrl/⌘+Enter, and all results with Shift+Enter", async () => {
    await type("report");
    press("Enter", { ctrlKey: true, metaKey: true });
    await flush();
    expect(fake.revealed).toEqual(["/docs/report.pdf"]);
    press("Enter", { shiftKey: true });
    await flush();
    expect(api.shownAll).toEqual(["report"]);
  });

  it("opens the top result even when Enter comes before the results", async () => {
    qs.input.value = "notes";
    qs.input.dispatchEvent(new Event("input"));
    press("Enter");
    await flush(200);
    expect(fake.opened).toEqual(["/docs/notes.md"]);
  });

  it("says when nothing matches, and keeps the window open when a file can't be opened", async () => {
    await type("zzz-nothing");
    expect(document.querySelector(".qs-note")!.textContent).toBe("No files match “zzz-nothing”.");
    await type("notes");
    api.failOpen = "The file no longer exists.";
    press("Enter");
    await flush();
    expect(document.querySelector(".qs-note")!.textContent).toContain("no longer exists");
    expect(api.hidden).toBe(0);
  });

  it("clears with the first Esc and closes with the second", async () => {
    await type("invoice");
    press("Escape");
    await flush();
    expect(qs.input.value).toBe("");
    expect(rows()).toEqual([]);
    expect(api.hidden).toBe(0);
    press("Escape");
    await flush();
    expect(api.hidden).toBe(1);
  });

  it("re-runs the last search when shown again, with the text selected for replacing", async () => {
    await type("invoice");
    const before = fake.requests.length;
    api.show!();
    await flush();
    expect(fake.requests.length).toBe(before + 1);
    expect(document.activeElement).toBe(qs.input);
  });
});
