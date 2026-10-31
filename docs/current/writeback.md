# 写回

无改动（`pdf_dirty == false`）时不会重写 PDF；导出无改动时直接复制源文件。
写回前会检查源 PDF 的 mtime/长度是否变化；变化时拒绝覆盖，避免抹掉其它阅读器的修改。
新增注释写 `/M`（ModificationDate）与 `/CreationDate`（UTC PDF 日期）；
已有注释改正文时刷新 `/M`，`/CreationDate` 不变。
外部修改导致拒绝时，当前编辑仍在内存中；可丢弃修改后重开，或等待“另存为”。 PDF（S5b）：FFI 说明与排查手册

> 现状文档：描述**现在是什么样**。改 `crates/backend-pdfium/src/writeback.rs` 或
> `crates/app/src/writeback.rs` 时**必须同步本文**。
> 背景与取舍见 [评估](../decisions/evaluations/s5-writeback.md) 与 D-123 / D-124。

## 1. 这是什么，不是什么

* **是**：把本程序内存里的标注（墨迹 / 高亮 / 下划线 / 删除线）写成**真正的 PDF 注释**，
  保存到一个**新文件**（`<stem>.annotated.pdf`），由主进程编排、子进程执行（S5b-2）。
* `pages` 允许为空：表示"把 PDF 里我们写过的注释全部删掉"（`/NM` 同步会删光，链接等不动）。
  界面上删光标注后，「写入 PDF」/「导出标注 PDF」仍可用（由 `pdf_marked_annotations` 决定）。
* **不是**："读回 PDF 注释"（S6）。读回之前，`.annot.json` 仍是本程序的编辑来源；
  原地写回（S5c）也**不删** overlay。
* **不在主进程里跑**：写回直接在 PDFium 里创建注释，`pdfium-render 0.9.4` 没有这层 API，
  所以我们用 `libloading` 自己绑定了 18 个 C 函数；这些调用**绕过 pdfium-render 的全局锁**，
  与后台渲染线程并发会让 PDFium 崩溃。因此产品路径是**隐藏子命令 + 子进程**。

## 2. 调用链与进程模型

```text
主进程（阅读器，工具栏「导出标注 PDF」）
  └─ app::export：overlay（几何 + 正文）→ job.json（系统临时目录）
       ├─ std::process::Command：<自身 exe> --writeback <job.json>     ← 子进程
       │    └─ app::writeback::run_job（解析 JSON）
                 └─ backend_pdfium::writeback::write（FFI）
                      │    ├─ FPDF_LoadDocument（只读源文件）
                      │    ├─ FPDFPage_CreateAnnot + 语义字段 + SetAP
                      │    ├─ FPDF_SaveAsCopy（导出：NO_INCREMENTAL；原地：INCREMENTAL）→ 内存缓冲
                      │    ├─ 写 spec.output（= 目标同目录的临时文件）
                      │    └─ 重开 output、逐页核对注释数
       ├─ smoke_check：重开 + 页数一致 + 渲染一页（0.5×）
       └─ fs::rename(temp → <stem>.annotated.pdf)
```

**为什么是子进程**（而不是"关掉文档再写"）：

1. PDFium 不是线程安全的；`pdfium-render` 默认 `thread_safe` 会给**它自己的调用**加全局锁，
   而我们的 FFI 不在那把锁里。子进程里没有别的 PDFium 使用者，天然互斥；
2. 写回崩溃只死子进程，阅读器不受影响；
3. Windows 上父进程可能还拿着原文件句柄 —— 子进程写的是**新文件**，根本不碰原文件；
4. 父进程可以把"校验 → rename"放在子进程退出之后做（原子替换），失败可回滚。

> 以后若真要"原地覆盖"（S5c），才需要设计"关闭会话 → 写 → 重开"的静止协议；
> 那时也仍需 temp + rename + 校验。见 D-124 的讨论。

## 3. 初始化契约

* `writeback::write` / `count_annotations` **不会**调用 `FPDF_InitLibrary` / `Destroy`。
* 调用方必须先执行 `backend_pdfium::pdfium()`（它加载动态库并完成进程级初始化）。
  `app --writeback` 分支已经这么做了；集成测试也先调它。
