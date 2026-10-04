# FastFind

Fast, local full-text search for your files. FastFind indexes the text inside documents, PDFs,
spreadsheets, presentations, e-books, web pages and source code, then answers queries in
milliseconds. It runs entirely on your computer: no cloud, no telemetry. The only network
request it ever makes is an optional, opt-in update check.

<p align="center">
 <img width="1603" height="1027" alt="image" src="https://github.com/user-attachments/assets/692810db-1637-4612-8d4c-5db526afc97d" />
</p>

* **Formats:** TXT, Markdown, CSV/TSV, logs, 150+ source/config types, HTML, XML, JSON, RTF,
  **DOC, DOCX, XLS, XLSX, PPT, PPTX**, ODT/ODS/ODP, EPUB and **PDF** (via PDFium). Scanned PDFs
  are detected and can be OCR'd with a local Tesseract.
* **Search:** words, `"phrases"`, `AND` / `OR` / `NOT`, `-exclusions`, `(grouping)`, `prefix*`,
  plus filters `filename:` `ext:` `type:` `path:` `in:` `modified:` `size:`. Case-sensitive,
  whole-word, exact-string and regex modes.
* **Results:** snippets with highlighted matches and exact locations (*Page 3*,
  *Slide 4 — Roadmap*, *Budget!B7*, *Line 120*), a preview with match-by-match navigation,
  and open / show in folder.
* **Index:** persistent and incremental. At startup only changed files are re-parsed.
  Live file watching keeps it current. Search works while indexing runs.
* **Window:** frameless, with the header acting as the title bar (drag to move, double-click to
  maximise, own minimise/maximise/close buttons). macOS keeps its native traffic lights over a
  hidden title bar (`src-tauri/tauri.macos.conf.json`).
* **Robust:** damaged, encrypted, huge or malicious files never stop indexing. They are listed
  under *Skipped files* with the reason.

**Website and live demo:** https://zerax4.github.io/FastFind/

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design, risks and trade-offs.

---

## Repository layout

```
FastFind/
├── crates/
│   ├── fastfind-core/        engine library (no UI)
│   │   ├── src/
│   │   │   ├── engine.rs     facade used by the app and the CLI
│   │   │   ├── search/       query syntax, planner, execution, matcher, snippets
│   │   │   ├── index/        catalog (SQLite), store (Tantivy), text store, pipeline
│   │   │   ├── parsers/      one module per format + sniffing, zip-bomb guard, PDF helpers
│   │   │   ├── fs/           scanner, filters, watcher, SSD/HDD probe
│   │   │   ├── preview.rs · textsource.rs · ocr.rs · config.rs · logging.rs · gen.rs
│   │   └── tests/            integration, real-Office fixtures, PDF engine tests
│   └── fastfind-cli/         `fastfind-cli` binary: index, search, benchmark, gen-data
├── src-tauri/                desktop shell (Tauri 2): commands, config, icons, bundled PDFium
├── ui/                       TypeScript UI (no framework) + vitest/jsdom UI tests
├── scripts/                  fetch-pdfium.{sh,ps1}, make-office-fixtures.ps1
├── docs/ARCHITECTURE.md
└── .github/workflows/        CI (3 OS) and release builds
```

---

## Prerequisites

| | Windows | macOS | Linux |
|---|---|---|---|
| Rust | 1.85+ (`rustup`), MSVC toolchain + Visual Studio Build Tools | 1.85+, Xcode Command Line Tools | 1.85+ |
| Node.js | 20+ | 20+ | 20+ |
| System libraries | WebView2 (preinstalled on Windows 10/11; the installer bootstraps it otherwise) | — | `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf` |

Users of the packaged app need none of these. Installers bundle everything, including PDFium.

## First-time setup

```bash
npm ci
```

Download the PDFium library (bundled into the app; pinned to `chromium/8066`):

```bash
scripts/fetch-pdfium.sh
```

On Windows:

```bash
pwsh scripts/fetch-pdfium.ps1
```

