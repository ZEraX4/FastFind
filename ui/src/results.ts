// Virtualised result list: only visible rows exist in the DOM, so 100k results scroll at 60 FPS.

import { Flags, type ResultItem, type SnippetResult } from "./api";
import { clear, h, highlighted } from "./dom";
import { bytes, date, kindBadge, num } from "./format";

const OVERSCAN = 6;
/** Row height in rem (kept in sync with --row-h in styles.css). */
const ROW_REM = 6;

export interface ResultsCallbacks {
  onSelect(index: number): void;
  onOpen(index: number): void;
  onNeedMore(): void;
  onVisible(first: number, last: number): void;
}

export class ResultsList {
  readonly el: HTMLDivElement;
  private spacer: HTMLDivElement;
  private items: ResultItem[] = [];
  private total = 0;
  private selected = -1;
  private snippets = new Map<string, SnippetResult>();
  private rows = new Map<number, HTMLElement>();
  private raf = 0;

  constructor(private cb: ResultsCallbacks) {
    this.spacer = h("div", { class: "results-spacer" });
    this.el = h("div", { class: "results", role: "listbox", tabindex: 0, "aria-label": "Search results" }, this.spacer);
    this.el.addEventListener("scroll", () => this.schedule(), { passive: true });
    if (typeof ResizeObserver !== "undefined") new ResizeObserver(() => this.schedule()).observe(this.el);
  }

  rowHeight(): number {
    const fs = parseFloat(getComputedStyle(document.documentElement).fontSize) || 14;
    return ROW_REM * fs;
  }

  setData(items: ResultItem[], total: number): void {
    this.items = items;
    this.total = total;
    this.selected = -1;
    this.snippets.clear();
    for (const r of this.rows.values()) r.remove();
    this.rows.clear();
    this.el.scrollTop = 0;
    this.render();
  }

  append(items: ResultItem[]): void {
    this.items = this.items.concat(items);
    this.render();
  }

  get length(): number {
    return this.items.length;
  }

  get totalCount(): number {
    return this.total;
  }

  item(i: number): ResultItem | undefined {
    return this.items[i];
  }

  selectedIndex(): number {
    return this.selected;
  }

  hasSnippet(path: string): boolean {
    return this.snippets.has(path);
  }

  setSnippets(list: SnippetResult[]): void {
    for (const s of list) this.snippets.set(s.path, s);
    // Re-render affected visible rows.
    for (const [i, row] of this.rows) {
      const it = this.items[i];
      if (it && list.some((s) => s.path === it.path)) {
        const fresh = this.buildRow(i);
        row.replaceWith(fresh);
        this.rows.set(i, fresh);
      }
    }
  }

  select(i: number, scroll = true): void {
    if (this.items.length === 0) return;
    const n = Math.max(0, Math.min(this.items.length - 1, i));
    const prev = this.rows.get(this.selected);
    prev?.classList.remove("selected");
    prev?.setAttribute("aria-selected", "false");
    this.selected = n;
    const row = this.rows.get(n);
    row?.classList.add("selected");
    row?.setAttribute("aria-selected", "true");
    this.el.setAttribute("aria-activedescendant", `result-${n}`);
    if (scroll) this.scrollTo(n);
    this.cb.onSelect(n);
    if (n >= this.items.length - 5) this.cb.onNeedMore();
  }

  scrollTo(i: number): void {
    const rh = this.rowHeight();
    const top = i * rh;
    const bottom = top + rh;
    if (top < this.el.scrollTop) this.el.scrollTop = top;
    else if (bottom > this.el.scrollTop + this.el.clientHeight) this.el.scrollTop = bottom - this.el.clientHeight;
    this.render();
  }

  pageSize(): number {
    return Math.max(1, Math.floor(this.el.clientHeight / this.rowHeight()) - 1);
  }