* 同一进程里**只能串行**调用本模块（测试用互斥锁），见 §13。

## 4. 对外 API

| 项 | 说明 |
| --- | --- |
| `WritebackSpec` | `source`（只读源）、`output`（新文件）、`pages` |
| `WritebackPage` | `page: PageIndex` + `annotations` |
| `WritebackAnnotation` | `kind`（复用 `pdfcore::annotation::AnnotationKind`）、`color`、`opacity`（仅记录）、`body` |
| `WritebackReport` | `annotations`（写入条数）、`bytes` |
| `write(&spec)` | 写文件并自检；`source == output` / 退化几何 / 加密 / 签名 / 页号越界都返回错误 |
| `count_annotations(path)` | 逐页注释数（校验用，不渲染） |
| `WritebackError` | `Io` / `Library` / `Invalid` / `Pdfium { stage, message }`；`stage` 直接对应 FFI 函数名 |

`write` 的副作用只有"创建/覆盖 `spec.output`"；**绝不修改 `spec.source`**。

## 5. job.json（`app::writeback`）

**父进程写 job 前会把 `pdf`/`output` 转成绝对路径**（`current_dir().join(path)`；
MSRV 1.75 下不能用 `std::path::absolute`，它 1.79 才稳定）；
子进程仍按“相对 job 文件所在目录”解析 —— 只是防御，正常路径不会走到。
（用户实测事故：相对路径被按 job 目录解析成 `/tmp/scripts/...`，见 D-127。）

```json
{
  "pdf": "in.pdf",
  "output": "out.pdf",
  "pages": [
    { "page": 0, "annotations": [
      { "kind": "ink", "points": [[50, 720], [300, 720]], "width": 2.0,
        "color": [255, 0, 0], "opacity": 1.0, "body": "墨迹" },
      { "kind": "highlight", "quads": [[50, 660, 200, 14]],
        "color": [255, 255, 0], "opacity": 0.35 },
      { "kind": "underline", "quads": [[50, 620, 200, 14]], "color": [255, 0, 0] },
      { "kind": "strike_out", "quads": [[50, 580, 200, 14]], "color": [255, 0, 0] }
    ] }
  ]
}
```

* `kind`：`ink` / `highlight` / `underline` / `strike_out`；
* `quads`：`[x, y, 宽, 高]`，**未旋转页面空间**（与 `pdfcore::annotation` 相同）；
* `points`：墨迹折线顶点（页面空间）；`width`：墨迹线宽（页面空间，默认 2.0）；
* `color`：RGB 0..255（默认黄色）；`opacity`：目前**只记录**，PDF 侧无法表达（§14）；
* `body`：写到注释的 `/Contents`（UTF-16LE）。

## 6. FFI 函数表（全部来自 `libpdfium.so`）

