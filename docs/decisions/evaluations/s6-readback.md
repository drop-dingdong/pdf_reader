# S6 评估：读回 PDF 注释

> 状态：**评估完成，待决策**（2026-10-27）。本文只回答"能不能、怎么做、先做什么"，
> 不包含实现。相关：S5 评估（[`s5-writeback.md`](s5-writeback.md)）、`docs/current/writeback.md`。

## 1. 结论摘要

* **可行**：PDFium 的读注释 API 在现有动态库里全部存在（§3.1），坐标、颜色、正文、墨迹都能取到。
* **建议分三步**，不要一次做成"任意阅读器的注释都能编辑"：
  1. **S6a（先做）**：读回**我们自己写的**注释（`/NM = pd-fr:<id>`），用于
     ①写回结果的自动校验（round-trip）②overlay 丢失/换机器后的恢复；
  2. **S6b**：读回**外部注释**（Okular 等）→ 只读展示（不改、不删、不写回）；
  3. **S6c（最后）**：迁移——确认 PDF 能完整表达 overlay 之后，才谈删 `.annot.json`。
* 做 S6a 之前有三个**前置修正/决定**：
  1. 写回目前**没有写 `/CA`（透明度）**——读回会把 0.35 的高亮读成 1.0，round-trip 不保真；
  2. 读注释用裸 FFI，而主进程里有 pdfium-render 的全局锁（渲染线程）——**不能在打开新文档时
     直接在主进程读**（旧会话的 worker 可能还在渲染）。建议沿写回的子进程模式（§3.3）；
  3. 模型需要"来源/只读"（S6b）与"多笔迹"（外部 ink 可能一条注释含多条路径）（§6）。
* 合并策略推荐 **overlay 优先**（§5）：PDF 是投影，overlay 是编辑源；这一条需要你确认。

## 2. 现状（代码事实）

* 模型：`Annotation { id, page, kind, color, opacity, body_title }`，
  `kind` 四种：Highlight / Underline / StrikeOut / Ink（几何全在**未旋转页面空间**）。
* 正文不在 `Annotation` 上：`body_title = "#annot:<id>"` 指向 `custom_texts` 里的条目
  （持久化在 `<pdf>.json`）；`.annot.json` 只存几何/颜色/透明度。
* 写回（S5）已经写出的字段：

  | 字段 | 我们写的值 | 读回是否可用 |
  | --- | --- | --- |
  | `/Subtype` | 9/10/11/12 | 是 |
  | `/Rect` |  `Annotation::bounds()`（ink 已含线宽/2） | 是（但几何要从 QuadPoints/InkList 取） |
  | `/C` | RGB 0..255 → 0..1 | 是 |
  | `/CA` | **没写**（SetColor 的 alpha 固定 255） | **读回只能得到 1.0**（前置修正） |
  | `/F` | 4（Print） | 是（隐藏位要过滤） |
  | `/NM` | `pd-fr:<id>`（有 id 时） | 是（S6a 的识别依据） |
  | `/Contents` | 有正文时写 UTF-16 | 是 |
  | `/BS` `/W` | ink 线宽 | 是（外部注释可能没有） |
  | `/InkList` | 一笔的路径 | 是；**外部注释可能有多条路径** |
  | `/QuadPoints` | 文本类每行 8 个数（左上、右上、左下、右下） | 是 |
  | `/AP` | ink：`AppendObject` 原生 Form；下划线/删除线：`SetAP`；高亮：无 | 不需要读 |

* 写入时**先删掉所有 `pd-fr:` 标记的注释再重建**（D-131），所以同一 id 在 PDF 里最多一条。
* 注释标记：`/NM = "pd-fr:<id>"`；非标记注释（链接、其它程序的注释）写回**不碰**。

## 3. 能力核查

### 3.1 PDFium C API：够用

在 `vendor/pdfium-linux-x64/lib/libpdfium.so` 里核对了导出符号，以下全部存在：

