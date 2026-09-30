# Contributing to FastFind

Thanks for helping. Bug reports, parser fixes for real-world files and performance work are
especially welcome.

## Setup

Follow *Prerequisites* and *First-time setup* in the [README](README.md): install Rust and
Node.js, run `npm ci` and fetch PDFium with `scripts/fetch-pdfium.sh` (or `.ps1` on Windows).

## Before opening a pull request

All of these must pass; CI runs them on Windows, macOS and Linux.

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
npx tsc --noEmit -p ui
```

```bash
npm test
```

OCR tests run only when Tesseract is installed and are skipped otherwise.

## Guidelines

* **Tests with changes.** Parser changes need a unit test, and a fixture in
  `crates/fastfind-core/tests/fixtures` when the bug came from a real file.
* **No private data in fixtures.** Office and PDF files store author names, company names and
  the folder they were saved from. Strip these before committing a new fixture.
* **Performance.** Changes to the index, scanner or search path should include before/after
  numbers from `fastfind-cli benchmark` (see *Benchmarks* in the README).
* **Local only.** FastFind must never send files, text or usage data over the network.
* **Style.** `cargo fmt` for Rust; match the surrounding code in the UI.

## Reporting bugs

Open an issue with the OS, FastFind version, the steps to reproduce and, if possible, the log
from the data folder's `logs/` (it contains paths and errors, never document text). For a file
that fails to index, a sample file with the same problem helps most. Don't attach private
documents.

Security problems: see [SECURITY.md](SECURITY.md).
