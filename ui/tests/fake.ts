// Shared in-memory Api for UI tests.
import type { Api, IndexStatus, Preview, ResultItem, RootInfo, SearchRequest, SearchResponse, Settings, SnippetResult, UpdateApi, WindowControls } from "../src/api";

export const settings = (): Settings => ({
  search: { defaultMode: "smart", caseSensitive: false, wholeWord: false, pageSize: 50, resultLimit: 10000, nameBoost: 3, metadataBoost: 1.5, phraseBoost: 2, verifyBudgetMs: 3000, regexBudgetMs: 10000 },
  indexing: {
    workerCount: 0, maxFileSizeMb: 1024, maxPdfSizeMb: 512, maxIndexedTextMb: 16, storedTextKb: 1024, includedExtensions: [], excludedExtensions: [],
    excludedDirs: ["node_modules"], followSymlinks: false, indexHidden: false, indexSystem: false, indexAllFilenames: true, detectTextFiles: true,
    contentHash: true, watchChanges: true, rescanIntervalMin: 60, parseTimeoutSecs: 120, ocr: { enabled: false, tesseractPath: "", languages: "eng", images: false },
  },
  performance: { cpu: "balanced", memoryCacheMb: 256, background: "automatic" },
  appearance: { theme: "system", fontScale: 1, highContrast: false, showPreview: true },
  updates: { checkAutomatically: null },
});

export function item(i: number, name: string, extra: Partial<ResultItem> = {}): ResultItem {
  return { path: `/docs/${name}`, name, dir: "/docs", ext: name.split(".").pop() ?? "", kind: "text", size: 1000 + i, modified: 1_760_000_000, score: 1, flags: 0, pages: null, ...extra };
}

export class FakeApi implements Api {
  window?: WindowControls;
  updates?: UpdateApi;
  docs: ResultItem[] = [item(0, "invoice-2026.txt"), item(1, "report.pdf", { kind: "pdf", ext: "pdf" }), item(2, "notes.md"), item(3, "<img src=x onerror=alert(1)>.txt")];
  roots: RootInfo[] = [{ id: 1, path: "/docs", fileCount: 4, lastScanAt: 1_760_000_000, watchStatus: "watching", scanning: false, available: true }];
  requests: SearchRequest[] = [];
  opened: string[] = [];
  revealed: string[] = [];
  saved: Settings[] = [];
  added: string[] = [];
  pick: string[] = ["/new/folder"];
  cfg = settings();
  indexListener: ((g: number) => void) | null = null;

  async search(req: SearchRequest): Promise<SearchResponse> {
    this.requests.push(structuredClone(req));
    const q = req.query.toLowerCase().replace(/"/g, "");
    let items = this.docs.filter((d) => !q || d.name.toLowerCase().includes(q.split(" ")[0]));
    if (req.filters.kinds.length) items = items.filter((d) => req.filters.kinds.includes(d.kind));
    return { items: items.slice(req.offset, req.offset + req.limit), total: items.length, totalIsLowerBound: false, offset: req.offset, elapsedMs: 1.2, strategy: "indexed", partial: false, warnings: [], generation: 1 };
  }
  async snippets(_req: SearchRequest, paths: string[]): Promise<SnippetResult[]> {
    return paths.map((p) => ({ path: p, snippets: [{ text: "…the <b>invoice</b> total was approved…", highlights: [[8, 15]], location: "Line 3" }], matchCount: 2, matchCountCapped: false, note: null }));
  }
  async preview(_req: SearchRequest, path: string): Promise<Preview> {
    return {
      path, name: path.split("/").pop()!, kind: "text", size: 1000, modified: 1_760_000_000, flags: 0, pages: null, title: null, author: null,
      totalMatches: 3, matchesCapped: false, notes: [], head: null,
      sections: [
        { text: "first invoice here", highlights: [[6, 13]], location: "Line 1", firstMatch: 0 },
        { text: "invoice again and invoice", highlights: [[0, 7], [18, 25]], location: "Line 9", firstMatch: 1 },
      ],
    };
  }
  async status(): Promise<IndexStatus> {
    return {
      filesTotal: 4, indexed: 3, nameOnly: 1, skipped: 0, failed: 0, encrypted: 0, needsOcr: 0, indexBytes: 2048, textStoreBytes: 0, lastUpdated: Date.now() / 1000 - 180,
      progress: { active: false, scanning: false, paused: false, discovered: 0, queued: 0, processed: 0, bytesProcessed: 0, filesPerSec: 0, percent: null, currentPath: null, ocrPending: 0 },
      roots: this.roots, generation: 1,
    };
  }
  async addRoot(path: string) {
    this.added.push(path);
    const r = { id: 2, path, fileCount: 0, lastScanAt: null, watchStatus: "watching", scanning: true, available: true };
    this.roots = [...this.roots, r];
    return r;
  }
  async removeRoot() {}
  async rescan() {}
  async setPaused() {}
  async rebuild() {}
  async getSettings() { return structuredClone(this.cfg); }
  async saveSettings(s: Settings) { this.saved.push(structuredClone(s)); this.cfg = s; return s; }
  async problems() { return { items: [], total: 0 }; }
  async diagnostics() { return { version: "1.0.0", dataDir: "/data", memoryRssBytes: 1, workerThreads: 4, storageRotational: false, pdfEngine: "PDFium", ocrEngine: null, indexSegments: 1, indexDocs: 4, queryCacheEntries: 0, queryCacheHits: 0, queryCacheMisses: 0, logDir: "/data/logs" }; }
  async openFile(p: string) { this.opened.push(p); }
  async revealFile(p: string) { this.revealed.push(p); }
  async openLogs() {}
  async supportedExtensions() { return ["txt", "pdf"]; }
  async pickFolders() { return this.pick; }
  onIndexUpdated(cb: (g: number) => void) { this.indexListener = cb; }
}

