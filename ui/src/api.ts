// Typed bridge to the Rust backend. The UI depends only on the `Api` interface, so tests can
// supply an in-memory implementation.

import { isMac } from "./dom";

export type SearchMode = "smart" | "exact" | "regex" | "filename";
export type SortOrder = "relevance" | "modified" | "size" | "name";

export interface SearchFilters {
  exts: string[];
  kinds: string[];
  dirs: string[];
  modifiedAfter: number | null;
  modifiedBefore: number | null;
  sizeMin: number | null;
  sizeMax: number | null;
}

export interface SearchRequest {
  query: string;
  mode: SearchMode;
  caseSensitive: boolean;
  wholeWord: boolean;
  filters: SearchFilters;
  sort: SortOrder;
  offset: number;
  limit: number;
}

export interface ResultItem {
  path: string;
  name: string;
  dir: string;
  ext: string;
  kind: string;
  size: number;
  modified: number;
  score: number;
  flags: number;
  pages: number | null;
}

export interface SearchResponse {
  items: ResultItem[];
  total: number;
  totalIsLowerBound: boolean;
  offset: number;
  elapsedMs: number;
  strategy: "indexed" | "verified" | "scan";
  partial: boolean;
  warnings: string[];
  generation: number;
}

export interface Snippet {
  text: string;
  highlights: [number, number][];
  location: string | null;
}

export interface SnippetResult {
  path: string;
  snippets: Snippet[];
  matchCount: number;
  matchCountCapped: boolean;
  note: string | null;
}

export interface PreviewSection {
  text: string;
  highlights: [number, number][];
  location: string | null;
  firstMatch: number;
}

export interface Preview {
  path: string;
  name: string;
  kind: string;
  size: number;
  modified: number;
  flags: number;
  pages: number | null;
  title: string | null;
  author: string | null;
  totalMatches: number;
  matchesCapped: boolean;
  sections: PreviewSection[];
  head: string | null;
  notes: string[];
}

export interface RootInfo {
  id: number;
  path: string;
  fileCount: number;
  lastScanAt: number | null;
  watchStatus: string;
  scanning: boolean;
  available: boolean;
}

export interface IndexProgress {
  active: boolean;
  scanning: boolean;
  paused: boolean;
  discovered: number;
  queued: number;
  processed: number;
  bytesProcessed: number;
  filesPerSec: number;
  percent: number | null;
  currentPath: string | null;
  ocrPending: number;
}

export interface IndexStatus {
  filesTotal: number;
  indexed: number;
  nameOnly: number;
  skipped: number;
  failed: number;
  encrypted: number;
  needsOcr: number;
  /** OCR is on but cannot run (Tesseract missing, language not installed). */
  ocrProblem: string | null;
  indexBytes: number;
  textStoreBytes: number;
  lastUpdated: number | null;
  progress: IndexProgress;
  roots: RootInfo[];
  generation: number;
}

export interface SkippedFile {
  path: string;
  status: string;
  reason: string | null;
  size: number;
  modified: number;
}

export interface Page<T> {
  items: T[];
  total: number;
}

export interface Diagnostics {
  version: string;
  dataDir: string;
  memoryRssBytes: number;
  workerThreads: number;
  storageRotational: boolean | null;
  pdfEngine: string;
  ocrEngine: string | null;
  indexSegments: number;
  indexDocs: number;
  queryCacheEntries: number;
  queryCacheHits: number;
  queryCacheMisses: number;
  logDir: string;
}

export interface Settings {
  search: {
    defaultMode: SearchMode;
    caseSensitive: boolean;
    wholeWord: boolean;
    pageSize: number;
    resultLimit: number;
    nameBoost: number;
    metadataBoost: number;
    phraseBoost: number;
    verifyBudgetMs: number;
    regexBudgetMs: number;
  };
  indexing: {
    workerCount: number;
    maxFileSizeMb: number;
    maxPdfSizeMb: number;
    maxIndexedTextMb: number;
    storedTextKb: number;
    includedExtensions: string[];
    excludedExtensions: string[];
    excludedDirs: string[];
    followSymlinks: boolean;
    indexHidden: boolean;
    indexSystem: boolean;
    indexAllFilenames: boolean;
    detectTextFiles: boolean;
    contentHash: boolean;
    watchChanges: boolean;
    rescanIntervalMin: number;
    parseTimeoutSecs: number;
    ocr: { enabled: boolean; tesseractPath: string; languages: string; images: boolean };
  };
  performance: {
    cpu: "low" | "balanced" | "high";
    memoryCacheMb: number;
    background: "automatic" | "manual" | "paused";
  };
  appearance: {
    theme: "system" | "light" | "dark";
    fontScale: number;
    highContrast: boolean;
    showPreview: boolean;
  };
  updates: {
    /** null until the user has been asked; no network request is made before that. */
    checkAutomatically: boolean | null;
  };
}

export interface OcrSetup {
  tesseract: string | null;
  version: string | null;
  languages: string[];
  missingLanguages: string[];
  /** Why OCR cannot run; null when ready. */
  problem: string | null;
}

export interface UpdateInfo {
  version: string;
  currentVersion: string;
  notes: string | null;
  date: string | null;
  /** False for Linux package-manager installs: offer the download page instead. */
  canInstall: boolean;
}