Without PDFium everything still works; PDFs fall back to a pure-Rust extractor, which handles
fewer fonts and reads no metadata.

## Development

Run the desktop app with hot-reloading UI:

```bash
npm run tauri dev
```

Run only the UI in a normal browser against built-in demo data (no Rust needed):

```bash
npm run dev
```

> **Windows note:** if the repository lives in a very deep folder, set a short
> `CARGO_TARGET_DIR` (e.g. `C:\fftarget`). The MSVC linker cannot write paths longer
> than 260 characters.

## Tests

```bash
cargo test --workspace
```

```bash
npm test
```

* **Unit tests** cover the tokenizer, every parser, the query parser, the planner, the
  matcher, snippets, location maps, the catalog, the text store, filters, the scanner and
  the watcher debouncer.
* **Integration tests** (`crates/fastfind-core/tests/integration.rs`) run
  folder → scanner → parsers → index → query → results. They cover every search mode and
  filter, snippets with locations, previews, modify/delete/add/rename and restart without
  re-indexing, the live file watcher, corrupted-index recovery, zip bombs, and malformed or
  deeply nested files.
* **Fixture tests** (`tests/fixtures.rs`) use real `.doc/.docx/.xls/.xlsx/.ppt/.pptx` files
  saved by Microsoft Office, including password-protected ones.
* **PDF tests** (`tests/pdf.rs`) cover a Word-exported PDF (pages, metadata), 40-page
  documents and scanned-PDF classification.
* **OCR tests** (`tests/ocr.rs`, need a local Tesseract; skipped otherwise) use genuine scans:
  text rendered to pixels with PDFium and wrapped in image-only PDFs and PNGs. They cover
  Tesseract recognising a page, a two-page scanned PDF keeping its page numbers, and the full
  flow from "Scanned PDF requiring OCR" to searchable once OCR is enabled in Settings,
  including existing images and an unreadable image that must not block the queue.
* **OCR tests** (`tests/ocr.rs`) also cover setup problems: Tesseract missing, a wrong
  executable, languages that are not installed, and that queued scans wait instead of failing.
* **UI tests** (`ui/tests`, vitest + jsdom) cover search-as-you-type, debouncing, lazy
  snippets, HTML-injection safety, preview match navigation, keyboard navigation, open and
  reveal, modes, toggles, filters, adding folders, settings, help, the refresh offer,
  onboarding and the custom title-bar controls.

## Benchmarks

Generate a deterministic corpus with planted "needle" words, then benchmark it. The benchmark
uses a fresh temporary index and doesn't touch your real one.

```bash
cargo run --release -p fastfind-cli -- gen-data ./test-data --files 100000 --profile mixed
```

```bash
cargo run --release -p fastfind-cli -- benchmark ./test-data --iterations 30 --json bench.json
```

Profiles: `mixed` (txt/md/csv/json/html/xml/log/docx/xlsx/pptx/pdf), `small` (many small
text files), `large-text` (`--large-mb` sized logs), `office`, `pdf`.

The benchmark reports:
* scan and index throughput (files/s, MB/s);
* where indexing time went (parse, writer, commits);
* index size;
* startup time with an empty and an existing index;
* peak memory and average CPU;
* per-query average, p50, p95, p99 and max latency (cache cleared before each run);
* snippet latency for a full page.

### Results (24-core CPU, 32 GB RAM, NVMe SSD, Windows 11, PDFium helpers)

| Corpus | Files | Size | Index time | Throughput | Index + text store | Peak RAM | Startup (existing index) |
|---|---|---|---|---|---|---|---|
| mixed | 100,000 | 2.6 GB | 15.5 s | 6,455 files/s · 173 MB/s | 686 MB + 279 MB | 591 MB | 76 ms |
| small text | 1,000,000 | 4.5 GB | 57 s | 17,535 files/s · 79 MB/s | 1.4 GB | 717 MB | 51 ms |

Query latency in ms, with the result cache cleared before every run (30 runs each):

