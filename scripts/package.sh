#!/usr/bin/env bash
# 打一个"自带 PDFium"的发布目录。**只能打包当前平台**。
#   ./scripts/package.sh                 # 自动识别当前平台，默认 latex-svg 后端
#   ./scripts/package.sh linux-x64       # 显式写出来（必须与当前平台一致，只是个自检）
#   ./scripts/package.sh --backend ratex # 打包默认 RaTeX 后端的二进制
#   ./scripts/package.sh --backend latex-png
#
# 为什么不接受"打包别的平台"：这个脚本调用的是宿主的 `cargo build`，产出的是**宿主格式**
# 的可执行文件。在 Linux 上 `./scripts/package.sh win-x64` 会得到一个 ELF 却命名为 Windows 包 ——
# 那不是在打包，是在造一个假的产物。交叉编译需要额外的 target + 链接器，本脚本不尝试；
# Windows/macOS 的产物请在那两个平台（或 CI 的对应 runner）上跑本脚本。
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

backend=latex-svg
platform=""
while [ $# -gt 0 ]; do
  case "$1" in
    --backend|--math-backend)
      backend="${2:?--backend 需要 latex-svg|latex-png|ratex}"
      shift 2
      ;;
    *)
      platform="$1"
      shift
      ;;
  esac
done
case "$backend" in
  latex-svg) features=() ;;
  latex-png) features=(--features math-latex-png) ;;
  ratex) features=(--features math-ratex) ;;
  *) echo "未知数学后端：$backend（可用 latex-svg / latex-png / ratex）" >&2; exit 2 ;;
esac

host="$(uname -s 2>/dev/null || echo unknown)-$(uname -m 2>/dev/null || echo unknown)"
case "$host" in
  Linux-x86_64)   host_tag=linux-x64 ;;
  Linux-aarch64)  host_tag=linux-arm64 ;;
  Darwin-arm64)   host_tag=mac-arm64 ;;
  Darwin-x86_64)  host_tag=mac-x64 ;;
  MINGW*|MSYS*|CYGWIN*) host_tag=win-x64 ;;
  *) echo "无法识别当前平台：$host（本脚本只能打包当前平台）" >&2; exit 2 ;;
esac

target="${platform:-$host_tag}"
if [ "$target" != "$host_tag" ]; then
  echo "拒绝打包：本脚本只能产出**当前平台**的可执行文件。" >&2
  echo "  当前平台：$host_tag（$host）" >&2
  echo "  请求平台：$target" >&2
  echo "  在 $host_tag 上构建出的二进制是 $host_tag 的格式，改名成 $target 只会得到跑不起来的假产物。" >&2
  echo "  正确做法：在 $target 的机器上（或 CI 的 $target runner 上）运行本脚本。" >&2
  echo "  交叉编译需要额外安装 target 与链接器，本脚本不提供。" >&2
  exit 2
fi

case "$target" in
  linux-*) libname=libpdfium.so ;;
  mac-*)   libname=libpdfium.dylib ;;
  win-*)   libname=pdfium.dll ;;
  *) echo "未知平台 $target" >&2; exit 2 ;;
esac

vendor="vendor/pdfium-$target/lib/$libname"
# Windows 的上游包把运行库放在 bin/ 下（lib/ 里是导入库 pdfium.dll.lib）；两种布局都接受
if [ ! -f "$vendor" ] && [ -f "vendor/pdfium-$target/bin/$libname" ]; then
  vendor="vendor/pdfium-$target/bin/$libname"
fi
if [ ! -f "$vendor" ]; then
  echo "找不到 vendor/pdfium-$target/{lib,bin}/$libname" >&2
  echo "  注意：Windows 上 lib/pdfium.dll.lib 是**导入库**（链接期用），运行库是 bin/pdfium.dll。" >&2
  echo "  先运行 ./scripts/fetch-pdfium.sh $target（它会把运行库归位到 lib/）" >&2
  exit 2
fi

out="dist/pdf_reader_${target//-/_}"
if [ "$backend" != "latex-svg" ]; then
  out="${out}_${backend//-/_}"
fi
rm -rf "$out"
mkdir -p "$out/lib"

echo "== cargo build --release（数学后端：$backend） =="
cargo build --release -p app --offline "${features[@]}"

exe="target/release/app"
[ -f "$exe" ] || exe="target/release/app.exe"
cp "$exe" "$out/app"
[ "$exe" = "target/release/app.exe" ] && mv "$out/app" "$out/app.exe"
cp "$vendor" "$out/lib/$libname"

# 附一份来源与校验信息，便于排查"用户机器上加载了别的 pdfium"
{
  echo "platform: $target"
  echo "math_backend: $backend"
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
