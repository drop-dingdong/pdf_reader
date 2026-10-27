# S5 评估：把标注写回 PDF

> **状态**：评估稿 + **S5a spike 已完成**（2026-10-27），实现未开始。
> 结论见 §1，S5a 实测结果见 §12，决策已确认（§10）。
> **事实基线**：本仓库 `vendor/pdfium-linux-x64/lib/libpdfium.so` 的导出符号、
> `pdfium-render 0.9.4` 源码、以及两个一次性的实测探针（复现方法见 §11）。
> 关联：D-118（注释功能计划，S5 定义）、D-119/D-120/D-121（S4 的实现）。

## 1. 结论摘要

**能写，但不能照 D-118 原来的设想"FPDFAnnot_* 一把梭 + 写成功后删 overlay"直接做。**
三个硬约束决定了方案形状：

1. **墨迹、下划线、删除线必须自带外观流 `/AP`，否则别的阅读器看不见；高亮不用。**
   实测：`/Ink` 没有 `/AP` 时 PDFium 只有 **1 个彩色像素**（不可见），补外观后 162 个；
   **下划线无 `/AP` 是 0 像素**；删除线只有 24 像素（只覆盖文字、空页为 0）；高亮相反，
   没有 `/AP` 也能被合成（黄色 3996 + 文字 271）。
   **S5a 已找到通用解**：`FPDFAnnot_SetAP` 接一段外观内容，PDFium 会建出 BBox = `/Rect` 的 Form；
   `AppendObject` 反而只支持 ink/stamp（对下划线返回失败）。
2. **`pdfium-render 0.9.4` 没有任何注释 API，也不暴露文档/页面句柄，保存 flags 写死为 0。**
   写回必须自带一层很小的 FFI（`libloading` 已在依赖里），而且这层 FFI 会**绕过 pdfium-render 的全局锁** ——
   PDFium 不是线程安全的，后台渲染线程随时可能在里面。→ 建议**在子进程里写回**（隔离 + 崩溃安全 + Windows 文件占用也好处理）。
3. **我们的程序读不回 PDF 注释。** 打开文档只读大纲/链接/文本，标注只来自 `<pdf>.annot.json`。
   所以"写成功后删 overlay"等于**让标注在本程序里永久消失且不可编辑**。
   → S5 的第一形态应当是**"导出带标注的副本"**，overlay 保留；"原地替换 + 迁移"要等能读回 PDF 注释之后（记为 S6）。

**推荐路线**：
`S5a` spike（**已完成**，见 §12）→ `S5b` 导出副本 MVP（2–4 天）→
`S5c` 原地替换（2–3 天，依赖 S6 的读回能力）。
spike 结论：**四种标注都能写，且不需要 lopdf、不需要 AppendObject**——统一用
`CreateAnnot` + 语义字段（`InkList` / `QuadPoints`）+ `SetAP` 生成外观（高亮除外）。

## 2. 现状（代码事实）

| 项 | 现状 | 位置 |
| --- | --- | --- |
| 打开来源 | `Source::Path` 或 `Source::Bytes`；**内存来源没有路径**，无法写回 | [`document.rs`](../../../crates/pdfcore/src/document.rs) |
| 目前保存什么 | 只有两个 sidecar：`<pdf>.json`、`<pdf>.annot.json`；**从不写 PDF** | [`app.rs`](../../../crates/app/src/app.rs) 的 `save_all` / `save_overlay`，[`persist.rs`](../../../crates/app/src/persist.rs) |
| 原子性 | sidecar 用 `write_atomic`（temp + rename） | `persist::write_atomic` |
| 权限 | `DocumentInfo.permissions.annotate` 已有（PDFium 权限位映射） | `backend-pdfium/src/document.rs` |
| 加密 | `DocumentInfo.encrypted` 已有 | 同上 |
| 数字签名 | pdfium-render 有 `PdfDocument::signatures()`，但 **pdfcore 没有暴露** | 需要新增能力位 |
| 标注几何 | **未旋转页面空间**（与 PDF 的 `/InkList`、`/QuadPoints` 同坐标系） | `pdfcore::annotation` |
| 标注读回 | **没有**。打开时只解析链接/大纲/文本 | `backend-pdfium/src/document.rs` |
| 线程 | `Capabilities::max_concurrency = 1`；pdfium-render 默认 features 含 `thread_safe`（全局互斥） | `pdfcore::document`、`backend-pdfium` |