| 符号 | 签名（Rust 侧） | 用途 | 失败表现 |
| --- | --- | --- | --- |
| `FPDF_LoadDocument` | `(path: *const c_char, password: *const c_char) -> FPDF_DOCUMENT` | 打开源/校验输出 | 返回 null（加密/损坏/无权限） |
| `FPDF_CloseDocument` | `(doc)` | 关闭 | 无返回 |
| `FPDF_GetPageCount` | `(doc) -> c_int` | 页数 | 负数表示失败 |
| `FPDF_LoadPage` | `(doc, index: c_int) -> FPDF_PAGE` | 取页 | 返回 null |
| `FPDF_ClosePage` | `(page)` | 释放页 | 无返回 |
| `FPDFPage_GetAnnotCount` | `(page) -> c_int` | 数注释（同步/校验） | 负数 |
| `FPDFPage_GetAnnot` | `(page, index) -> FPDF_ANNOTATION` | 枚举注释（读 `/NM`） | null |
| `FPDFPage_RemoveAnnot` | `(page, index) -> c_int` | 删除过期标记注释 | 0 |
| `FPDFAnnot_GetStringValue` | `(annot, key, *mut u16, len) -> c_ulong` | 读 `/NM`（UTF-16LE） | 0 / 长度 |
| `FPDFPage_CreateAnnot` | `(page, subtype: c_uint) -> FPDF_ANNOTATION` | 建注释 | 返回 null（子类型不支持） |
| `FPDFPage_CloseAnnot` | `(annot)` | 释放注释句柄（**不删除**注释） | 无返回 |
| `FPDFAnnot_SetRect` | `(annot, *const FS_RECTF) -> c_int` | `/Rect` | 0 |
| `FPDFAnnot_SetColor` | `(annot, type, R,G,B,A) -> c_int` | `/C`、`/CA` | 0 |
| `FPDFAnnot_SetFlags` | `(annot, flags: c_int) -> c_int` | `/F`（这里写 4 = Print） | 0（忽略） |
| `FPDFAnnot_SetStringValue` | `(annot, key, *const u16) -> c_int` | `/Contents`（UTF-16LE） | 0 |
| `FPDFAnnot_AddInkStroke` | `(annot, *const FS_POINTF, count) -> c_int` | 墨迹 `/InkList` | **-1**；成功返回 0 基索引（0 是合法值！） |
| `FPDFAnnot_AppendAttachmentPoints` | `(annot, *const FS_QUADPOINTSF) -> c_int` | 文本类 `/QuadPoints` | 0 |
| `FPDFAnnot_SetAP` | `(annot, mode: c_uint, *const u16) -> c_int` | 外观内容流 | 0 |
| `FPDF_SaveAsCopy` | `(doc, *mut FPDF_FILEWRITE, flags: c_uint) -> c_int` | 保存 | 0 |
| `FPDF_GetSecurityHandlerRevision` | `(doc) -> c_int` | 是否加密（`-1` = 未加密） | — |
| `FPDF_GetSignatureCount` | `(doc) -> c_int` | 数字签名数量 | 负数表示错误 |

子类型常量：`Highlight=9`、`Underline=10`、`StrikeOut=12`、`Ink=15`。

> **命名陷阱**：`AppendAttachmentPoints` 名字是"附件点"，但 PDFium 的头文件写的是
> "attachment points (**i.e. quadpoints**)"，实际写的就是 `/QuadPoints`。文本类没有别的入口。

## 7. ABI 结构（`#[repr(C)]`）

| 结构 | 字段 | 字节 | 说明 |
| --- | --- | --- | --- |
| `FsPointF` | `x, y: f32` | 8 | `FS_POINTF`，页面空间 |
| `FsQuadPointsF` | `x1,y1 … x4,y4: f32` | 32 | `FS_QUADPOINTSF`；顺序见 §8 |
| `FsRectF` | `left, top, right, bottom: f32` | 16 | **不是** x/y/w/h；top > bottom |
| `FpdfFileWrite` | `version: c_int` + `write_block: Option<fn>` | 16（x86_64，含 4 字节填充） | `version` 必须是 **1** |
| `Writer` | 第一个字段是 `FpdfFileWrite`，后面是 `Vec<u8>` | — | 回调把 `this` 转回 `Writer`（依赖"首字段偏移 0"），所以必须 `#[repr(C)]` |

写回调签名：`unsafe extern "C" fn(*mut FpdfFileWrite, *const c_void, c_ulong) -> c_int`，
写成功返回 1；`size == 0` 直接返回 1（PDFium 偶尔会这样调用）。

## 8. 几何与坐标（最容易写错的地方）

* 所有几何都是**未旋转页面空间**（左下原点，PDF point）——与 `pdfcore::annotation` 的存储一致，
  **不要**在写回时预旋转。
* `/Rect`：`left = x`、`right = x + 宽`、`bottom = y`、`top = y + 高`（我们的 `bounds()`）。
* `/QuadPoints` 顺序：**左上、右上、左下、右下**（默认坐标系，y 向上）：
  `(x, y+h), (x+w, y+h), (x, y), (x+w, y)`。
