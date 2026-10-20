#!/usr/bin/env bash
# 下载 PDFium 预编译动态库到 vendor/（用于本地开发与测试；不进版本库）。
#
# 用法：
#   scripts/fetch-pdfium.sh              # 自动识别当前平台
#   scripts/fetch-pdfium.sh linux-x64    # 指定平台
#
# 说明：bblanchon/pdfium-binaries **只提供动态库**，不提供静态库（见 docs/framework-selection.md §4）。
# 支持的平台标识：linux-x64 / linux-arm64 / mac-x64 / mac-arm64 / mac-univ / win-x64 / win-x86 / win-arm64
#
# Windows 注意：bblanchon 的 tar 包里，**运行库** pdfium.dll 在 `bin/` 下，
# 而 `lib/pdfium.dll.lib` 是**导入库**（链接期用，运行时没用）。本脚本会把运行库
# 统一归位到 `vendor/pdfium-<平台>/lib/`，并删掉导入库/静态库，避免"看起来有、其实加载不了"。
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
    MINGW*|MSYS*|CYGWIN*) echo "Windows 请在 Git Bash/PowerShell 里显式传入 win-x64 / win-x86 / win-arm64" >&2; exit 1 ;;
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

# 归位：把**运行库**放进 lib/（Windows 上游放在 bin/），并清掉导入库/静态库
case "$PLATFORM" in
  win-*) LIBNAME=pdfium.dll ;;
  mac-*) LIBNAME=libpdfium.dylib ;;
  *)     LIBNAME=libpdfium.so ;;
esac
mkdir -p "$DEST/lib"
if [ ! -f "$DEST/lib/$LIBNAME" ]; then
  FOUND="$(find "$DEST" -maxdepth 2 -type f -name "$LIBNAME" | head -1)"
  if [ -n "$FOUND" ]; then
    mv "$FOUND" "$DEST/lib/$LIBNAME"
    echo "已把运行库归位：$FOUND → $DEST/lib/$LIBNAME"
  else
    echo "警告：包里没有找到运行库 $LIBNAME（Windows 上它在 bin/ 下；若只有 *.dll.lib 那是导入库）" >&2
  fi
fi
# 导入库（*.dll.lib）、静态库（*.a）、pdb 对运行没有意义，删掉以免误用
find "$DEST" -type f \( -name '*.dll.lib' -o -name '*.a' -o -name '*.pdb' \) -delete 2>/dev/null || true

find "$DEST" -maxdepth 2 -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) -print
