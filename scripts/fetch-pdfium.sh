#!/usr/bin/env sh
# Downloads a prebuilt PDFium library (https://github.com/bblanchon/pdfium-binaries) into
# src-tauri/pdfium/ so it is bundled with the app. Pinned for reproducible builds.
# Usage: scripts/fetch-pdfium.sh [version] [platform]
#   platform: linux-x64 | linux-arm64 | mac-univ | mac-arm64 | mac-x64 (auto-detected)
set -eu
VERSION="${1:-chromium/8066}"
if [ -n "${2:-}" ]; then
  PLATFORM="$2"
else
  case "$(uname -s)-$(uname -m)" in
    Darwin-*) PLATFORM="mac-univ" ;;
    Linux-x86_64) PLATFORM="linux-x64" ;;
    Linux-aarch64|Linux-arm64) PLATFORM="linux-arm64" ;;
    *) echo "unsupported platform $(uname -s)-$(uname -m); pass one explicitly" >&2; exit 1 ;;
  esac
fi
DIR="$(cd "$(dirname "$0")/.." && pwd)/src-tauri/pdfium"
mkdir -p "$DIR"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
ENC_VERSION="$(printf '%s' "$VERSION" | sed 's#/#%2F#g')"
URL="https://github.com/bblanchon/pdfium-binaries/releases/download/${ENC_VERSION}/pdfium-${PLATFORM}.tgz"
echo "Downloading $URL"
curl -fsSL "$URL" -o "$TMP/pdfium.tgz"
tar -xzf "$TMP/pdfium.tgz" -C "$TMP"
case "$PLATFORM" in
  mac-*) cp "$TMP/lib/libpdfium.dylib" "$DIR/" ;;
  *) cp "$TMP/lib/libpdfium.so" "$DIR/" ;;
esac
[ -f "$TMP/LICENSE" ] && cp "$TMP/LICENSE" "$DIR/PDFIUM-LICENSE"
echo "$VERSION $PLATFORM" > "$DIR/VERSION"
echo "PDFium installed to $DIR"
