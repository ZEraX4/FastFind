// In-browser demo backend: lets the UI run with `npm run dev` in a normal browser (no Tauri),
// for UI development and screenshots. Never used inside the desktop app.

import { emptyFilters, type Api, type IndexStatus, type Preview, type ResultItem, type SearchRequest, type SearchResponse, type Settings, type SnippetResult } from "./api";

const now = Math.floor(Date.now() / 1000);

const DOCS: (ResultItem & { body: string; loc: string; count: number })[] = [
  ["Q3 Board Report.docx", "C:\\Users\\Dana\\Documents\\Reports\\2026", "word", "docx", 184_320, 2, 12, "≈ Page 4", "The quarterly revenue grew by 12 percent while search latency dropped by approximately 40% after the index migration."],
  ["invoice-2026-0917.pdf", "C:\\Users\\Dana\\Documents\\Finance\\Invoices", "pdf", "pdf", 96_512, 7, 1, "Page 1", "Invoice INV-2026-0917 for Northwind Traders. Total due within 30 days of the invoice date."],
  ["budget-forecast.xlsx", "C:\\Users\\Dana\\Documents\\Finance", "spreadsheet", "xlsx", 58_880, 9, 3, "Forecast!C14", "Quarterly forecast\tQ1\tQ2\tQ3\tQ4 Revenue\t1.2M\t1.4M\t1.5M\t1.7M"],
  ["Roadmap 2027.pptx", "C:\\Users\\Dana\\Documents\\Planning", "presentation", "pptx", 2_404_352, 21, 2, "Slide 3 — Search performance", "Reduce search latency below 100 ms for indexes with one million documents."],
  ["meeting-notes.md", "C:\\Users\\Dana\\Notes", "text", "md", 4_210, 1, 5, "Line 18", "Action items: follow up on the invoice discrepancy and schedule the quarterly review."],
  ["contract-signed-scan.pdf", "C:\\Users\\Dana\\Documents\\Legal", "pdf", "pdf", 3_145_728, 60, 0, "", ""],
  ["search_engine.rs", "C:\\src\\fastfind\\crates\\core\\src", "code", "rs", 18_944, 0, 4, "Line 211", "fn search(&self, req: &SearchRequest) -> Result<SearchResponse> { // latency budget"],
  ["customers.csv", "C:\\Users\\Dana\\Documents\\Exports", "spreadsheet", "csv", 812_000, 12, 30, "Line 1204", "10442,Northwind Traders,invoice,2026-09-17,1840.00"],
].map(([name, dir, kind, ext, size, days, count, loc, body], i) => ({
  path: `${dir}\\${name}`,
  name: name as string,
  dir: dir as string,
  kind: kind as string,
  ext: ext as string,
  size: size as number,
  modified: now - (days as number) * 86400 - i * 3600,
  score: 10 - i,
  flags: name === "contract-signed-scan.pdf" ? 2 : 0,
  pages: kind === "pdf" ? 3 + i : kind === "presentation" ? 24 : null,
  body: body as string,
  loc: loc as string,
  count: count as number,
})) as (ResultItem & { body: string; loc: string; count: number })[];

