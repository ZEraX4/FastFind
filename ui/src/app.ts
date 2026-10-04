// Application controller: layout, search flow, filters, keyboard, status polling.

import { emptyFilters, type Api, type IndexStatus, type SearchFilters, type SearchMode, type SearchRequest, type SearchResponse, type Settings, type SortOrder, type WindowControls } from "./api";
import { closeDialog, confirmDialog, openHelp, openIndexPanel, openProblems, openSettings } from "./dialogs";
import { clear, debounce, h, icon, modKey, modLabel } from "./dom";
import { ago, bytes, duration, KIND_OPTIONS, num } from "./format";
import { I } from "./icons";
import { PreviewPane } from "./preview";
import { ResultsList } from "./results";
import { Updates } from "./updates";

const MODES: [SearchMode, string, string][] = [
  ["smart", "Smart", "Words, phrases, AND/OR/NOT and filters"],
  ["exact", "Exact", "Exact text including punctuation"],
  ["regex", "Regex", "Regular expression (slower)"],
  ["filename", "File name", "Match file names only"],
];

const DATE_PRESETS: [string, string, number | null][] = [
  ["any", "Any time", null],
  ["1d", "Today", 1],
  ["7d", "Past 7 days", 7],
  ["30d", "Past 30 days", 30],
  ["365d", "Past year", 365],
  ["custom", "Custom range…", null],
];

const SIZE_PRESETS: [string, string, number | null, number | null][] = [
  ["any", "Any size", null, null],
  ["tiny", "Under 100 KB", null, 100 * 1024],
  ["small", "100 KB – 1 MB", 100 * 1024, 1 << 20],
  ["medium", "1 – 10 MB", 1 << 20, 10 << 20],
  ["large", "10 – 100 MB", 10 << 20, 100 << 20],
  ["huge", "Over 100 MB", 100 << 20, null],
];

export class App {
  private settings!: Settings;
  private status: IndexStatus | null = null;
  private mode: SearchMode = "smart";
  private caseSensitive = false;
  private wholeWord = false;
  private sort: SortOrder = "relevance";
  private filters: SearchFilters = emptyFilters();
  private datePreset = "any";
  private sizePreset = "any";

  private seq = 0;
  private lastReq: SearchRequest | null = null;
  private lastQueryRun = "";
  private loadingMore = false;
  private pendingSnippets = new Set<string>();
  private previewSeq = 0;
  private generationShown = 0;
  private pollTimer: ReturnType<typeof setTimeout> | undefined;

  // Elements
  private input!: HTMLInputElement;
  private modeButtons: HTMLButtonElement[] = [];
  private caseBtn!: HTMLButtonElement;
  private wordBtn!: HTMLButtonElement;
  private statusBtn!: HTMLButtonElement;
  private rootsBar!: HTMLElement;
  private filtersBtn!: HTMLButtonElement;
  private filtersPanel!: HTMLElement;
  private regexBanner!: HTMLElement;
  private indexBanner!: HTMLElement;
  private updates: Updates | null = null;
  private summary!: HTMLElement;
  private sortSel!: HTMLSelectElement;
  private resultsHost!: HTMLElement;
  private emptyState!: HTMLElement;
  private split!: HTMLElement;
  private live!: HTMLElement;
  private toasts!: HTMLElement;
  results: ResultsList;
  preview: PreviewPane;

  private runDebounced = debounce(() => void this.runSearch(), 150);
  private previewDebounced = debounce((i: number) => void this.loadPreview(i), 70);
  private snippetsDebounced = debounce(() => void this.loadSnippets(), 40);
  private visible: [number, number] = [0, 0];
  private keyHandler = (e: KeyboardEvent) => this.onKey(e);
  private destroyed = false;

  constructor(private root: HTMLElement, private api: Api) {
    this.results = new ResultsList({
      onSelect: (i) => this.previewDebounced(i),
      onOpen: (i) => this.openIndex(i),
      onNeedMore: () => void this.loadMore(),
      onVisible: (a, b) => {
        this.visible = [a, b];
        this.snippetsDebounced();
      },
    });
    this.preview = new PreviewPane({
      onOpen: (p) => void this.open(p),
      onReveal: (p) => void this.reveal(p),
      onCopy: (p) => void this.copy(p),
    });
  }

  async init(): Promise<void> {
    this.settings = await this.api.getSettings();
    this.mode = this.settings.search.defaultMode;
    this.caseSensitive = this.settings.search.caseSensitive;
    this.wholeWord = this.settings.search.wholeWord;
    this.build();
    this.applyAppearance();
    this.api.onIndexUpdated((g) => this.onIndexUpdated(g));
    document.addEventListener("keydown", this.keyHandler);
    await this.refreshStatus();
    this.updates?.start();
    this.input.focus();
  }

