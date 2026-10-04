// Modal dialogs: settings, index details, skipped files, help.

import type { Api, IndexStatus, Settings, SkippedFile } from "./api";
import { clear, h, icon, modLabel } from "./dom";
import { ago, bytes, num } from "./format";
import { I } from "./icons";
import type { Updates } from "./updates";

/** Open a native modal <dialog> (with a fallback for environments without showModal). */
export function modal(title: string, body: HTMLElement, footer: HTMLElement[] = [], cls = ""): HTMLDialogElement {
  const id = `dlg-${Math.random().toString(36).slice(2)}`;
  const d = h(
    "dialog",
    { class: `dialog ${cls}`, "aria-labelledby": id },
    h(
      "header",
      { class: "dlg-head" },
      h("h2", { id }, title),
      h("button", { class: "icon-btn", "aria-label": "Close", title: "Close (Esc)", onclick: () => closeDialog(d) }, icon(I.x)),
    ),
    h("div", { class: "dlg-body" }, body),
    footer.length ? h("footer", { class: "dlg-foot" }, ...footer) : null,
  );
  d.addEventListener("close", () => d.remove());
  d.addEventListener("cancel", (e) => {
    e.preventDefault();
    closeDialog(d);
  });
  document.body.appendChild(d);
  if (typeof d.showModal === "function") d.showModal();
  else d.setAttribute("open", "");
  return d;
}

export function closeDialog(d: HTMLDialogElement): void {
  if (typeof d.close === "function" && d.open) d.close();
  d.remove();
}

export function confirmDialog(title: string, message: string, confirmLabel: string, danger = false): Promise<boolean> {
  return new Promise((resolve) => {
    let done = false;
    const finish = (v: boolean) => {
      if (done) return;
      done = true;
      resolve(v);
      closeDialog(d);
    };
    const ok = h("button", { class: `btn ${danger ? "danger" : "primary"}`, onclick: () => finish(true) }, confirmLabel);
    const d = modal(title, h("p", {}, message), [h("button", { class: "btn", onclick: () => finish(false) }, "Cancel"), ok], "small");
    d.addEventListener("close", () => finish(false));
    ok.focus();
  });
}

// ------------------------------------------------------------------------------------------
// Settings
// ------------------------------------------------------------------------------------------

type Get<T> = () => T;
type Set<T> = (v: T) => void;

function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
  const id = control.id || `f-${Math.random().toString(36).slice(2)}`;
  control.id = id;
  return h("div", { class: "field" }, h("label", { for: id }, label), control, hint ? h("div", { class: "hint" }, hint) : null);
}

function check(label: string, get: Get<boolean>, set: Set<boolean>, hint?: string): HTMLElement {
  const input = h("input", { type: "checkbox" }) as HTMLInputElement;
  input.checked = get();
  input.addEventListener("change", () => set(input.checked));
  const id = `c-${Math.random().toString(36).slice(2)}`;
  input.id = id;
  return h("div", { class: "field check" }, input, h("label", { for: id }, label), hint ? h("div", { class: "hint" }, hint) : null);
}

function numberInput(label: string, min: number, max: number, get: Get<number>, set: Set<number>, hint?: string, step = 1): HTMLElement {
  const input = h("input", { type: "number", min, max, step }) as HTMLInputElement;
  input.value = String(get());
  input.addEventListener("change", () => {
    const v = Number(input.value);
    if (!Number.isNaN(v)) set(Math.min(max, Math.max(min, v)));
  });
  return field(label, input, hint);
}

function select<T extends string>(label: string, options: [T, string][], get: Get<T>, set: Set<T>, hint?: string): HTMLElement {
  const s = h("select", {}, ...options.map(([v, l]) => h("option", { value: v }, l))) as HTMLSelectElement;
  s.value = get();
  s.addEventListener("change", () => set(s.value as T));
  return field(label, s, hint);
}

function listInput(label: string, get: Get<string[]>, set: Set<string[]>, hint?: string, placeholder = ""): HTMLElement {
  const t = h("textarea", { rows: 4, placeholder, spellcheck: "false" }) as HTMLTextAreaElement;
  t.value = get().join("\n");
  t.addEventListener("change", () => set(t.value.split(/[\n,]/).map((s) => s.trim()).filter(Boolean)));
  return field(label, t, hint);
}