* `/InkList`：每条 stroke 一个点数组；`AddInkStroke` 会自己按点集更新 `/Rect`（我们仍显式设一次）。
* 旋转页（`/Rotate 90` 等）：注释坐标**不跟着旋转**（规范如此）；位置由阅读器应用页面旋转。
  但见 §14 的渲染器差异。

## 9. 语义字段 vs 外观（`/AP`）

| 种类 | 语义字段 | 外观 | 为什么 |
| --- | --- | --- | --- |
| 墨迹 | `AddInkStroke` → `/InkList` | `SetAP` 内容流（每条笔画一个子路径） | 无 AP 时 PDFium 渲染 1 px（不可见） |
| 高亮 | `AppendAttachmentPoints` → `/QuadPoints` | **不写** | 写了就是不透明填充、盖住文字；不写时阅读器按 QuadPoints 合成（文字可读） |
| 下划线 | 同上 | `SetAP`（每个 quad 底部一条线） | 无 AP 时 **0 px**（完全不渲染） |
| 删除线 | 同上 | `SetAP`（每个 quad 中线一条线） | 无 AP 时只覆盖文字（24 px），空页 0 |

**`/NM` 标记**：写回时设 `/NM = "pd-fr:<id>"`；`id` 来自 job 的 `"id"` 字段（overlay 的标注 id）。
没有 `id` 的标注不打标记（每次都会新增，仅用于兼容旧 job）。

**`SetAP` 的实际行为**（核对过保存后的 PDF）：PDFium 会生成
`/AP << /N <Form XObject> >>`，该 Form 的 `/BBox` = 注释的 `/Rect`，**没有 `/Matrix`、没有 `/Resources`**，
流内容就是我们传入的字符串。因此：

* 外观内容用**页面坐标**直接写，不需要任何变换；
* **不能用 ExtGState**（没有 Resources）→ 无法表达透明度；
* **墨迹的外观由 PDFium 原生生成**：`FPDFPageObj_CreateNewPath` + `FPDFPath_LineTo` 构造一条
  路径（一条笔画 = 一个子路径），设描边颜色/线宽后 `FPDFAnnot_AppendObject`。生成的 Form XObject
  带 `/BBox`（=注释 Rect）、`/Matrix`、`/Resources` —— 别的编辑器（Okular）保存时不会因为缺键报错（D-132）。
  线宽同时写进 `/BS`（`/Border`）。
* **下划线/删除线**仍用 `SetAP` 写一段页面坐标的内容流（`AppendObject` 只支持 ink/stamp）：
  `q <r> <g> <b> RG <w> w <x y> m <x y> l … S Q`；RGB 用 0..1（2 位小数），
  数字固定 2 位小数，非有限值一律写 `0.00`。

文本类的导出线宽固定 `TEXT_MARK_LINE_WIDTH = 2.0`（页面 pt）。界面里的下划线/删除线画的是
2 **屏幕**像素（与缩放无关），所以非 100% 缩放下导出线的视觉粗细会与界面不同 —— 已知差异。

## 10. 保存策略

* **差分同步**（D-144）：读回源 PDF 的标记注释，只写有变化的部分；内容修改复用对象号。
* **注释必须是独立对象**（D-142）：PDFium 新建的注释会写成 `/Annots [ <<…>> ]` 内联字典；
  Okular 能读能编辑，但保存时因为没有对象号会把修改丢掉。保存后由 `pdf_repair` 追加一段
  增量更新，把内联字典外置成 `N 0 R`（页面对象重写、经典 xref、`/Prev` 指回原 xref）。
  解析失败就保留原字节（宁可少修，不弄坏文件）。
* 保存模式（D-141/D-143）：**导出副本与原地写入都用 `FPDF_INCREMENTAL`**（`flags=1`）。
  增量保存保留原文件的对象流与外部注释结构；用户实测全量重写 Adobe 的 PDF 后，Okular 另存会报
  “文件有问题”。`write()` 的全量模式保留给测试/工具，产品路径不再使用。
  代价：每次写入追加一段（文件变大），不自动压缩。
* 先保存到**内存缓冲**，再 `std::fs::write(spec.output)`；`spec.output` 由调用方给临时路径，
  rename 与冲突处理由调用方做。