## 3. 能力核查（导出符号 + 源码 + 实测）

### 3.1 PDFium C API：够用，但有两个坑

`libpdfium.so` 导出 **60 个 `FPDFAnnot_*`**。与 S5 直接相关的：

| 能力 | 函数 | 备注 |
| --- | --- | --- |
| 创建注释 | `FPDFPage_CreateAnnot(page, subtype)` | 文档列出的可创建子类型包含 **highlight / ink / strikeout / underline**（还有 square、circle、freetext 等） |
| 墨迹几何 | `FPDFAnnot_AddInkStroke(annot, points, count)` | 写 `/InkList`，并负责 `/Rect` |
| 文本类几何 | `FPDFAnnot_SetAttachmentPoints` / `AppendAttachmentPoints` | 文档原文：“attachment points (**i.e. quadpoints**)”；字面是"附件点"，实际就是 `/QuadPoints` —— **必须 spike 验证对 Highlight 生效** |
| 外观流 | `FPDFAnnot_AppendObject(annot, obj)` | 文档明确：“**Currently, only ink and stamp annotations are supported**” |
| 画外观的路径对象 | `FPDFPageObj_CreateNewPath` / `FPDFPath_MoveTo` / `LineTo` / `SetDrawMode` / `FPDFPageObj_SetStrokeColor` / `SetStrokeWidth` | 都从 `.so` 导出 |
| 正文/颜色/矩形/标志 | `FPDFAnnot_SetStringValue`（UTF-16LE）/ `SetColor` / `SetRect` / `SetFlags` / `SetBorder` | |
| 保存 | `FPDF_SaveAsCopy` / `FPDF_SaveWithVersion` | flags：`FPDF_INCREMENTAL=1`、`FPDF_NO_INCREMENTAL=2`、`FPDF_REMOVE_SECURITY=3` |
| **没有** | 没有叫 `SetQuadPoints` / `SetVertices` 的函数 | 文本类只能走上面的 `SetAttachmentPoints` |
| **没有** | 没有"生成外观"的一键 API | 墨迹要自己 `AppendObject` 画路径 |

### 3.2 `pdfium-render 0.9.4`：帮不上忙

| 限制 | 证据 |
| --- | --- |
| 没有 annotation 模块，也没有 page object 模块 | crate 的 `src/pdf/` 只有 document/font/path(页面内容路径)/link/…，没有 annotation |
| 文档/页面句柄不可见 | `PdfDocument::handle()`、`PdfPage::page_handle()` 都是 `pub(crate)` |
| 绑定模块私有 | `lib.rs` 里是 `mod bindings;`（不是 `pub mod`） |
| 保存 flags 写死 0 | `save_to_writer` 源码注释是 “TODO: investigate supporting FPDF_INCREMENTAL…” 然后 `let flags = 0;` |
| 默认 `thread_safe` | 所有 PDFium 调用走它的**全局锁**；我们的 FFI 不受这把锁保护 |

→ 结论：**要么 fork pdfium-render**（维护成本高），**要么自己写一层 FFI**（推荐，见 §5）。
`libloading` 已作为 pdfium-render 的依赖存在，离线环境可用；**不要再引入新的 PDF 引擎**（`lopdf` 不在离线缓存里，见 §4）。

### 3.3 外观流实测（最关键的一条）

用 Rust 现场拼 PDF（300×300 页面），同一份几何分别带/不带 `/AP`，再用现有后端渲染、数像素：