| 函数 | 用途 | 备注 |
| --- | --- | --- |
| `FPDFPage_GetAnnotCount` / `FPDFPage_GetAnnot` | 枚举 | 已有（写回在用） |
| `FPDFAnnot_GetSubtype` | 类型 | Highlight=9, Underline=10, StrikeOut=11, Ink=12 |
| `FPDFAnnot_GetRect` | 包围盒 | `FS_RECTF{left,bottom,right,top}` |
| `FPDFAnnot_GetColor` | `/C`、`/CA` | 颜色类型 0；alpha 单独取 |
| `FPDFAnnot_GetBorder` | `/BS` 线宽 | 外部注释可能失败 → 用默认值 |
| `FPDFAnnot_GetStringValue` | `/NM`、`/Contents` | 返回**字节数**，UTF-16LE |
| `FPDFAnnot_HasKey` / `GetValueType` | 判空 | 比"取失败"更明确 |
| `FPDFAnnot_GetFlags` | `/F` | 过滤 Hidden(2)/NoView(32) |
| `FPDFAnnot_CountAttachmentPoints` / `GetAttachmentPoints` | QuadPoints | `FS_QUADPOINTSF` 四个点 |
| `FPDFAnnot_GetInkListCount` / `GetInkListPath` | InkList | 返回**点数**；先数再分配 |

`pdfium-render 0.9.4` 仍然没有注释 API（与 S5 评估一致），所以要么复用 `writeback.rs` 里的
`Ffi`（把它抽成共享模块），要么新写一张表。**建议抽 `annot_ffi.rs`**，读写共用一张表，
避免两份符号定义漂移。

### 3.2 坐标与顺序（容易错的两处）

* **旋转页**：PDF 注释的 `/Rect`、`/QuadPoints`、`/InkList` 都在**未旋转的页面空间**
  （和我们的模型一致），读取时**不要**再套 `/Rotate`。写回测试里已有旋转页样本，
  读回要加一条 round-trip（§12）。
* **QuadPoints 顺序**是"左上、右上、左下、右下"（y 向上）；读回时不要依赖顺序，
  取 `min/max` 还原成矩形（我们写的时候顺序是对的，但外部注释未必）。
* **InkList**：一条注释可以有多条 path；每条 path 是一串点。我们写回只写一条。

### 3.3 线程安全：读回放在哪

* 主进程里 `pdfium-render` 开了 `thread_safe`（内部全局锁），渲染 worker 走这把锁；
  我们用 `libloading` 直接调 FFI **绕过这把锁**。
* 应用"打开新文档"时，旧 `DocumentSession` 还没被替换（`self.session` 在成功后才换），
  旧 worker 可能仍在渲染 → 此时在新文档上直接调 FFI 有数据竞争风险。
* 三个选项：

  | 方案 | 优点 | 缺点 |
  | --- | --- | --- |
  | **A. 子进程** `app --readback`（推荐） | 与写回同构；不碰主进程锁；崩了也不影响 UI | 每次打开多一次进程启动（约 30–80ms）；密码要传（用 stdin，不落盘） |
  | B. 主进程 + 先关旧会话 | 快 | 要重排 `open()`（打开失败会丢旧文档）；仍要证明没有其它 PDFium 调用 |
  | C. 主进程 + 自己加锁 | 快 | pdfium-render 的锁不对外暴露，等于自己再造一套，容易和它冲突 |

  建议 **A**：`app --readback -` 从 stdin 读 JSON（pdf 路径、可选密码），stdout 输出注释 JSON；
  退出码 0 表示"读成功"（单条坏注释跳过并记 warnings），非 0 表示整份打不开。

## 4. 字段映射（PDF → 模型）

| 模型 | 来源 | 规则 / 回退 |
| --- | --- | --- |
| 页号 | 枚举所在页 | 直接 |
| 类型 | `/Subtype` | 9/10/11/12 → 四种；其它跳过 |
| 几何（文本类） | `/QuadPoints` | 每 4 个点 → `Rect::new(min_x,min_y,w,h)`；无 QuadPoints 时用 `/Rect` 的 15% 高度？**不**：直接跳过并警告 |
| 几何（墨迹） | `/InkList` | 每条 path → 一串 `Point`；单点 path 跳过；**多 path 的处理见 §6** |
| 颜色 | `/C` | 0..1 → `round(clamp*255)`；缺省用种类默认（高亮黄、墨迹深色） |
| 透明度 | `/CA` | 0..1；缺省 1.0；**写回要先补 `/CA`，否则我们自己的高亮读回是 1.0** |
| 线宽 | `/BS` `/W` | 缺省 2.0（我们的默认；外部 1.0 也常见，需样本确认） |
| id | `/NM` | `pd-fr:<u64>` 才能进"可编辑"集合；其它（如 Okular 的 `okular-{uuid}`）→ 外部只读 |
| 正文 | `/Contents` | 有 `custom_texts` 同标题条目时**以条目为准**；否则用 Contents 建条目；空串 → 无正文 |
| 只读 | 无 `/NM` 标记 | S6b 的 `origin = Pdf`；不参与写回删除/重建 |
| 过滤 | `/F` | Hidden、NoView 跳过；`/Subtype /Popup`、`/Widget`、`/Link`、表单控件跳过 |

