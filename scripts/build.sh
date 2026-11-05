#!/usr/bin/env bash
# 编译不同默认数学后端的可执行文件（运行时仍可用 PDF_READER_MATH_BACKEND 覆盖）。
#
#   ./scripts/build.sh --backend latex-svg
#   ./scripts/build.sh --backend latex-png --release
#   ./scripts/build.sh --backend ratex --release
#   ./scripts/build.sh --backend ratex --release --output dist/app-ratex
set -euo pipefail
cd "$(dirname "$0")/.."

backend=latex-svg
release=0
offline=1
output=""
while [ $# -gt 0 ]; do
  case "$1" in
    --backend)
      backend="${2:?--backend 需要 latex-svg|latex-png|ratex}"
      shift 2
      ;;
    --release)
      release=1
      shift
      ;;
    --online)
      offline=0
      shift
      ;;
    --output)
      output="${2:?--output 需要目录}"
      shift 2
      ;;
    -h|--help)
      sed -n '2,12p' "$0"
      exit 0
      ;;
    *)
      echo "未知参数：$1" >&2
      exit 2
      ;;
  esac
done

case "$backend" in
  latex-svg) features=() ;;
  latex-png) features=(--features math-latex-png) ;;
  ratex) features=(--features math-ratex) ;;
  *) echo "未知后端：$backend（可用 latex-svg / latex-png / ratex）" >&2; exit 2 ;;
esac

profile=debug
cargo_args=(build -p app)
if [ "$release" -eq 1 ]; then
  cargo_args+=(--release)
  profile=release
fi
if [ "$offline" -eq 1 ]; then
  cargo_args+=(--offline)
fi
cargo_args+=("${features[@]}")

echo "== 后端：$backend；profile：$profile；feature：${features[*]:-（无）} =="
cargo "${cargo_args[@]}"
exe="target/$profile/app"
[ -f "$exe" ] || exe="target/$profile/app.exe"
if [ ! -f "$exe" ]; then
  echo "构建完成但找不到 $exe" >&2
  exit 1
fi
echo "可执行文件：$exe"

if [ -n "$output" ]; then
  mkdir -p "$output"
  cp "$exe" "$output/"
  echo "已复制到：$output/$(basename "$exe")"
fi
