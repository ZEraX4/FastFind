# Regenerates the Outlook .msg fixtures in crates/fastfind-core/tests/fixtures using a local
# Outlook install (COM). Items are created in memory and saved as files only: nothing is sent,
# and nothing is saved to the mailbox.
# Usage: pwsh scripts/make-outlook-fixtures.ps1   (needs modern.docx from make-office-fixtures.ps1)
$ErrorActionPreference = "Stop"
$fx = (Resolve-Path (Join-Path $PSScriptRoot "..\crates\fastfind-core\tests\fixtures")).Path
$tmp = Join-Path ([IO.Path]::GetTempPath()) "fastfind-msg-fixtures"
New-Item -ItemType Directory -Force $tmp | Out-Null
$ol = New-Object -ComObject Outlook.Application

$notes = Join-Path $tmp "notes.txt"
[IO.File]::WriteAllText($notes, "Attachment notes mention kestrelword and the Q3 audit.`r`n", [Text.Encoding]::UTF8)

# The Outlook item attached inside the main message.
$inner = $ol.CreateItem(0)
$inner.Subject = "Original request"
$inner.Body = "Please send the ospreyfigure totals by Friday."
$innerPath = Join-Path $tmp "inner.msg"
$inner.SaveAs($innerPath, 9)

$m = $ol.CreateItem(0)
$m.Subject = "Geçiş izni — تقرير الميزانية"
$r = $m.Recipients.Add("Ayşe Yılmaz <ayse@example.com>"); $r.Type = 1
$r = $m.Recipients.Add("team@example.com"); $r.Type = 2
$m.Body = "Merhaba,`r`nGeçiş izni onaylandı. zanzibarquota is the code word.`r`nالميزانية معتمدة للمدرسة.`r`n"
[void]$m.Attachments.Add((Join-Path $fx "modern.docx"))
[void]$m.Attachments.Add($notes)
[void]$m.Attachments.Add($inner, 5)                      # olEmbeddeditem
$m.SaveAs((Join-Path $fx "outlook-unicode.msg"), 9)      # olMSGUnicode

# The older ANSI format: 8-bit strings in the message's code page, HTML body.
$a = $ol.CreateItem(0)
$a.Subject = "Budget review"
$r = $a.Recipients.Add("Jane Doe <jane@example.com>"); $r.Type = 1
$a.HTMLBody = "<html><body><p>The <b>pelicanmargin</b> figures are final.</p></body></html>"
$a.SaveAs((Join-Path $fx "outlook-ansi.msg"), 3)         # olMSG

foreach ($i in @($m, $a, $inner)) { $i.Close(1) }        # olDiscard
Remove-Item -Recurse $tmp

# Outlook writes the signed-in account's address into unsent items. Replace it with a
# placeholder of the same length (ASCII and UTF-16), which keeps every stream size intact.
$latin1 = [Text.Encoding]::Latin1
foreach ($acct in $ol.Session.Accounts) {
  $addr = $acct.SmtpAddress
  if (-not $addr) { continue }
  $at = $addr.IndexOf("@")
  $ph = ("u" * $at) + "@" + ("x" * ($addr.Length - $at - 5)) + ".com"
  foreach ($f in "outlook-unicode.msg", "outlook-ansi.msg") {
    $p = Join-Path $fx $f
    $s = $latin1.GetString([IO.File]::ReadAllBytes($p))
    foreach ($pair in @(@($addr, $ph), @($addr.ToUpper(), $ph.ToUpper()))) {
      $s = $s.Replace($pair[0], $pair[1])
      $s = $s.Replace($latin1.GetString([Text.Encoding]::Unicode.GetBytes($pair[0])), $latin1.GetString([Text.Encoding]::Unicode.GetBytes($pair[1])))
    }
    [IO.File]::WriteAllBytes($p, $latin1.GetBytes($s))
  }
}
Write-Host "Done. Check the files for any other personal data before committing."