| Query | 100k mixed avg / p95 | 1M files avg / p95 |
|---|---|---|
| `invoice` | 1.9 / 2.3 | 3.7 / 4.7 |
| `zeppelin` (rare) | 1.8 / 2.4 | 1.2 / 1.5 |
| `fastfind` (matches every file) | 3.4 / 3.8 | 17.2 / 22.4 |
| `"total was approved"` | 2.4 / 2.6 | 6.4 / 7.6 |
| `invoice AND customer` | 4.0 / 4.4 | 4.7 / 5.4 |
| `invoice OR quarterly` | 4.2 / 4.7 | 6.1 / 7.1 |
| `invoice -customer` | 4.2 / 4.7 | 9.1 / 10.9 |
| `quart` (prefix) | 1.9 / 2.6 | 1.8 / 2.7 |
| `invoice ext:pdf` | 2.5 / 2.8 | 1.1 / 1.4 |
| `filename:file00012` (substring) | 18.8 / 19.2 | 119 / 150 |
| Exact `The invoice total` | 25.8 / 42.1 | 19.4 / 21.5 |
| Regex `zeppel[a-z]+ total` | 33.0 / 40.4 | 162 / 203 |

Snippets for a page of 100 results take 3–15 ms. `docs/ARCHITECTURE.md` §8 lists the
profiling-driven optimisations behind these numbers.

### Benchmark notes

Numbers depend on hardware, the OS file cache and antivirus (first reads of freshly created
files are slower). Run the benchmark twice to compare warm-cache results.

---

## Packaging

```bash
npm run build:unsigned
```

This produces platform installers in `target/release/bundle/`. `build:unsigned` skips the
signed update packages; a plain `npm run tauri build` makes them too, but needs the update
signing key in `TAURI_SIGNING_PRIVATE_KEY` (see *Releasing* below).

| Platform | Artifacts |
|---|---|
| Windows | `nsis/FastFind_<version>_x64-setup.exe` (per-user installer), `msi/FastFind_<version>_x64_en-US.msi` |
| macOS | `macos/FastFind.app`, `dmg/FastFind_<version>_universal.dmg` (use `--target universal-apple-darwin`) |
| Linux | `appimage/FastFind_<version>_amd64.AppImage`, `deb/FastFind_<version>_amd64.deb` |

Run `scripts/fetch-pdfium.*` for the target platform before building so PDFium is bundled.

### Releasing

Tagged pushes (`v*`) build all three platforms in CI (`.github/workflows/release.yml`), sign the
update packages and attach everything, including `latest.json` (the update manifest), to a draft
GitHub release. Installed copies only see a release after you publish the draft.

1. Bump `version` in `Cargo.toml`, `package.json` and `src-tauri/tauri.conf.json`, and add a
   `CHANGELOG.md` entry.
2. Commit, then tag and push: `git tag -a v1.1.0 -m "FastFind 1.1.0"` and
   `git push origin main v1.1.0`.
3. Check the draft's assets, paste the changelog entry as the release notes (the update banner
   shows them under *What's new*) and publish.

**Update signing key (one-time setup).** Updates are verified with a key pair that only you hold.
Generate it once, outside the repository:

```bash
npx tauri signer generate -w ~/.tauri/fastfind.key
```