  // ------------------------------------------------------------------------------------------
  // Layout
  // ------------------------------------------------------------------------------------------

  private build(): void {
    this.input = h("input", {
      type: "search",
      class: "search-input",
      placeholder: "Search files…",
      "aria-label": "Search",
      autocomplete: "off",
      spellcheck: "false",
    }) as HTMLInputElement;
    this.input.addEventListener("input", () => this.runDebounced());
    this.input.addEventListener("keydown", (e) => this.onInputKey(e));

    const modes = h("div", { class: "modes", role: "radiogroup", "aria-label": "Search mode" });
    MODES.forEach(([m, label, tip], i) => {
      const b = h("button", { class: "mode", role: "radio", "aria-checked": "false", title: `${tip} (${modLabel}+${i + 1})`, onclick: () => this.setMode(m) }, label) as HTMLButtonElement;
      b.dataset.mode = m;
      modes.appendChild(b);
      this.modeButtons.push(b);
    });
    modes.addEventListener("keydown", (e) => {
      if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
      const i = MODES.findIndex(([m]) => m === this.mode);
      const n = (i + (e.key === "ArrowRight" ? 1 : MODES.length - 1)) % MODES.length;
      this.setMode(MODES[n][0]);
      this.modeButtons[n].focus();
      e.preventDefault();
    });

    this.caseBtn = h("button", { class: "toggle", "aria-pressed": "false", title: "Match case", "aria-label": "Match case", onclick: () => this.toggleCase() }, "Aa") as HTMLButtonElement;
    this.wordBtn = h("button", { class: "toggle", "aria-pressed": "false", title: "Whole words only", "aria-label": "Whole words only", onclick: () => this.toggleWord() }, h("span", { class: "ww" }, "ab")) as HTMLButtonElement;

    this.statusBtn = h("button", { class: "status-pill", title: "Index status", onclick: () => this.showIndex() }) as HTMLButtonElement;
    // The header doubles as the title bar: its empty parts move the window (double-click
    // maximises), handled by Tauri for elements marked with data-tauri-drag-region.
    const drag = { "data-tauri-drag-region": "" };
    const header = h(
      "header",
      { class: "topbar", ...drag },
      h("div", { class: "brand", ...drag }, h("span", { class: "logo", "aria-hidden": "true" }), h("span", {}, "FastFind")),
      h("div", { class: "grow", ...drag }),
      this.statusBtn,
      h("button", { class: "icon-btn", title: "Help (F1)", "aria-label": "Help", onclick: () => openHelp() }, icon(I.help)),
      h("button", { class: "icon-btn", title: `Settings (${modLabel}+,)`, "aria-label": "Settings", onclick: () => this.showSettings() }, icon(I.settings)),
    );
    const win = this.api.window;
    if (win?.nativeButtons) header.classList.add("native-controls");
    else if (win) header.append(this.buildWindowControls(win));

    const searchRow = h(
      "section",
      { class: "searchbar" },
      h("div", { class: "search-field" }, icon(I.search, "icon search-icon"), this.input, this.caseBtn, this.wordBtn),
      modes,
    );

    this.rootsBar = h("div", { class: "roots", "aria-label": "Indexed folders" });
    this.filtersBtn = h("button", { class: "btn ghost", "aria-expanded": "false", title: `Filters (${modLabel}+Shift+F)`, onclick: () => this.toggleFilters() }) as HTMLButtonElement;
    const rootsRow = h(
      "section",
      { class: "rootsbar" },
      this.rootsBar,
      h("button", { class: "btn", title: `Add folder (${modLabel}+O)`, onclick: () => void this.addFolder() }, icon(I.folderPlus), "Add folder"),
      h("div", { class: "grow" }),
      this.filtersBtn,
    );

    this.filtersPanel = h("section", { class: "filters", hidden: true, "aria-label": "Filters" });
    this.regexBanner = h("div", { class: "banner regex", hidden: true, role: "note" }, icon(I.alert), "Regex mode scans document text and is slower than Smart search on large indexes. Filters (type, folder, date) make it faster.");
    this.indexBanner = h("div", { class: "banner indexing", hidden: true, role: "status" });
    if (this.api.updates) {
      this.updates = new Updates(this.api.updates, {
        settings: () => this.settings,
        saveSettings: async (s) => {
          this.settings = await this.api.saveSettings(s);
        },
        toast: (m, e) => this.toast(m, e),
      });
    }

    this.sortSel = h(
      "select",
      { class: "sort", "aria-label": "Sort results" },
      h("option", { value: "relevance" }, "Relevance"),
      h("option", { value: "modified" }, "Date modified"),
      h("option", { value: "size" }, "Size"),
      h("option", { value: "name" }, "Name"),
    ) as HTMLSelectElement;
    this.sortSel.addEventListener("change", () => {
      this.sort = this.sortSel.value as SortOrder;
      void this.runSearch();
    });
    this.summary = h("div", { class: "summary-text", "aria-live": "polite" });
    const summaryRow = h("div", { class: "summary" }, this.summary, h("div", { class: "grow" }), h("label", { class: "muted small" }, "Sort ", this.sortSel));

    this.emptyState = h("div", { class: "empty" });
    this.resultsHost = h("div", { class: "results-pane" }, this.results.el, this.emptyState);
    const divider = h("div", { class: "divider", role: "separator", "aria-orientation": "vertical", "aria-label": "Resize preview", tabindex: 0 });
    this.split = h("main", { class: "split" }, this.resultsHost, divider, this.preview.el);
    this.setupDivider(divider);

    this.live = h("div", { class: "sr-only", "aria-live": "assertive" });
    this.toasts = h("div", { class: "toasts", "aria-live": "polite" });
    clear(this.root);
    this.root.append(header, searchRow, rootsRow, this.filtersPanel, this.regexBanner, this.indexBanner, ...(this.updates ? [this.updates.el] : []), summaryRow, this.split, this.live, this.toasts);
    this.buildFilters();
    this.syncControls();
    this.showEmpty();
  }

