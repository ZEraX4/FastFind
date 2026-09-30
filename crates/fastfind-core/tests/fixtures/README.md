# Parser fixtures

Real documents saved by Microsoft Office 365 (Word, Excel, PowerPoint) through COM automation.
They exist to validate the hand-written legacy `.doc` / `.ppt` extractors and the encryption
detection against genuine Office output rather than hand-crafted samples.

| File | Content checked by `tests/fixtures.rs` |
|---|---|
| `legacy.doc`, `modern.docx` | paragraphs, a 2×2 table, header "zanzibar", footer "yellowstone", title/author properties |
| `legacy.xls`, `modern.xlsx` | sheets `Budget` (B3 "Flamingo", D10 4242) and `Forecast` (C2 "Pangolin projection") |
| `legacy.ppt`, `modern.pptx` | 2 slides with titles, body text and a speaker note ("kiwis") on slide 2 |
| `protected.doc`, `protected.docx`, `protected.xls` | password `secret123` → must be reported as encrypted |

To regenerate on a Windows machine with Office installed, run `scripts/make-office-fixtures.ps1`.

Office writes the Windows user name into "last modified by" (and `.xls` into its saved-by
record), and Excel records the folder a workbook was saved from. The committed files have
these replaced with `Fixture User` and the folder removed. Do the same before committing
regenerated fixtures.
