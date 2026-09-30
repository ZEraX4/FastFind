# Regenerates crates/fastfind-core/tests/fixtures/* using a local Microsoft Office install (COM).
# Usage: pwsh scripts/make-office-fixtures.ps1
$ErrorActionPreference = "Stop"
$fx = Join-Path $PSScriptRoot "..\crates\fastfind-core\tests\fixtures"
New-Item -ItemType Directory -Force $fx | Out-Null
$fx = (Resolve-Path $fx).Path

function Set-DocProp($doc, $name, $value) {
  $props = $doc.BuiltInDocumentProperties
  $p = [System.__ComObject].InvokeMember("Item", [System.Reflection.BindingFlags]::GetProperty, $null, $props, @($name))
  [System.__ComObject].InvokeMember("Value", [System.Reflection.BindingFlags]::SetProperty, $null, $p, @($value)) | Out-Null
}

$w = New-Object -ComObject Word.Application
$w.Visible = $false; $w.DisplayAlerts = 0
try {
  $d = $w.Documents.Add()
  $d.Content.Text = "Legacy Word document about the marmalade factory."
  $r = $d.Content; $r.InsertParagraphAfter(); $r.InsertAfter("Second paragraph mentions quokka habitats.")
  $r.InsertParagraphAfter()
  $end = $d.Content; $end.Collapse(0)
  $t = $d.Tables.Add($end, 2, 2)
  $t.Cell(1,1).Range.Text = "tablecell alpha"; $t.Cell(2,2).Range.Text = "tablecell omega"
  $d.Sections(1).Headers(1).Range.Text = "Header zanzibar"
  $d.Sections(1).Footers(1).Range.Text = "Footer yellowstone"
  Set-DocProp $d "Title" "Marmalade Report"
  Set-DocProp $d "Author" "Fixture Author"
  $d.SaveAs2("$fx\legacy.doc", 0)                          # wdFormatDocument97
  $d.SaveAs2("$fx\modern.docx", 16)                        # wdFormatDocumentDefault
  $d.SaveAs2("$fx\protected.docx", 16, $false, "secret123")
  $d.SaveAs2("$fx\protected.doc", 0, $false, "secret123")
  $d.Close(0)
} finally { $w.Quit() }

$x = New-Object -ComObject Excel.Application
$x.Visible = $false; $x.DisplayAlerts = $false
try {
  $wb = $x.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1); $ws.Name = "Budget"
  $ws.Cells.Item(1,1).Value2 = "Item"; $ws.Cells.Item(1,2).Value2 = "Amount"
  $ws.Cells.Item(3,2).Value2 = "Flamingo"; $ws.Cells.Item(3,1).Value2 = "Catering"
  $ws.Cells.Item(10,4).Value2 = 4242
  $ws2 = $wb.Worksheets.Add([Type]::Missing, $ws); $ws2.Name = "Forecast"
  $ws2.Cells.Item(2,3).Value2 = "Pangolin projection"
  $wb.SaveAs("$fx\legacy.xls", 56)                         # xlExcel8
  $wb.SaveAs("$fx\modern.xlsx", 51)                        # xlOpenXMLWorkbook
  $wb.SaveAs("$fx\protected.xls", 56, "secret123")
  $wb.Close($false)
} finally { $x.Quit() }

$p = New-Object -ComObject PowerPoint.Application
try {
  $pres = $p.Presentations.Add(0)
  $s1 = $pres.Slides.Add(1, 1)
  $s1.Shapes.Item(1).TextFrame.TextRange.Text = "Kickoff Meeting"
  $s1.Shapes.Item(2).TextFrame.TextRange.Text = "Welcome to the aardvark programme"
  $s2 = $pres.Slides.Add(2, 2)
  $s2.Shapes.Item(1).TextFrame.TextRange.Text = "Roadmap Plans"
  $s2.Shapes.Item(2).TextFrame.TextRange.Text = "Ship the wombat release`rHire engineers"
  $s2.NotesPage.Shapes.Placeholders.Item(2).TextFrame.TextRange.Text = "Speaker note about kiwis"
  $pres.SaveAs("$fx\legacy.ppt", 1)                        # ppSaveAsPresentation
  $pres.SaveAs("$fx\modern.pptx", 24)                      # ppSaveAsOpenXMLPresentation
  $pres.Close()
} finally { $p.Quit() }
Get-ChildItem $fx