  private buildWindowControls(win: WindowControls): HTMLElement {
    const max = h("button", { class: "win-btn", onclick: () => void win.toggleMaximize() }) as HTMLButtonElement;
    const sync = async () => {
      const on = await win.isMaximized().catch(() => false);
      const label = on ? "Restore" : "Maximize";
      max.title = label;
      max.setAttribute("aria-label", label);
      max.replaceChildren(icon(on ? I.winRestore : I.winMax));
    };
    void sync();
    win.onResized(() => void sync());
    return h(
      "div",
      { class: "win-controls", role: "group", "aria-label": "Window" },
      h("button", { class: "win-btn", title: "Minimize", "aria-label": "Minimize", onclick: () => void win.minimize() }, icon(I.winMin)),
      max,
      h("button", { class: "win-btn close", title: "Close", "aria-label": "Close", onclick: () => void win.close() }, icon(I.x)),
    );
  }

  private setupDivider(divider: HTMLElement): void {
    const saved = Number(safeGet("ff.split"));
    if (saved > 20 && saved < 80) this.split.style.setProperty("--left", `${saved}%`);
    const setPct = (pct: number) => {
      const p = Math.min(75, Math.max(25, pct));
      this.split.style.setProperty("--left", `${p}%`);
      safeSet("ff.split", String(p));
    };
    divider.addEventListener("pointerdown", (e) => {
      divider.setPointerCapture(e.pointerId);
      const move = (ev: PointerEvent) => {
        const r = this.split.getBoundingClientRect();
        setPct(((ev.clientX - r.left) / r.width) * 100);
      };
      const up = () => {
        divider.removeEventListener("pointermove", move);
        divider.removeEventListener("pointerup", up);
      };
      divider.addEventListener("pointermove", move);
      divider.addEventListener("pointerup", up);
    });
    divider.addEventListener("keydown", (e) => {
      const cur = parseFloat(this.split.style.getPropertyValue("--left")) || 50;
      if (e.key === "ArrowLeft") setPct(cur - 3);
      if (e.key === "ArrowRight") setPct(cur + 3);
    });
  }