function textInput(label: string, get: Get<string>, set: Set<string>, hint?: string, placeholder = ""): HTMLElement {
  const i = h("input", { type: "text", placeholder, spellcheck: "false" }) as HTMLInputElement;
  i.value = get();
  i.addEventListener("change", () => set(i.value.trim()));
  return field(label, i, hint);
}

export async function openSettings(api: Api, current: Settings, onSaved: (s: Settings) => void, updates: Updates | null = null): Promise<void> {
  const s: Settings = structuredClone(current);
  const tabs: [string, () => HTMLElement][] = [
    ["Search", () => h(
      "div", {},
      select("Default search mode", [["smart", "Smart search"], ["exact", "Exact phrase"], ["regex", "Regular expression"], ["filename", "File name"]], () => s.search.defaultMode, (v) => (s.search.defaultMode = v)),
      check("Case sensitive by default", () => s.search.caseSensitive, (v) => (s.search.caseSensitive = v)),
      check("Match whole words only by default", () => s.search.wholeWord, (v) => (s.search.wholeWord = v), "Off: “invoic” also finds “invoices”. On: words must match exactly."),
      numberInput("Results loaded per page", 10, 500, () => s.search.pageSize, (v) => (s.search.pageSize = v)),
      numberInput("Maximum results per search", 100, 1_000_000, () => s.search.resultLimit, (v) => (s.search.resultLimit = v)),
      h("h3", {}, "Ranking"),
      numberInput("File-name match weight", 0, 20, () => s.search.nameBoost, (v) => (s.search.nameBoost = v), "How much more a match in the file name counts than a match in the text.", 0.5),
      numberInput("Document title/author weight", 0, 20, () => s.search.metadataBoost, (v) => (s.search.metadataBoost = v), undefined, 0.5),
      numberInput("Exact phrase weight", 1, 20, () => s.search.phraseBoost, (v) => (s.search.phraseBoost = v), undefined, 0.5),
    )],
    ["Indexing", () => h(
      "div", {},
      listInput("Excluded folders", () => s.indexing.excludedDirs, (v) => (s.indexing.excludedDirs = v), "Folder names (node_modules), patterns (*.tmp) or full paths. One per line.", "node_modules"),
      listInput("Only index these file types", () => s.indexing.includedExtensions, (v) => (s.indexing.includedExtensions = v), "Extensions without dot. Leave empty to index all supported types.", "pdf"),
      listInput("Never index these file types", () => s.indexing.excludedExtensions, (v) => (s.indexing.excludedExtensions = v), undefined, "log"),
      numberInput("Maximum file size (MB)", 1, 65536, () => s.indexing.maxFileSizeMb, (v) => (s.indexing.maxFileSizeMb = v), "Larger files are found by name only."),
      numberInput("Maximum PDF size (MB)", 1, 16384, () => s.indexing.maxPdfSizeMb, (v) => (s.indexing.maxPdfSizeMb = v)),
      numberInput("Maximum text indexed per file (MB)", 1, 512, () => s.indexing.maxIndexedTextMb, (v) => (s.indexing.maxIndexedTextMb = v)),
      check("Index file names of unsupported files", () => s.indexing.indexAllFilenames, (v) => (s.indexing.indexAllFilenames = v), "Lets filename search find images, archives, executables…"),
      check("Detect text in files with unknown extensions", () => s.indexing.detectTextFiles, (v) => (s.indexing.detectTextFiles = v)),
      check("Index hidden files and folders", () => s.indexing.indexHidden, (v) => (s.indexing.indexHidden = v)),
      check("Index system files", () => s.indexing.indexSystem, (v) => (s.indexing.indexSystem = v)),
      check("Follow symbolic links", () => s.indexing.followSymlinks, (v) => (s.indexing.followSymlinks = v), "Loops are detected and skipped."),
      check("Watch folders for changes", () => s.indexing.watchChanges, (v) => (s.indexing.watchChanges = v)),
      numberInput("Full re-check interval (minutes, 0 = never)", 0, 10080, () => s.indexing.rescanIntervalMin, (v) => (s.indexing.rescanIntervalMin = v), "Catches changes that watchers miss, e.g. on network drives."),
      check("Skip unchanged files by content hash", () => s.indexing.contentHash, (v) => (s.indexing.contentHash = v)),
      h("h3", {}, "OCR (scanned documents)"),
      check("Recognise text in scanned PDFs", () => s.indexing.ocr.enabled, (v) => (s.indexing.ocr.enabled = v), "Uses a locally installed Tesseract. Runs in the background only when indexing is idle."),
      check("Also recognise text in images", () => s.indexing.ocr.images, (v) => (s.indexing.ocr.images = v)),
      textInput("OCR languages", () => s.indexing.ocr.languages, (v) => (s.indexing.ocr.languages = v), "Tesseract language codes, e.g. eng or eng+deu.", "eng"),
      textInput("Tesseract executable", () => s.indexing.ocr.tesseractPath, (v) => (s.indexing.ocr.tesseractPath = v), "Leave empty to detect automatically.", "auto"),
    )],
    ["Performance", () => h(
      "div", {},
      select("CPU usage while indexing", [["low", "Low — keep the computer responsive"], ["balanced", "Balanced (recommended)"], ["high", "High — index as fast as possible"]], () => s.performance.cpu, (v) => (s.performance.cpu = v)),
      numberInput("Parser threads (0 = automatic)", 0, 128, () => s.indexing.workerCount, (v) => (s.indexing.workerCount = v), "Automatic tunes for cores, memory and SSD/HDD."),
      numberInput("Memory for index buffers and caches (MB)", 64, 8192, () => s.performance.memoryCacheMb, (v) => (s.performance.memoryCacheMb = v), "Changes to the index buffer apply after restarting FastFind."),
      select("Background indexing", [["automatic", "Automatic — index changes as they happen"], ["manual", "Manual — only when I click Update"], ["paused", "Paused"]], () => s.performance.background, (v) => (s.performance.background = v)),
      numberInput("Per-file time limit (seconds)", 5, 3600, () => s.indexing.parseTimeoutSecs, (v) => (s.indexing.parseTimeoutSecs = v)),
    )],
    ["Appearance", () => h(
      "div", {},
      select("Theme", [["system", "Match system"], ["light", "Light"], ["dark", "Dark"]], () => s.appearance.theme, (v) => (s.appearance.theme = v)),
      check("High contrast", () => s.appearance.highContrast, (v) => (s.appearance.highContrast = v)),
      numberInput("Text size (%)", 80, 160, () => Math.round(s.appearance.fontScale * 100), (v) => (s.appearance.fontScale = v / 100), `Also ${modLabel}+Plus / ${modLabel}+Minus / ${modLabel}+0.`, 10),
      check("Show preview panel", () => s.appearance.showPreview, (v) => (s.appearance.showPreview = v)),
    )],
    ["Privacy", () => h(
      "div", { class: "privacy" },
      h("p", { class: "lead" }, icon(I.shield), "FastFind works entirely on this computer."),
      h("ul", {},
        h("li", {}, "Your files and their contents are never uploaded or sent anywhere."),
        h("li", {}, "There is no telemetry, analytics or crash reporting."),
        h("li", {}, "The index is stored only in FastFind's data folder on this computer."),
        h("li", {}, "Log files contain file paths and error messages, never document text."),
        h("li", {}, "OCR (if enabled) runs a local Tesseract program; nothing leaves the machine."),
        h("li", {}, "The only network request FastFind makes is the update check, and only if you allow it (Settings → Updates). It asks GitHub whether a newer version exists; nothing about your files is sent."),
      ),
    )],
    ...(updates ? [["Updates", () => updatesPanel(api, s, updates)] as [string, () => HTMLElement]] : []),
    ["Diagnostics", () => diagnosticsPanel(api)],
  ];
  const tabList = h("div", { class: "tabs", role: "tablist" });
  const panel = h("div", { class: "tab-panel", role: "tabpanel" });
  const buttons = tabs.map(([name, build], i) => {
    const b = h("button", { class: "tab", role: "tab", "aria-selected": i === 0 ? "true" : "false", onclick: () => activate(i) }, name);
    tabList.appendChild(b);
    return { b, build };
  });
  function activate(i: number) {
    buttons.forEach((x, j) => x.b.setAttribute("aria-selected", i === j ? "true" : "false"));
    clear(panel);
    panel.appendChild(buttons[i].build());
  }
  tabList.addEventListener("keydown", (e) => {
    const cur = buttons.findIndex((x) => x.b.getAttribute("aria-selected") === "true");
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      const n = (cur + (e.key === "ArrowRight" ? 1 : buttons.length - 1)) % buttons.length;
      activate(n);
      buttons[n].b.focus();
    }
  });
  activate(0);
  const status = h("span", { class: "dlg-status", role: "status" });
  const save = h("button", { class: "btn primary" }, "Save");
  const d = modal("Settings", h("div", { class: "settings" }, tabList, panel), [status, h("button", { class: "btn", onclick: () => closeDialog(d) }, "Cancel"), save], "wide");
  save.addEventListener("click", async () => {
    // Commit a focused field whose change event has not fired yet.
    (document.activeElement as HTMLElement | null)?.blur?.();
    save.setAttribute("disabled", "");
    try {
      const saved = await api.saveSettings(s);
      onSaved(saved);
      closeDialog(d);
    } catch (e) {
      status.textContent = String(e);
      save.removeAttribute("disabled");
    }
  });
}