| 用例 | 渲染结果 | 结论 |
| --- | --- | --- |
| `/Ink`，**无** `/AP` | 红色像素 **1 个** | 肉眼不可见（那 1 个是抗锯齿边角） |
| `/Ink`，**有** `/AP`（Form XObject 画一条红线） | 红色像素 **162 个** | 可见 |
| `/Highlight`，**无** `/AP`（只有 `/QuadPoints`） | 黄色 **3996** 个 + 深色（文字）**271** 个 | PDFium 会**合成**高亮外观；文字仍可读（白底×黄 = 黄，字仍是深色，效果类似 multiply） |
| `/Highlight`，**有** `/AP`（手写但尺寸写错） | 黄色 **324** 个，区域内不是黄色 | **一旦提供 `/AP`，外观完全以它为准**，写错就难看 |
| `/Underline`，**无** `/AP` | 红色 **0** 个（空页与带文字页都是 0） | 下划线**不会**被合成，必须写 AP |
| `/StrikeOut`，**无** `/AP` | 红色 **24** 个，且只在文字上（空页 0） | 删除线只按文字层合成，必须写 AP |
| `/Underline`，AP 由 **`SetAP`** 提供 | 红色 **440** 个，包围盒 (40,258)-(259,259) | SetAP 是通用解：Form 的 BBox = `/Rect`、内容用页面坐标 |
| `/Ink`，AP 由 **`SetAP`** 提供（两条子路径） | 红色 **618** 个，包围盒 (40,180)-(259,240) | 墨迹也可以只用 SetAP；`AddInkStroke` 仍要写（语义） |

→ **墨迹：没有 AP 就白写**；**文本类：可以不写 AP**（但透明度不受我们控制，见 §3.4）。

### 3.4 数据格式与坐标

* `/InkList` 与 `/QuadPoints` 都是**未旋转页面空间** ⇒ 我们的存储可以**原样落值**，不需要做旋转换算（旋转页也一样；位置由阅读器应用 `/Rotate`）。
* `/Rect` 必须覆盖几何：`AddInkStroke` 会自己算；文本类需要由 `quads` 的并集自己给（我们已有 `Annotation::bounds`）。
* `/Contents`（注释正文）用 `FPDFAnnot_SetStringValue`，值是 **UTF-16LE**；往返可读。
* `/C`：颜色。**不透明度 `/CA` 没有公开 setter**（`SetStringValue` 只能写字符串，写数字键是错的）。
  → 文本类写回后**不会保留我们 35% 的透明度**；PDFium 合成的高亮看起来接近不透明黄。缓解：选浅一点的颜色，或接受差异并写进文档。
* 旋转页：`/Rect` 与 `/QuadPoints` 仍是未旋转坐标系（与 PDF 规范一致）；spike 要用带 `/Rotate 90` 的夹具验证渲染位置。

## 4. 方案对比

| 方案 | 优点 | 缺点 | 判断 |
| --- | --- | --- | --- |
| **A. PDFium FFI（子进程）** | 与渲染同一个引擎；创建 API 齐全；`AddInkStroke`/`AppendObject` 直接支持墨迹；无需新依赖 | 要自己维护 ~25 个 FFI 声明；必须处理 AP；线程安全靠子进程隔离 | **推荐** |
| B. 引入 `lopdf`，纯 Rust 改 PDF 字典 | 能写任意键（含 `/CA`）；不需要 FFI；单测不需要 PDFium | **离线缓存里没有这个 crate**（门禁全用 `--offline`）；全量重写整个文件；加密/签名/大文件风险；两套 PDF 引擎 | 备选（A 的 spike 失败时再评估） |
| C. 把标注**烘焙**进页面内容（PDFium path object + `InsertObject` + `GenerateContent`） | 任何阅读器都能显示；不依赖 AP | 不是注释（不能在 Acrobat 注释列表里增删）；权限语义从 "annotate" 变 "modify"；不可逆 | 只作为"打印/固化"导出，不作为 S5 |
| D. 不写回（只导出图片/打印） | 零风险 | 不满足需求 | 否 |

## 5. 推荐实施路线

### S5a：spike（**已完成**，2026-10-27；结果见 §12）

在 `backend-pdfium` 的测试/示例里搭一个最小 FFI（不要进产品路径），验证五个假设：

1. `CreateAnnot(Ink)` + `AddInkStroke` + `SetColor` + `AppendObject(路径对象)` + `SaveAsCopy` →
   重新用现有后端渲染，墨迹**可见**（红色像素 > 100）。
2. `CreateAnnot(Highlight/Underline/StrikeOut)` + `SetRect` + `SetColor` + `SetAttachmentPoints` →
   保存后渲染，**位置正确**（含 `/Rotate 90` 夹具），文字可见。
