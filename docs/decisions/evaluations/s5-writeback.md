# S5 评估：把标注写回 PDF

> **状态**：评估稿（2026-10-27），**未实现**。结论见 §1，待你确认的决策见 §10。
> **事实基线**：本仓库 `vendor/pdfium-linux-x64/lib/libpdfium.so` 的导出符号、
> `pdfium-render 0.9.4` 源码、以及两个一次性的实测探针（复现方法见 §11）。
> 关联：D-118（注释功能计划，S5 定义）、D-119/D-120/D-121（S4 的实现）。

## 1. 结论摘要

**能写，但不能照 D-118 原来的设想"FPDFAnnot_* 一把梭 + 写成功后删 overlay"直接做。**
三个硬约束决定了方案形状：

1. **墨迹必须自带外观流 `/AP`，否则别的阅读器看不见。**
   实测：同样一条 `/Ink` 注释，没有 `/AP` 时 PDFium 渲染出来只有 **1 个彩色像素**（等于不可见）；
   补一个 Form XObject 外观后是 **162 个**。文本类（高亮）相反：没有 `/AP` 也能被 PDFium 合成出来。
2. **`pdfium-render 0.9.4` 没有任何注释 API，也不暴露文档/页面句柄，保存 flags 写死为 0。**
   写回必须自带一层很小的 FFI（`libloading` 已在依赖里），而且这层 FFI 会**绕过 pdfium-render 的全局锁** ——
   PDFium 不是线程安全的，后台渲染线程随时可能在里面。→ 建议**在子进程里写回**（隔离 + 崩溃安全 + Windows 文件占用也好处理）。
3. **我们的程序读不回 PDF 注释。** 打开文档只读大纲/链接/文本，标注只来自 `<pdf>.annot.json`。
   所以"写成功后删 overlay"等于**让标注在本程序里永久消失且不可编辑**。
   → S5 的第一形态应当是**"导出带标注的副本"**，overlay 保留；"原地替换 + 迁移"要等能读回 PDF 注释之后（记为 S6）。

**推荐路线**：
`S5a` 一次性 spike（0.5–1 天，验证三个 API 假设）→ `S5b` 导出副本 MVP（2–4 天）→
`S5c` 原地替换（2–3 天，依赖 S6 的读回能力）。**不要跳过 S5a**：spike 的任一假设不成立，方案就要换（见 §4/§6）。

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

### S5a：一次性 spike（0.5–1 天，先做，不写产品代码）

在 `backend-pdfium` 的测试/示例里搭一个最小 FFI（不要进产品路径），验证五个假设：

1. `CreateAnnot(Ink)` + `AddInkStroke` + `SetColor` + `AppendObject(路径对象)` + `SaveAsCopy` →
   重新用现有后端渲染，墨迹**可见**（红色像素 > 100）。
2. `CreateAnnot(Highlight/Underline/StrikeOut)` + `SetRect` + `SetColor` + `SetAttachmentPoints` →
   保存后渲染，**位置正确**（含 `/Rotate 90` 夹具），文字可见。
3. `SetStringValue("Contents", 中文)` → 保存后 `GetStringValue` 能读回（UTF-16LE 往返）。
4. `SaveAsCopy` 的 flags：`0` 与 `FPDF_NO_INCREMENTAL` 的实际差别（文件大小、是否可被 PDFium 重新打开）。
5. 失败路径：只读目录、加密夹具（若暂时造不出加密 PDF，就记录为待办）。

**判定**：1/2 任一失败 → 停止，回到 §4 重新选方案（B 或 C）。
spike 的代码**不并入产品**，结论与关键片段写进 D-122。

### S5b：导出带标注的副本（2–4 天）

* **形态**：菜单/按钮「导出带标注的 PDF…」→ 默认输出 `<原名>.annotated.pdf`；**不做原地替换**。
* **执行**：主进程把任务写成临时 JSON（pdf 路径、输出路径、逐页标注几何/颜色/正文），
  用 `std::process::Command` 启动**同二进制的隐藏子命令** `--writeback <job.json>`。
  子进程内 `FPDF_InitLibrary` → 载入 → 创建注释 → 保存到临时文件 → 退出。
  父进程校验（重开 + 渲染抽样 + 注释计数），再 rename 到目标。
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
| R1 | 墨迹没有 `/AP` 就不可见 | 实测 1 px vs 162 px（§3.3） | 必须 `AppendObject` 生成外观；spike 第 1 条 |
| R2 | 文本类几何只能靠 `SetAttachmentPoints`（名字叫"附件点"） | 文档写 "i.e. quadpoints"，但无 `SetQuadPoints` | spike 第 2 条验证；失败则改用 B/C |
| R3 | FFI 绕过 pdfium-render 全局锁 → 与渲染线程竞争 | crate 默认 `thread_safe`；`max_concurrency = 1` 只是限流 | 子进程隔离（S5b） |
| R4 | 加密文档保存需要 owner 密码；`FPDF_REMOVE_SECURITY` 会**解密重写** | flags 文档 + 常识 | MVP 直接拒绝加密文档（`DocumentInfo.encrypted`） |
| R5 | 写回会让数字签名失效 | PDF 签名覆盖字节范围 | MVP 拒绝有签名的文档；需要先把 `signatures()` 暴露到 `Document` |
| R6 | 原文件被占用（Windows） / rename 跨设备失败 | PDFium 可能持有文件句柄 | 导出到新路径；spike 记录；原地替换时先关闭文档 |
| R7 | 单向迁移丢编辑能力 | 我们**读不回** PDF 注释 | 不迁移（保留 overlay），S5c 前先做 S6 |
| R8 | 大文件内存/耗时 | `SaveAsCopy` 全量重写 | 用 `many_pages`（120 页）样本测时间/内存，不进 CI 门禁 |
| R9 | PDFium 版本不固定 | `scripts/fetch-pdfium.sh` 取 **latest** | 写回落地前把版本钉死，并在 CI 记录版本 |
| R10 | 透明度/线宽与界面不一致 | `/CA` 无 setter；线宽可写 `/BS /W` | 文档写明差异；颜色选浅色；墨迹线宽用页面空间值 |

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

| # | 问题 | 我的建议 |
| --- | --- | --- |
| 1 | **形态**：导出副本 / 原地写回 / 两者都要 | **先做导出副本**（原地要等 S6 能读回注释，否则标注在本程序里消失） |
| 2 | **overlay 去留**：写成功后保留 / 删除 | **保留**（删除 = 永久失去编辑能力；可提示"已导出副本"） |
| 3 | **范围**：四种都写 / 只写墨迹 | **四种都写**，但文本类依赖 S5a 第 2 条假设；假设不成立时退回"只写墨迹 + 保留文本类 overlay" |
| 4 | **加密/签名/无权限**：拒绝 / 提示后强制 | **拒绝**（MVP），错误信息说清原因 |
| 5 | **输出命名/覆盖**：`<pdf>.annotated.pdf`，同名是否覆盖 | 默认不覆盖（已存在则加序号或询问） |

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