/** Signed in-app updates (desktop app only; the network request happens in the backend). */
export interface UpdateApi {
  check(): Promise<UpdateInfo | null>;
  /** Downloads, verifies and installs the update found by the last check, then restarts. */
  install(): Promise<void>;
  openReleasePage(): Promise<void>;
  onAvailable(cb: (info: UpdateInfo) => void): void;
  onProgress(cb: (downloaded: number, total: number | null) => void): void;
  /** Installing failed after the index was closed; the app restarts the current version. */
  onRestarting(cb: (error: string) => void): void;
}

export interface Api {
  search(req: SearchRequest): Promise<SearchResponse>;
  snippets(req: SearchRequest, paths: string[]): Promise<SnippetResult[]>;
  preview(req: SearchRequest, path: string): Promise<Preview>;
  status(): Promise<IndexStatus>;
  addRoot(path: string): Promise<RootInfo>;
  removeRoot(id: number): Promise<void>;
  rescan(id?: number): Promise<void>;
  setPaused(paused: boolean): Promise<void>;
  rebuild(): Promise<void>;
  getSettings(): Promise<Settings>;
  saveSettings(s: Settings): Promise<Settings>;
  problems(status: string | null, query: string, offset: number, limit: number): Promise<Page<SkippedFile>>;
  diagnostics(): Promise<Diagnostics>;
  openFile(path: string): Promise<void>;
  revealFile(path: string): Promise<void>;
  openLogs(): Promise<void>;
  supportedExtensions(): Promise<string[]>;
  /** Checks OCR settings (possibly unsaved): Tesseract found and working, languages installed. */
  checkOcr(ocr: Settings["indexing"]["ocr"]): Promise<OcrSetup>;
  pickFolders(): Promise<string[]>;
  onIndexUpdated(cb: (generation: number) => void): void;
  /** Present in the desktop app, whose window has no native title bar. */
  window?: WindowControls;
  /** Present in the desktop app only. */
  updates?: UpdateApi;
}

export interface WindowControls {
  /** macOS draws its own traffic lights over the header; other platforms need our buttons. */
  nativeButtons: boolean;
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  isMaximized(): Promise<boolean>;
  onResized(cb: () => void): void;
}

/// Result flags (mirror of `fastfind_core::model::flags`).
export const Flags = {
  TRUNCATED: 1,
  NEEDS_OCR: 2,
  OCR: 4,
  NAME_ONLY: 8,
  ENCRYPTED: 16,
  FAILED: 32,
  APPROX_PAGES: 64,
} as const;

export function emptyFilters(): SearchFilters {
  return { exts: [], kinds: [], dirs: [], modifiedAfter: null, modifiedBefore: null, sizeMin: null, sizeMax: null };
}

/** Production implementation over Tauri IPC. Imported lazily so tests never load Tauri. */
export async function tauriApi(): Promise<Api> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const dialog = await import("@tauri-apps/plugin-dialog");
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const win = getCurrentWindow();
  return {
    search: (req) => invoke("search", { req }),
    snippets: (req, paths) => invoke("snippets", { req, paths }),
    preview: (req, path) => invoke("preview", { req, path }),
    status: () => invoke("status"),
    addRoot: (path) => invoke("add_root", { path }),
    removeRoot: (id) => invoke("remove_root", { id }),
    rescan: (id) => invoke("rescan", { id: id ?? null }),
    setPaused: (paused) => invoke("set_paused", { paused }),
    rebuild: () => invoke("rebuild_index"),
    getSettings: () => invoke("get_settings"),
    saveSettings: (settings) => invoke("save_settings", { settings }),
    problems: (status, query, offset, limit) => invoke("problems", { status, query, offset, limit }),
    diagnostics: () => invoke("diagnostics"),
    openFile: (path) => invoke("open_file", { path }),
    revealFile: (path) => invoke("reveal_file", { path }),
    openLogs: () => invoke("open_logs"),
    supportedExtensions: () => invoke("supported_extensions"),
    checkOcr: (ocr) => invoke("check_ocr", { ocr }),
    pickFolders: async () => {
      const r = await dialog.open({ directory: true, multiple: true, title: "Add folders to index" });
      if (r === null) return [];
      return Array.isArray(r) ? r : [r];
    },
    onIndexUpdated: (cb) => {
      void listen<number>("index-updated", (e) => cb(e.payload));
    },
    updates: {
      check: () => invoke("check_for_update"),
      install: () => invoke("install_update"),
      openReleasePage: () => invoke("open_release_page"),
      onAvailable: (cb) => {
        void listen<UpdateInfo>("update-available", (e) => cb(e.payload));
      },
      onProgress: (cb) => {
        void listen<{ downloaded: number; total: number | null }>("update-progress", (e) => cb(e.payload.downloaded, e.payload.total));
      },
      onRestarting: (cb) => {
        void listen<string>("update-failed-restarting", (e) => cb(e.payload));
      },
    },
    window: {
      nativeButtons: isMac,
      minimize: () => win.minimize(),
      toggleMaximize: () => win.toggleMaximize(),
      close: () => win.close(),
      isMaximized: () => win.isMaximized(),
      onResized: (cb) => {
        void win.onResized(() => cb());
      },
    },
  };
}