* **拒绝 `source == output`**：写回永远不直接覆盖输入（原子替换是调用方的责任）。

## 10.0 原地写入（S5c）

入口：工具栏「写入 PDF」→ 确认框「备份并写入」。

```text
保存脏 sidecar（重开会从磁盘重新加载 overlay/正文）
  → 子进程写同目录临时文件（与导出相同的 job，但源 = 原文件；内部按 /NM 同步）
  → 冒烟校验（重开 + 页数 + 渲染一页）
  → 关闭会话（释放 PDFium 文件句柄；Windows 上不放开就没法替换）
  → 备份：<文件名>.bak-<纳秒>
  → fs::rename(临时文件 → 原文件)
  → 重新打开原文件并恢复 页 / 缩放 / 滚动
```

**为什么不会把注释叠两遍**：每条写回的注释都带 `/NM = "pd-fr:<id>"`。写回时逐页：

| 情况 | 处理 |
| --- | --- |
| 标记注释的**正文**（`/Contents`）变了，几何/颜色/透明度没变 | **原地更新同一个对象**（对象号不变，不重建页面） |
| 标记注释完全没变 | **不写** |
| 几何/颜色/透明度变了 | 删掉旧引用、新建对象（PDFium 的 `SetColor` 对已存在墨迹失败，没有可靠的“改几何”API） |
| 标记注释的 id 已不在 overlay | 删除引用 |
| overlay 新增 id | 新建对象并追加引用 |
| 没有 `/NM`（链接、其它程序加的注释） | **一律不碰** |

因此"写入 → 再写入"是幂等的；只改正文时同步逻辑不会重建页面对象。
**但注意**：PDFium 的 `FPDF_INCREMENTAL` 实测仍会把整份文档对象追加一版
（无任何变化时 2979 → 4351 字节）。也就是说，**“只写被修改的注释”目前停在同步逻辑层**；
要做到 Okular 那样只追加一个对象，需要绕过 PDFium 的保存器、自己写增量更新
（`pdf_repair` 已有对象/xref 解析器，列为后续优化）。`WritebackReport.annotations` 是
"这份 PDF 里我们的注释总数"（新增 + 更新 + 未变）。
导出副本也带同样的标记，后续原地写入能识别。

**已知边界**：

* 本程序写回的注释是 **overlay 的投影**：差分同步，正文/颜色/几何都以 overlay 为准；
  别的程序对这些注释的修改会被覆盖（别的程序**自己新增**的注释不受影响）；
* 写完后界面里仍显示 overlay（这是有意的：读回 PDF 注释（S6）之前，删 overlay 等于让标注在本程序里消失）；
* 写完后 undo/redo 只改 overlay，PDF 要等下一次写入才同步；
* 每次写入都会留一份 `.bak-<纳秒>`，不会自动清理。

## 10.1 主进程编排（S5b-2）

入口：工具栏第 0 行的“导出标注 PDF”（需要已打开 PDF 且 `annotations` 非空，否则禁用并说明原因）。

| 步骤 | 实现 | 失败时 |
| --- | --- | --- |
| 预检 | 父进程看 `DocumentInfo.encrypted`；标注为空直接返回 | 状态栏给出原因，不起子进程 |
| 目标命名 | `book.pdf` → **同目录 `book.annotated.pdf`**（`export::default_output_path`） | — |
| 不覆盖 | `ensure_destination_free`：目标已存在直接拒绝 | “已存在同名文件，不覆盖：…” |
| 组 job | `ExportItem`（几何/颜色/不透明/正文）+ `updates`/`deletes`（对象号）→ `job_json`（按页分组、绝对路径） | 写 job 失败即中止 |
| 起子进程 | `<current_exe> --writeback <job>`，捕获 stdout/stderr；子进程把标注写进**目标同目录的临时文件** | 把子进程 stderr 原样放进消息；删除临时文件 |
| 冒烟校验 | `smoke_check`：后端重开临时文件、页数一致、渲染一页（`Scale(0.5)`） | 删除临时文件，原文件不受影响 |
| 落盘 | `fs::rename(temp → 目标)`（同目录，原子） | 删除临时文件并报错 |

