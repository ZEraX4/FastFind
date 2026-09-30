# Downloads a prebuilt PDFium library (https://github.com/bblanchon/pdfium-binaries) into
# src-tauri/pdfium/ so it is bundled with the app. Pinned for reproducible builds.
# Usage: pwsh scripts/fetch-pdfium.ps1 [-Version chromium/8066] [-Platform win-x64|win-arm64|win-x86]
param(
  [string]$Version = "chromium/8066",
  [string]$Platform = "win-x64"
)
$ErrorActionPreference = "Stop"
$dest = Join-Path $PSScriptRoot "..\src-tauri\pdfium"
New-Item -ItemType Directory -Force $dest | Out-Null
$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$([uri]::EscapeDataString($Version))/pdfium-$Platform.tgz"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("pdfium-" + [guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
  Write-Host "Downloading $url"
  Invoke-WebRequest -Uri $url -OutFile "$tmp\pdfium.tgz" -UseBasicParsing
  tar -xzf "$tmp\pdfium.tgz" -C $tmp
  Copy-Item "$tmp\bin\pdfium.dll" $dest -Force
  if (Test-Path "$tmp\LICENSE") { Copy-Item "$tmp\LICENSE" "$dest\PDFIUM-LICENSE" -Force }
  Set-Content -Path "$dest\VERSION" -Value "$Version $Platform"
  Write-Host "PDFium installed to $dest"
} finally {
  Remove-Item -Recurse -Force $tmp
}
