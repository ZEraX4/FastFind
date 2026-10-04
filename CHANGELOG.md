# Changelog

## Unreleased

* OCR now works for files whose name or folder contains characters outside the Windows
  system code page, such as Turkish "Geçiş izni.jpg" on a non-Turkish system, or Arabic and
  Chinese names. Before, Tesseract could not open them and the file was marked failed with
  "cannot read input file". Images now reach Tesseract as data, never as a path, and PDF
  pages no longer go through temporary files. Files that failed this way are retried
  automatically after updating.

## 1.1.0 — 2026-10-04

* In-app updates. FastFind asks once whether it may check GitHub for a new version (at most
  daily); updates are signed, shown with their release notes and installed only when you click
  *Install and restart*. Manual check and setting in *Settings → Updates*.
* OCR setup problems no longer fail files. If Tesseract is missing or none of the configured
  languages is installed, scanned files keep waiting instead of being marked failed for good,
  and the reason is shown in *Settings → Indexing* (checked live as you edit), in the index
  panel and in the status tooltip. Changing the OCR settings retries files whose OCR failed.
* OCR interrupted by new indexing work no longer saves a partly recognised document.
* The *Tesseract executable* setting only accepts the Tesseract program itself.

## 1.0.0 — 2026-10-01

First release.

* Full-text search in TXT, Markdown, CSV, logs, source code, HTML, XML, JSON, RTF, DOC, DOCX,
  XLS, XLSX, PPT, PPTX, ODF, EPUB and PDF (PDFium), with optional Tesseract OCR for scanned
  PDFs and images.
* Persistent incremental index with live file watching.
* Smart, Exact, Regex and File name modes; boolean syntax and `filename:` `ext:` `type:`
  `path:` `in:` `modified:` `size:` filters.
* Snippets and preview with page, sheet, cell, slide and line locations.
* Frameless window, dark mode, high contrast, keyboard navigation.
* Skipped-files view with reasons; `fastfind-cli` for indexing, search and benchmarks.
