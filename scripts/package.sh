#!/usr/bin/env bash
# 打一个"自带 PDFium"的发布目录。用法：
#   ./scripts/package.sh                 # 本机平台
#   ./scripts/package.sh linux-x64       # 显式指定（见 backend-pdfium::library::platform_tag）
#
# 产物：
#   dist/pdf_reader_<平台>/
#     app            （Windows 上是 app.exe）
#     lib/libpdfium.so（macOS 是 libpdfium.dylib，Windows 是 pdfium.dll）
#
# 为什么这个布局就够用：`library.rs` 的查找顺序里有一条"**可执行文件所在目录**及其
# `lib/` 子目录"，而且是按**绝对路径**交给 `bind_to_library` 的 —— 所以不需要 rpath、
# 不需要 LD_LIBRARY_PATH，也不依赖启动时的工作目录。
set -euo pipefail
cd "$(dirname "$0")/.."

target="${1:-}"
if [ -z "$target" ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=linux-x64 ;;
    Linux-aarch64) target=linux-arm64 ;;
    Darwin-arm64) target=mac-arm64 ;;
    Darwin-x86_64) target=mac-x64 ;;
    *) echo "无法识别当前平台，请显式传参（linux-x64 / linux-arm64 / mac-arm64 / mac-x64 / win-x64）" >&2; exit 2 ;;
  esac
fi

case "$target" in
  linux-*) libname=libpdfium.so ;;
  mac-*)   libname=libpdfium.dylib ;;
  win-*)   libname=pdfium.dll ;;
  *) echo "未知平台 $target" >&2; exit 2 ;;
esac

vendor="vendor/pdfium-$target/lib/$libname"
if [ ! -f "$vendor" ]; then
  echo "找不到 $vendor —— 先运行 ./scripts/fetch-pdfium.sh $target" >&2
  exit 2
fi

out="dist/pdf_reader_${target//-/_}"
rm -rf "$out"
mkdir -p "$out/lib"

echo "== cargo build --release =="
cargo build --release -p app --offline

exe="target/release/app"
[ -f "$exe" ] || exe="target/release/app.exe"
cp "$exe" "$out/app"
[ "$exe" = "target/release/app.exe" ] && mv "$out/app" "$out/app.exe"
cp "$vendor" "$out/lib/$libname"

# 附一份来源与校验信息，便于排查"用户机器上加载了别的 pdfium"
{
  echo "platform: $target"
  echo "binary:   $(basename "$exe")  sha256=$(sha256sum "$exe" 2>/dev/null | cut -d' ' -f1)"
  echo "pdfium:   $libname  sha256=$(sha256sum "$vendor" | cut -d' ' -f1)"
  echo "built:    $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out/BUILD-INFO.txt"

echo "== 自检：在临时目录里直接运行打包产物（不设 PDFIUM_PATH、不依赖当前目录） =="
tmp="$(mktemp -d)"
cp -r "$out/." "$tmp/"
( cd "$tmp" && unset PDFIUM_PATH && "./app" --selftest ) \
  || { echo "自检失败：打包产物加载不到 lib/$libname" >&2; rm -rf "$tmp"; exit 1; }
rm -rf "$tmp"

du -sh "$out" | sed 's/^/产物大小：/'
find "$out" -maxdepth 2 -type f | sed 's/^/  /'
echo "完成：$out"
