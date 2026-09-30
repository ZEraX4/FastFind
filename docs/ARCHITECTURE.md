# FastFind — Architecture

FastFind is a local-first desktop tool for full-text file search. The user picks folders.
FastFind builds a persistent, incrementally maintained index of file contents: plain text,
code, HTML, JSON, XML, RTF, legacy and modern Office, OpenDocument, EPUB and PDF. Queries are
answered in milliseconds.

---

## 1. Technology comparison

| Criterion | **Rust + Tauri** | C++ + Qt | C#/.NET + Avalonia | Python + Qt | Electron + TS |
|---|---|---|---|---|---|
| CPU / parse speed | ★★★★★ native, no GC | ★★★★★ | ★★★★ JIT + GC | ★★ (GIL) | ★★★ |
| Concurrency | excellent (threads, channels, rayon; data-race freedom) | manual, error-prone | very good | poor | worker threads |
| Memory at idle | ~40 MB + system webview | ~30 MB | 80–100 MB | 120 MB+ | 250 MB+ |
| Full-text engine | **Tantivy** (Lucene-class, actively developed) | CLucene (stale), Xapian (GPL) | Lucene.NET 4.8 (perpetual beta) | Whoosh (slow) | in-RAM JS indexes |
| Legacy `.doc` / `.ppt` | no mature crate → targeted extractor on `cfb` | wvWare/antiword (C, GPL) | NPOI | textract (shells out) | none |
| `.xls` / `.xlsx` | calamine / streaming XML | QXlsx | NPOI | openpyxl | SheetJS |
| PDF | **PDFium** via pdfium-render | PDFium / Poppler | PdfPig | pdfminer | pdf.js |
| Safety with untrusted files | memory-safe | unsafe | memory-safe | memory-safe | memory-safe |
| UI | web UI in the system webview; tiny bundle | native, best | Skia-drawn | Qt | web UI, heavy runtime |
| Distribution | single native exe + system webview, ~10 MB | medium | 60–80 MB self-contained | fragile | ~150 MB |

### Decision: Rust + Tauri 2, with Tantivy, SQLite and PDFium

* **Speed and memory.** Scanning, parsing and indexing are CPU- and I/O-bound. Native code without
  a garbage collector gives the best throughput per core and a small resident footprint. The
  measurements are in §8.
* **Tantivy** is a Lucene-class inverted index: segments, block-compressed posting lists with
  positions, BM25, fast fields (columnar), FST term dictionary with automaton (regex/prefix)
  queries, memory-mapped files, and crash-safe commits.
* **Memory safety matters.** Every document is untrusted input. Rust parsers cannot corrupt
  memory. The one C++ component, PDFium, runs in helper processes (§3.3).
* **Tauri** delivers a polished, fully custom UI through the OS webview (WebView2, WKWebView,
  WebKitGTK) without shipping a browser. The heavy lifting stays in Rust, and the UI is
  plain TypeScript with no framework, about 18 KB gzipped.
* **The Rust ecosystem's gap is legacy Word and PowerPoint.** It is closed with two focused
  extractors built on the mature `cfb` (OLE2) crate, following Microsoft's published
  [MS-DOC]/[MS-PPT] specifications. They only extract text; they do not render or edit documents.
  Both are tested against files saved by Microsoft Office itself
  (`crates/fastfind-core/tests/fixtures`).

[MS-DOC]: https://learn.microsoft.com/openspecs/office_file_formats/ms-doc/
[MS-PPT]: https://learn.microsoft.com/openspecs/office_file_formats/ms-ppt/

---

## 2. Major technical risks

