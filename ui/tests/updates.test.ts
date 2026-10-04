import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UpdateApi, UpdateInfo } from "../src/api";
import { App } from "../src/app";
import { FakeApi } from "./fake";

class FakeUpdates implements UpdateApi {
  checks = 0;
  installs = 0;
  releasePageOpened = 0;
  next: UpdateInfo | null = null;
  installError: string | null = null;
  available?: (i: UpdateInfo) => void;
  progressCb?: (done: number, total: number | null) => void;
  restartingCb?: (e: string) => void;
  async check() {
    this.checks++;
    return this.next;
  }
  async install() {
    this.installs++;
    if (this.installError) throw this.installError;
    await new Promise(() => {}); // success never returns: the app restarts
  }
  async openReleasePage() {
    this.releasePageOpened++;
  }
  onAvailable(cb: (i: UpdateInfo) => void) { this.available = cb; }
  onProgress(cb: (done: number, total: number | null) => void) { this.progressCb = cb; }
  onRestarting(cb: (e: string) => void) { this.restartingCb = cb; }
}

const release = (extra: Partial<UpdateInfo> = {}): UpdateInfo => ({
  version: "1.1.0", currentVersion: "1.0.0", notes: "Faster PDF indexing\n<img src=x onerror=alert(1)>", date: "2026-11-01", canInstall: true, ...extra,
});

const flush = (ms = 0) => vi.advanceTimersByTimeAsync(ms);
const banner = () => document.querySelector(".banner.update") as HTMLElement | null;
const button = (label: string) => [...document.querySelectorAll("button")].find((b) => b.textContent === label) as HTMLButtonElement | undefined;

describe("updates", () => {
  let api: FakeApi;
  let upd: FakeUpdates;
  let app: App | null = null;

  async function start(checkAutomatically: boolean | null) {
    api.cfg.updates.checkAutomatically = checkAutomatically;
    app?.destroy();
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    await flush();
  }

  beforeEach(() => {
    vi.useFakeTimers();
    api = new FakeApi();
    upd = new FakeUpdates();
    api.updates = upd;
  });

  afterEach(() => {
    app?.closeAllDialogs();
    app?.destroy();
    app = null;
    vi.useRealTimers();
  });

  it("asks once before any check, and checks right away when allowed", async () => {
    await start(null);
    expect(banner()!.hidden).toBe(false);
    expect(banner()!.textContent).toContain("Check for updates automatically?");
    expect(upd.checks).toBe(0);

    upd.next = release();
    button("Check automatically")!.click();
    await flush();
    expect(api.saved.at(-1)!.updates.checkAutomatically).toBe(true);
    expect(upd.checks).toBe(1);
    expect(banner()!.textContent).toContain("FastFind 1.1.0 is available.");
  });

  it("declining saves the choice and makes no request", async () => {
    await start(null);
    button("Don't check")!.click();
    await flush();
    expect(api.saved.at(-1)!.updates.checkAutomatically).toBe(false);
    expect(upd.checks).toBe(0);
    expect(banner()!.hidden).toBe(true);
    expect(document.querySelector(".toast")!.textContent).toContain("Settings → Updates");
  });

  it("does not ask again once answered", async () => {
    await start(false);
    expect(banner()!.hidden).toBe(true);
    await start(true);
    expect(banner()!.hidden).toBe(true);
    expect(upd.checks).toBe(0);
  });

  it("shows a background-found update with release notes as plain text", async () => {
    await start(true);
    upd.available!(release());
    expect(banner()!.textContent).toContain("You have 1.0.0.");
    button("What's new")!.click();
    const notes = document.querySelector("dialog .release-notes")!;
    expect(notes.textContent).toContain("Faster PDF indexing");
    expect(notes.querySelector("img")).toBeNull();
    expect(document.querySelector("dialog h2")!.textContent).toBe("What's new in FastFind 1.1.0");
  });

  it("installs with download progress", async () => {
    await start(true);
    upd.available!(release());
    button("Install and restart")!.click();
    await flush();
    expect(upd.installs).toBe(1);
    expect(banner()!.textContent).toContain("Downloading FastFind 1.1.0…");
    upd.progressCb!(5 * 1024 * 1024, 10 * 1024 * 1024);
    expect(banner()!.textContent).toContain("50%");
    expect((banner()!.querySelector(".bar") as HTMLElement).style.width).toBe("50%");
    // A late background notification must not replace the progress display.
    upd.available!(release());
    expect(banner()!.textContent).toContain("50%");
    upd.progressCb!(10 * 1024 * 1024, 10 * 1024 * 1024);
    expect(banner()!.textContent).toContain("FastFind will restart");
  });

  it("reports a failed install and offers it again", async () => {
    await start(true);
    upd.installError = "The update could not be downloaded or its signature is invalid";
    upd.available!(release());
    button("Install and restart")!.click();
    await flush();
    expect(document.querySelector(".toast.error")!.textContent).toContain("signature is invalid");
    expect(button("Install and restart")).toBeDefined();
  });

  it("explains a restart after the index was closed for a failed install", async () => {
    await start(true);
    upd.available!(release());
    button("Install and restart")!.click();
    await flush();
    upd.restartingCb!("installer could not start");
    expect(banner()!.textContent).toContain("FastFind is restarting");
  });

  it("offers the download page when this copy cannot update itself", async () => {
    await start(true);
    upd.available!(release({ canInstall: false, notes: null }));
    expect(button("Install and restart")).toBeUndefined();
    expect(button("What's new")).toBeUndefined();
    button("Download")!.click();
    expect(upd.releasePageOpened).toBe(1);
  });

  it("Later hides the banner", async () => {
    await start(true);
    upd.available!(release());
    button("Later")!.click();
    expect(banner()!.hidden).toBe(true);
  });

  it("has an Updates tab with the automatic-check setting and Check now", async () => {
    await start(false);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: ",", ctrlKey: true, metaKey: true, bubbles: true }));
    await flush();
    const dlg = document.querySelector("dialog")!;
    ([...dlg.querySelectorAll(".tab")].find((t) => t.textContent === "Updates") as HTMLButtonElement).click();
    await flush();
    expect(dlg.textContent).toContain("Installed version: 1.0.0");
    const box = [...dlg.querySelectorAll("label")].find((l) => l.textContent!.includes("Check for updates automatically"))!.closest(".field")!.querySelector("input") as HTMLInputElement;
    expect(box.checked).toBe(false);

    button("Check now")!.click();
    await flush();
    expect(dlg.textContent).toContain("FastFind is up to date.");
    upd.next = release();
    button("Check now")!.click();
    await flush();
    expect(dlg.textContent).toContain("FastFind 1.1.0 is available");
    expect(banner()!.textContent).toContain("FastFind 1.1.0 is available.");

    box.click();
    button("Save")!.click();
    await flush();
    expect(api.saved.at(-1)!.updates.checkAutomatically).toBe(true);
  });

  it("the browser demo has no update UI", async () => {
    api.updates = undefined;
    await start(null);
    expect(banner()).toBeNull();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: ",", ctrlKey: true, metaKey: true, bubbles: true }));
    await flush();
    expect([...document.querySelectorAll("dialog .tab")].map((t) => t.textContent)).not.toContain("Updates");
    ([...document.querySelectorAll("dialog .tab")].find((t) => t.textContent === "Privacy") as HTMLButtonElement).click();
    expect(document.querySelector("dialog")!.textContent).toContain("The only network request");
  });
});
