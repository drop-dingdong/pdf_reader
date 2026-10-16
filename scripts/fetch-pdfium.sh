#!/usr/bin/env bash
# 下载 PDFium 预编译动态库到 vendor/（用于本地开发与测试；不进版本库）。
#
# 用法：
#   scripts/fetch-pdfium.sh              # 自动识别当前平台
#   scripts/fetch-pdfium.sh linux-x64    # 指定平台
#
# 说明：bblanchon/pdfium-binaries **只提供动态库**，不提供静态库（见 docs/framework-selection.md §4）。
# 支持的平台标识：linux-x64 / linux-arm64 / mac-x64 / mac-arm64 / mac-univ / win-x64 / win-arm64
set -euo pipefail

PLATFORM="${1:-}"
if [ -z "$PLATFORM" ]; then
  OS="$(uname -s)"
  ARCH="$(uname -m)"
  case "$OS/$ARCH" in
    Linux/x86_64)   PLATFORM="linux-x64" ;;
    Linux/aarch64)  PLATFORM="linux-arm64" ;;
    Darwin/arm64)   PLATFORM="mac-arm64" ;;
    Darwin/x86_64)  PLATFORM="mac-x64" ;;
    *) echo "无法识别的平台：$OS/$ARCH，请显式传入平台标识" >&2; exit 1 ;;
  esac
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/pdfium-$PLATFORM"
URL="https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-$PLATFORM.tgz"

echo "平台：$PLATFORM"
echo "下载：$URL"
mkdir -p "$DEST"
curl -fL --retry 3 -o "$DEST/pdfium.tgz" "$URL"
tar -xzf "$DEST/pdfium.tgz" -C "$DEST"
rm -f "$DEST/pdfium.tgz"

echo "已解压到：$DEST"
find "$DEST" -maxdepth 2 -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' -o -name '*.a' -o -name '*.lib' \) -print
