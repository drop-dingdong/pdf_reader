# 写回 PDF（S5b）：FFI 说明与排查手册

> 现状文档：描述**现在是什么样**。改 `crates/backend-pdfium/src/writeback.rs` 或
> `crates/app/src/writeback.rs` 时**必须同步本文**。
> 背景与取舍见 [评估](../decisions/evaluations/s5-writeback.md) 与 D-123 / D-124。

## 1. 这是什么，不是什么

* **是**：把本程序内存里的标注（墨迹 / 高亮 / 下划线 / 删除线）写成**真正的 PDF 注释**，
  保存到一个**新文件**（`<原名>.annotated.pdf`）。
* **不是**：不是 UI（S5b-2 才接按钮）、不是原地覆盖（S5c）、不是"读回 PDF 注释"（S6）。
* **不在主进程里跑**：写回直接在 PDFium 里创建注释，`pdfium-render 0.9.4` 没有这层 API，
  所以我们用 `libloading` 自己绑定了 18 个 C 函数；这些调用**绕过 pdfium-render 的全局锁**，
  与后台渲染线程并发会让 PDFium 崩溃。因此产品路径是**隐藏子命令 + 子进程**。

## 2. 调用链与进程模型

```text
主进程（阅读器）
  └─ 生成 job.json（临时文件）
       └─ std::process::Command：app --writeback <job.json>     ← 子进程（S5b-2 接）
            └─ app::writeback::run_job（解析 JSON）
                 └─ backend_pdfium::writeback::write（FFI）
                      ├─ FPDF_LoadDocument（只读源文件）
                      ├─ FPDFPage_CreateAnnot + 语义字段 + SetAP
                      ├─ FPDF_SaveAsCopy（FPDF_NO_INCREMENTAL）→ 内存缓冲
                      ├─ 写 spec.output
                      └─ 重开 output、逐页核对注释数
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

路径相对 **job 文件所在目录**：

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
| `FPDFPage_GetAnnotCount` | `(page) -> c_int` | 数注释（写前/校验） | 负数 |
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

**`SetAP` 的实际行为**（核对过保存后的 PDF）：PDFium 会生成
`/AP << /N <Form XObject> >>`，该 Form 的 `/BBox` = 注释的 `/Rect`，**没有 `/Matrix`、没有 `/Resources`**，
流内容就是我们传入的字符串。因此：

* 外观内容用**页面坐标**直接写，不需要任何变换；
* **不能用 ExtGState**（没有 Resources）→ 无法表达透明度；
* 内容格式（本模块生成）：`q <r> <g> <b> RG <w> w <x y> m <x y> l … S Q`；
  RGB 用 0..1（2 位小数），数字固定 2 位小数，非有限值一律写 `0.00`。

文本类的导出线宽固定 `TEXT_MARK_LINE_WIDTH = 2.0`（页面 pt）。界面里的下划线/删除线画的是
2 **屏幕**像素（与缩放无关），所以非 100% 缩放下导出线的视觉粗细会与界面不同 —— 已知差异。

## 10. 保存策略

* `FPDF_SaveAsCopy(..., FPDF_NO_INCREMENTAL)`（`flags=2`）：全量重写，避免在可能被其它工具
  改过的文件上叠增量段。实测 `flags=0` 与 `2` 对同一文档产物同样大小、都能重开。
* 先保存到**内存缓冲**，再 `std::fs::write(spec.output)`；`spec.output` 由调用方给临时路径，
  rename 与冲突处理由调用方做。
* **拒绝 `source == output`**：写回永远不直接覆盖输入（原子替换是调用方的责任）。

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
| L3 | **旋转页的渲染验证不可信** | PDFium 的 `FPDF_RenderPageBitmap`（pdfium-render 走它）在页面自带 `/Rotate` 时对注释用了带纵横比缩放的错误矩阵：**原始 PDF** 也能复现（`/Rotate 90` 下，页面空间 (50,100)-(150,120) 的高亮渲染成 (50,100)-(59,299)）。所以自动测试只断言"`/QuadPoints` 保持页面空间 + 注释数正确"，真实位置**必须人工**在 Chrome/Acrobat 里看 |
| L4 | 加密 / 签名文档不支持 | 见 §12；无夹具 |
| L5 | 全量重写的内存峰值 | 文件大小量级；大文件（>100MB）需要实测 |
| L6 | PDFium 版本未固定 | `scripts/fetch-pdfium.sh` 取 latest；`SetAP` 的 Form 形状依赖它的实现，升级后需重跑 spike/测试 |
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

## 16. 复现与调试

* **S5a 探针**（一次性、保留在仓库）：`cargo run -p backend-pdfium --example s5_spike`
  会打印墨迹/高亮/下划线/删除线/SetAP/保存 flags/真实样本的实测数字，
  是判断"PDFium 行为变了没有"最快的办法。
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
