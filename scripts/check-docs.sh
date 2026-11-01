#!/usr/bin/env bash
# 文档一致性：所有相对链接可解析；不再引用已移出工作区的旧文档。
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import os, re, sys
docs = ["docs/README.md", "agent.md"]
for dirpath, _, files in os.walk("docs"):
    for name in files:
        if name.endswith(".md"):
            docs.append(os.path.join(dirpath, name))
for dirpath, _, files in os.walk("crates"):
    for name in files:
        if name == "README.md":
            docs.append(os.path.join(dirpath, name))
bad = []
for path in sorted(set(docs)):
    text = open(path, encoding="utf-8").read()
    for match in re.finditer(r"\[[^\]]*\]\(([^)]+)\)", text):
        target = match.group(1).split("#")[0].strip()
        if not target or target.startswith(("http://", "https://", "mailto:")):
            continue
        resolved = os.path.normpath(os.path.join(os.path.dirname(path), target))
        if not os.path.exists(resolved):
            bad.append(f"{path} -> {target}")
# 2) 代码注释里不能再指向已移出工作区的文档（文档本身可以"以史为据"地提到它们）
retired = ("docs/current/", "docs/decisions/", "docs/decision-log.md", "docs/data-flow.md",
           "docs/trait-definitions.md", "docs/traits.md", "docs/feedback-round",
           "docs/patch-plan-tiling", "docs/preview-ladder-design", "docs/deep-zoom-design",
           "docs/framework-selection", "docs/backlog.md", "docs/custom-data-versions.md")
for dirpath, _, files in os.walk("crates"):
    for name in files:
        if not name.endswith(".rs"):
            continue
        path = os.path.join(dirpath, name)
        text = open(path, encoding="utf-8").read()
        for ref in retired:
            if ref in text:
                bad.append(f"{path} 引用了已移出的 {ref}")
if bad:
    print("文档检查失败：")
    for item in bad:
        print(" -", item)
    sys.exit(1)
print(f"文档检查通过（{len(set(docs))} 篇，0 条失效链接）")
PY