  private schedule(): void {
    if (this.raf) return;
    const raf = globalThis.requestAnimationFrame ?? ((f: FrameRequestCallback) => setTimeout(() => f(0), 16) as unknown as number);
    this.raf = raf(() => {
      this.raf = 0;
      this.render();
    });
  }

  render(): void {
    const rh = this.rowHeight();
    this.spacer.style.height = `${this.items.length * rh}px`;
    const first = Math.max(0, Math.floor(this.el.scrollTop / rh) - OVERSCAN);
    const visible = Math.ceil((this.el.clientHeight || 600) / rh);
    const last = Math.min(this.items.length - 1, Math.floor(this.el.scrollTop / rh) + visible + OVERSCAN);
    for (const [i, row] of this.rows) {
      if (i < first || i > last) {
        row.remove();
        this.rows.delete(i);
      }
    }
    for (let i = first; i <= last; i++) {
      if (!this.rows.has(i)) {
        const row = this.buildRow(i);
        this.rows.set(i, row);
        this.spacer.appendChild(row);
      }
    }
    if (this.items.length > 0) {
      this.cb.onVisible(first, last);
      if (last >= this.items.length - 10 && this.items.length < this.total) this.cb.onNeedMore();
    }
  }

  private buildRow(i: number): HTMLElement {
    const it = this.items[i];
    const sn = this.snippets.get(it.path);
    const badge = kindBadge(it.kind, it.ext);
    const tags: HTMLElement[] = [];
    if (it.flags & Flags.NEEDS_OCR) tags.push(h("span", { class: "tag warn", title: "Scanned document — OCR required to search its content" }, "Scanned · OCR needed"));
    if (it.flags & Flags.ENCRYPTED) tags.push(h("span", { class: "tag warn", title: "Password protected — only the file name is indexed" }, "Locked"));
    if (it.flags & Flags.FAILED) tags.push(h("span", { class: "tag warn", title: "Content could not be read — only the file name is indexed" }, "Unreadable"));
    if (it.flags & Flags.OCR) tags.push(h("span", { class: "tag", title: "Text recognised by OCR" }, "OCR"));
    const count = sn && sn.matchCount > 0 ? h("span", { class: "count" }, `${num(sn.matchCount)}${sn.matchCountCapped ? "+" : ""} ${sn.matchCount === 1 ? "match" : "matches"}`) : null;
    const snippetEl = h("div", { class: "snippet" });
    const s0 = sn?.snippets[0];
    if (s0) {
      if (s0.location) snippetEl.appendChild(h("span", { class: "loc" }, s0.location));
      snippetEl.appendChild(highlighted(s0.text, s0.highlights));
    } else if (sn?.note) {
      snippetEl.appendChild(h("span", { class: "muted" }, sn.note));
    } else if (!sn && !(it.flags & Flags.NAME_ONLY)) {
      snippetEl.appendChild(h("span", { class: "skeleton" }));
    }
    const row = h(
      "div",
      {
        class: `row${i === this.selected ? " selected" : ""}`,
        role: "option",
        id: `result-${i}`,
        "aria-selected": i === this.selected ? "true" : "false",
        "aria-label": `${it.name}, ${it.dir}`,
        "data-index": i,
      },
      h("span", { class: `badge ${badge.cls}`, "aria-hidden": "true" }, badge.label),
      h(
        "div",
        { class: "row-main" },
        h("div", { class: "row-top" }, h("span", { class: "name", title: it.name }, it.name), ...tags, h("span", { class: "grow" }), count),
        h("div", { class: "row-path", title: it.path }, it.dir),
        snippetEl,
      ),
      h("div", { class: "row-meta" }, h("span", {}, date(it.modified)), h("span", {}, bytes(it.size))),
    );
    row.style.transform = `translateY(${i * ROW_REM}rem)`;
    row.addEventListener("mousedown", () => this.select(i, false));
    row.addEventListener("dblclick", () => this.cb.onOpen(i));
    return row;
  }

  reset(): void {
    this.setData([], 0);
    clear(this.spacer);
    this.rows.clear();
  }
}