function words(q: string): string[] {
  return q.toLowerCase().replace(/"/g, " ").split(/\s+/).filter((w) => w && !w.includes(":") && w !== "and" && w !== "or");
}

function spans(text: string, ws: string[]): [number, number][] {
  const out: [number, number][] = [];
  const lower = text.toLowerCase();
  for (const w of ws) {
    let i = lower.indexOf(w);
    while (i >= 0) {
      out.push([i, i + w.length]);
      i = lower.indexOf(w, i + w.length);
    }
  }
  return out.sort((a, b) => a[0] - b[0]);
}

let settings: Settings = {
  search: { defaultMode: "smart", caseSensitive: false, wholeWord: false, pageSize: 100, resultLimit: 10000, nameBoost: 3, metadataBoost: 1.5, phraseBoost: 2, verifyBudgetMs: 3000, regexBudgetMs: 10000 },
  indexing: {
    workerCount: 0, maxFileSizeMb: 1024, maxPdfSizeMb: 512, maxIndexedTextMb: 16, storedTextKb: 1024, includedExtensions: [], excludedExtensions: [],
    excludedDirs: [".git", "node_modules", "target", "bin", "obj", "build", "dist", ".cache", ".vscode", ".idea"], followSymlinks: false, indexHidden: false,
    indexSystem: false, indexAllFilenames: true, detectTextFiles: true, contentHash: true, watchChanges: true, rescanIntervalMin: 60, parseTimeoutSecs: 120,
    ocr: { enabled: false, tesseractPath: "", languages: "eng", images: false },
  },
  performance: { cpu: "balanced", memoryCacheMb: 256, background: "automatic" },
  appearance: { theme: "system", fontScale: 1, highContrast: false, showPreview: true },
  updates: { checkAutomatically: null },
};

export function demoApi(): Api {
  const match = (req: SearchRequest) => {
    const ws = words(req.query);
    let items = DOCS.filter((d) => ws.every((w) => (d.body + " " + d.name).toLowerCase().includes(w)));
    if (req.filters.kinds.length) items = items.filter((d) => req.filters.kinds.includes(d.kind));
    if (/ext:(\w+)/.test(req.query)) items = items.filter((d) => d.ext === /ext:(\w+)/.exec(req.query)![1]);
    return { items, ws };
  };
  return {
    async search(req): Promise<SearchResponse> {
      await new Promise((r) => setTimeout(r, 30));
      const { items } = match(req);
      return { items: items.slice(req.offset, req.offset + req.limit), total: items.length, totalIsLowerBound: false, offset: req.offset, elapsedMs: 3 + Math.random() * 5, strategy: "indexed", partial: false, warnings: [], generation: 1 };
    },
    async snippets(req, paths): Promise<SnippetResult[]> {
      const ws = words(req.query);
      return paths.map((p) => {
        const d = DOCS.find((x) => x.path === p)!;
        if (!d.body) return { path: p, snippets: [], matchCount: 0, matchCountCapped: false, note: null };
        return { path: p, snippets: [{ text: d.body, highlights: spans(d.body, ws), location: d.loc || null }], matchCount: Math.max(d.count, spans(d.body, ws).length), matchCountCapped: false, note: null };
      });
    },
    async preview(req, path): Promise<Preview> {
      const d = DOCS.find((x) => x.path === path)!;
      const ws = words(req.query);
      const sections = d.body
        ? [
            { text: `${d.body}\n\nThe index is updated incrementally: only new and changed files are parsed again, so ${d.name} stays searchable within seconds of being saved.`, highlights: spans(d.body, ws), location: d.loc || null, firstMatch: 0 },
          ]
        : [];
      let n = 0;
      for (const s of sections) {
        s.firstMatch = n;
        n += s.highlights.length;
      }
      return {
        path, name: d.name, kind: d.kind, size: d.size, modified: d.modified, flags: d.flags, pages: d.pages,
        title: d.kind === "word" ? "Q3 Board Report" : null, author: d.kind === "word" ? "Dana Lee" : null,
        totalMatches: n, matchesCapped: false, sections, head: d.body ? null : null,
        notes: d.flags & 2 ? ["Scanned document without a text layer — OCR is required to search its content (enable OCR in Settings → Indexing)."] : [],
      };
    },
    async status(): Promise<IndexStatus> {
      return {
        filesTotal: 1_284_392, indexed: 1_261_530, nameOnly: 20_372, skipped: 1_720, failed: 512, encrypted: 258, needsOcr: 1_204, ocrProblem: null,
        indexBytes: 2_576_980_378, textStoreBytes: 0, lastUpdated: now - 180,
        progress: { active: false, scanning: false, paused: false, discovered: 0, queued: 0, processed: 0, bytesProcessed: 0, filesPerSec: 0, percent: null, currentPath: null, ocrPending: 0 },
        roots: [
          { id: 1, path: "C:\\Users\\Dana\\Documents", fileCount: 412_880, lastScanAt: now - 600, watchStatus: "watching", scanning: false, available: true },
          { id: 2, path: "C:\\src", fileCount: 871_512, lastScanAt: now - 900, watchStatus: "watching", scanning: false, available: true },
        ],
        generation: 1,
      };
    },
    async addRoot(path) { return { id: 3, path, fileCount: 0, lastScanAt: null, watchStatus: "watching", scanning: true, available: true }; },
    async removeRoot() {},
    async rescan() {},
    async setPaused() {},
    async rebuild() {},
    async getSettings() { return structuredClone(settings); },
    async saveSettings(s) { settings = structuredClone(s); return s; },
    async problems() {
      return { items: [
        { path: "C:\\Users\\Dana\\Documents\\Legal\\nda-protected.docx", status: "encrypted", reason: "password protected or encrypted", size: 40_122, modified: now - 86400 * 40 },
        { path: "C:\\Users\\Dana\\Downloads\\broken.pdf", status: "failed", reason: "corrupt or malformed: FormatError", size: 1024, modified: now - 86400 * 3 },
      ], total: 2 };
    },
    async diagnostics() { return { version: "1.0.0 (demo)", dataDir: "(browser demo)", memoryRssBytes: 142_000_000, workerThreads: 12, storageRotational: false, pdfEngine: "PDFium", ocrEngine: null, indexSegments: 14, indexDocs: 1_284_392, queryCacheEntries: 12, queryCacheHits: 40, queryCacheMisses: 12, logDir: "(demo)" }; },
    async openFile() {},
    async revealFile() {},
    async openLogs() {},
    async supportedExtensions() { return ["pdf", "docx", "xlsx"]; },
    async checkOcr() { return { tesseract: "(demo)", version: "5.5.0", languages: ["eng"], missingLanguages: [], problem: null }; },
    async pickFolders() { return []; },
    onIndexUpdated() {},
  };
}

export const demoFilters = emptyFilters;