function updatesPanel(api: Api, s: Settings, updates: Updates): HTMLElement {
  const version = h("p", { class: "muted" }, "Installed version: …");
  void api.diagnostics().then((d) => (version.textContent = `Installed version: ${d.version}`)).catch(() => {});
  const result = h("span", { class: "muted small", role: "status" });
  const checkBtn = h("button", { class: "btn" }, icon(I.refresh), "Check now") as HTMLButtonElement;
  checkBtn.addEventListener("click", async () => {
    checkBtn.setAttribute("disabled", "");
    result.textContent = "Checking…";
    try {
      const info = await api.updates!.check();
      result.textContent = info ? `FastFind ${info.version} is available — see the banner in the main window.` : "FastFind is up to date.";
      if (info) updates.show(info);
    } catch (e) {
      result.textContent = String(e);
    } finally {
      checkBtn.removeAttribute("disabled");
    }
  });
  return h(
    "div", {},
    version,
    check(
      "Check for updates automatically",
      () => s.updates.checkAutomatically === true,
      (v) => (s.updates.checkAutomatically = v),
      "Once a day FastFind asks GitHub whether a newer version exists. Nothing about your files is sent. Updates are only installed when you choose to, and every download is verified with FastFind's signing key.",
    ),
    h("div", { class: "row-actions" }, checkBtn, result),
  );
}

