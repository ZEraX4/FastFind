# Changelog

## Unreleased

* **i-net Clear Reports.** Report designs (`.rpt` files from i-net Clear Reports, formerly
  i-net Crystal-Clear) are now searchable: the report title, every label and database field in
  reading order (a label stays next to its field), the SQL queries, formulas and parameter
  prompts. Find which reports use a table or column, or show a label. Snippets tell you where
  the match is: *Details*, *Page header*, *SQL query*, *Formula: name*. Report files already
  in your indexed folders are read once after updating. Classic Crystal Reports `.rpt` files
  use a different, closed format and are still found by name only.

## 1.5.0 — 2026-10-13

* **Outlook archives.** E-mails in Outlook data files (`.pst`) are now searchable, each as its
  own result, named by its subject and shown with the archive and folder it is in. Attachments
  are searched too (Word, PDF, Excel and the other supported formats), and so are Outlook items
  attached to an e-mail. Opening a result opens that e-mail in your mail app; *Show in folder*
  shows the archive. Archives have no size limit. While Outlook has an archive open it keeps
  the file locked: FastFind tries again later, and earlier results stay searchable. Offline
  mailbox caches (`.ost`) are found by name only. Archives already in your indexed folders are
  read once after updating.

## 1.4.0 — 2026-10-12

* **Quick search from anywhere.** Press Ctrl+Shift+F (⌘⇧F on macOS) in any application to open
  a Spotlight-style search box: results as you type, Enter to open, Ctrl/⌘+Enter to show in
  the folder, Shift+Enter for all results in FastFind, Esc to close. The shortcut is set in the
  new Settings → General tab; if another application already uses it, FastFind asks for a
  different one. Inside FastFind's own window, Ctrl+Shift+F still opens the filters.
* **Runs in the background.** Closing the window now keeps FastFind in the system tray (menu
  bar on macOS), so the shortcut works and the index stays current; quit from the tray icon's
  menu. Both this and a new **Start FastFind when I sign in** option are in Settings → General.

## 1.3.2 — 2026-10-12

* **Fixed:** the Index panel showed a snapshot taken when it was opened: new files, counts,
  folder states and indexing progress only appeared after closing and reopening it. It now
  updates live, and Rescan, Update now, Pause and Remove keep it open so you can watch the
  result.

## 1.3.1 — 2026-10-12

* **Zip file names in Turkish, Arabic and other languages.** Zips made by Windows' built-in
  *Compressed folder* (before Windows 11) and many older tools store file names in the PC's
  DOS code page without saying so, and FastFind showed them garbled. Names are now decoded
  like Windows Explorer does: UTF-8 when valid, otherwise the system's code page (Windows).
* **Progress window for index upgrades.** When an update changes the index format, a small
  *Updating FastFind's index* window now shows while the index is rebuilt from stored text,
  instead of the app appearing only once it's done.

## 1.3.0 — 2026-10-12

* **Faster file-name search.** `filename:`, the *File name* mode and the file-name part of
  regex searches no longer walk every name in the index: about 1 ms instead of 19 ms at
  100,000 files, and 2 ms instead of 119 ms at a million. Regex searches got faster with it.
* **Faster updates of the index format.** When a release changes how text is indexed, existing
  indexes are now rebuilt from the text FastFind already stored, instead of reading every file
  again. Recognised (OCR) text is kept, so scans are never recognised a second time. Only
  plain-text files and a few very large documents are read again.
* **Fixed (found by fuzzing):** crafted `.xls` files could make FastFind try to allocate
  gigabytes or terabytes of memory, which can close the app: through the file's container, or
  through a sheet's declared size, or by placing two cells far apart. The workbook is now read
  from a validated copy with the size hints removed, and a sheet that would need more than about
  4 million cells of memory is indexed by name only. An HTML file ending inside an unclosed tag could fail to index.
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