**注意**：

* 导出是**同步**调用：等待子进程与冒烟渲染期间 UI 会短暂停顿（通常几十毫秒；超大文件另说）；
* 父进程**不比较像素**：冒烟校验只保证"文件能打开、页数对、渲染不报错"；
  旋转页的像素位置由 `backend-pdfium` 的回归测试守（INV-12 / L3 已修正），真实阅读器仍需人工抽查；
* P5 起保存路径统一：**只有新注释**（`pdf_ref=None`）进 `pages`；所有已有注释
  （自有/外部）正文变化走 `updates`，删除走 `deletes`；marker 不再用来隐式删除。
  正文直接从 `Annotation.body` 取；空正文不写 `/Contents`；
* 正文更新/删除按 **xref 对象号** 写：`updates` 用 `pdf_patch` 追加同 `(num,gen)` 版本；
  `deletes` 用页对象 patch 从 `/Annots` 去掉引用（含 `/Popup`）；内联字典无对象号，不支持；
  PDFium 不再负责删除或正文原地更新；
* 目标文件名**不含** `.pdf` 之外的后缀拼接坑：`a.b.pdf` → `a.b.annotated.pdf`（只替换最后一个扩展名）。

## 10.5 外部注释的按对象号更新/删除（P2）

job 新增两个可选数组：

```json
{
  "updates": [ { "num": 16, "gen": 0, "body": "新正文" } ],
  "deletes": [ { "num": 18, "gen": 1 } ]
}
```

* 子进程用 `pdf_index::page_annotations()` 重新解析**当前源文件**，把对象号映射到页/下标；
  找不到就报错，不做猜测；
* `updates` 只改 `/Contents`（对象号不变）；`deletes` 删除父注释，并通过 `pdf_index`
  的 `/Popup` 引用一起删子对象；
* 与 `/NM` 差分同步并存：自有注释仍走 marker 同步，外部注释走对象号；
* 删除后再写入不会重复删除（对象已不在当前文件时，重新打开后不再进入待删集合）。
* **增量保存的兜底（D-153）**：如果目标对象的 generation > 0，`write_incremental()`
  自动改用全量保存。原因是 PDFium 增量写出 gen0 新对象后，重新打开仍可能解析旧版
  （Okular 样本 `11 1 R`）；全量保存会把对象统一重写，读回一致。
  其它情况（包括 Adobe ObjStm、evince/firefox 的多版历史）仍走增量，避免 Okular
  另存时报警，也保留原对象流。

## 11. 自检（`write()` 内置）

保存后重新打开 `spec.output`，逐页比对：

1. 页数不变；
2. 每个目标页的注释数 == **写前注释数 + 新增数**（写前数量在创建前就记下）。
   这同时证明"原有链接注释没有被破坏"。

不一致返回 `Pdfium { stage: "verify", .. }`，输出文件保留供排查（调用方应删除临时文件）。

## 12. 闸门（用户确认的策略）

| 情况 | 处理 | 依据 |
| --- | --- | --- |
| 加密文档（`FPDF_GetSecurityHandlerRevision >= 0`） | 拒绝 | 保存需要 owner 密码；权限位可能禁止批注 |
| 有数字签名（`FPDF_GetSignatureCount > 0`） | 拒绝 | 写回会让签名失效 |
| `annotate == false` | 由"加密"闸门覆盖 | PDF 权限位只存在于加密文档 |
| 源文件不存在 / 页号越界 / 几何退化 | 拒绝（`Invalid`） | 不产生半成品 |

> 加密与签名两条闸门**没有自动化夹具**（当前没有现成的加密/签名 PDF，也没有生成器）。
> 加夹具之前，它们只有代码审查级的保障。

## 13. 线程安全

* **产品代码只允许在 `--writeback` 子进程里调用**；
* 集成测试在同一进程里调用，但**同一测试文件内的测试用 `Mutex` 串行**，
  不同测试二进制由 cargo 顺序执行；