3. `SetStringValue("Contents", 中文)` → 保存后 `GetStringValue` 能读回（UTF-16LE 往返）。
4. `SaveAsCopy` 的 flags：`0` 与 `FPDF_NO_INCREMENTAL` 的实际差别（文件大小、是否可被 PDFium 重新打开）。
5. 失败路径：只读目录、加密夹具（若暂时造不出加密 PDF，就记录为待办）。

**判定（实际结果）**：全部通过，但下划线/删除线**必须写 AP**（无 AP 时 0 像素）。
采用 `SetAP` 统一生成外观，`AppendObject` 不再需要。
spike 代码保留为可复现的 example：`crates/backend-pdfium/examples/s5_spike.rs`
（`cargo run -p backend-pdfium --example s5_spike`），作为 S5b 的起点。

### S5b：导出带标注的副本（2–4 天）

* **形态**：菜单/按钮「导出带标注的 PDF…」→ 默认输出 `<原名>.annotated.pdf`；**不做原地替换**。
* **执行**：主进程把任务写成临时 JSON（pdf 路径、输出路径、逐页标注几何/颜色/正文），
  用 `std::process::Command` 启动**同二进制的隐藏子命令** `--writeback <job.json>`。
  子进程内 `FPDF_InitLibrary` → 载入 → 创建注释（`CreateAnnot` + `InkList`/`QuadPoints`）
  → 生成外观（`SetAP`；高亮不写）→ `FPDF_NO_INCREMENTAL` 保存到临时文件 → 退出。
  父进程校验（重开 + 渲染抽样 + 注释计数），再 rename 到目标。
* **外观生成**（spike 已验证）：每条 AP 是一段内容流字符串
  `q R G B RG w x1 y1 m x2 y2 l … S Q`（页面坐标、固定小数位）；下划线画在
  `quad.bottom`、删除线画在 `quad.center.y`、墨迹按每条 stroke 一个子路径。
* **子进程的理由**：① PDFium 线程安全（父进程有后台渲染线程）；② 写回崩溃不拖垮阅读器；
  ③ Windows 上父进程可能还占着原文件（导出到新路径本来也规避了这点）。
* **UI 反馈**：失败必须说清原因（只读 / 加密 / 无权限 / 磁盘），不能"看起来成功了"。
* **不做**：加密文档、有数字签名的文档、`permissions.annotate == false`（明确拒绝并提示）。

### S5c：原地替换 + 迁移（2–3 天，**等 S6 能读回 PDF 注释**）

* 备份策略：先写 `<pdf>.s5-backup`（或带时间戳），再原子 rename；失败保留备份并提示。
* 闸门：加密 / 签名 / `annotate == false` / 只读路径 / 内存来源（无路径）。
* 迁移：只有在"我们能把 PDF 注释读回来画"之后，才能删 `<pdf>.annot.json` 与 `custom_texts` 里的
  `#annot:` 条目；否则删掉就是永久丢编辑能力。

## 6. 风险清单

