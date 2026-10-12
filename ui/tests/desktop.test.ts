import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app";
import { FakeApi } from "./fake";

const flush = (ms = 0) => vi.advanceTimersByTimeAsync(ms);

describe("quick search shortcut in the main window", () => {
  let api: FakeApi;
  let app: App;

  const dialog = (title: string) => [...document.querySelectorAll("dialog")].find((d) => d.querySelector("h2")?.textContent === title) as HTMLDialogElement | undefined;
  const button = (d: HTMLElement, label: string) => [...d.querySelectorAll("button")].find((b) => b.textContent === label) as HTMLButtonElement;
  async function record(rec: HTMLButtonElement, code: string, mods: Partial<KeyboardEventInit>) {
    rec.click();
    window.dispatchEvent(new KeyboardEvent("keydown", { code, key: code.replace("Key", "").toLowerCase(), bubbles: true, cancelable: true, ...mods }));
    await flush();
  }

  async function start() {
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    await flush();
  }

  beforeEach(() => {
    vi.useFakeTimers();
    api = new FakeApi();
  });

  afterEach(() => {
    app.destroy();
    vi.useRealTimers();
  });

  it("asks for another shortcut when the default one is taken, and saves the new one", async () => {
    api.shortcut = { enabled: true, shortcut: "CommandOrControl+Shift+F", registered: false, problem: "Ctrl+Shift+F is already used by another application." };
    await start();
    const d = dialog("Quick search shortcut")!;
    expect(d.textContent).toContain("Ctrl+Shift+F is already used by another application.");
    await record(d.querySelector(".shortcut-rec") as HTMLButtonElement, "Space", { ctrlKey: true, altKey: true });
    button(d, "Use this shortcut").click();
    await flush();
    expect(api.saved.at(-1)!.desktop.quickSearchShortcut).toBe("CommandOrControl+Alt+Space");
    expect(dialog("Quick search shortcut")).toBeUndefined();
  });

  it("keeps asking if the new shortcut is taken too, or turns quick search off", async () => {
    api.shortcut = { enabled: true, shortcut: "CommandOrControl+Shift+F", registered: false, problem: "Ctrl+Shift+F is already used by another application." };
    api.takenShortcuts = ["CommandOrControl+Alt+KeyK"];
    await start();
    const d = dialog("Quick search shortcut")!;
    await record(d.querySelector(".shortcut-rec") as HTMLButtonElement, "KeyK", { ctrlKey: true, altKey: true });
    button(d, "Use this shortcut").click();
    await flush();
    expect(dialog("Quick search shortcut")).toBeDefined();
    expect(d.textContent).toContain("already used by another application. Try another one.");
    button(d, "Turn off quick search").click();
    await flush();
    expect(api.saved.at(-1)!.desktop.quickSearch).toBe(false);
    expect(dialog("Quick search shortcut")).toBeUndefined();
  });

  it("doesn't ask when the shortcut works", async () => {
    await start();
    expect(dialog("Quick search shortcut")).toBeUndefined();
  });

  it("offers the shortcut and background options in Settings → General", async () => {
    await start();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: ",", ctrlKey: true, metaKey: true, bubbles: true }));
    await flush();
    const d = dialog("Settings")!;
    expect(d.querySelector(".tab")!.textContent).toBe("General");
    expect(d.textContent).toContain("Keep FastFind running when the window is closed");
    expect(d.textContent).toContain("Start FastFind when I sign in");
    api.takenShortcuts = ["CommandOrControl+Alt+KeyK"];
    await record(d.querySelector(".shortcut-rec") as HTMLButtonElement, "KeyK", { ctrlKey: true, altKey: true });
    expect(d.querySelector(".shortcut-rec")!.textContent).toBe("Ctrl+Alt+K");
    button(d, "Save").click();
    await flush();
    // Saved, but the dialog stays open to say the shortcut didn't work.
    expect(api.saved.at(-1)!.desktop.quickSearchShortcut).toBe("CommandOrControl+Alt+KeyK");
    expect(dialog("Settings")!.querySelector(".dlg-status")!.textContent).toMatch(/^Saved\. .* is already used by another application\. Choose another shortcut on the General tab\.$/);
  });

  it("runs the pop-up's query when it asks for all results", async () => {
    await start();
    api.showQueryListener!("report");
    await flush(300);
    expect((document.querySelector('input[type="search"]') as HTMLInputElement).value).toBe("report");
    expect(api.requests.at(-1)!.query).toBe("report");
  });

  it("keeps Ctrl/⌘+Shift+F for the filters inside FastFind's own window", async () => {
    await start();
    const filters = () => (document.querySelector(".filters") as HTMLElement | null)?.hidden;
    expect(filters()).toBe(true);
    api.appShortcutListener!("CommandOrControl+Shift+F");
    await flush();
    expect(filters()).toBe(false);
  });

  it("has no General tab outside the desktop app", async () => {
    api.desktop = undefined;
    await start();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: ",", ctrlKey: true, metaKey: true, bubbles: true }));
    await flush();
    expect(dialog("Settings")!.querySelector(".tab")!.textContent).toBe("Search");
  });
});