  private buildFilters(): void {
    const p = this.filtersPanel;
    clear(p);
    const kinds = h("div", { class: "chips", role: "group", "aria-label": "File types" });
    for (const [k, label] of KIND_OPTIONS) {
      const b = h("button", { class: "chip", "aria-pressed": this.filters.kinds.includes(k) ? "true" : "false" }, label) as HTMLButtonElement;
      b.addEventListener("click", () => {
        const on = !this.filters.kinds.includes(k);
        this.filters.kinds = on ? [...this.filters.kinds, k] : this.filters.kinds.filter((x) => x !== k);
        b.setAttribute("aria-pressed", on ? "true" : "false");
        this.filtersChanged();
      });
      kinds.appendChild(b);
    }
    const ext = h("input", { type: "text", placeholder: "e.g. pdf, docx", "aria-label": "Extensions", value: this.filters.exts.join(", ") }) as HTMLInputElement;
    ext.addEventListener("change", () => {
      this.filters.exts = ext.value.split(/[\s,;]+/).map((s) => s.replace(/^\*?\./, "").toLowerCase()).filter(Boolean);
      this.filtersChanged();
    });
    const dateSel = h("select", { "aria-label": "Modified" }, ...DATE_PRESETS.map(([v, l]) => h("option", { value: v }, l))) as HTMLSelectElement;
    dateSel.value = this.datePreset;
    const from = h("input", { type: "date", "aria-label": "Modified from", hidden: this.datePreset !== "custom" }) as HTMLInputElement;
    const to = h("input", { type: "date", "aria-label": "Modified to", hidden: this.datePreset !== "custom" }) as HTMLInputElement;
    const applyDate = () => {
      this.datePreset = dateSel.value;
      from.hidden = to.hidden = this.datePreset !== "custom";
      const preset = DATE_PRESETS.find(([v]) => v === this.datePreset);
      if (this.datePreset === "custom") {
        this.filters.modifiedAfter = from.value ? Math.floor(new Date(`${from.value}T00:00:00`).getTime() / 1000) : null;
        this.filters.modifiedBefore = to.value ? Math.floor(new Date(`${to.value}T00:00:00`).getTime() / 1000) + 86400 : null;
      } else if (preset && preset[2] !== null) {
        const start = new Date();
        start.setHours(0, 0, 0, 0);
        start.setDate(start.getDate() - (preset[2] - 1));
        this.filters.modifiedAfter = Math.floor(start.getTime() / 1000);
        this.filters.modifiedBefore = null;
      } else {
        this.filters.modifiedAfter = this.filters.modifiedBefore = null;
      }
      this.filtersChanged();
    };
    dateSel.addEventListener("change", applyDate);
    from.addEventListener("change", applyDate);
    to.addEventListener("change", applyDate);
    const sizeSel = h("select", { "aria-label": "Size" }, ...SIZE_PRESETS.map(([v, l]) => h("option", { value: v }, l))) as HTMLSelectElement;
    sizeSel.value = this.sizePreset;
    sizeSel.addEventListener("change", () => {
      this.sizePreset = sizeSel.value;
      const pr = SIZE_PRESETS.find(([v]) => v === this.sizePreset)!;
      this.filters.sizeMin = pr[2];
      this.filters.sizeMax = pr[3] === null ? null : pr[3] - 1;
      this.filtersChanged();
    });
    const folderSel = h("select", { "aria-label": "Folder" }, h("option", { value: "" }, "All folders"), ...(this.status?.roots ?? []).map((r) => h("option", { value: r.path }, r.path))) as HTMLSelectElement;
    folderSel.value = this.filters.dirs[0] ?? "";
    folderSel.addEventListener("change", () => {
      this.filters.dirs = folderSel.value ? [folderSel.value] : [];
      this.filtersChanged();
    });
    const reset = h("button", { class: "btn ghost", onclick: () => {
      this.filters = emptyFilters();
      this.datePreset = this.sizePreset = "any";
      this.buildFilters();
      this.filtersChanged();
    } }, "Clear filters");
    p.append(
      h("div", { class: "filter-row" }, h("span", { class: "filter-label" }, "Type"), kinds),
      h("div", { class: "filter-row" },
        h("label", { class: "filter-label" }, "Extension"), ext,
        h("label", { class: "filter-label" }, "Modified"), dateSel, from, to,
        h("label", { class: "filter-label" }, "Size"), sizeSel,
        h("label", { class: "filter-label" }, "Folder"), folderSel,
        h("div", { class: "grow" }), reset),
    );
  }

  private activeFilterCount(): number {
    const f = this.filters;
    return (f.kinds.length ? 1 : 0) + (f.exts.length ? 1 : 0) + (f.dirs.length ? 1 : 0) + (f.modifiedAfter !== null || f.modifiedBefore !== null ? 1 : 0) + (f.sizeMin !== null || f.sizeMax !== null ? 1 : 0);
  }

  private filtersChanged(): void {
    this.syncControls();
    void this.runSearch();
  }

  private toggleFilters(): void {
    this.filtersPanel.hidden = !this.filtersPanel.hidden;
    this.filtersBtn.setAttribute("aria-expanded", String(!this.filtersPanel.hidden));
    if (!this.filtersPanel.hidden) this.buildFilters();
  }

  private syncControls(): void {
    for (const b of this.modeButtons) b.setAttribute("aria-checked", b.dataset.mode === this.mode ? "true" : "false");
    this.caseBtn.setAttribute("aria-pressed", String(this.caseSensitive));
    this.wordBtn.setAttribute("aria-pressed", String(this.wholeWord));
    this.wordBtn.disabled = this.mode === "filename";
    this.regexBanner.hidden = this.mode !== "regex";
    this.input.placeholder = {
      smart: "Search files…   e.g. \"annual report\" ext:pdf",
      exact: "Exact text, e.g. foo.bar(x)",
      regex: "Regular expression, e.g. invoice-[0-9]{4}",
      filename: "File name, e.g. report*.docx",
    }[this.mode];
    const n = this.activeFilterCount();
    clear(this.filtersBtn);
    this.filtersBtn.append(icon(I.filter), n ? `Filters (${n})` : "Filters");
    this.filtersBtn.classList.toggle("active", n > 0);
  }

