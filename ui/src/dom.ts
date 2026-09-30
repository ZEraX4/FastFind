// Minimal DOM helpers. Document-derived text is ALWAYS inserted as text nodes, never as HTML:
// indexed files are untrusted input and must not be able to inject markup into the UI.

type Attrs = Record<string, string | number | boolean | null | undefined | EventListener>;
type Child = Node | string | null | undefined | false;

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Attrs = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined || v === false) continue;
    if (k.startsWith("on") && typeof v === "function") {
      el.addEventListener(k.slice(2).toLowerCase(), v as EventListener);
    } else if (k === "class") {
      el.className = String(v);
    } else if (v === true) {
      el.setAttribute(k, "");
    } else {
      el.setAttribute(k, String(v));
    }
  }
  append(el, ...children);
  return el;
}

export function append(el: Node, ...children: Child[]): void {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    el.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
  }
}

export function clear(el: Element): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

/** Static, app-authored SVG icons only (never document content). */
export function icon(svg: string, cls = "icon"): HTMLSpanElement {
  const s = document.createElement("span");
  s.className = cls;
  s.setAttribute("aria-hidden", "true");
  s.innerHTML = svg;
  return s;
}

/**
 * Render `text` with `<mark>` around highlight ranges (UTF-16 offsets, as produced by the
 * backend). `markClass(i)` lets callers tag individual matches (e.g. the current one).
 */
export function highlighted(text: string, ranges: [number, number][], markClass?: (i: number) => string, firstIndex = 0): DocumentFragment {
  const frag = document.createDocumentFragment();
  let pos = 0;
  const sorted = [...ranges].map((r, i) => ({ r, i })).sort((a, b) => a.r[0] - b.r[0]);
  for (const { r, i } of sorted) {
    const [s, e] = r;
    if (s < pos || e <= s || s > text.length) continue;
    if (s > pos) frag.appendChild(document.createTextNode(text.slice(pos, s)));
    const m = document.createElement("mark");
    m.textContent = text.slice(s, Math.min(e, text.length));
    m.dataset.match = String(firstIndex + i);
    if (markClass) m.className = markClass(firstIndex + i);
    frag.appendChild(m);
    pos = Math.min(e, text.length);
  }
  if (pos < text.length) frag.appendChild(document.createTextNode(text.slice(pos)));
  return frag;
}

export function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number): ((...a: A) => void) & { cancel(): void } {
  let t: ReturnType<typeof setTimeout> | undefined;
  const d = (...a: A) => {
    if (t) clearTimeout(t);
    t = setTimeout(() => fn(...a), ms);
  };
  d.cancel = () => t && clearTimeout(t);
  return d;
}

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/** Platform modifier: Cmd on macOS, Ctrl elsewhere. */
export function modKey(e: KeyboardEvent | MouseEvent): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}

export const modLabel = isMac ? "⌘" : "Ctrl";