* 不要在阅读器主进程里调用（会与渲染线程竞争 → 随机崩溃，且崩溃点不可复现）。

## 14. 已知限制

| # | 限制 | 说明 |
| --- | --- | --- |
| L1 | **透明度丢失** | AP 没有 `/Resources`，无法用 ExtGState；`/CA` 没有公开 setter。高亮靠阅读器合成，下划线/删除线本来就实色 |
| L2 | 文本类线宽固定 2pt | 界面是 2 屏幕像素，非 100% 缩放时观感不同 |
| L3 | ~~旋转页的渲染验证不可信~~ **已修正（D-126）** | 早先以为 PDFium 对旋转页注释有 bug；实际是**适配层把"最终旋转"又传了一遍**（PDFium 自己会应用 intrinsic `/Rotate`），导致 180° + 压扁。修正后旋转页的位置可用像素断言：`writeback` 的 `a_highlight_on_a_rotated_page_lands_where_the_rotation_says` 与 `intrinsic_rotation` 测试都过了。人工验收仍建议做（Chrome/Acrobat 的注释绘制路径与我们的渲染器不同） |
| L4 | 加密 / 签名文档不支持 | 见 §12；无夹具 |
| L5 | 全量重写的内存峰值 | 文件大小量级；大文件（>100MB）需要实测 |
| L6 | PDFium 版本未固定 | `scripts/fetch-pdfium.sh` 取 latest；`SetAP` 的 Form 形状依赖它的实现，升级后需重跑 spike/测试 |
| L8 | 导出同步阻塞 | 子进程 + 冒烟渲染在 UI 线程上等待；大文件可能造成可感知停顿（暂无进度条/取消） |
| L7 | 只支持 little-endian | `FPDF_WIDESTRING` 直接传 `u16` 序列；当前只发布 x86_64/arm64 的 LE 平台 |

## 15. 排查手册

| 症状 | 先查什么 | 怎么做 |
| --- | --- | --- |
| `Library("找不到 PDFium")` | 动态库路径 | `scripts/fetch-pdfium.sh`；或设 `PDFIUM_PATH`；`pdfium_library_path()` 只找 `vendor/` 候选 |
| `Library("缺符号 FPDFAnnot_…")` | PDFium 版本太旧 | `nm -D <lib> | grep FPDFAnnot` 对照 §6 表；升级 PDFium 并重跑 `cargo run -p backend-pdfium --example s5_spike`（历史探针，见 §16） |
| `FPDF_LoadDocument` 返回 null | 加密 / 损坏 / 路径 | 先用阅读器打开；加密文档按策略拒绝 |
| `FPDFPage_CreateAnnot` 返回 null | 子类型常量写错 / 页无效 | 核对 §6 的 9/10/12/15 |
| `AddInkStroke` "失败" | **成功返回 0 是索引** | 只有 **-1** 才是失败；这是本项目踩过的坑 |
| 保存后注释不可见 | 是否漏了 AP | 墨迹/下划线/删除线必须有 AP；高亮故意没有 |
| 注释数量不符 | 是否把已有的链接注释算漏 | 用 `count_annotations` 看真实数量；`verify` 的期望值是"原有 + 新增" |
| 中文乱码 | `/Contents` 编码 | 必须 UTF-16LE（`wide_string`），不要传 UTF-8 |
| 位置偏移 | 是否预旋转了几何 | 几何必须保持**未旋转页面空间**；旋转页见 L3 |
| 导出的墨迹拐角有缝 | 墨迹 AP 是否由 `AppendObject` 生成（一条子路径、miter 连接） | 见 §9；`a_thick_right_angle_has_no_gap_at_the_joint` 守这条 |
| Okular 改注释后保存报“文件有问题” | 我们的 AP Form 是否缺 `/Resources`/`/Matrix` | 墨迹已改用 `AppendObject`（D-132）；下划线/删除线仍是 `SetAP`，若也报错再评估 |
| 原地写入后注释叠了两遍 | job 里有没有 `"id"`、写回是否走了同步段 | 见 §10.0；`writing_twice_is_idempotent_*` 守这条 |
| 写入后本程序里还能看到/编辑标注 | **预期** | overlay 保留（S6 之前删了就等于丢编辑能力） |
| 删掉/改了正文后再写回，其它软件仍显示旧文本 | 已修（D-131） | 写回时会更新已有标记注释的 `/Contents`；`rewriting_updates_contents_of_existing_marked_annotations` 守这条 |
| 原地写入提示“有改动未能保存” | sidecar 被更新版本占用或只读目录 | 先解决 sidecar 保存问题（`Ctrl+S` 看错误），再写入 |
| 按钮是灰的 | 没打开 PDF / 没有标注 | 打开一个带标注的 PDF；`annotations` 为空时故意禁用 |
| “已存在同名文件” | 上次导出留下的 `<stem>.annotated.pdf` | 改名或删除旧文件（**不会**自动覆盖） |
| “写回子进程失败” | 子进程 stderr | 消息里已带原因；手动跑 `cargo run -p app -- --writeback <job>` 复现 |
| “源文件不存在：/tmp/scripts/…” | job 里写的是相对路径 | 父进程必须绝对化（D-127）；job 里 `pdf` 应为 `/…/scripts/replay/x.pdf` |
| “导出的文件打不开 / 页数变了” | 冒烟校验失败 | 临时文件已删；检查磁盘/权限，重试 |

