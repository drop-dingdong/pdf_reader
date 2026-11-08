#!/usr/bin/env bash
# 本地/CI 统一门禁。用法：./scripts/check.sh
#
# 覆盖：格式、clippy（-D warnings）、全部测试、回放结构基线、文档链接。
# 需要 PDFium 动态库（./scripts/fetch-pdfium.sh）；缺库时回放与真实 PDF 用例会**跳过**，
# 此时脚本会给出警告 —— CI 里必须设置 PDF_READER_REQUIRE_PDFIUM=1（TODO）让它变成失败。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo fmt --check =="
cargo fmt --all -- --check

echo "== cargo clippy -D warnings =="
cargo clippy --workspace --all-targets --offline -- -D warnings

echo "== cargo test --workspace =="
cargo test --workspace --offline

echo "== 回放结构基线（硬件无关） =="
if ! cargo run -q -p app --offline -- --samples scripts/replay >/dev/null 2>&1; then
  echo "警告：样本生成失败（缺 PDFium？）—— 回放门禁被跳过" >&2
else
  for f in perf/baseline/*.json; do
    name="$(basename "$f")"
    cargo run -q -p app --offline -- --replay "scripts/replay/$name" --compare "$f"
  done
fi

echo "== PowerShell UTF-8 BOM =="
for f in scripts/*.ps1; do
  bom="$(head -c 3 "$f" | od -An -tx1 | tr -d ' \n')"
  if [ "$bom" != "efbbbf" ]; then
    echo "缺少 UTF-8 BOM：$f（Windows PowerShell 5.1 会按本地代码页读中文）" >&2
    exit 1
  fi
done

echo "== 文档链接 =="
./scripts/check-docs.sh

echo "全部检查通过"