| # | 风险 | 证据 | 缓解 / MVP 决策 |
| --- | --- | --- | --- |
| R1 | 墨迹/下划线/删除线没有 `/AP` 就不可见 | 实测：墨迹 1 px、下划线 0 px、删除线 24 px（只覆盖文字）；高亮可合成（§3.3/§12） | 用 `SetAP` 生成外观（已验证）；高亮不写 AP |
| R2 | 文本类几何只能靠 `SetAttachmentPoints`（名字叫"附件点"） | 文档写 "i.e. quadpoints"；spike 实测高亮/下划线/删除线的 `/QuadPoints` 与渲染位置都正确 | **已消除**：`AppendAttachmentPoints` 可用（spike 第 2/6 条） |
| R3 | FFI 绕过 pdfium-render 全局锁 → 与渲染线程竞争 | crate 默认 `thread_safe`；`max_concurrency = 1` 只是限流 | 子进程隔离（S5b） |
| R4 | 加密文档保存需要 owner 密码；`FPDF_REMOVE_SECURITY` 会**解密重写** | flags 文档 + 常识 | MVP 直接拒绝加密文档（`DocumentInfo.encrypted`） |
| R5 | 写回会让数字签名失效 | PDF 签名覆盖字节范围 | MVP 拒绝有签名的文档；需要先把 `signatures()` 暴露到 `Document` |
| R6 | 原文件被占用（Windows） / rename 跨设备失败 | PDFium 可能持有文件句柄 | 导出到新路径；spike 记录；原地替换时先关闭文档 |
| R7 | 单向迁移丢编辑能力 | 我们**读不回** PDF 注释 | 不迁移（保留 overlay），S5c 前先做 S6 |
| R8 | 大文件内存/耗时 | `SaveAsCopy` 全量重写 | 用 `many_pages`（120 页）样本测时间/内存，不进 CI 门禁 |
| R9 | PDFium 版本不固定 | `scripts/fetch-pdfium.sh` 取 **latest** | 写回落地前把版本钉死，并在 CI 记录版本 |
| R10 | 透明度/线宽与界面不一致 | `/CA` 无 setter；AP 的 Form **没有 `/Resources`**，用不了 ExtGState | 文档写明差异；高亮交给阅读器合成；墨迹线宽用 `w` 写页面空间值 |
| R11 | AP 内容是**原始 PDF 操作符字符串** | `SetAP` 收内容流（UTF-16LE），PDFium 建 BBox=`/Rect` 的 Form（spike 已核对保存后的字典） | 生成时数字固定 2 位小数（避免 NaN/科学计数法）；长度设上限；坐标只允许有限值 |
| R12 | 旋转页上的注释位置 | spike 的渲染调用传的是 `Rotation::None`，**没有**验证 `/Rotate 90` 下的实际位置 | S5b 必测：用 `/Rotate 90` 夹具 + 应用层旋转映射断言位置 |

## 7. 测试与验收计划

| 层次 | 内容 | 依赖 |
| --- | --- | --- |
| FFI 单元 | 创建/计数/读取 `/Contents`、`/InkList`、`/QuadPoints`；保存后重新解析 | PDFium（可跳过） |
| 端到端渲染 | 导出的 PDF 用**现有后端**渲染，断言墨迹/高亮在预期页面坐标上出现（这是最强断言，能同时抓 AP、坐标、旋转） | PDFium |
| 拒绝路径 | 加密 / 有签名 / `annotate=false` / 只读目录 → 明确错误，原文件字节不变 | 需要加密/签名夹具（待造） |
| 原子性 | 写失败时目标文件不存在或保持旧内容；临时文件被清理 | 只读目录即可 |
| 幂等 | 同一导出跑两次 → 注释数量不翻倍（导出到新文件天然幂等） | PDFium |
| 大文件 | 120 页样本导出耗时/峰值内存 | PDFium |

## 8. 兼容性

* **PDFium（Chrome / Edge / 我们的渲染）**：实测见 §3.3（墨迹需 AP；高亮可合成）。
* **Acrobat / Foxit / macOS Preview**：**未实测**，需要人工验收（同一份导出文件，看四种标注是否显示、位置是否正确、文字是否可读）。
* 这不是"应该没问题"的部分 —— 交付前必须让用户在至少一个非 PDFium 阅读器里看过。

## 9. 工作量

| 阶段 | 内容 | 估时 |
| --- | --- | --- |
| S5a | spike（五个假设） | 0.5–1 天 |
| S5b | 子进程 + job + 导出 + 校验 + UI + 测试 | 2–4 天 |
| S5c | 原地替换 + 闸门 + 备份（不含 S6） | 2–3 天 |
| 文档/门禁 | D-12x、现状文档、基线 | 1 天 |

## 10. 需要你确认的决策

**已确认（2026-10-27）**，全部按建议执行：

| # | 问题 | 结论 |
| --- | --- | --- |
| 1 | 形态 | **导出副本**（`<原名>.annotated.pdf`）；原地写回留给 S5c（依赖 S6） |
| 2 | overlay 去留 | **保留**（导出不改 `<pdf>.annot.json`） |
| 3 | 范围 | **四种都写**（spike 已验证可行） |
| 4 | 加密/签名/无权限 | **拒绝并说明原因** |
| 5 | 输出命名 | `<原名>.annotated.pdf`，**不覆盖**（已存在则报错/加序号） |

## 11. 附录：探针怎么复现

1. 用 Rust 拼一个 300×300 的 PDF：页面 `/Annots [5 0 R]`，对象 5 是
   `/Type /Annot /Subtype /Ink /Rect [40 40 260 260] /InkList [[40 40 260 40 260 260]] /C [1 0 0] /F 4 /Border [0 0 1]`
   （**不带 `/AP`**）。
