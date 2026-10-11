# Fuzzing

FastFind opens files it has never seen with hand-written readers (legacy Word, Excel and
PowerPoint, RTF, Outlook messages) and with libraries (zip, XML, e-mail, JSON). These fuzz
targets feed each reader millions of mutated files to find inputs that crash it, hang it, or
make it use runaway memory. A plain panic only fails that one file, because the indexer catches
panics, but a stack overflow or an out-of-memory abort takes the whole app down, and a hang
ties up an indexing worker.

| Target | Reader |
|---|---|
| `doc`, `xls`, `ppt` | legacy Office (`parsers/doc.rs`, `xls.rs`, `ppt.rs`) |
| `docx`, `xlsx`, `pptx`, `odf`, `epub` | zip-based documents |
| `rtf`, `html`, `xml`, `json`, `plain` | text formats and encoding detection |
| `eml`, `msg`, `zip` | containers, including their attachments and entries |
| `query` | the search box: query parser, tokenizer, regex and phrase matching |

PDFs are not fuzzed here: PDFium is fuzzed upstream by Chromium, and FastFind runs it in
separate helper processes.

## Running

Needs Linux or macOS (on Windows, use WSL or let CI run it: Rust's libFuzzer coverage
instrumentation does not link on Windows), the nightly toolchain and cargo-fuzz:

```bash
rustup toolchain install nightly --profile minimal
cargo +nightly install cargo-fuzz --locked
```

From this folder:

```bash
cargo run --bin seed                       # starting inputs in corpus/<target>/
cargo +nightly fuzz run rtf corpus/rtf -- -max_total_time=300 -timeout=30
```

A finding is saved under `artifacts/<target>/`. Reproduce it with
`cargo +nightly fuzz run <target> artifacts/<target>/<file>`, fix the reader, and copy the input
into `crates/fastfind-core/tests/regressions/` with the reader's extension. `tests/regressions.rs`
parses every file there on every CI run, with an allocator that fails any allocation over 256 MB.

CI (`.github/workflows/fuzz.yml`) runs every target for 2 minutes on pull requests that touch
the parsers, for 10 minutes each week, and for as long as you like from *Actions → Fuzz → Run
workflow*. Findings are attached to the run as artifacts.
