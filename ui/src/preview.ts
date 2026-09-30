// Preview panel: metadata, notes, match-centred sections with highlight navigation.

import { Flags, type Preview, type ResultItem } from "./api";
import { append, clear, h, highlighted, icon, modLabel } from "./dom";
import { bytes, dateTime, kindBadge, num } from "./format";
import { I } from "./icons";

export interface PreviewCallbacks {
  onOpen(path: string): void;
  onReveal(path: string): void;
  onCopy(path: string): void;
}

export class PreviewPane {
  readonly el: HTMLElement;
  private header: HTMLElement;
  private nav: HTMLElement;
  private body: HTMLElement;
  private current = 0;
  private total = 0;
  private preview: Preview | null = null;
  private counter: HTMLElement;
  private locLabel: HTMLElement;

  constructor(private cb: PreviewCallbacks) {
    this.header = h("div", { class: "pv-header" });
    this.counter = h("span", { class: "pv-counter", "aria-live": "polite" });
    this.locLabel = h("span", { class: "pv-loc" });
    const prev = h("button", { class: "icon-btn", title: "Previous match (Shift+F3)", "aria-label": "Previous match", onclick: () => this.step(-1) }, icon(I.chevUp));
    const next = h("button", { class: "icon-btn", title: "Next match (F3)", "aria-label": "Next match", onclick: () => this.step(1) }, icon(I.chevDown));
    this.nav = h("div", { class: "pv-nav", hidden: true }, prev, next, this.counter, this.locLabel);
    this.body = h("div", { class: "pv-body", tabindex: 0, "aria-label": "Document preview" });
    this.el = h("aside", { class: "preview", role: "region", "aria-label": "Preview" }, this.header, this.nav, this.body);
    this.empty();
  }

  empty(message = "Select a result to preview it here."): void {
    this.preview = null;
    clear(this.header);
    clear(this.body);
    this.nav.hidden = true;
    this.body.appendChild(h("div", { class: "pv-empty" }, message));
  }

  loading(item: ResultItem): void {
    this.renderHeader(item.name, item.path, item.kind, item.ext, item.size, item.modified, item.pages, item.flags, null, null);
    clear(this.body);
    this.nav.hidden = true;
    this.body.appendChild(h("div", { class: "pv-loading" }, h("span", { class: "spinner", "aria-hidden": "true" }), "Loading preview…"));
  }

  error(item: ResultItem, message: string): void {
    this.renderHeader(item.name, item.path, item.kind, item.ext, item.size, item.modified, item.pages, item.flags, null, null);
    clear(this.body);
    this.nav.hidden = true;
    this.body.appendChild(h("div", { class: "pv-note warn" }, icon(I.alert), message));
  }

  show(p: Preview, ext: string): void {
    this.preview = p;
    this.renderHeader(p.name, p.path, p.kind, ext, p.size, p.modified, p.pages, p.flags, p.title, p.author);
    clear(this.body);
    for (const n of p.notes) this.body.appendChild(h("div", { class: "pv-note" }, icon(I.alert), n));
    this.total = p.totalMatches;
    this.current = 0;
    if (p.sections.length > 0) {
      for (const s of p.sections) {
        const sec = h("section", { class: "pv-section" });
        if (s.location) sec.appendChild(h("div", { class: "pv-sec-loc" }, s.location));
        const pre = h("div", { class: "pv-text" });
        pre.appendChild(highlighted(s.text, s.highlights, undefined, s.firstMatch));
        sec.appendChild(pre);
        this.body.appendChild(sec);
      }
      if (p.matchesCapped) this.body.appendChild(h("div", { class: "pv-note" }, `Showing the first ${num(p.totalMatches)} matches.`));
      this.nav.hidden = false;
      this.focusMatch(0, false);
    } else {
      this.nav.hidden = true;
      if (p.head) {
        this.body.appendChild(h("div", { class: "pv-sec-loc" }, "Beginning of document"));
        this.body.appendChild(h("div", { class: "pv-text" }, p.head));
      } else if (p.notes.length === 0) {
        this.body.appendChild(h("div", { class: "pv-empty" }, "No text to preview."));
      }
    }
  }

  private renderHeader(name: string, path: string, kind: string, ext: string, size: number, modified: number, pages: number | null, flags: number, title: string | null, author: string | null): void {
    clear(this.header);
    const badge = kindBadge(kind, ext);
    const meta: string[] = [bytes(size), `Modified ${dateTime(modified)}`];
    if (pages) meta.push(kind === "presentation" ? `${pages} slides` : kind === "spreadsheet" ? `${pages} sheets` : `${pages} pages`);
    const isPdf = kind === "pdf";
    const pdfKind = isPdf ? (flags & Flags.NEEDS_OCR ? "Scanned PDF requiring OCR" : flags & Flags.OCR ? "Scanned PDF (OCR text)" : "Text searchable PDF") : null;
    append(
      this.header,
      h("div", { class: "pv-title-row" }, h("span", { class: `badge ${badge.cls}`, "aria-hidden": "true" }, badge.label), h("h2", { class: "pv-name", title: name }, name)),
      h("div", { class: "pv-path", title: path }, path),
      h("div", { class: "pv-meta" }, meta.join(" · "), pdfKind ? h("span", { class: `tag${flags & Flags.NEEDS_OCR ? " warn" : ""}` }, pdfKind) : null),
      title || author ? h("div", { class: "pv-docmeta" }, title ? `“${title}”` : "", title && author ? " — " : "", author ?? "") : null,
      h(
        "div",
        { class: "pv-actions" },
        h("button", { class: "btn primary", title: "Open (Enter)", onclick: () => this.cb.onOpen(path) }, icon(I.external), "Open"),
        h("button", { class: "btn", title: `Show in folder (${modLabel}+Enter)`, onclick: () => this.cb.onReveal(path) }, icon(I.reveal), "Show in folder"),
        h("button", { class: "btn ghost", title: `Copy path (${modLabel}+Shift+C)`, onclick: () => this.cb.onCopy(path) }, "Copy path"),
      ),
    );
  }

  /** Move to the next/previous match (wraps around). */
  step(delta: number): void {
    if (!this.preview || this.total === 0) return;
    this.focusMatch((this.current + delta + this.total) % this.total, true);
  }

  private focusMatch(i: number, scroll: boolean): void {
    this.body.querySelector("mark.current")?.classList.remove("current");
    const m = this.body.querySelector<HTMLElement>(`mark[data-match="${i}"]`);
    this.current = i;
    this.counter.textContent = this.total > 0 ? `${num(i + 1)} / ${num(this.total)}${this.preview?.matchesCapped ? "+" : ""}` : "";
    if (m) {
      m.classList.add("current");
      const loc = m.closest(".pv-section")?.querySelector(".pv-sec-loc")?.textContent ?? "";
      this.locLabel.textContent = loc;
      if (scroll) m.scrollIntoView?.({ block: "center" });
    }
  }

  currentMatch(): number {
    return this.current;
  }
}