Put the contents of `fastfind.key.pub` into `plugins.updater.pubkey` in
`src-tauri/tauri.conf.json`. Under *Settings → Environments*, create an environment named
`Updater` and add two secrets to it: `TAURI_SIGNING_PRIVATE_KEY` (the contents of
`fastfind.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The release job uses that environment. Back the private key and password up somewhere safe: if they
are lost, installed copies can never be updated again and users must reinstall by hand. The
release workflow refuses to run until both are configured.

**OS code signing (optional).** Without it Windows SmartScreen and macOS Gatekeeper warn on first
launch. Add a Windows code-signing certificate and the `APPLE_*` notarisation secrets to remove
the warnings.

### Website

`site/` holds the project website. `npm run site:build` assembles it into `_site/` together with
the in-browser UI demo (`_site/demo/`, sample data only). `.github/workflows/pages.yml` publishes
it with GitHub Pages on every push to `main` that touches the site or the UI. Enable it once under
*Settings → Pages → Source: GitHub Actions*. The download buttons read the latest release from
GitHub's API, so they need no change for new versions.

**Flatpak:** the AppImage/deb cover most distributions. A Flatpak manifest can wrap the
`deb` payload with the `org.gnome.Platform` runtime (WebKitGTK), but is not included yet.

**Updates:** see *Updates* under *Using FastFind*.

---

## Using FastFind

### Search syntax

| Query | Meaning |
|---|---|
| `invoice` | words starting with "invoice" (turn on **Whole words** for exact words) |
| `"annual report"` | exact phrase |
| `invoice customer` / `invoice AND customer` | both |
| `invoice OR receipt` | either (also `\|`) |
| `invoice -draft` / `NOT draft` | exclude |
| `(invoice OR receipt) 2026` | grouping |
| `invoi*` | explicit prefix |
| `filename:report`, `name:*.pdf` | file name contains / glob |
| `ext:pdf`, `ext:docx,xlsx` | extensions |
| `type:spreadsheet` | pdf, word, spreadsheet, presentation, text, code, data, web, ebook, image, other |
| `path:Projects` | folder path contains the word |
| `in:"C:\Work\2026"` | only inside this folder |
| `modified:2026-01-01`, `>2026-01`, `<=2025`, `2026-01..2026-03`, `today`, `7d`, `4w` | dates |
| `size:>10mb`, `size:<100kb`, `size:1mb..5mb` | sizes |

Modes: **Smart** (the syntax above), **Exact** (literal text including punctuation),
**Regex** (linear-time regular expressions; scans text, so it is slower on huge indexes),
**File name** (names only, `*` and `?` wildcards).

### Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl/⌘+K, Ctrl/⌘+F | focus search |
| Enter | search now / open the selected file |
| Esc | clear the search |
| ↑ ↓ Page Up/Down Home End | move through results |
| Ctrl/⌘+Enter | show in folder |
| Ctrl/⌘+Shift+C | copy path |
| F3 / Shift+F3 | next / previous match in the preview |
| Ctrl/⌘+1…4 | Smart / Exact / Regex / File name |
| Ctrl/⌘+O | add folder |
| Ctrl/⌘+Shift+F | filters |
| Ctrl/⌘+, | settings |
| Ctrl/⌘+Plus / Minus / 0 | text size |
| F1 | help |

### Command line

```bash
fastfind-cli index ~/Documents
```

```bash
fastfind-cli search "annual report" --snippets
```

```bash
fastfind-cli status
```

```bash
fastfind-cli problems --status failed
```

The CLI uses the same data directory as the app. Close the app first, because the index
allows a single writer. Use `--data-dir` or `FASTFIND_DATA_DIR` for a separate index.

### Updates

On first launch FastFind asks whether it may check for updates. If you agree, it asks GitHub
once a day whether a newer version exists. That request is the only network access FastFind
makes, and it sends nothing about your files. You can change the choice or check by hand in
*Settings → Updates*.

When an update is available, a banner offers *What's new* and *Install and restart*. Nothing
is installed until you click it. The download is checked against FastFind's signing key, and a
modified or older package is rejected. The index is closed cleanly, the new version is
installed, and FastFind restarts. Windows installers, the macOS app and the Linux AppImage
update themselves. With the `.deb`, the banner links to the download page instead.

Copies of 1.0.0 have no updater, so install the first version that has one by hand.

### Privacy and logs

FastFind never uploads files or their contents and contains no telemetry. The only network
request is the optional update check described above. Logs (JSON lines,
kept for 7 days in the data folder's `logs/`) record paths, timings and error messages, never
document text. Set `FASTFIND_LOG=debug` for more detail.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Report security problems privately as described in
[SECURITY.md](SECURITY.md).

## License

MIT, see [LICENSE](LICENSE). PDFium is distributed under its own BSD-style license (`src-tauri/pdfium/PDFIUM-LICENSE`).