| # | Risk | Why it matters | Solution (implemented) | Trade-offs |
|---|---|---|---|---|
| R1 | **Legacy `.doc`** | Binary format with FIB, piece tables, fast-save, fields, sub-documents | `parsers/doc.rs`: FIB → CLX → PlcPcd piece table. Handles 8-bit and UTF-16 pieces. Hides field codes and keeps field results. Adds section anchors for footnotes, headers and footers, comments, endnotes and text boxes. Metadata comes from `SummaryInformation`. Encrypted or obfuscated files → "encrypted". Word 6/95 → unsupported, never guessed. Tested with real Word output. | No formatting; text boxes only when present in the CP stream. |
| R2 | **Legacy `.ppt`** | Record-based binary; slide order lives in a persist directory | `parsers/ppt.rs`: Current User → UserEdit chain → persist directory → SlideListWithText (presentation order) + text atoms inside each slide's drawing. Notes are matched to slides by slide id, titles come from TextHeaderAtom, and master placeholders are skipped. A damaged file falls back to a linear scan of all text atoms. | Charts and embedded OLE objects are not extracted. |
| R3 | **PDF** | CID/Type3 fonts, broken xref tables, huge files, scans, encryption; PDFium is C++ and single-threaded | **PDFium** loaded dynamically from the bundle and run in a **pool of helper processes** (the app's own exe with `--fastfind-pdf-worker`). This gives parallel parsing, crash and hang isolation (watchdog kill + respawn), and idle helpers exit after 60 s. Text is extracted page by page with page anchors. Password-protected files → "encrypted". Scanned documents (image pages without text) → flagged *Scanned PDF requiring OCR*, as distinct from *Text searchable PDF*. Optional OCR through a local Tesseract. The pure-Rust `pdf-extract` is the fallback when PDFium is missing. | Ships a 5–7 MB native library per platform. |
| R4 | **Search at 1M+ files** | Scanning every file per query is O(corpus) | Inverted index (Tantivy). Filters are indexed fields: extension/kind terms, a directory-prefix term, numeric ranges on size and mtime. Results are top-k with offset and an exact count. Ranking is deterministic, with a path-hash tie-break. LRU caches cover pages, snippets and compiled queries. | Index ≈ 25–30 % of source size for text-heavy corpora. |
| R5 | **Huge directory trees** | Millions of entries, deep nesting, unreadable folders | `fs/scanner.rs`: parallel workers over a shared directory priority queue (recently opened "hot" folders first, then depth-first). File type and Windows metadata come from the directory listing itself, with no extra `stat`. Exclusions are pruned before descent. A folder that fails to list is reported so its indexed children are **not** deleted. Symlinks and junctions are skipped by default; when following them, canonical paths prevent loops. | One thread on spinning disks. |
| R6 | **Huge files (100 MB–1 GB+)** | Loading a whole file blows up RAM | Text is **streamed** in 256 KB line-aligned chunks with incremental decoding. Every parser writes into a bounded `TextSink` (default 16 MB of text per file, plus a per-file time limit); truncation is flagged and shown in the UI. PDFium reads PDFs lazily from disk. Files ≥ 2 MB use a separate lane so small files are never starved. Snippets and previews of plain text are streamed from the original file. | Text beyond the cap is not searchable (configurable). |
| R7 | **Cross-platform FS behaviour** | Case sensitivity, separators, mtime granularity, watcher quirks | Paths are stored as the OS returns them. Case-folded "filter form" is used on Windows/macOS. Change detection uses size + mtime (ns), with an optional xxHash3 content hash for expensive formats. Watchers (`notify` → ReadDirectoryChangesW / FSEvents / inotify) are debounced, and each changed path is re-examined on disk. Queue overflows and watch-limit errors fall back to rescans, and a periodic reconcile scan runs as a safety net. Windows long paths work through Rust's std. | Network shares depend on the periodic rescan. |
| R8 | **Corruption / crash recovery** | Power loss mid-commit; three stores drifting apart | Commit order: **Tantivy → text store → catalog**. Every write is an idempotent path-keyed upsert, so a crash only re-indexes the last few seconds. On startup the index is verified (schema version, readable meta), the catalog is checked (`PRAGMA quick_check`) and the text store is checked. A damaged store is moved to `quarantine/`, rebuilt, and the others are reset so they stay consistent. Tested by corrupting `meta.json`. | Real corruption costs one re-index. |
| R9 | **Memory** | Big catalogs, unbounded queues, big documents | Bounded channels everywhere (back-pressure to the scanner). The scan diff uses a path-hash map (~40 B per file). Tantivy uses a fixed writer budget. The docstore holds only small metadata; text for snippets lives in a separate zstd store, capped at 1 MB per document. Plain-text formats are never duplicated. | Text files are re-read for snippets (fast; usually in the OS cache). |
| R10 | **UI responsiveness** | Any blocking call makes the UI stutter | Every command runs on Tauri's blocking pool, never on the UI or async runtime. Type-ahead is debounced (150 ms) and each new search **cancels** the previous one. The result list is virtualised and pages load lazily. Snippets load for visible rows only, and the preview loads lazily. Workers run at below-normal priority. | Snippets appear a few ms after the rows. |
| R11 | **Untrusted input / DoS** | Zip bombs, entity expansion, deep nesting, ReDoS, injection into the UI | Zip entry count, declared size and compression ratio are checked, **and** a byte-budget reader enforces the limit on actual output. XML never expands custom entities (tested). RTF group depth is limited; JSON uses a streaming lexer with no recursion; PPT record depth is limited. Rust's `regex` is linear-time and size-limited, so it has no catastrophic backtracking (tested). Each parser call is wrapped in `catch_unwind`. The UI renders document text only as text nodes (tested against HTML payloads). Strict CSP. Open and reveal only work for indexed paths. | Regex backreferences and lookaround are unsupported (they cannot run in linear time). |
| R12 | **HDD vs SSD** | Parallel random reads collapse on spinning disks | Seek-penalty probe (Windows `IOCTL_STORAGE_QUERY_PROPERTY`, Linux sysfs `rotational`). On HDD: one scanner thread and at most two parser workers. Worker count also scales with CPU preference and available RAM. | macOS assumes SSD. |

---

## 3. Component architecture

```
┌──────────────── UI (ui/, TypeScript, no framework, ~18 KB gz) ─────────────────┐
│ search box · modes · toggles · folder chips · filters · virtualised results    │
│ preview (match navigation) · settings · index panel · skipped files · help     │
└──────────────▲───────────────────────────────── Tauri IPC (JSON) ──────────────┘
               │ src-tauri: commands on the blocking pool, search cancellation,
               │ single-instance, "index-updated" events, open/reveal (indexed only)
┌──────────────┴──────────────── fastfind-core (Rust) ───────────────────────────┐
│ engine      facade: open/recover, roots, settings, status, search, preview     │
│ search      query syntax → AST → plan (Tantivy) · verified & regex execution    │
│             matcher (same tokenizer as the index) · snippets · LRU caches       │
│ index       catalog (SQLite) · store (Tantivy) · textstore (zstd/SQLite)        │
│             service: coordinator → scanner → differ → lanes → workers → writer  │
│ parsers     plain · html · xml · json · rtf · pdf (+ helper pool) · doc · docx  │
│             xls · xlsx · ppt · pptx · odt/ods/odp · epub · sniffing · zipsafe   │
│ fs          scanner · filter · watcher (debounced) · storage probe             │
│ ocr         Tesseract integration (background, idle-only)                      │
└────────────────────────────────────────────────────────────────────────────────┘
fastfind-cli: index · search · status · problems · gen-data · benchmark
```

### 3.1 Indexing pipeline

```
scan requests ─► coordinator (one scan at a time; newly added folders jump the queue)
                   │ parallel scanner (prunes exclusions, hot folders first)
                   ▼
                differ: catalog snapshot (path-hash → size, mtime, hash)
                   │ only new / modified files            after the scan: deletions + renames
        ┌──────────┼──────────────┐                        (same size+mtime → move; stored text reused)
        ▼          ▼              ▼
   small lane   large lane   pdf lane (only without helper processes)
        └────┬─────┘              │
      parser workers (N = f(CPU preference, cores, RAM, HDD))   PDF → helper-process pool
             └──────────────┬──────────────┘
                            ▼
                single writer ── every 2 s (early) / 10 s: Tantivy commit → text store → catalog
                            │
                            └─► "index-updated" event → UI offers refresh
watcher events ─► debounce 600 ms ─► re-examine path ─► same differ logic
```

* **Search works during indexing.** Commits are frequent, so already indexed files are
  searchable within seconds.
* **Priorities:** newly added folders first, recently opened folders first within a scan,
  small files before large ones (one worker is biased towards large files so they still
  progress). OCR runs only when everything else is idle.
* **Incremental:** at startup each folder is reconciled against the catalog, and only changed
  files are parsed. Touched-but-identical files (same size, different mtime) are detected by
  content hash and only have their timestamp updated.

### 3.2 Document fields (Tantivy)

| Field | Type | Purpose |
|---|---|---|
| `path` | raw, stored | upsert/delete key |
| `pid` | u64 fast | xxh3(path): deterministic tie-break, text-store key |
| `name` | text, stored | file-name tokens (boosted ×3) |
| `name_lc` | raw, fast | lowercase name: substring/glob search, name sort |
| `dir` | raw | case-folded parent + `/`: directory-prefix filter (`in:`, folder filter) |
| `path_t` | text | directory tokens (`path:`) |
| `ext`, `kind` | raw, stored | `ext:` / `type:` filters |
| `size`, `mtime` | u64 / i64, indexed + fast + stored | range filters, sorting |
| `content` | text with positions | full text, phrases |
| `meta` | text, stored | title/author/subject/keywords (boosted ×1.5) |
| `flags`, `pages`, `mode`, `info` | stored | UI badges, text-recovery mode, preview metadata |

**Analysis:** runs of alphanumeric characters form tokens (so `invoice-2024`, `my_var` and
`a.b` are found by their parts). CJK ideographs are indexed one per token, text is lowercased
and diacritics are folded (`Café` = `cafe`). The same tokenizer drives highlighting, so a
highlight always matches what the index matched.

### 3.3 Query execution

1. **Indexed** (Smart, File name): pure Tantivy. Each text leaf becomes
   `content ∨ name^3 ∨ meta^1.5`, phrases get a ×2 boost, and filters are constant-score. With
   *whole word* off, words of 3+ characters also match as prefixes (the exact term ranks higher).
2. **Verified** (case-sensitive, Exact): Tantivy returns a candidate superset in rank order.
   Batches of 256 are verified in parallel against the document text until the page is full.
   A session keeps the position so the next page continues where the last one stopped; the
   count is shown as "N+" until the candidates are exhausted.
3. **Scan** (Regex): the prefilter requires every literal the regex must contain (taken from
   the regex syntax tree) and scans those candidates within a time budget. Without a usable
   literal, the UI warns that the whole index is scanned.

### 3.4 Text recovery for snippets and preview

| Mode | Formats | Source |
|---|---|---|
| Raw | text, code, CSV, logs… | original file, streamed |
| Reparse | HTML, XML, JSON | re-extracted on demand |
| Stored | PDF, Office, ODF, EPUB, RTF | zstd blob in `text.db` (≤ 1 MB/doc) |

Location anchors recorded during extraction map a byte offset to `Page 3`, `≈ Page 2`
(DOCX), `Slide 4 — Title`, `Sheet!B7` (exact cell, including sparse rows and columns),
`Headers & footers`, or `Line 120` (plain text).

---

## 4. Storage layout

```
<data dir>  (Windows %LOCALAPPDATA%\FastFind\FastFind\data, macOS ~/Library/Application Support/app.FastFind.FastFind,
             Linux $XDG_DATA_HOME/fastfind; override with FASTFIND_DATA_DIR)
├── settings.json        user settings (atomic write)
├── catalog.db (+wal)    roots · files(path, size, mtime, hash, kind, status, reason, flags) · dir_hits
├── index/               Tantivy segments (memory-mapped)
├── text.db (+wal)       compressed extracted text of binary formats
├── logs/                JSON lines, daily rotation, 7 files; paths and errors only, never document text
└── quarantine/          damaged stores moved aside during automatic recovery
```

---

## 5. Concurrency and resource policy

| Knob | Default |
|---|---|
| Parser workers | Balanced: cores/2 (Low: cores/4 at background priority, High: cores−1); ≤ 2 on HDD or < 2 GB free RAM |
| Tantivy indexing threads | Balanced: cores/4 (2–6) |
| PDF helper processes | cores/4 (1–6), exit after 60 s idle |
| Queues | small 2048, large 64, pdf 512 items (bounded) |
| Writer memory | max(½ × memory setting, 24 MB × indexing threads) |
| Per-file limits | 1 GB file size, 512 MB PDF, 16 MB text indexed, 1 MB text stored, 120 s parse time |

---

## 6. Security

* Local only: no network access, no telemetry. OCR, if enabled, is a local program.
* Web view: strict CSP (`script-src 'self'`); no opener permission for the web view. The
  backend opens only paths present in the index. Document text is always inserted as text nodes.
* Parsers: size, time, depth, decompression and entity limits (R11); each call is isolated,
  and PDFium runs out of process.
* Single instance: a second launch focuses the running window, so there is only one index writer.

---

## 7. Future work

* **Automatic updates:** add `tauri-plugin-updater` with a signed static JSON manifest
  (GitHub Releases or any static host). Nothing in the app depends on an online service. The
  release workflow already produces the artifacts, and signing keys come from CI secrets.
* A Flatpak manifest (the AppImage and .deb are produced today).
* Extracting images embedded in Office documents for OCR.

---

## 8. Measured performance and profiling log

Measurements come from `fastfind-cli benchmark`, run on the development machine: 24-core CPU,
32 GB RAM, NVMe SSD, Windows 11. The query result cache is cleared before every timed run.
See the README for the full tables and how to reproduce them.

Optimisations were made only after the benchmark's stage timers identified a bottleneck:

| Observation | Cause | Change | Effect (100k mixed files) |
|---|---|---|---|
| Indexing at 9 % CPU | Only 2 Tantivy indexing threads, saturated | Indexing threads scale with CPU preference | 1,759 → 2,285 files/s (20k corpus) |
| Plain-text files read twice | Content hash computed for every file | Hash only formats that are expensive to parse | → 9,900 files/s (20k corpus, fallback PDF) |
| PDFium serialises every PDF | Not thread-safe | Helper-process pool (also crash isolation) | 1,558 → 7,231 files/s (20k corpus) |
| 100k corpus at 5 % CPU | Lost wake-up in the helper pool; parser workers blocked on PDFs | One mutex + condvar; dedicated PDF lane with one thread per helper | 108.7 s → 17.9 s |
| Peak RSS 1.1 GB | Unbounded writer queue; text blobs held until commit | Bounded writer queue; text flushed in 32 MB chunks; commit every 20k docs | 1.1 GB → 0.59 GB; 15.5 s |
| Startup 0.4–1 s | `PRAGMA quick_check` reads the whole catalog | Cheap header/version check + runtime corruption marker | 405 ms → 76 ms |
| Regex 153 ms | Prefilter chose an unselective literal | AND of all required literals | 153 → 21 ms (20k corpus) |
