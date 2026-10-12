import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app";
import { FakeApi } from "./fake";

const flush = (ms = 0) => vi.advanceTimersByTimeAsync(ms);

describe("Index panel", () => {
  let api: FakeApi;
  let app: App;

  const panel = () => document.querySelector("dialog .index-panel") as HTMLElement | null;
  const stat = (label: string) =>
    [...panel()!.querySelectorAll(".stat")].find((s) => s.querySelector(".stat-l")!.textContent === label)!.querySelector(".stat-v")!.textContent;
  const button = (label: string) => [...document.querySelectorAll("dialog button")].find((b) => b.textContent?.includes(label)) as HTMLButtonElement;

  async function openPanel(): Promise<void> {
    (document.querySelector(".status-pill") as HTMLButtonElement).click();
    await flush();
    expect(panel()).not.toBeNull();
  }

  beforeEach(async () => {
    vi.useFakeTimers();
    api = new FakeApi();
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    await flush();
  });

  afterEach(() => {
    app.destroy();
    vi.useRealTimers();
  });

  it("updates when new files are indexed while it is open", async () => {
    await openPanel();
    expect(stat("Indexed files")).toBe("4");
    api.filesTotal = 7;
    api.roots = [{ ...api.roots[0], fileCount: 7 }];
    api.indexListener!(2);
    await flush();
    expect(stat("Indexed files")).toBe("7");
    expect(panel()!.querySelector(".rl-meta")!.textContent).toContain("7 files");
  });

  it("follows the regular status refresh, including folders added elsewhere", async () => {
    await openPanel();
    await api.addRoot("/more");
    api.filesTotal = 9;
    await flush(5_000);
    expect(stat("Indexed files")).toBe("9");
    expect([...panel()!.querySelectorAll(".rl-path")].map((e) => e.textContent)).toEqual(["/docs", "/more"]);
  });

  it("keeps keyboard focus on the same button across updates", async () => {
    await openPanel();
    const rescan = panel()!.querySelector('[data-focus-key="rescan-1"]') as HTMLButtonElement;
    rescan.focus();
    api.filesTotal = 5;
    await app.refreshStatus();
    const focused = document.activeElement as HTMLElement;
    expect(focused).not.toBe(rescan);
    expect(focused.dataset.focusKey).toBe("rescan-1");
  });

  it("stays open when pausing and shows the new state", async () => {
    await openPanel();
    button("Pause indexing").click();
    await flush();
    expect(panel()).not.toBeNull();
    expect(button("Resume indexing")).toBeTruthy();
  });

  it("stops updating once closed", async () => {
    await openPanel();
    document.querySelector<HTMLButtonElement>('dialog [aria-label="Close"]')!.click();
    api.filesTotal = 11;
    await app.refreshStatus();
    expect(panel()).toBeNull();
  });
});
