import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app";
import { FakeApi } from "./fake";

const flush = (ms = 0) => vi.advanceTimersByTimeAsync(ms);

describe("OCR setup feedback", () => {
  let api: FakeApi;
  let app: App;

  async function openIndexingSettings(): Promise<HTMLDialogElement> {
    document.dispatchEvent(new KeyboardEvent("keydown", { key: ",", ctrlKey: true, metaKey: true, bubbles: true }));
    await flush();
    const dlg = document.querySelector("dialog")!;
    ([...dlg.querySelectorAll(".tab")].find((t) => t.textContent === "Indexing") as HTMLButtonElement).click();
    await flush(400);
    return dlg;
  }

  const field = (dlg: HTMLElement, label: string) =>
    [...dlg.querySelectorAll("label")].find((l) => l.textContent === label)!.closest(".field")!.querySelector("input") as HTMLInputElement;

  beforeEach(async () => {
    vi.useFakeTimers();
    api = new FakeApi();
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    await flush();
  });

  afterEach(() => {
    app.closeAllDialogs();
    app.destroy();
    vi.useRealTimers();
  });

  it("checks Tesseract when OCR is switched on and shows it is ready", async () => {
    const dlg = await openIndexingSettings();
    const status = dlg.querySelector(".ocr-status") as HTMLElement;
    expect(status.hidden).toBe(true);
    expect(api.ocrChecks).toHaveLength(0);

    const box = field(dlg, "Recognise text in scanned PDFs");
    box.click();
    await flush(400);
    expect(api.ocrChecks.at(-1)!.enabled).toBe(true);
    expect(status.hidden).toBe(false);
    expect(status.classList.contains("ok")).toBe(true);
    expect(status.textContent).toContain("Tesseract 5.5.0 is ready. Installed languages: eng.");
  });

  it("warns about a missing language and re-checks the edited, unsaved value", async () => {
    api.cfg.indexing.ocr.enabled = true;
    api.ocrSetup = { tesseract: "/usr/bin/tesseract", version: "5.5.0", languages: ["eng"], missingLanguages: ["deu"], problem: "Tesseract has no data for language “deu”. Installed: eng." };
    app.destroy();
    document.body.innerHTML = '<div id="app"></div>';
    app = new App(document.getElementById("app")!, api);
    await app.init();
    const dlg = await openIndexingSettings();
    const langs = field(dlg, "OCR languages");
    langs.value = "eng+deu";
    langs.dispatchEvent(new Event("change"));
    await flush(400);
    expect(api.ocrChecks.at(-1)!.languages).toBe("eng+deu");
    const status = dlg.querySelector(".ocr-status") as HTMLElement;
    expect(status.classList.contains("warn")).toBe(true);
    expect(status.textContent).toContain("no data for language “deu”");
    expect(status.textContent).toContain("nothing is marked as failed");
    expect(api.saved).toHaveLength(0);
  });

  it("explains in the index panel why scanned files are waiting", async () => {
    api.needsOcr = 12;
    api.ocrProblem = "Tesseract was not found.";
    await app.refreshStatus();
    expect((document.querySelector(".status-pill") as HTMLElement).title).toContain("12 need OCR (waiting: Tesseract was not found.)");
    (document.querySelector(".status-pill") as HTMLButtonElement).click();
    await flush();
    const note = document.querySelector("dialog .ocr-status.warn")!;
    expect(note.textContent).toContain("12 scanned files are waiting for OCR. Tesseract was not found.");
  });

  it("shows no OCR warning when OCR can run", async () => {
    api.needsOcr = 3;
    await app.refreshStatus();
    (document.querySelector(".status-pill") as HTMLButtonElement).click();
    await flush();
    expect(document.querySelector("dialog .ocr-status")).toBeNull();
  });
});