function diagnosticsPanel(api: Api): HTMLElement {
  const el = h("div", { class: "diag" }, "Loading…");
  void api.diagnostics().then((d) => {
    clear(el);
    const rows: [string, string][] = [
      ["Version", d.version],
      ["Memory in use", bytes(d.memoryRssBytes)],
      ["Parser threads", String(d.workerThreads)],
      ["Storage", d.storageRotational === null ? "unknown" : d.storageRotational ? "Hard disk (reduced parallel I/O)" : "SSD"],
      ["PDF engine", d.pdfEngine],
      ["OCR engine", d.ocrEngine ?? "Tesseract not found"],
      ["Indexed documents", num(d.indexDocs)],
      ["Index segments", String(d.indexSegments)],
      ["Query cache", `${num(d.queryCacheEntries)} entries · ${num(d.queryCacheHits)} hits / ${num(d.queryCacheMisses)} misses`],
      ["Data folder", d.dataDir],
    ];
    el.appendChild(h("dl", {}, ...rows.flatMap(([k, v]) => [h("dt", {}, k), h("dd", {}, v)])));
    el.appendChild(h("button", { class: "btn", onclick: () => void api.openLogs() }, "Open log folder"));
  });
  return el;
}

// ------------------------------------------------------------------------------------------
// Index details
// ------------------------------------------------------------------------------------------

