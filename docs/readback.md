# 读回 PDF 注释

> 实现：`backend-pdfium/src/readback.rs`（裸 FFI，主进程调用）+ `app::PdfReaderApp::merge_readback`。
> 相关约束见 [architecture.md](architecture.md) §4.5 / §8。

## 1. 什么时候读

打开文档时（`PdfReaderApp::open`）读一次，**先关掉旧会话**（它的渲染 worker 必须 join），
再直接调 PDFium FFI —— 这张表不经过 pdfium-render 的全局锁，不能和渲染线程并发。

读回是**增强**：失败只打印一行 `[readback] …`，不影响打开文档。

## 2. 读什么

只读五种：Highlight(9) / Underline(10) / StrikeOut(12) / Ink(15) / Text(1，便签)；
链接、Widget、Popup、Squiggly 等一律跳过。隐藏（`/F` Hidden/NoView）跳过。

| 字段 | 来源 | 回退 |
| --- | --- | --- |
| 几何（文本类） | `/QuadPoints`（min/max → 矩形） | 为空则跳过该注释 |
| 几何（墨迹） | `/InkList` 第一条 path | 少于 2 点跳过；额外 path 只计数、当前不显示 |
| 几何（便签） | `/Rect`（`FPDFAnnot_GetRect`） | 宽/高 ≤0 跳过 |
| 颜色/透明度 | `FPDFAnnot_GetColor`；**带 AP 时它一定失败**，改从 AP 内容流解析 `RG/rg/G/g` 与 `CA/ca`；显式 `/CA 1.0` 对内部注释保留 100%，对外部高亮沿用旧的 35% 兼容回退 | 种类默认值 |
| 墨迹线宽 | `/BS` `/W` | 2.0 |
| id | `/NM = pd-fr:<id>` | 无标记 → `None`（外部注释） |
| 正文 | `/Contents`（UTF-16LE） | 无 |

写回会把透明度写进 `/CA`；下划线/删除线的 AP 内容流里也带 `CA/ca`，
所以读回能还原 `0.35` 这类值（高亮没有 AP，直接读 `/C`+`/CA`）。

## 3. 三个文件的分工

| 文件 | 内容 | 谁写 |
| --- | --- | --- |
| `<pdf>` | 页面 + 写回的注释（`/NM` 标记、`/Contents`、`/AP`） | 「保存」/「另存为」 |
| `<pdf>.annot.json` | 旧格式，**已完全不读不写**（遗留文件忽略） | — |
| `<pdf>.json` | 自定义 preview、链接、被引用点（**不含注释正文**） | 「保存」（`data_dirty` 时） |
| PDF | 注释的唯一持久化位置（几何 + 正文） | 「保存」/「另存为」 |

「保存」（Ctrl+S 同路径）会把**有改动的两类文件都写**：注释写回 PDF，自定义数据写 `.json`；
只写数据时不碰 PDF。

## 3.1 打开时的表现

| PDF | `<pdf>.json` | 打开后看到什么 |
| --- | --- | --- |
| 有 | 无 | 从 PDF 读回注释（`pd-fr:` 标记恢复 id）；自定义数据为空 |
| 有 | 有 | 从 PDF 读回注释；preview/链接/被引用点来自 `.json` |
| 任意 | 任意 | `.annot.json` 与 `.annot.json.bak-*` 一律忽略 |

## 3.2 合并规则

| 场景 | 行为 |
| --- | --- |
| 带 `/NM = pd-fr:<id>` | 恢复为自有注释，id 固定；正文来自 `/Contents` |
| 没有 `/NM` 的外部注释 | 进入主集合：可读正文、改正文、删除；几何/颜色只读 |
| 同一 id 出现多次 | 后端读回会保留每个对象；应用合并时保留第一条，后续条目分配新 id 并标 `pdf_ref=None`，保存时重建对象并写唯一 `/NM` |
| 读回失败 / 单条坏 | 跳过并计数，打开流程继续 |

## 4. 已知限制

* 外部注释与自有注释**一视同仁**：读正文、改正文、删除；几何/颜色仍只读（UI 没有这些操作）。
 一条外部墨迹有多条 `/InkList` path 时按 path 拆成多条注释（模型仍是一条折线）。
 页面位图**默认不烤注释**（INV-14），外部注释也由主集合绘制。
* **内联字典**（`/Annots [ <<…>> ]`，没有对象号）：可读；改正文/删除按“页 + /Annots 位置”写回
 （`InlineChanges`），不改对象号。
* 一条墨迹有多条 `/InkList` path 时只显示第一条；额外 path 只计数（模型仍是单折线）。
* 别的程序把 `/NM` 去掉后，这条注释会被当成外部注释；下次写回可能重复添加（评估 §9 的风险）。
* 读回在**主进程**；如果将来在打开期间还有别的 PDFium 调用（后台任务、第二个窗口），必须迁到子进程。

## 5. 外部注释

打开时，**没有 `pd-fr:` 标记**的四种注释也进入 `annotations` 主集合：

| 项 | 做法 |
| --- | --- |
| 类型 | Highlight/Underline/StrikeOut/Ink/Text；链接、Widget、Popup 等跳过（Popup 随父删除） |
| 几何 | QuadPoints / InkList 第一条 path；多条 path 只计数 |
| 颜色/透明度 | `/C`、`/CA`；**内部注释显式值（含 1.0）优先；外部高亮 `/CA` 缺失或 1.0 回退 0.35，其它按显式值；非高亮缺失时 1.0** |
| 线宽 | `/BS` `/W`；缺失回退 2.0 |
| 正文 | `/Contents`，显示在注释自己的浮窗里（可编辑） |
| 身份 | `pdf_index` 解析页面树 + `/Annots`（含 ObjStm），得到 `(num,gen)`；按页与 PDFium 读回顺序配对 |
| 交互 | 悬停/删除/正文浮窗；几何/颜色只读 |
| 持久化 | 正文修改/删除只存内存，由「保存」按对象号落盘 |
| 内联字典 | 无对象号：可读；改正文/删除按 `/Annots` 位置写回 |

状态栏不再显示“只读层”；外部注释计入当前标注集合，但不计入 `pdf_marked_annotations`。
样本与断言见
`crates/backend-pdfium/tests/fixtures/s6b/` 和 `tests/s6b_samples.rs`：

* Okular：四类齐全（墨迹 56 点、单 path）；
* Evince：只给文本类、颜色回退黄；
* Firefox：高亮 + 墨迹（128 点）；
* Adobe：注释在对象流里（ObjStm），墨迹 181 点；
* Foxit：四类齐全（墨迹 104 点）；
* 样本覆盖单 path；`without_content` 版本没有 `/Contents`。

## 6. 从 PDF 重建（入口已移除）

注释直接从 PDF 读回，不需要旧图层；
"丢弃内存改动、以 PDF 为准"等价于**关掉再打开同一文件**，因此「从 PDF 重建标注」入口及其实现已移除。

仍然成立的行为：

* 打开 PDF 时按 `pd-fr:` 标记恢复自有注释 id，其余注释按外部注释导入；
* 未写回 PDF 的注释在关闭/重开时丢失（以 PDF 为准）；
* 外部注释的正文修改/删除仍由「保存」按对象号写回。

## 7. 手动验收

```bash
# 1) 打开一份 PDF、加注释，2) 文件 → 保存（写到原 PDF），3) 关闭再打开
cargo run -p app -- scripts/replay/rotated_2p.pdf
```

期望：标注回来了、正文可点开；再点「保存」不会重复写注释。