  private setMode(m: SearchMode): void {
    if (this.mode === m) return;
    this.mode = m;
    this.syncControls();
    void this.runSearch();
  }

  private toggleCase(): void {
    this.caseSensitive = !this.caseSensitive;
    this.syncControls();
    void this.runSearch();
  }

  private toggleWord(): void {
    this.wholeWord = !this.wholeWord;
    this.syncControls();
    void this.runSearch();
  }

  // ------------------------------------------------------------------------------------------
  // Search
  // ------------------------------------------------------------------------------------------

  private request(offset = 0): SearchRequest {
    return {
      query: this.input.value,
      mode: this.mode,
      caseSensitive: this.caseSensitive,
      wholeWord: this.wholeWord,
      filters: structuredClone(this.filters),
      sort: this.sort,
      offset,
      limit: this.settings.search.pageSize,
    };
  }

  async runSearch(): Promise<void> {
    this.runDebounced.cancel();
    const req = this.request();
    const seq = ++this.seq;
    this.lastQueryRun = this.signature(req);
    if (!req.query.trim() && this.activeFilterCount() === 0) {
      this.lastReq = null;
      this.results.reset();
      this.preview.empty();
      this.summary.textContent = "";
      this.showEmpty();
      return;
    }
    this.summary.textContent = "Searching…";
    let resp: SearchResponse;
    try {
      resp = await this.api.search(req);
    } catch (e) {
      if (seq !== this.seq || String(e) === "cancelled") return;
      this.lastReq = null;
      this.results.reset();
      this.preview.empty();
      this.summary.textContent = "";
      this.showEmpty(String(e));
      return;
    }
    if (seq !== this.seq) return; // superseded while in flight
    this.lastReq = req;
    this.generationShown = resp.generation;
    this.pendingSnippets.clear();
    this.results.setData(resp.items, resp.total);
    this.renderSummary(resp);
    if (resp.items.length === 0) {
      this.preview.empty("");
      this.showEmpty(null, true);
    } else {
      this.emptyState.hidden = true;
      this.results.el.hidden = false;
      this.results.select(0);
    }
  }

  private signature(r: SearchRequest): string {
    return JSON.stringify([r.query, r.mode, r.caseSensitive, r.wholeWord, r.filters, r.sort]);
  }

  private renderSummary(resp: SearchResponse): void {
    clear(this.summary);
    const count = `${num(resp.total)}${resp.totalIsLowerBound ? "+" : ""} ${resp.total === 1 ? "result" : "results"}`;
    this.summary.append(h("strong", {}, count), h("span", { class: "muted" }, ` · ${duration(resp.elapsedMs)}`));
    if (resp.partial) this.summary.append(h("span", { class: "tag warn", title: "The time limit was reached; scroll down to continue searching" }, "Partial"));
    for (const w of resp.warnings) this.summary.append(h("span", { class: "tag", title: w }, "Slow query"));
    this.live.textContent = count;
  }

  private async loadMore(): Promise<void> {
    if (!this.lastReq || this.loadingMore || this.results.length >= this.results.totalCount) return;
    this.loadingMore = true;
    const seq = this.seq;
    try {
      const resp = await this.api.search({ ...this.lastReq, offset: this.results.length });
      if (seq !== this.seq) return;
      this.results.append(resp.items);
      if (resp.total !== this.results.totalCount || resp.totalIsLowerBound) this.renderSummary({ ...resp, total: Math.max(resp.total, this.results.length) });
    } catch {
      // Ignore: the next scroll retries.
    } finally {
      this.loadingMore = false;
    }
  }

  private async loadSnippets(): Promise<void> {
    if (!this.lastReq) return;
    const [a, b] = this.visible;
    const paths: string[] = [];
    for (let i = a; i <= b; i++) {
      const it = this.results.item(i);
      if (it && !this.results.hasSnippet(it.path) && !this.pendingSnippets.has(it.path)) paths.push(it.path);
    }
    if (paths.length === 0) return;
    paths.forEach((p) => this.pendingSnippets.add(p));
    const seq = this.seq;
    try {
      const res = await this.api.snippets(this.lastReq, paths);
      if (seq === this.seq) this.results.setSnippets(res);
    } catch {
      paths.forEach((p) => this.pendingSnippets.delete(p));
    }
  }