2. 用现有后端 `PdfiumBackend::open(Source::Bytes(&bytes))` + `raster.render(整页, scale=1)`。
3. 数 `chunks_exact(4)` 里"红色"像素：`R>180 && G<100 && B<100`。
   无 AP → 1；把 `/AP << /N 6 0 R >>` 指向一个 Form XObject（`BBox [0 0 300 300]`，
   内容 `1 0 0 RG 2 w 40 40 m 260 40 l S`）→ 162。
4. 高亮同理：`/Subtype /Highlight /QuadPoints [40 60 260 60 40 40 260 40] /C [1 1 0]`，
   页面内容加一行 `BT /F1 24 Tf 60 45 Td (Hello) Tj ET`；无 AP → 黄色 3996 + 深色 271。

> 探针代码是一次性的，已从仓库删除；需要时按上面 4 步重建（不超过 150 行）。

## 12. S5a spike 实测（2026-10-27，已完成）

跑法：`cargo run -p backend-pdfium --example s5_spike`（需要 PDFium；example 已入库，作为 S5b 的起点）。
下面的数字都是这次实际输出（300×300 页面、整页 `Scale::IDENTITY`）。

| # | 假设 | 结果 |
| --- | --- | --- |
| 1 | 墨迹：`CreateAnnot(Ink)` + `AddInkStroke` + 外观 | **通过**。`AddInkStroke` 返回 **0**（是 0 基索引，不是失败）；保存 1058 字节；重开后注释数 1；渲染红色 220 像素，包围盒 (40,259)-(259,259)（页面 y=40 → 位图行 260，坐标是直写的） |
| 2 | 文本类：`SetAttachmentPoints` 即 `/QuadPoints` | **通过**。高亮 3996 黄 + 271 深色（文字可见），包围盒精确 (40,240)-(259,259)；下划线/删除线见 #6 |
| 3 | `/Contents` UTF-16LE | **通过**。`中文正文测试` 原样读回 |
| 4 | 保存 flags 与兼容性 | **通过**。`flags=0` 与 `FPDF_NO_INCREMENTAL` 都是 832 字节、都能重开且注释数 1；真实样本（`rotated_2p.pdf`）写回后仍是 2 页、第 1 页链接 1 条 |
| 5 | 旋转页 | 能创建/保存/渲染（`/Rotate 90` 读回为 1）；**但本轮渲染传的是 `Rotation::None`，没有验证旋转后的注释位置** → 列为 S5b 必测项 |
| 6 | 下划线/删除线的外观 | **无 AP 时不可见**：下划线 0 像素、删除线 24 像素（只在文字上，空页 0）。`AppendObject` 对下划线返回 **0（失败）**；**`SetAP` 成功**：下划线 440 像素、包围盒 (40,258)-(259,259) |
| 7 | 墨迹 + `SetAP`（两条子路径） | **通过**：618 像素，包围盒 (40,180)-(259,240) → 一个 AP 内容流可以画多条笔画 |

**`SetAP` 的实际语义**（核对保存后的 PDF）：PDFium 生成 `/AP << /N <Form XObject> >>`，
该 Form 的 `/BBox` = 注释的 `/Rect`、**没有 `/Matrix`、也没有 `/Resources`**，流内容就是传入的字符串。
因此：外观坐标 = **页面空间**（无需变换），但**用不了 ExtGState**（不能设透明度）。

**对 S5b 的直接结论**：

1. 四种标注都写：`CreateAnnot` + （墨迹 `AddInkStroke` / 文本类 `AppendAttachmentPoints`）+ **`SetAP` 生成外观**（**高亮除外**）。
2. 高亮不写 AP：写了就是不透明填充，会盖住文字；不写时 PDFium/Acrobat 会按 `/QuadPoints` 合成。
3. 保存统一用 `FPDF_NO_INCREMENTAL`（与 0 实测同尺寸，语义上明确要全量重写）。
4. AP 内容生成要注意：数字固定 2 位小数、只允许有限值、单条内容长度设上限。
5. 不引入 lopdf、不需要 `AppendObject`，FFI 函数从 ~25 个降到 ~18 个。