export function openIndexPanel(api: Api, st: IndexStatus, actions: { refresh(): void; showProblems(): void; addFolder(): void }): void {
  const p = st.progress;
  const problems = st.skipped + st.failed + st.encrypted;
  const summary = h(
    "div", { class: "stats" },
    stat("Indexed files", num(st.filesTotal)),
    stat("Successful", num(st.indexed + st.nameOnly)),
    stat("Skipped / failed", num(problems)),
    stat("Need OCR", num(st.needsOcr)),
    stat("Index size", bytes(st.indexBytes + st.textStoreBytes)),
    stat("Last updated", ago(st.lastUpdated)),
  );
  const roots = h("ul", { class: "root-list" });
  for (const r of st.roots) {
    roots.appendChild(h(
      "li", {},
      icon(I.folder),
      h("div", { class: "grow" },
        h("div", { class: "rl-path" }, r.path),
        h("div", { class: "rl-meta" }, `${num(r.fileCount)} files · checked ${ago(r.lastScanAt)} · ${!r.available ? "folder unavailable" : r.scanning ? "scanning…" : r.watchStatus || "not watched"}`)),
      h("button", { class: "btn small", title: "Check this folder for changes now", onclick: () => void api.rescan(r.id).then(actions.refresh) }, "Rescan"),
      h("button", { class: "btn small danger", title: "Stop indexing this folder and remove it from the index", onclick: async () => {
        if (await confirmDialog("Remove folder", `Remove “${r.path}” and its ${num(r.fileCount)} files from the index? The files themselves are not touched.`, "Remove", true)) {
          await api.removeRoot(r.id);
          closeDialog(d);
          actions.refresh();
        }
      } }, "Remove"),
    ));
  }
  const pause = h("button", { class: "btn", onclick: async () => {
    await api.setPaused(!p.paused);
    closeDialog(d);
    actions.refresh();
  } }, icon(p.paused ? I.play : I.pause), p.paused ? "Resume indexing" : "Pause indexing");
  const body = h(
    "div", { class: "index-panel" },
    summary,
    p.active ? h("p", { class: "muted" }, `Indexing: ${num(p.processed)} processed, ${num(p.queued)} queued${p.filesPerSec > 0 ? ` · ${Math.round(p.filesPerSec)} files/s` : ""}`) : null,
    h("h3", {}, "Folders"),
    st.roots.length ? roots : h("p", { class: "muted" }, "No folders yet."),
  );
  const d = modal("Index", body, [
    h("button", { class: "btn", onclick: () => { closeDialog(d); actions.addFolder(); } }, icon(I.folderPlus), "Add folder"),
    h("button", { class: "btn", disabled: problems + st.needsOcr === 0, onclick: () => { closeDialog(d); actions.showProblems(); } }, "View skipped files"),
    pause,
    h("button", { class: "btn", onclick: () => void api.rescan().then(() => { closeDialog(d); actions.refresh(); }) }, icon(I.refresh), "Update now"),
    h("button", { class: "btn danger", onclick: async () => {
      if (await confirmDialog("Rebuild index", "Delete the whole index and index all folders again from scratch? Search keeps working for files that are already re-indexed.", "Rebuild", true)) {
        await api.rebuild();
        closeDialog(d);
        actions.refresh();
      }
    } }, "Rebuild…"),
  ], "wide");
}

function stat(label: string, value: string): HTMLElement {
  return h("div", { class: "stat" }, h("div", { class: "stat-v" }, value), h("div", { class: "stat-l" }, label));
}

// ------------------------------------------------------------------------------------------
// Skipped files
// ------------------------------------------------------------------------------------------

