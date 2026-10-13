# Regenerates crates/fastfind-core/tests/fixtures/outlook-archive.pst with a local Outlook (COM).
# The archive is added to the Outlook profile only while the script runs, then removed; items
# are moved into it unsaved and never sent: saving a new unsent message, even one made with a
# folder's Items.Add, files it in the mailbox's Drafts. (Posts would stay in the archive, but
# Outlook stamps them with the signed-in user's name.) Needs modern.docx from make-office-fixtures.ps1. Outlook keeps the archive locked
# until it exits: close Outlook before running the tests.
# Usage: pwsh scripts/make-outlook-pst-fixture.ps1
$ErrorActionPreference = "Stop"
$fx = (Resolve-Path (Join-Path $PSScriptRoot "..\crates\fastfind-core\tests\fixtures")).Path
$pst = Join-Path $fx "outlook-archive.pst"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "fastfind-pst-fixture"
Remove-Item -Force $pst -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $tmp | Out-Null

$started = (Get-Date).AddSeconds(-5)
$wasRunning = [bool](Get-Process OUTLOOK -ErrorAction SilentlyContinue)
$ol = New-Object -ComObject Outlook.Application
$ns = $ol.GetNamespace("MAPI")
$ns.AddStoreEx($pst, 2)   # olStoreUnicode
$store = $null
foreach ($s in $ns.Stores) { if ($s.FilePath -eq $pst) { $store = $s } }
if (-not $store) { throw "the new archive did not appear in the profile" }
$root = $store.GetRootFolder()
try {
  $year = $root.Folders.Add("Archive 2025")
  $projects = $year.Folders.Add("Projects")

  $notes = Join-Path $tmp "notes.txt"
  [IO.File]::WriteAllText($notes, "Inventory of the ibisinventory warehouse.`r`n", [Text.Encoding]::UTF8)

  # 1. Turkish and Arabic, a Word attachment.
  $m = $ol.CreateItem(0)
  $m.Subject = "Geçiş izni — تقرير الميزانية"
  [void]$m.Recipients.Add("Ayşe Yılmaz <ayse@example.com>")
  $m.Body = "Merhaba,`r`nGeçiş izni onaylandı. albatrossledger is the code word.`r`nالميزانية معتمدة للمدرسة`r`n"
  [void]$m.Attachments.Add((Join-Path $fx "modern.docx"))
  [void]$m.Move($year)

  # 2. HTML only, a text attachment.
  $m = $ol.CreateItem(0)
  $m.Subject = "Budget review"
  [void]$m.Recipients.Add("Jane Doe <jane@example.com>")
  $m.HTMLBody = "<html><body><p>The <b>condorbudget</b> figures are final.</p></body></html>"
  [void]$m.Attachments.Add($notes)
  [void]$m.Move($projects)

  # 3. An attached Outlook item. The item itself stays in the folder too: with no recipients and
  # no attachments it is stored without sub-nodes, a case readers must handle.
  $inner = $ol.CreateItem(0)
  $inner.Subject = "Original request"
  $inner.Body = "Please send the ospreyfigure totals by Friday."
  $inner = $inner.Move($projects)
  $m = $ol.CreateItem(0)
  $m.Subject = "Forwarded request"
  $m.Body = "See the attached pelicanmemo."
  [void]$m.Attachments.Add($inner, 5)   # olEmbeddeditem
  [void]$m.Move($projects)
} finally {
  $ns.RemoveStore($root)
  [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($root)
  [GC]::Collect(); [GC]::WaitForPendingFinalizers()
  # Nothing may be left in the mailbox itself.
  $subjects = "Budget review", "Forwarded request", "Original request"
  $since = $started
  foreach ($id in 16, 3) {   # Drafts, Deleted Items
    foreach ($it in @($ns.GetDefaultFolder($id).Items)) {
      if (($subjects -contains $it.Subject -or $it.Subject -like "Ge*izni*") -and $it.CreationTime -ge $since) { Write-Warning "left in the mailbox: $($it.Subject)" }
    }
  }
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
# Outlook writes the signed-in account's address into every item (last modified by): replace it
# with a placeholder of the same length (see scrub-pst.py).
$scrub = @()
foreach ($acct in $ol.Session.Accounts) {
  $addr = $acct.SmtpAddress
  if (-not $addr) { continue }
  $at = $addr.IndexOf("@")
  $scrub += $addr, (("u" * $at) + "@" + ("x" * ($addr.Length - $at - 5)) + ".com")
}
# Outlook releases the archive only when it exits: quit it if this script started it.
if (-not $wasRunning) { $ol.Quit() }
[void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($ns)
[void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($ol)
$ns = $null; $ol = $null
[GC]::Collect(); [GC]::WaitForPendingFinalizers()
$scrubCmd = "python scripts/scrub-pst.py `"$pst`" " + (($scrub | ForEach-Object { "`"$_`"" }) -join " ")
$free = $false
foreach ($i in 1..180) {
  try { [IO.File]::Open($pst, "Open", "ReadWrite", "None").Dispose(); $free = $true; break } catch { Start-Sleep 1 }
}
if (-not $free) {
  Write-Warning "Outlook still has the archive open. Close Outlook, then run: $scrubCmd"
  exit 1
}
if ($scrub) {
  python (Join-Path $PSScriptRoot "scrub-pst.py") $pst @scrub
  if ($LASTEXITCODE) { throw "scrubbing the archive failed" }
}
Write-Host "Done: $pst. Check it for personal data before committing (see tests/fixtures/README.md)."