  private async loadPreview(i: number): Promise<void> {
    const it = this.results.item(i);
    if (!it || !this.lastReq || !this.settings.appearance.showPreview) return;
    const seq = ++this.previewSeq;
    this.preview.loading(it);
    try {
      const p = await this.api.preview(this.lastReq, it.path);
      if (seq === this.previewSeq) this.preview.show(p, it.ext);
    } catch (e) {
      if (seq === this.previewSeq) this.preview.error(it, String(e));
    }
  }

  private showEmpty(error: string | null = null, noResults = false): void {
    clear(this.emptyState);
    this.emptyState.hidden = false;
    this.results.el.hidden = true;
    const roots = this.status?.roots ?? [];
    if (error) {
      this.emptyState.append(h("div", { class: "empty-card error" }, icon(I.alert, "icon big"), h("h2", {}, "That query couldn't run"), h("p", {}, error)));
    } else if (roots.length === 0 && this.status) {
      this.emptyState.append(h(
        "div", { class: "empty-card" },
        icon(I.folderPlus, "icon big"),
        h("h2", {}, "Choose folders to search"),
        h("p", {}, "FastFind indexes the text inside your documents, PDFs, spreadsheets, presentations and code so you can find anything in milliseconds. Everything stays on this computer."),
        h("button", { class: "btn primary big", onclick: () => void this.addFolder() }, icon(I.folderPlus), "Add a folder"),
      ));
    } else if (noResults) {
      const tips = [this.mode === "smart" && !this.wholeWord ? null : "Turn off “Whole words” to also match word beginnings.", this.activeFilterCount() ? "Clear some filters." : null, this.status?.progress.active ? "Indexing is still running — more files will become searchable shortly." : null, "Try Exact or File name mode, or check the spelling."].filter(Boolean) as string[];
      this.emptyState.append(h("div", { class: "empty-card" }, h("h2", {}, "No matches"), h("ul", {}, ...tips.map((t) => h("li", {}, t)))));
    } else {
      this.emptyState.append(h(
        "div", { class: "empty-card subtle" },
        h("h2", {}, "Search your files"),
        h("p", {}, "Type words to search inside documents. Use quotes for phrases, OR / - for alternatives and exclusions, and filters like ext:pdf or modified:7d."),
        h("button", { class: "linklike", onclick: () => openHelp() }, "Search syntax and shortcuts"),
      ));
    }
  }

  // ------------------------------------------------------------------------------------------
  // Actions
  // ------------------------------------------------------------------------------------------

  private openIndex(i: number): void {
    const it = this.results.item(i);
    if (it) void this.open(it.path);
  }

  private async open(path: string): Promise<void> {
    try {
      await this.api.openFile(path);
    } catch (e) {
      this.toast(String(e), true);
    }
  }

  private async reveal(path: string): Promise<void> {
    try {
      await this.api.revealFile(path);
    } catch (e) {
      this.toast(String(e), true);
    }
  }

