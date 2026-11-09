# PDF Reader

一个本地优先的 PDF 阅读器，用 Rust 编写，核心亮点是：

- **注释写回 PDF**：高亮、下划线、删除线、墨迹、便签直接写入 PDF 注释对象；
- **自定义 preview**：文字/图片 preview 和引用点（dest）单独保存在 `<pdf>.json`；
- **Markdown / LaTeX 文字浮窗**：注释正文和文字 preview 可切换 Markdown 渲染，支持公式；
- **纯逻辑与 UI 分层**：渲染核心不依赖 egui，PDFium 只出现在薄适配层。

这是一个面向个人的阅读器，不是完整替代 Adobe/Okular 的通用 PDF 套件；跨阅读器兼容性边界见[阅读](#reading)和[注释](#annotations)。

<a id="architecture"></a>
## 架构

```
crates/pdfcore          trait / 几何 / 会话 / 布局 / 渲染调度，零 UI 依赖
crates/backend-pdfium   PDFium 适配、注释读回、写回（子进程 FFI）、整理副本
crates/app              egui 界面 + 回放器 + 设置/存档/摘要
```

`pdfcore` 不依赖窗口；坐标、画法、快照、浮窗几何等纯逻辑在 `view` 中，能用 `cargo test` 验证。更细的分层、线程、坐标系和不变量见 [docs/architecture.md](docs/architecture.md)。

<a id="features"></a>
## 功能概览

| 功能 | 简介 |
| --- | --- |
| [阅读与浏览](#reading) | 连续/横向滚动、缩放、旋转、搜索、大纲、页内链接、引用点 |
| [注释](#annotations) | 五类注释、正文/作者/时间、侧栏浏览、读回与写回；**兼容性有限，务必读限制** |
| [编辑与撤销](#undo) | 注释、正文、preview、dest 的撤销/重做；与保存点联动 |
| [自定义数据](#custom-data) | 文字/图片 preview、引用点尺寸与图片；`<pdf>.json`，不改 PDF |
| [保存与写回](#save) | 保存/另存为/冲突提示、PDF 注释增量写回、可选 qpdf 检查 |
| [整理副本](#compact) | 去掉历史修订、生成更小的单段 PDF |
| [浮窗](#floats) | 图片类/文字类浮窗；Markdown、LaTeX、图片与文本引用 |
| [设置](#settings) | 窗口/侧栏、注释默认样式、每文件阅读状态、顶栏开关 |
| [消息记录](#messages) | 最近 100 条状态历史，可滚动回看；qpdf 检查结果也在这里 |

<a id="install"></a>
## 安装与运行

### 依赖

- Rust：见 `Cargo.toml` / `rust-toolchain`；LaTeX 数学的 `latex-rust` 后端要求 Rust ≥ 1.99；
- PDFium 动态库：仓库不提交二进制，用脚本下载到 `vendor/pdfium-<平台>/lib/`；
- 可选：`qpdf`，用于 `CHECK_PDF_FILE` 保存检查；
- 可选：LaTeX/`ratex` 相关后端按构建 feature 决定，见[数学后端](#math-backends)。

### 快速开始

Linux：

```bash
./scripts/fetch-pdfium.sh          # → vendor/pdfium-<平台>/lib/
cargo run -p app --                 # 空状态启动；拖入 PDF 打开
cargo run -p app -- some.pdf       # 直接打开
```

Windows（PowerShell）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-pdfium.ps1
cargo run -p app -- some.pdf
```

找不到 PDFium 时，错误会列出尝试过的路径；`cargo run -p app -- --selftest` 可单独验证动态库。

### 构建与打包

```bash
cargo build --release -p app           # target/release/app；不带 lib/pdfium
./scripts/package.sh                   # → dist/pdf_reader_<平台>/{app, lib/pdfium, BUILD-INFO.txt}
./scripts/package.sh --backend ratex   # → dist/pdf_reader_<平台>_ratex/...
```

`cargo build` / `scripts/build.sh` 只产出可执行文件；**`scripts/package.sh` / `package.ps1`
才负责复制 `libpdfium` 到 `lib/` 并做自检**。打包产物自带 PDFium，不需要
`LD_LIBRARY_PATH`。只能在目标平台打包（Windows 用 `scripts\package.ps1 -Backend ratex`）。

<a id="reading"></a>
## 阅读与浏览

支持：

- 连续纵向滚动、横向阅读流；横向阅读时可单独启用横向滚动；
- 缩放（步进/输入百分比/适应高度）、旋转、翻页、跳页；
- 选中文字复制、页内搜索、大纲、页内链接、引用点跳转；
- 最近文件与每文件阅读位置（页 + 锚点）恢复；
- 全屏；浅色/深色主题。

### 阅读限制

- **PDF 版本**：正常注释面向 1.5–1.7；低于 1.5 仍可打开阅读，但注释编辑被拒绝；
  PDF 2.0 只做了有限验证。详见[注释版本矩阵](docs/annotation-versions.md)。
- **加密 PDF**：包括“用户密码为空”的权限加密 PDF，可以打开阅读，但本程序统一拒绝写回。
- **不支持的过滤器**：例如 PDF 2.0 Brotli 原型文件，当前 PDFium 可能返回 0 页；应用会提示
  “页数为 0/可能 Brotli 未启用”，不会访问第 1 页。
- **对象流/坏结构**：部分 1.5+ 文件某些页的注释索引不完整；这些页只读，其他页仍可用。
- **显示差异**：注释、链接框、引用点等是 UI 叠加在页面位图上的，不是 PDF 页面内容；
  因而和 Adobe/Okular/浏览器内置阅读器的外观可能有差异。
- **Linux 文件拖放**：Wayland 下 winit 0.30 不投递 DnD；可用 `env -u WAYLAND_DISPLAY`
  走 X11/XWayland。XWayland 下窗口移动可能有黑帧/迟滞，原生 Wayland 和 Windows 无此现象。
- 截图框选没有普通用户入口；仅 `PDF_READER_DEV_ENTRY=1` 的验收辅助入口。

<a id="annotations"></a>
## 注释

支持五类：

| 类型 | 说明 |
| --- | --- |
| Text | 便签/图标 + 正文 |
| Highlight | 文本高亮，`/QuadPoints` 多行 |
| Underline | 下划线，带 AP 外观流 |
| StrikeOut | 删除线，带 AP 外观流 |
| Ink | 墨迹，`/InkList` + AP |

基本流程：

1. 顶栏「注释工具」选择高亮/下划线/删除线/墨迹/便签；
2. 在页面上拖动（文本标记需先选中文字）；
3. 点击注释打开正文浮窗，`⋯`/右键打开菜单；
4. 侧栏按页/最近修改浏览、搜索、筛选；可删除、改正文、改样式。

### 内部注释与外部注释

- **内部注释**：由本程序创建，带 `/NM (pd-fr:<id>)`；id 不复用，正文/样式/几何可编辑并写回。
- **外部注释**：其它阅读器创建。当前只识别五种支持类型；几何/颜色/样式只读，正文和删除可
  写回（外部便签/标记的正文更新走对象 patch）。
- 同一 id 出现多份时会保留第一条、后续重编号并在保存时重建对象；不能静默覆盖。
- 页级索引不完整时，该页旧注释只读并禁止新建；完整页不受影响。

### 兼容性说明

**注释兼容性尚未经过全面实机验证。** 已知边界：

- 其它阅读器更丰富的注释类型（Caret、Redaction、RichMedia 等）当前不显示；
- Adobe/Okular/Evince/Chromium 的显示和可编辑性不完全一致；
- 跨阅读器写回可能受原 PDF 的 xref/object stream、权限、注释外观影响；
- 可用 `CHECK_PDF_FILE=1` 在保存时调用 qpdf 做结构检查；qpdf 检查的是 PDF 结构，不是
  Adobe 视觉兼容性。

导出注释摘要：`文件 → 导出注释摘要…`，生成同目录 `<stem>.annotations.md`（不覆盖；递增）。
如果注释摘要的正文中引用了内部图片，图片会转成 `data:image/png;base64,...`。

<a id="undo"></a>
## 编辑与撤销

撤销/重做覆盖：

- 注释新增/删除/改正文/改样式/移动墨迹；
- 自定义 preview 的标题/正文/尺寸/位置/图片替换；
- dest 矩形/尺寸。

不覆盖页面导航、缩放、滚动、侧栏筛选等视图操作。历史保存点与 PDF/自定义数据的脏位联动：
撤销回到保存点会重新变干净；保存后历史点更新。

<a id="custom-data"></a>
## 自定义数据

本程序会在 PDF 同目录维护 `<pdf>.json`（不是替换扩展名）：

- **文字 preview**：标题 + 正文，可 Markdown/LaTeX；
- **图片 preview**：标题 + 单页局部截图；
- **引用点（dest）**：页面矩形、截图、显示尺寸、引用图片记录；
- 链接、稳定 id、修订号等。

内存中还有临时状态；只有保存时才写入 sidecar。特点：

- 自定义数据**不会修改 PDF**，即使写坏也只影响 `<pdf>.json`；
- 但它也不是 PDF 注释，换阅读器看不到；
- 另存为对话框可分别决定是否写原 PDF 数据文件、是否写新 PDF 数据文件；
- 若目标 PDF 所在目录没有 sidecar，链接会从 PDF 重新解析；
- 整理副本只处理 PDF，不处理 sidecar。

<a id="save"></a>
## 保存与写回

### 脏位

顶部「文件」菜单的 `*` 表示有未保存改动。脏位来源：

- **PDF 脏位**：注释新增/删除/样式/几何/正文、外部注释正文更新；
- **数据脏位**：preview/dest/sidecar 迁移等；
- **修复标记**：打开时发现重复 `/NM` 等需要保存时重建对象的异常；不是普通用户开关，
  保存成功后自动清除。

保存按钮/关闭提示会区分“只保存数据”和“同时写 PDF”。未保存时关闭/打开另一个文件会提示。

### 保存 / 另存为

- **保存**：原地增量写回 PDF，并写 sidecar；
- **另存为**：把 PDF+注释写到新文件；可选择写原 PDF 的 sidecar、新 PDF 的 sidecar，或都不写；
- 外部修改冲突：只能覆盖保存或另存为；
- 原地保存前备份由设置控制；备份名形如 `<pdf>.bak-<时间>`；
- 加密 PDF（包括空用户密码）统一拒绝写回。

### PDF 写回机制

- 增量写回：追加新注释对象、页对象新版本、xref+trailer；不重写整个文件；
- `/NM = pd-fr:<id>` 作为身份锚点；
- 高亮不写 AP，依赖 `/QuadPoints`；下划线/删除线/墨迹写 AP；
- 增量 trailer 的 `/Size` 覆盖最高现存对象，避免 Adobe 在多次保存后拒绝页面/注释；
- 加密/签名文件不尝试写回；整理副本拒绝签名/加密文件。

### 可选 qpdf 检查

```bash
CHECK_PDF_FILE=1 cargo run -p app -- some.pdf
```

环境变量存在且 PATH 中有 qpdf 时，每次保存会在替换目标前检查**系统临时目录中的副本**：

- qpdf exit 0/3（警告）放行；
- 真实结构错误拒绝替换，保留原文件；
- qpdf 缺失或副本不可读只记录“跳过”，不阻断保存；
- 检查结果写入[消息记录](#messages)。

实现/限制见 [docs/diagnostics.md](docs/diagnostics.md)。

<a id="compact"></a>
## 整理副本

`文件 → 整理副本…` 或：

```bash
cargo run -p app -- --compact in.pdf out.pdf
```

作用：去掉历史修订、生成更小的单段 PDF。特点：

- 不覆盖源文件；
- 拒绝签名/加密文件；
- 不处理 `<pdf>.json`；
- 不改变注释语义；注释仍按 PDF 对象保留。

<a id="floats"></a>
## 浮窗

浮窗是主视图内可移动、可调整大小、可置顶的内容窗口，分两类：

### 图片类

- dest 引用点截图；
- 自定义图片 preview；
- 可复用为 Markdown 图片引用（内部 `pd-img:` URI），也可复制为浮窗链接。

### 文字类

- 自定义文字 preview；
- 注释正文；
- 支持普通文本和 **Markdown 模式**（顶栏 `Markdown` 开关）：
  - 单换行按笔记语义保留为硬换行；
  - 代码块/表格/长 URL 会按内容宽度横向滚动；
  - `$...$` / `$$...$$` 数学按 PNG 纹理渲染；
  - 图片引用可插入内部图片；
  - 外部链接只做提示，不打开浏览器；
  - 支持 Markdown 内部标题链接跳转。
- 链接跳转范围：
  - `pd-float://` 可打开/置顶内部注释、文字 preview、图片 preview；
  - 外部注释默认不提供复制入口，开启“允许跨会话外部注释链接”后才可用；
  - 引用点/截图/测试浮窗没有可复制链接；引用点只支持 `→` 跳到来源。
  链接语法、断链和外部注释 token 规则见 [docs/float-links.md](docs/float-links.md)。

### 编辑与按钮

标题栏提供关闭、编辑正文、图片引用、复制浮窗链接、跳到来源等按钮；侧栏的 `⋯`/右键
提供更多操作。正文编辑使用可调整大小的正式窗口，内部可滚动。浮窗的交互限制（滚轮归属、
临时预览、点击固定等）见 [docs/app-ui.md](docs/app-ui.md)。

数学后端与字体限制见[数学后端](#math-backends)。

<a id="settings"></a>
## 设置

设置保存在平台配置目录的 `settings.json`（Linux 通常是 `~/.config/pdf-reader/`）：

- 窗口大小/侧栏宽度/主题（浅色/深色）；
- 注释默认颜色/透明度/线宽；
- 每文件阅读状态：页码、缩放、锚点、横向排版；
- 最近文件；
- 顶栏开关：链接框、引用点、标注、Markdown、hover MD、Pin 悬浮响应、横向滚动；
- 原地保存前备份。

逐项字段见 [docs/persistence.md](docs/persistence.md)。

<a id="messages"></a>
## 消息记录

顶栏「消息」打开右侧历史侧栏：

- 最多最近 100 条状态消息；
- 带本地时间 `HH:MM:SS`；
- 可滚动回看、清空、关闭；
- 连续相同消息去重；
- `CHECK_PDF_FILE` 的 qpdf 结果也记录在这里。

<a id="math-backends"></a>
## 数学后端

默认构建使用 `latex-rust` SVG。可选构建：

```bash
./scripts/build.sh --backend latex-svg
./scripts/build.sh --backend latex-png
./scripts/build.sh --backend ratex
./scripts/package.sh --backend ratex
```

运行时可用 `PDF_READER_MATH_BACKEND=svg|png|ratex` 覆盖（`ratex` 需要 `math-ratex` feature）。
RaTeX 的 CJK `\text{...}` 需要系统 Unicode 字体，或设置 `RATEX_UNICODE_FONT=/path/to/CJK.otf`；
字体不覆盖汉字时回退原文，不显示被拉宽的残缺排版。

数学公式是临时纹理缓存，不写入 PDF；详情见 [docs/app-ui.md](docs/app-ui.md) 的 Markdown/数学小节。

<a id="quality"></a>
## 开发与质量门禁

```bash
./scripts/check.sh
```

依次执行 `fmt --check` → `clippy -D warnings` → `cargo test --workspace` → 回放结构基线比较
→ 文档链接检查。当前 **536 个测试**（以实际 `cargo test` 为准）。

PDFium 缺失时相关测试会跳过并打印指引；设置 `PDF_READER_REQUIRE_PDFIUM=1` 可让缺库变成失败。

### 回放

```bash
cargo run -p app -- --replay scripts/replay/tiling_800.json --out /tmp/run.jsonl
cargo run -p app -- --replay scripts/replay/tiling_800.json --timing
```

回放不是普通阅读功能，用于结构基线、性能诊断和无人窗口回归；只覆盖主视图渲染主线，
浮窗/侧栏/保存提示等仍需人工验收。

<a id="env"></a>
## 环境变量

| 变量 | 作用 |
| --- | --- |
| `PDFIUM_PATH` | 指定 PDFium 库文件或所在目录 |
| `PDF_READER_DIAGNOSE=1` | 运行期诊断（贴图/画法/波段/纹理与位图占用） |
| `PDF_READER_TILE_BORDERS=1` | 给瓦片画红边（默认关；调试分块错位） |
| `PDF_READER_TEXTURE_BUDGET_MB` | GPU 纹理预算（默认 256 MiB，软上限） |
| `PDF_READER_BITMAP_BUDGET_MB` | CPU 位图缓存预算（默认 512 MiB，0 = 不限制） |
| `PDF_READER_MATH_BACKEND` | 运行时数学后端 `svg` / `png` / `ratex` |
| `RATEX_UNICODE_FONT` | RaTeX CJK 字体路径 |
| `CHECK_PDF_FILE=1` | 保存时用 qpdf 检查暂存 PDF 副本；失败时不替换目标 |
| `PDF_READER_DEV_ENTRY=1` | 显示开发/验收辅助入口（截图、测试浮窗） |
| `PDF_READER_WRITEBACK_EXE` | 测试用：指定写回子进程可执行文件 |

<a id="docs"></a>
## 文档

- [docs/README.md](docs/README.md)：文档地图
- [docs/status.md](docs/status.md)：当前能力、门禁状态、已知限制
- [docs/architecture.md](docs/architecture.md)：分层、线程、坐标系、不变量
- [docs/app-ui.md](docs/app-ui.md)：UI/交互状态机
- [docs/rendering.md](docs/rendering.md)：渲染/分块/预览
- [docs/persistence.md](docs/persistence.md)：设置、sidecar、数据格式
- [docs/readback.md](docs/readback.md) / [docs/writeback.md](docs/writeback.md)：注释读写
- [docs/annotation-versions.md](docs/annotation-versions.md)：PDF 1.4–2.0 注释字段/版本策略
- [docs/diagnostics.md](docs/diagnostics.md)：诊断与 qpdf 检查
- [docs/testing.md](docs/testing.md)：门禁、回放、人工验收
- [docs/roadmap.md](docs/roadmap.md)：未完成/暂缓/明确不做
- [docs/pdfjs-corpus.md](docs/pdfjs-corpus.md) / [docs/pdfjs-bug-list.md](docs/pdfjs-bug-list.md)：本地 pdf.js 测试集扫描说明与问题文件（测试集不随仓库分发）
- [docs/view-split-evaluation.md](docs/view-split-evaluation.md) / [docs/state-grouping-evaluation.md](docs/state-grouping-evaluation.md)：拆分评估

<a id="license"></a>
## 许可

MIT OR Apache-2.0

LaTeX 数学使用 [latex-rust](https://crates.io/crates/latex-rust)（MIT OR Apache-2.0），
其内嵌的 STIX Two Math 字体为 SIL OFL 1.1；发布包需一并保留其许可声明。
