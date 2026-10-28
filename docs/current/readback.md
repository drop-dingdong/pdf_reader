# 读回 PDF 注释（S6a）

> 实现：`backend-pdfium/src/readback.rs`（裸 FFI，主进程调用）+ `app::PdfReaderApp::merge_readback`。
> 评估见 [`../decisions/evaluations/s6-readback.md`](../decisions/evaluations/s6-readback.md)；决策 D-134/D-135。

## 1. 什么时候读

打开文档时（`PdfReaderApp::open`）读一次，**先关掉旧会话**（它的渲染 worker 必须 join），
再直接调 PDFium FFI —— 这张表不经过 pdfium-render 的全局锁，不能和渲染线程并发。

读回是**增强**：失败只打印一行 `[readback] …`，不影响打开文档。

## 2. 读什么

只读四种：Highlight(9) / Underline(10) / StrikeOut(12) / Ink(15)；链接、Widget、Popup、
Squiggly 等一律跳过。隐藏（`/F` Hidden/NoView）跳过。

| 字段 | 来源 | 回退 |
| --- | --- | --- |
| 几何（文本类） | `/QuadPoints`（min/max → 矩形） | 为空则跳过该注释 |
| 几何（墨迹） | `/InkList` 第一条 path | 少于 2 点跳过；额外 path 记警告（S6b 前不支持多笔迹） |
| 颜色/透明度 | `FPDFAnnot_GetColor`；**带 AP 时它一定失败**，改从 AP 内容流解析 `RG/rg/G/g` 与 `CA/ca` | 种类默认值 |
| 墨迹线宽 | `/BS` `/W` | 2.0 |
| id | `/NM = pd-fr:<id>` | 无标记 → `None`（外部注释） |
| 正文 | `/Contents`（UTF-16LE） | 无 |

写回（D-134 起）会把透明度写进 `/CA`；下划线/删除线的 AP 内容流里也带 `CA/ca`，
所以读回能还原 `0.35` 这类值（高亮没有 AP，直接读 `/C`+`/CA`）。

## 3. 三个文件的分工

| 文件 | 内容 | 谁写 |
| --- | --- | --- |
| `<pdf>` | 页面 + 写回的注释（`/NM` 标记、`/Contents`、`/AP`） | 仅「写入 PDF」/「导出标注 PDF」 |
| `<pdf>.annot.json` | 注释几何/颜色/透明度/`body_title`（按页） | 「保存」（`overlay_dirty` 时） |
| `<pdf>.json` | 自定义 preview、链接、被引用点；**注释正文**（`#annot:<id>`） | 「保存」（`data_dirty` 时） |

「保存」按钮（`*保存自定义数据`，Ctrl+S 同路径）会把**有改动的两类文件都写**，但**不碰 PDF**；
「写入 PDF」会先 `save_all()`（落盘 A/J），再写 PDF、备份、重开；「导出标注 PDF」只写副本，不保存 sidecar。

## 3.1 文件存在性 × 打开时的表现

| PDF | `.annot.json` | `.json` | 打开后看到什么 |
| --- | --- | --- | --- |
| 有 | 无 | 无 | 从 PDF 读回带标记的注释（可编辑），正文来自 `/Contents`（自动建正文条目） |
| 有 | 有 | 无 | 用 A 的几何；正文条目缺失时**从 `/Contents` 补回**（状态栏“补回 N 条正文”），补不回来就显示“正文已丢失” |
| 有 | 无 | 有 | 从 PDF 读回注释；正文优先用 J 里的 `#annot:<id>` 条目，缺了才用 `/Contents` |
| 有 | 有 | 有 | A 的几何 + J 的正文；PDF 只用于补 A 中缺失的正文（overlay 优先） |
| 有 | 坏文件 | 任意 | A 按“不存在”处理（提示一句），走“无 A”的恢复路径 |
| 有 | 无 | 有（PDF 无注释） | 没有注释；J 里的正文条目成为**孤儿**（还在 preview 列表里） |
| 有 | 有 | 无（PDF 无正文） | 注释在；正文显示“正文已丢失”；写入 PDF 时正文为空 |

## 3.2 合并规则（overlay 优先）

| 场景 | 行为 |
| --- | --- |
| `.annot.json` 存在 | **overlay 赢**：读取只用于校验；PDF 里多出来的标记注释不导入（用户删掉的，下次写回会删） |
| `.annot.json` 不存在 | 导入所有带标记的注释为**可编辑**（id 取自 `/NM`）；正文优先复用已有 `custom_texts` 条目，没有才用 `/Contents` 新建 |
| 没有 `/NM` 的外部注释 | S6b 再做（只读）；当前跳过 |
| 读回失败 / 单条坏 | 跳过并计数，打开流程继续 |

导入后置 `overlay_dirty`（状态栏提示"已从 PDF 恢复 N 条标注"），保存时写回 `.annot.json`；
在保存之前重开，会再次从 PDF 恢复（幂等）。

## 4. 已知限制

* 只读回我们自己的标记注释；外部注释要等 S6b（只读展示，不改写回）。
  另外页面位图**默认不烤注释**（INV-14），所以外部注释在 S6b 之前完全不可见。
* 一条外部 ink 若有多条 path，只取第一条（S6b 需要模型扩展）。
* 别的程序把 `/NM` 去掉后，这条注释会被当成外部注释；下次写回可能重复添加（评估 §9 的风险）。
* 读回在**主进程**；如果将来在打开期间还有别的 PDFium 调用（后台任务、第二个窗口），必须迁到子进程。

## 5. 从 PDF 重建（S6c）

工具栏「从 PDF 重建」（PDF 里至少有一条 `pd-fr:` 标记时才可用）：

```text
确认框（PDF 里 N 条标记注释 / 当前 overlay X 条标注、Y 条正文）
→ 备份 <pdf>.annot.json 与 <pdf>.json 为 .bak-<纳秒>
→ 删除 <pdf>.annot.json
→ 只删除 <pdf>.json 里标题以 #annot: 开头的正文条目（链接/自定义 preview 保留）
→ 重开文档：A 不存在 → 走 S6a 从 PDF 导入标注与 /Contents 正文
→ 自动保存新的 A/J（overlay_dirty/data_dirty）
```

语义是“**以 PDF 为准**”：

* overlay 里有、PDF 里没有的标注（还没写回的）会丢；
* 正文只认 PDF `/Contents`；J 里那些 `#annot:` 正文先被删掉，再按 PDF 重建；
* 备份保留，出问题可以手工恢复。

## 6. 手动验收

```bash
# 1) 写一份带注释的 PDF（界面「写入 PDF」），2) 删掉旁边的 .annot.json，3) 重开
cargo run -p app -- scripts/replay/rotated_2p.pdf
```

期望：标注回来了、正文可点开、状态栏有"已从 PDF 恢复 N 条标注"；再点「写入 PDF」不重复。