export function openProblems(api: Api): void {
  const PAGE = 100;
  let offset = 0;
  const statusSel = h("select", { "aria-label": "Status" },
    h("option", { value: "" }, "All problems"),
    h("option", { value: "failed" }, "Failed (damaged or unreadable)"),
    h("option", { value: "skipped" }, "Skipped (too large or excluded)"),
    h("option", { value: "encrypted" }, "Password protected"),
    h("option", { value: "needsOcr" }, "Scanned (OCR needed)"),
  ) as HTMLSelectElement;
  const q = h("input", { type: "search", placeholder: "Filter by path…", "aria-label": "Filter by path" }) as HTMLInputElement;
  const list = h("table", { class: "problems" }, h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", {}, "Status"), h("th", {}, "Reason"), h("th", {}, "Size"))));
  const tbody = h("tbody");
  list.appendChild(tbody);
  const more = h("button", { class: "btn", hidden: true }, "Load more");
  const count = h("span", { class: "muted", role: "status" });
  const labels: Record<string, string> = { failed: "Failed", skipped: "Skipped", encrypted: "Locked", needsOcr: "Needs OCR" };
  async function load(reset: boolean) {
    if (reset) {
      offset = 0;
      clear(tbody);
    }
    const page = await api.problems(statusSel.value || null, q.value, offset, PAGE);
    for (const f of page.items as SkippedFile[]) {
      tbody.appendChild(h("tr", {},
        h("td", { class: "p-path", title: f.path }, h("button", { class: "linklike", title: "Show in folder", onclick: () => void api.revealFile(f.path) }, f.path)),
        h("td", {}, labels[f.status] ?? f.status),
        h("td", { class: "p-reason" }, f.reason ?? ""),
        h("td", {}, bytes(f.size)),
      ));
    }
    offset += page.items.length;
    count.textContent = `${num(page.total)} files`;
    more.hidden = offset >= page.total;
  }
  statusSel.addEventListener("change", () => void load(true));
  let t: ReturnType<typeof setTimeout>;
  q.addEventListener("input", () => {
    clearTimeout(t);
    t = setTimeout(() => void load(true), 200);
  });
  more.addEventListener("click", () => void load(false));
  modal("Skipped files", h("div", { class: "problems-panel" }, h("div", { class: "toolbar" }, statusSel, q, count), h("div", { class: "table-wrap" }, list), more), [], "wide");
  void load(true);
}

// ------------------------------------------------------------------------------------------
// Help
// ------------------------------------------------------------------------------------------

export function openHelp(): void {
  const syntax: [string, string][] = [
    ["invoice", "Documents containing words starting with “invoice” (invoices, invoiced…)"],
    ["\"annual report\"", "Exact phrase"],
    ["invoice customer", "Both words (AND is implied)"],
    ["invoice AND customer", "Both words"],
    ["invoice OR receipt", "Either word (also |)"],
    ["invoice -draft", "Exclude a word (also NOT draft)"],
    ["(invoice OR receipt) 2026", "Group with parentheses"],
    ["invoi*", "Explicit prefix"],
    ["filename:report", "File name contains “report” (also name:*.pdf)"],
    ["ext:pdf  ext:docx,xlsx", "Only these extensions"],
    ["type:spreadsheet", "pdf, word, spreadsheet, presentation, text, code, data, web, ebook, image"],
    ["path:Projects", "Folder path contains the word"],
    ["in:\"C:\\Work\\2026\"", "Only inside this folder"],
    ["modified:2026-01-01", "Changed on that day; also >2026-01, <=2025, 2026-01..2026-03, today, yesterday, 7d, 4w"],
    ["size:>10mb", "Also size:<100kb, size:1mb..5mb"],
  ];
  const keys: [string, string][] = [
    [`${modLabel}+K / ${modLabel}+F`, "Focus the search box"],
    ["Enter", "Search now; again to open the selected file"],
    ["Esc", "Clear the search"],
    ["↑ / ↓, Page Up / Page Down", "Move through results"],
    [`${modLabel}+Enter`, "Show the selected file in its folder"],
    [`${modLabel}+Shift+C`, "Copy the selected file's path"],
    ["F3 / Shift+F3", "Next / previous match in the preview"],
    [`${modLabel}+1 … ${modLabel}+4`, "Smart, Exact, Regex, File name mode"],
    [`${modLabel}+O`, "Add a folder"],
    [`${modLabel}+Shift+F`, "Show or hide filters"],
    [`${modLabel}+,`, "Settings"],
    [`${modLabel}+Plus / Minus / 0`, "Larger / smaller / default text"],
    ["F1", "This help"],
  ];
  const table = (rows: [string, string][]) => h("table", { class: "help-table" }, h("tbody", {}, ...rows.map(([a, b]) => h("tr", {}, h("td", {}, h("code", {}, a)), h("td", {}, b)))));
  modal("Help", h(
    "div", { class: "help" },
    h("h3", {}, "Search modes"),
    h("ul", {},
      h("li", {}, h("strong", {}, "Smart"), " — words, phrases, operators and filters below. Fastest."),
      h("li", {}, h("strong", {}, "Exact"), " — the text exactly as typed, including punctuation (\"foo.bar()\")."),
      h("li", {}, h("strong", {}, "Regex"), " — regular expressions such as invoice-[0-9]{4}. Scans document text, so it is slower on large indexes; add filters to speed it up."),
      h("li", {}, h("strong", {}, "File name"), " — match file names only; * and ? wildcards are supported."),
    ),
    h("h3", {}, "Search syntax"),
    table(syntax),
    h("h3", {}, "Keyboard shortcuts"),
    table(keys),
  ), [], "wide");
}
