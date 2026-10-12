// The pop-up search (desktop app): a search box opened from anywhere with a global shortcut.
// Type, then Enter opens the top result; arrows choose another; Ctrl/⌘+Enter shows it in its
// folder; Shift+Enter shows all results in the main window; Esc clears, then closes.

import { emptyFilters, type ResultItem, type SearchRequest, type SearchResponse, type Settings } from "./api";
import { clear, debounce, h, icon, modKey, modLabel } from "./dom";
import { kindBadge } from "./format";
import { I } from "./icons";

export interface QuickApi {
  search(req: SearchRequest): Promise<SearchResponse>;
  openFile(path: string): Promise<void>;
  revealFile(path: string): Promise<void>;
  getSettings(): Promise<Settings>;
  /** Give the page keyboard focus (after the window has been shown). */
  focus(): Promise<void>;
  /** Hide the pop-up window. */
  hide(): Promise<void>;
  /** Fit the window to its content (CSS pixels). */
  resize(height: number): Promise<void>;
  /** Run the query in the main window. */
  showAll(query: string): Promise<void>;
  /** The pop-up was opened by the shortcut or the tray menu. */
  onShow(cb: () => void): void;
}

const LIMIT = 8;

export class QuickSearch {
  readonly input: HTMLInputElement;
  private box: HTMLElement;
  private list: HTMLElement;
  private foot: HTMLElement;
  private note: HTMLElement;
  private items: ResultItem[] = [];
  /** The query `items` belong to. */
  private shownQuery = "";
  private selected = 0;
  private seq = 0;
  private settings: Settings | null = null;
  private runDebounced = debounce(() => void this.run(), 70);

  constructor(root: HTMLElement, private api: QuickApi) {
    this.input = h("input", {
      type: "search",
      class: "qs-input",
      placeholder: "Search your files…",
      spellcheck: "false",
      autocomplete: "off",
      "aria-label": "Search your files",
      "aria-controls": "qs-list",
    }) as HTMLInputElement;
    this.list = h("ul", { class: "qs-list", id: "qs-list", role: "listbox", hidden: true });
    this.note = h("div", { class: "qs-note", role: "status", hidden: true });
    this.foot = h("div", { class: "qs-foot", hidden: true },
      h("span", {}, h("kbd", {}, "↵"), " open"),
      h("span", {}, h("kbd", {}, `${modLabel}+↵`), " show in folder"),
      h("span", {}, h("kbd", {}, "Shift+↵"), " all results"),
      h("span", {}, h("kbd", {}, "Esc"), " close"));
    this.box = h("div", { class: "qs" }, h("div", { class: "qs-bar" }, icon(I.search), this.input), this.note, this.list, this.foot);
    root.append(this.box);
  }

  async init(): Promise<void> {
    await this.loadSettings();
    this.input.addEventListener("input", () => this.runDebounced());
    this.input.addEventListener("keydown", (e) => this.onKey(e));
    this.api.onShow(() => void this.onShow());
    this.input.focus();
    await this.fit();
  }

  private async loadSettings(): Promise<void> {
    try {
      this.settings = await this.api.getSettings();
    } catch {
      return;
    }
    const a = this.settings.appearance;
    const html = document.documentElement;
    if (a.theme === "system") delete html.dataset.theme;
    else html.dataset.theme = a.theme;
    html.classList.toggle("hc", a.highContrast);
    html.style.fontSize = `${14 * a.fontScale}px`;
  }

  private async onShow(): Promise<void> {
    await this.api.focus();
    this.input.focus();
    this.input.select();
    // Theme and text size may have changed since it was last shown.
    await this.loadSettings();
    this.input.focus();
    this.input.select();
    // The index may have changed while hidden.
    if (this.input.value.trim()) await this.run();
    else await this.fit();
  }