  private async copy(path: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(path);
      this.toast("Path copied");
    } catch {
      this.toast("Could not copy to the clipboard", true);
    }
  }

  async addFolder(): Promise<void> {
    const dirs = await this.api.pickFolders();
    for (const d of dirs) {
      try {
        const r = await this.api.addRoot(d);
        this.toast(`Indexing ${r.path}…`);
      } catch (e) {
        this.toast(`Couldn't add ${d}: ${e}`, true);
      }
    }
    if (dirs.length) await this.refreshStatus();
  }

  private async showIndex(): Promise<void> {
    const st = await this.api.status();
    openIndexPanel(this.api, st, { refresh: () => void this.refreshStatus(), showProblems: () => openProblems(this.api), addFolder: () => void this.addFolder() });
  }

  private showSettings(): void {
    void openSettings(this.api, this.settings, (s) => {
      const searchChanged = JSON.stringify(s.search) !== JSON.stringify(this.settings.search);
      this.settings = s;
      this.applyAppearance();
      if (searchChanged) void this.runSearch();
      this.toast("Settings saved");
    }, this.updates);
  }

  private applyAppearance(): void {
    const a = this.settings.appearance;
    const html = document.documentElement;
    if (a.theme === "system") delete html.dataset.theme;
    else html.dataset.theme = a.theme;
    html.classList.toggle("hc", a.highContrast);
    html.style.fontSize = `${14 * a.fontScale}px`;
    this.split.classList.toggle("no-preview", !a.showPreview);
    this.results.render();
  }

  private saveScaleDebounced = debounce(() => void this.api.saveSettings(this.settings).then((s) => (this.settings = s)), 600);

  private zoom(delta: number | null): void {
    const a = this.settings.appearance;
    a.fontScale = delta === null ? 1 : Math.round(Math.min(1.6, Math.max(0.8, a.fontScale + delta)) * 10) / 10;
    this.applyAppearance();
    this.saveScaleDebounced();
  }

  toast(message: string, error = false): void {
    const t = h("div", { class: `toast${error ? " error" : ""}`, role: error ? "alert" : "status" }, message);
    this.toasts.appendChild(t);
    setTimeout(() => t.remove(), error ? 6000 : 2500);
  }

  // ------------------------------------------------------------------------------------------
  // Status
  // ------------------------------------------------------------------------------------------

  async refreshStatus(): Promise<void> {
    clearTimeout(this.pollTimer);
    try {
      const st = await this.api.status();
      const rootsChanged = JSON.stringify(st.roots.map((r) => r.path)) !== JSON.stringify(this.status?.roots.map((r) => r.path));
      this.status = st;
      this.renderStatus(st);
      if (rootsChanged) {
        this.renderRoots(st);
        if (!this.lastReq) this.showEmpty();
        if (!this.filtersPanel.hidden) this.buildFilters();
      }
    } catch {
      // Backend busy; retry on next tick.
    }
    const active = this.status?.progress.active ?? false;
    if (!this.destroyed) this.pollTimer = setTimeout(() => void this.refreshStatus(), active ? 1000 : 5000);
  }

  private renderStatus(st: IndexStatus): void {
    const p = st.progress;
    clear(this.statusBtn);
    if (p.active && !p.paused) {
      const pct = p.percent !== null ? `${Math.floor(p.percent)}%` : p.scanning ? "scanning" : "";
      this.statusBtn.append(h("span", { class: "spinner", "aria-hidden": "true" }), `Indexing… ${pct}`);
    } else if (p.paused) {
      this.statusBtn.append(icon(I.pause), "Indexing paused");
    } else {
      this.statusBtn.append(icon(I.database), `Indexed: ${num(st.filesTotal)} files · ${bytes(st.indexBytes + st.textStoreBytes)} · Updated ${ago(st.lastUpdated)}`);
    }
    this.statusBtn.title = `${num(st.filesTotal)} files indexed\n${num(st.indexed + st.nameOnly)} successful\n${num(st.skipped + st.failed + st.encrypted)} skipped${st.needsOcr ? `\n${num(st.needsOcr)} need OCR` : ""}`;
    clear(this.indexBanner);
    this.indexBanner.hidden = !(p.active && !p.paused);
    if (!this.indexBanner.hidden) {
      const pct = p.percent ?? 0;
      this.indexBanner.append(
        h("div", { class: "banner-text" },
          h("strong", {}, p.scanning && p.percent === null ? `Scanning folders… ${num(p.discovered)} files found` : `Indexing… ${Math.floor(pct)}% complete`),
          h("span", { class: "muted" }, " · Search is available for already indexed files."),
          p.currentPath ? h("span", { class: "muted ellipsis current-file", title: p.currentPath }, p.currentPath) : null),
        h("div", { class: "progress", role: "progressbar", "aria-valuemin": 0, "aria-valuemax": 100, "aria-valuenow": Math.floor(pct) }, h("div", { class: `bar${p.percent === null ? " indeterminate" : ""}` })),
      );
      (this.indexBanner.querySelector(".bar") as HTMLElement).style.width = p.percent === null ? "30%" : `${pct}%`;
    }
  }

  private renderRoots(st: IndexStatus): void {
    clear(this.rootsBar);
    for (const r of st.roots) {
      const name = r.path.split(/[\\/]/).filter(Boolean).pop() ?? r.path;
      this.rootsBar.appendChild(h(
        "span", { class: `root-chip${r.available ? "" : " unavailable"}`, title: `${r.path}\n${num(r.fileCount)} files${r.available ? "" : "\nFolder is not available"}` },
        icon(I.folder),
        h("span", { class: "rc-name" }, name),
        h("button", { class: "rc-x", "aria-label": `Remove ${r.path}`, title: "Remove from index", onclick: async () => {
          if (await confirmDialog("Remove folder", `Remove “${r.path}” from the index? Your files are not touched.`, "Remove", true)) {
            await this.api.removeRoot(r.id);
            await this.refreshStatus();
            void this.runSearch();
          }
        } }, icon(I.x)),
      ));
    }
  }

  private onIndexUpdated(generation: number): void {
    void this.refreshStatus();
    if (!this.lastReq || generation <= this.generationShown) return;
    // Nothing on screen to lose: re-run automatically; otherwise offer a refresh.
    if (this.results.length === 0) {
      void this.runSearch();
      return;
    }
    if (!this.summary.querySelector(".refresh-link")) {
      this.summary.append(h("button", { class: "linklike refresh-link", onclick: () => void this.runSearch() }, icon(I.refresh), "New files indexed — refresh"));
    }
  }

  // ------------------------------------------------------------------------------------------
  // Keyboard
  // ------------------------------------------------------------------------------------------

  private moveSelection(delta: number): void {
    const cur = this.results.selectedIndex();
    this.results.select(cur < 0 ? 0 : cur + delta);
  }

  private onInputKey(e: KeyboardEvent): void {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        this.moveSelection(1);
        break;
      case "ArrowUp":
        e.preventDefault();
        this.moveSelection(-1);
        break;
      case "PageDown":
        e.preventDefault();
        this.moveSelection(this.results.pageSize());
        break;
      case "PageUp":
        e.preventDefault();
        this.moveSelection(-this.results.pageSize());
        break;
      case "Enter": {
        e.preventDefault();
        if (modKey(e)) break; // handled globally (reveal)
        const changed = this.signature(this.request()) !== this.lastQueryRun;
        if (changed || !this.lastReq) void this.runSearch();
        else this.openIndex(this.results.selectedIndex());
        break;
      }
      case "Escape":
        if (this.input.value) {
          e.preventDefault();
          e.stopPropagation();
          this.input.value = "";
          void this.runSearch();
        }
        break;
    }
  }

  private onKey(e: KeyboardEvent): void {
    if (document.querySelector("dialog[open]")) return;
    const mod = modKey(e);
    const k = e.key.toLowerCase();
    if (mod && (k === "k" || (k === "f" && !e.shiftKey))) {
      e.preventDefault();
      this.input.focus();
      this.input.select();
    } else if (mod && e.shiftKey && k === "f") {
      e.preventDefault();
      this.toggleFilters();
    } else if (mod && k === "o") {
      e.preventDefault();
      void this.addFolder();
    } else if (mod && k === ",") {
      e.preventDefault();
      this.showSettings();
    } else if (e.key === "F1" || (mod && k === "/")) {
      e.preventDefault();
      openHelp();
    } else if (mod && ["1", "2", "3", "4"].includes(e.key)) {
      e.preventDefault();
      this.setMode(MODES[Number(e.key) - 1][0]);
    } else if (mod && e.key === "Enter") {
      e.preventDefault();
      const it = this.results.item(this.results.selectedIndex());
      if (it) void this.reveal(it.path);
    } else if (mod && e.shiftKey && k === "c") {
      const it = this.results.item(this.results.selectedIndex());
      if (it) {
        e.preventDefault();
        void this.copy(it.path);
      }
    } else if (e.key === "F3") {
      e.preventDefault();
      this.preview.step(e.shiftKey ? -1 : 1);
    } else if (mod && (e.key === "=" || e.key === "+")) {
      e.preventDefault();
      this.zoom(0.1);
    } else if (mod && e.key === "-") {
      e.preventDefault();
      this.zoom(-0.1);
    } else if (mod && e.key === "0") {
      e.preventDefault();
      this.zoom(null);
    } else if (document.activeElement === this.results.el) {
      if (e.key === "ArrowDown") this.moveSelection(1);
      else if (e.key === "ArrowUp") this.moveSelection(-1);
      else if (e.key === "PageDown") this.moveSelection(this.results.pageSize());
      else if (e.key === "PageUp") this.moveSelection(-this.results.pageSize());
      else if (e.key === "Home") this.results.select(0);
      else if (e.key === "End") this.results.select(this.results.length - 1);
      else if (e.key === "Enter") this.openIndex(this.results.selectedIndex());
      else return;
      e.preventDefault();
    } else if (e.key === "Escape" && document.activeElement !== this.input) {
      this.input.focus();
    }
  }

  /** Detach global listeners and timers (tests, hot reload). */
  destroy(): void {
    this.destroyed = true;
    document.removeEventListener("keydown", this.keyHandler);
    clearTimeout(this.pollTimer);
    this.runDebounced.cancel();
    this.previewDebounced.cancel();
    this.snippetsDebounced.cancel();
    this.closeAllDialogs();
  }

  /** For tests. */
  closeAllDialogs(): void {
    document.querySelectorAll("dialog").forEach((d) => closeDialog(d as HTMLDialogElement));
  }
}

function safeGet(k: string): string | null {
  try {
    return localStorage.getItem(k);
  } catch {
    return null;
  }
}

function safeSet(k: string, v: string): void {
  try {
    localStorage.setItem(k, v);
  } catch {
    // Storage unavailable (private mode): the split position simply isn't remembered.
  }
}
