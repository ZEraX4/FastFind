# Parser fixtures

Real documents saved by Microsoft Office 365 (Word, Excel, PowerPoint, Outlook) through COM
automation. They exist to validate the hand-written legacy `.doc` / `.ppt` / `.msg` extractors
and the encryption detection against genuine Office output rather than hand-crafted samples.

| File | Content checked by `tests/fixtures.rs` |
|---|---|
| `legacy.doc`, `modern.docx` | paragraphs, a 2×2 table, header "zanzibar", footer "yellowstone", title/author properties |
| `legacy.xls`, `modern.xlsx` | sheets `Budget` (B3 "Flamingo", D10 4242) and `Forecast` (C2 "Pangolin projection") |
| `legacy.ppt`, `modern.pptx` | 2 slides with titles, body text and a speaker note ("kiwis") on slide 2 |
| `protected.doc`, `protected.docx`, `protected.xls` | password `secret123` → must be reported as encrypted |
| `outlook-unicode.msg` | Turkish and Arabic subject and body, To and Cc recipients; attached `modern.docx`, a text file and an Outlook item |
| `outlook-ansi.msg` | the older 8-bit format, with an HTML-only body |
| `outlook-archive.pst` | an archive with folders `Archive 2025` › `Projects`: the Turkish/Arabic message with `modern.docx`, an HTML message with a text attachment, one with an attached Outlook item, and that item on its own (stored without sub-nodes) |

To regenerate on a Windows machine with Office installed, run `scripts/make-office-fixtures.ps1`,
then `scripts/make-outlook-fixtures.ps1` for the `.msg` files and
`scripts/make-outlook-pst-fixture.ps1` for the archive. Outlook keeps an archive it has opened
locked until it exits, so close Outlook before running the tests on a new one.

Office writes the Windows user name into "last modified by" (and `.xls` into its saved-by
record), and Excel records the folder a workbook was saved from. The committed files have
these replaced with `Fixture User` and the folder removed. Outlook writes the signed-in
account's e-mail address into unsent items; the Outlook scripts replace it with a placeholder
of the same length (in the archive through `scripts/scrub-pst.py`, which re-encodes the
changed blocks and updates their checksums). Check for personal data before committing regenerated fixtures.