**不读**：作者/日期（`/T`、`/M`）、回复关系（`/IRT`）、外观流（`/AP`）、
声音/文件附件/图章（模型没有对应语义）。第一版不做。

## 5. 合并与优先级（overlay vs PDF）

三种候选：

* **A. overlay 优先（推荐）**：`.annot.json` 存在时，它是"哪些注释存在、长什么样"的权威；
  PDF 只用来补齐 overlay 缺失的正文/在 overlay 不存在时恢复。
* B. PDF 优先：每次打开以 PDF 覆盖 overlay（会把"还没写回的新标注"冲掉，不可接受）。
* C. 冲突时弹窗让用户选：最灵活，但打开文档就弹窗，体验差。

推荐 A 的具体规则：

| 场景 | 处理 |
| --- | --- |
| overlay 存在，且有 `pd-fr:<id>` 标记 | **overlay 赢**（几何/颜色/透明度）；只用 PDF 校验，不改 overlay |
| overlay 存在，PDF 有标记但 overlay 没有该 id | **忽略**（用户删了它；下次写回会删 PDF 里的） |
| overlay 存在，PDF 有**无标记**注释 | S6b：导入为**只读**（不写进 overlay 文件） |
| overlay **不存在**，PDF 有标记 | 导入为**可编辑**，id 取自标记；正文按 §4 的优先级 |
| overlay 不存在，PDF 有无标记引用 | S6b：只读 |
| overlay 有、PDF 没有对应标记 | 保留 overlay（这是"还没写进 PDF"的新标注） |

注意：`.annot.json` 与正文存档 `<pdf>.json` 是**两个文件**。只删 `.annot.json` 时，
正文条目还在 `<pdf>.json` 里 —— 导入时按 `#annot:<id>` 命中就直接复用（保留最新正文），
不命中才用 PDF 的 `/Contents` 新建。这样"删 overlay 再打开"不会丢正文。

## 6. 只读外部注释与模型改动（S6b）

要显示外部注释，模型至少需要两处扩展：

1. **来源/只读**：`Annotation { origin: Overlay | Pdf }`（或 `read_only: bool`）。
   * overlay 保存时**只写 `origin == Overlay`**（否则外部注释会被复制进 sidecar）；
   * UI：只读注释可悬停/可看正文，但**不能删除、不能改正文/颜色**；
   * 写回同步只处理 `pd-fr:` 标记，天然不会删外部注释。
2. **多笔迹**：`AnnotationKind::Ink { points, width }` → 建议改成
   `Ink { strokes: Vec<Vec<Point>>, width }`（至少一条；所有渲染/命中/写回按 strokes 展开）。
   否则 Okular 一类"一条注释多条 path"只能取第一条或拆成多条注释（正文会重复）。

**正文的展示**（只读）：两个选项：

* 只读注释也建 `custom_texts` 条目（标题仍 `#annot:<id>`）→ preview 列表会多出外部条目，
  需要额外标记"来自 PDF，只读"；
* 不建条目，正文只放在**临时映射**里（按 annotation id），点开时只读显示 → 不污染列表。
  推荐第二版做这个（先只显示几何 + 悬停提示"外部注释（只读）"，正文下一步）。

**过滤**：Popup/Widget/Link/FreeText/Stamp 等第一版不读；隐藏（Hidden/NoView）跳过；
`/IRT` 回复不展开。

## 7. 迁移（S6c，最后做）

只有同时满足以下条件才值得删 overlay：

1. S6a 的 round-trip 在四种类型 + 旋转页 + 透明正文上都通过；
2. 打开时所有 overlay 注释都能在 PDF 里找到同 id 标记；
3. 正文都能从 `custom_texts` 或 `/Contents` 取到；
4. 用户明确触发（菜单命令），并先备份 `<pdf>.annot.json.bak-<时间>`。

**不建议自动删**：`.annot.json` 同时承担"未写回的新标注"和撤销恢复的参照；
删掉后如果 PDF 有兼容性问题，编辑能力就没了。第一阶段保持"overlay 是编辑源"，
S6c 只提供一个显式的"以 PDF 为准重建 overlay"命令（先备份，再重建）。

## 8. 失败与容错

* 单条注释坏（QuadPoints 数量不对、InkList 0 路径、坐标 NaN）→ **跳过 + 计数**，
  不阻断打开；状态栏提示"已忽略 N 条无法读取的注释"。
* 整份文件打不开（加密/损坏）→ 读回子进程非 0 退出；主进程**仍按原流程打开**
  （S6 是增强，不是打开的前置条件），只给提示。
