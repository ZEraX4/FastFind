# Changelog

## Unreleased

* **Faster file-name search.** `filename:`, the *File name* mode and the file-name part of
  regex searches no longer walk every name in the index: about 1 ms instead of 19 ms at
  100,000 files, and 2 ms instead of 119 ms at a million. Regex searches got faster with it.
* **Zip archives.** Files inside `.zip` archives are found by name and by content: text files
  (in any encoding), Office documents, PDFs, e-mails and archives inside the archive. Results
  say where: *Page 2 · In archive: reports/annual.pdf*. Images and other unreadable files are
  found by name. Archives are read with the same zip-bomb protections as Office files, and
  containers inside containers (a zip in an e-mail in a zip) are opened at most 4 levels deep.
* **Outlook messages.** `.msg` files saved from Outlook are searchable like `.eml`: sender,
  recipients, date and subject, the body (plain text, HTML or Outlook's compressed RTF), and
  attachments, including attached Outlook items. Results say where: *Attachment: report.pdf*.
* **Scanned pages in typed PDFs.** A PDF used to go to OCR only when at least half its pages
  were scanned, so a typed contract with a scanned signature page or appendix never had those
  pages read. Now any scanned page queues the PDF, and OCR recognises only the scanned pages;
  typed pages keep their exact text. After updating, PDFs indexed earlier are re-read once in
  the background to find their scanned pages.
* **Fixed:** in large text files (logs, CSV, code), an Exact or Regex match, or a phrase, that
  ran across the boundary between two 256 KB pieces of the file was missed. Pieces now
  overlap, and a match is counted once.
* **Releases:** a CI job checks every update package in the draft release against the update
  key, as installed copies will, before it is published (`scripts/verify-release.py`).
* **CI:** OCR tests now run on Windows too, and fail instead of being skipped if Tesseract is
  missing.

## 1.2.0 — 2026-10-11

* **Arabic search.** Spelling variants now find each other: `أ إ آ ٱ` = `ا`, `ة` = `ه`,
  `ى` = `ي`, Persian `ی` and `ک`, tatweel (`كتـــاب`) and harakat are ignored, and Arabic-Indic
  or Persian digits match `0-9`. Words are found with or without the definite article and its
  attached forms (`مدرسة` ↔ `المدرسة`, `والمدرسة`, `للمدرسة`), in phrases too.
* **E-mail.** `.eml` messages are decoded: encoded subjects and names, base64 and
  quoted-printable bodies in any charset, HTML-only mail, and attachments. Attached documents
  (Word, Excel, PDF…), text files and forwarded messages are searchable, and results say
  where: *Page 2 · Attachment: report.pdf*. The sender, recipients and subject are searchable;
  the subject and sender appear in the preview.
* A lone diacritic between spaces no longer creates an empty word in the index.
* The index is rebuilt once after updating, because words are now analysed differently.
  Scanned documents are recognised again in the background if OCR is on.

## 1.1.1 — 2026-10-04

* OCR now works for files whose name or folder contains characters outside the Windows
  system code page, such as Turkish "Geçiş izni.jpg" on a non-Turkish system, or Arabic and
  Chinese names. Before, Tesseract could not open them and the file was marked failed with
  "cannot read input file". Images now reach Tesseract as data, never as a path, and PDF
  pages no longer go through temporary files. Files that failed this way are retried
  automatically after updating.
* Updated Tauri and its plugins to their latest patch releases.

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