## 16. 复现与调试

* **S5a 探针**（一次性、保留在仓库）：`cargo run -p backend-pdfium --example s5_spike`
  会打印墨迹/高亮/下划线/删除线/SetAP/保存 flags/真实样本的实测数字，
  是判断"PDFium 行为变了没有"最快的办法。
* **从界面原地写入**：打开带标注的 PDF → 工具栏「写入 PDF」→ 确认「备份并写入」→
  同目录出现 `<文件名>.bak-<时间>`，原文件被原子替换；状态栏显示备份路径。
  再点一次应当打印“已原地写入 N 条标注”（PDF 里数量不变，不会叠加）。
* **从界面导出**：打开带标注的 PDF → 工具栏「导出标注 PDF」→ 同目录出现 `<stem>.annotated.pdf`；
  状态栏显示“已导出 N 条标注：…（字节）”。再点一次会因“不覆盖”被拒绝（这是预期）。
* **手工跑一次写回**：

  ```bash
  cargo run -p app -- --writeback /tmp/job.json
  ```

  job 见 §5；`pdf`/`output` 相对 job 所在目录。
* **看保存后的字典**：`strings out.pdf`（PDFium 把注释字典写成明文；AP 流是 Flate 压缩）。

## 17. 升级/改动检查清单

改这个模块前确认：

1. 新增/修改 FFI 函数 → 同步 §6/§7，并跑 `writeback` 集成测试 + `s5_spike`；
2. 改 AP 生成规则 → 跑 `writeback::tests`（内容流字符串有单测）+ 像素级断言（未旋转页）；
3. 改闸门/校验 → 同步 §12/§11，并确认失败时不留下输出文件；
4. 任何"在主进程里调用"的想法 → 先读 §13 与 D-124。

## 18. 相关文件

| 文件 | 内容 |
| --- | --- |
| [`crates/backend-pdfium/src/writeback.rs`](../../crates/backend-pdfium/src/writeback.rs) | FFI、语义字段、AP 生成、保存与自检（含单测） |
| [`crates/backend-pdfium/tests/writeback.rs`](../../crates/backend-pdfium/tests/writeback.rs) | 端到端：四种标注、链接保留、旋转页几何、拒绝路径 |
| [`crates/app/src/writeback.rs`](../../crates/app/src/writeback.rs) | job.json 解析（子进程入口） |
| [`crates/app/tests/writeback_cli.rs`](../../crates/app/tests/writeback_cli.rs) | job → 文件 → 计数，以及失败路径 |
| [评估](../decisions/evaluations/s5-writeback.md) | 为什么是这套方案（含实测数字） |
| D-123 / D-124 | 用户确认的形态与实现决策 |