* 加密文档：打开时已有密码；读回子进程需要同一个密码 → 走 stdin，避免落盘。
* 外部注释的 `/Rect` 与几何不一致时，以 QuadPoints/InkList 为准（Rect 只是包围盒）。
* 别把 `/NM` 当唯一身份：外部程序可能去掉或改写它（风险 §9）；S6a 只信任
  `pd-fr:<u64>` 且 id 能对上 overlay 的情况。

## 9. 风险清单

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 别的程序剥掉 `/NM` | 我们的注释变"外部只读"；下次写回会**重复添加** | S6b 里按几何/id 匹配？不可靠；先记录已知限制，后续用自定义键或哈希 |
| 多笔迹外部 ink | 只能显示第一条 | §6 的 `strokes` 模型 |
| overlay 与 PDF 的正文冲突 | 显示旧文本 | §5：overlay 优先；写回已同步正文（D-131） |
| 读回性能 | 打开变慢 | 只读 `/Annots`，不渲染；实测后决定是否懒加载 |
| 主进程 FFI 竞态 | 崩溃/错乱 | 子进程（§3.3） |
| 注释被外部修改后，我们重建覆盖 | 丢外部编辑 | 已知限制（D-131 已记录）；S6b 只读不写外部注释 |

## 10. 实现切分与工作量

| 阶段 | 内容 | 估计 |
| --- | --- | --- |
| S6-0 | 写回补 `/CA`；抽 `annot_ffi.rs`；`--readback` 子进程 + JSON；round-trip 测试 | 0.5–1 天 |
| S6a | 解析 `pd-fr` 标记 → overlay 合并（overlay 优先）；缺 overlay 时恢复；正文条目 | 1 天 |
| S6b | `origin/read_only` + 多笔迹 + 外部注释只读展示 + 过滤 + fixture | 1–2 天 |
| S6c | 显式"以 PDF 为准重建"命令 + 备份 | 0.5–1 天 |

总计约 **3–4 天**（不含外部阅读器兼容性；那条线按你的计划单独收样本再做）。

## 11. 测试与验收计划

**自动**：

* round-trip：写四种注释（含透明度、正文、旋转页）→ 读回 → 与原始模型逐字段比较
  （几何容差 0.01pt；颜色/透明度容差 1/255）；
* 幂等：读回 → 再写 → 再读，结果不变；
* 外部样本 fixture：Okular freehand（无 AP、`/NM(okular-{uuid})`、`/InkList`），
  读回 1 条只读 Ink；缺 `/BS` 时线宽回退；缺 `/C` 时颜色回退；
* 合并场景（§5 的表）逐条单测；
* 坏注释：QuadPoints 数量错、InkList 空、坐标 NaN → 跳过且计数；
* 性能：500 条注释的文档，测打开耗时（仅记录，不设门禁）。

**人工**：

* 打开一份"写过注释、删掉 `.annot.json`"的 PDF：注释回来、正文在、可编辑、再写入不重复；
* 打开带 Okular 注释的 PDF：只读显示，不参与写回；
* 旋转页（`/Rotate 90`）上写回 → 删 overlay → 重开，位置与原来一致。

## 12. 需要你确认的决策

1. **合并策略**：A overlay 优先（推荐）/ B PDF 优先 / C 冲突弹窗；
2. **S6b 的外部注释范围**：只读几何（推荐，先做）/ 只读几何 + 正文 / 可编辑；
3. **迁移时机**：S6c 显式命令（推荐）/ 读回成功即自动删 overlay / 永远不删；
4. **读回进程模型**：子进程（推荐）/ 主进程先关旧会话。

## 13. 附录：怎么复现/取样

* 列出 PDF 注释字典（不依赖第三方库）：

  ```bash
  python3 - <<'PY'
  import re
  data=open('x.pdf','rb').read().decode('latin-1')
  for m in re.finditer(r'(?m)^(\d+) 0 obj', data):
      body=data[m.start():data.find('endobj',m.start())]
      d=body[:body.find('stream')] if 'stream' in body else body
      if re.search(r'/Subtype\s*/Ink', d):
          print(d[:800].replace('\n',' '))
  PY
  ```

* 外部阅读器样本：用对应软件画一条 ink / 高亮 / 下划线 / 删除线，保存后把 PDF 放进
  `crates/backend-pdfium/tests/fixtures/`（体积小、可入库），并在测试里注明来源与软件版本。
* 注意：`scripts/replay/*.pdf` 会被 `--samples` 重新生成，**不能**当外部样本的存放地。