  private request(): SearchRequest {
    const s = this.settings?.search;
    return {
      query: this.input.value,
      mode: s?.defaultMode ?? "smart",
      caseSensitive: s?.caseSensitive ?? false,
      wholeWord: s?.wholeWord ?? false,
      filters: emptyFilters(),
      sort: "relevance",
      offset: 0,
      limit: LIMIT,
    };
  }

  async run(): Promise<void> {
    this.runDebounced.cancel();
    const seq = ++this.seq;
    const query = this.input.value;
    if (!query.trim()) {
      this.items = [];
      this.shownQuery = query;
      this.showNote(null);
      return this.render();
    }
    try {
      const r = await this.api.search(this.request());
      if (seq !== this.seq) return;
      this.items = r.items;
      this.selected = 0;
      this.showNote(r.items.length ? null : `No files match “${query.trim()}”.`);
    } catch (e) {
      if (seq !== this.seq) return;
      this.items = [];
      this.showNote(String(e));
    }
    this.shownQuery = query;
    await this.render();
  }

  private showNote(text: string | null): void {
    this.note.hidden = text === null;
    this.note.textContent = text ?? "";
  }

  private async render(): Promise<void> {
    clear(this.list);
    this.items.forEach((it, i) => {
      const b = kindBadge(it.kind, it.ext);
      this.list.appendChild(h("li", {
        class: `qs-row${i === this.selected ? " selected" : ""}`,
        role: "option",
        id: `qs-${i}`,
        "aria-selected": i === this.selected ? "true" : "false",
        title: it.path,
        onclick: (e: Event) => void this.open(i, modKey(e as MouseEvent)),
        onmousemove: () => this.select(i),
      },
      h("span", { class: `badge ${b.cls}` }, b.label),
      h("span", { class: "qs-text" }, h("span", { class: "qs-name ellipsis" }, it.name), h("span", { class: "qs-dir ellipsis" }, it.dir))));
    });
    this.list.hidden = this.items.length === 0;
    this.foot.hidden = this.items.length === 0;
    if (this.items.length) this.input.setAttribute("aria-activedescendant", `qs-${this.selected}`);
    else this.input.removeAttribute("aria-activedescendant");
    await this.fit();
  }

  private select(i: number): void {
    if (i === this.selected || i < 0 || i >= this.items.length) return;
    const old = this.list.children[this.selected] as HTMLElement | undefined;
    old?.classList.remove("selected");
    old?.setAttribute("aria-selected", "false");
    this.selected = i;
    const row = this.list.children[i] as HTMLElement;
    row.classList.add("selected");
    row.setAttribute("aria-selected", "true");
    row.scrollIntoView?.({ block: "nearest" });
    this.input.setAttribute("aria-activedescendant", `qs-${i}`);
  }

  private async fit(): Promise<void> {
    await this.api.resize(Math.ceil(this.box.getBoundingClientRect().height));
  }

  private async open(i: number, reveal: boolean): Promise<void> {
    const it = this.items[i];
    if (!it) return;
    try {
      await (reveal ? this.api.revealFile(it.path) : this.api.openFile(it.path));
      await this.api.hide();
    } catch (e) {
      this.showNote(String(e));
      await this.fit();
    }
  }

  private onKey(e: KeyboardEvent): void {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        this.select(Math.min(this.items.length - 1, this.selected + 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        this.select(Math.max(0, this.selected - 1));
        break;
      case "Enter": {
        e.preventDefault();
        if (e.shiftKey) {
          if (this.input.value.trim()) void this.api.showAll(this.input.value);
          break;
        }
        const reveal = modKey(e);
        // Typed and pressed Enter before the results arrived: search first, then open.
        if (this.shownQuery !== this.input.value) void this.run().then(() => this.open(this.selected, reveal));
        else void this.open(this.selected, reveal);
        break;
      }
      case "Escape":
        e.preventDefault();
        if (this.input.value) {
          this.input.value = "";
          void this.run();
        } else {
          void this.api.hide();
        }
        break;
    }
  }
}
