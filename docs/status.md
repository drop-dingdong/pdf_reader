# 项目现状

> 快照：2026-11-07　分支：`refactor/app-split`　门禁：`./scripts/check.sh` 通过（525 个测试）。

## 1. 它是什么

一个本地单文件 PDF 阅读器（Rust + egui/eframe + PDFium），阅读之外支持**把注释写回 PDF**：

- 阅读：连续/横向滚动、缩放、分页、选字复制、搜索、大纲、页内链接、引用点、浮窗截图、
 旋转、全屏、最近文件与阅读位置恢复。
- 注释：高亮 / 下划线 / 删除线 / 墨迹 / 便签；正文、作者、时间；样式（颜色/透明度/线宽）；
 可撤销/重做；写入 PDF 后其它阅读器可见。
- 写回：注释直接写进 PDF 的注释对象（`/NM`、`/Contents`、`/AP` 等），不是旁挂 JSON；
 读回按 `/NM` 恢复 id，并与 PDF 对象号配对。
- 自定义数据：preview（文字/图片）、链接、被引用点尺寸存在同目录 `<pdf>.json`。
- 整理副本：去掉历史修订、生成更小的单段 PDF；不覆盖源文件，拒绝签名/加密文件。
- 用户设置：窗口/侧栏、注释默认样式、每个文件的页码/缩放/锚点/排版，以及顶栏的显示开关。

## 2. 最近一轮完成的东西

- **`app.rs` 拆分（Phase 1）**：12.2k 行的 `app.rs` 拆成 `app/` 子模块
  （`model`/`frame`/`canvas`/`toolbar`/`sidebar`/`floats`/`hover`/`body_editor`/`dialogs`/
  `save`/`document`/`annotate`/`modes`/`capture`/`edit`/`textures`/`paint`）；
  `app.rs` 只剩状态/构造/入口（~0.6k），内联测试移到 `app/tests.rs`。
  **Phase 2**：`draw_canvas` 拆成 `canvas::{paint,interact}`、`draw_floats` 抽出正文绘制、
  `toolbar_rows` 拆成三行子方法。逐模块提交，10 份回放结构基线全程一致。
- **引用点浮窗改大小**：新增 `ResizeDestMark`（历史写“调整引用点浮窗大小”），
  数据指纹补 `DestMark.body_size`，连续 resize 按 1 秒窗口合并成一条；撤销回保存点重新干净。
- **浮窗 z 序与指针归属**：双击编辑器只认最上层浮窗的正文区（标题栏按钮不再被下层正文抢）；
  画布注释/右键的原始命中先裁剪到 canvas，再排除弹层/窗口/模态、连续动作和任意浮窗；
  注释 hover 只保留 `Delete` 目标，不再高亮/置顶浮窗；指针离开画布立即清空。
  弹层判定用**图层顺序**（非 Background）而不是 `is_pointer_over_egui()`：
  后者按 root Ui 剩余矩形判断，而 `CentralPanel` 会消费掉剩余区域，导致画布 hover 被误判。
  新增 `canvas_annotation_hover_right_click_and_delete_are_scoped` 覆盖 hover/右键/Delete/侧栏越界。
- **浮窗拖动遮罩**：每个浮窗先注册一个只感知 drag 的全 rect 遮罩；高浮窗正文（Markdown 只感知 click）
  上的拖动不再落到下层浮窗的标题栏/图片正文，也不会漏到 PDF 正文。标题/正文/手柄的拖动仍按各自控件工作。
- **文本/图片浮窗跳转与稳定链接**：文字浮窗标题栏「↗ 复制引用链接」与「→ 跳到来源」
  也适用于图片 preview；注释链接改为 `pd-float://annotation/<page>/<id>`，
  preview 链接用 sidecar v5 的稳定记录 id（文字 preview 复用 `image_id`）：
  `pd-float://preview/<record_id>`。点击后复用注释单击/侧栏 preview 的打开/置顶/高亮路径；
  断链与歧义只在状态栏提示，不自动扫描或级联打开。旧写法
  `annotation/<id>`、`preview/<page>/<id>`、`preview/<id>` 继续兼容。
- **注释 id 稳定性**：新建内部注释用时间戳/单调、**不复用**的 id（<2^53，避免写回 job 的
  JSON number 被 f64 舍入）；改样式（颜色/透明度/墨迹线宽）改为同 id 重建
  （`pdf_ref=None` 进 deletes），改正文/移动墨迹/撤销重做也保持 id。外部注释仍按加载顺序
  分配 id。
- **外部注释链接**：默认禁止复制；`?` 中可开启“允许跨会话外部注释链接”。链接带本次
  文档加载 token；token 不匹配时校验页号/类型/创建时间，不完全可验证则弹确认框；
  关闭时 token 不匹配直接断链。外部编辑器改写 `/NM` 会让原内部注释退化为外部注释，
  旧链接明确断链而不是按 id 猜。
- **侧栏 ⋯ 菜单**：preview/注释行只保留一个 `⋯` 按钮，点按钮与右键弹同一菜单
  （复制链接、跳转、编辑/删除、复制图片引用）；行单击行为不变。
- **编辑窗口遮挡浮窗**：raw click 置顶先按图层判定，编辑窗口/弹层覆盖处不再置顶；
  浮窗按钮/拖动也在指针位于非 Background 图层时禁用。画布 hover/点击统一走
  `canvas_hover_allowed`（先 canvas 矩形，再“非画布本层”的窗口/弹层，再浮窗/连续动作），
  编辑窗口盖住的链接 hover、点击、选字、便签选点都不再触发。画布层用
  `app.canvas_layer` 记录，回放/布局中画布处于 Middle 层也能正确区分。
- **删除 preview 误关其它浮窗**：`shift_float_custom_indices` 先移除被删条目的浮窗，
  再把后续下标前移；之前先前移再 `retain(owner==index)` 会把前移后恰好变成 index 的
  其它 preview 浮窗一起删掉。新增回归测试。
- **透明度 100% 与外部兼容**：内部注释（有 `pd-fr:` `/NM`）显式 `/CA 1.0` 保留 100%，
  保存/重开不再回退成 35%；外部高亮仍沿用旧兼容规则（`/CA` 缺失或 1.0 → 35%），
  避免改变其它阅读器写出的高亮显示。
- **Phase 3 四组状态分组**：`app/state.rs` 收进 RuntimeState（诊断/时钟/观察值）、
  SaveFlowState（另存为/整理/关闭确认）、MarkdownState（Markdown/数学/图片缓存）、
  AnnotationPdfState（对象号/读回/内外注释簿记）；`PdfReaderApp` 顶层字段
  159 → 118。热点字段（session/floats/annotations/previews/settings）保持顶层，
  无行为变化。
- **UI 样式共识（未实施）**：新增按钮/侧栏行/图标/主题颜色的约定落在
  [ui-standards.md](ui-standards.md)；当前代码仍使用部分 Unicode 图标和未统一侧栏行。
- **UI 第二步：SVG 图标**：新增 `app/icons.rs` 与 `assets/icons/*.svg`，用 `resvg`
  按 DPI/主题色缓存纹理；浮窗关闭/编辑/link/goto/image-ref、顶栏文字/图片预览、
  侧栏类型/更多图标已替换，`CUSTOM_SYMBOL_*` 常量移除；删除图标资源已备，菜单项暂用文字。
- **UI 第一步：hover 固定 + dest goto**：链接 hover 的临时预览在按下帧不再被
  `using_pointer` 清掉，点击/拖动可固定；引用点/dest 浮窗增加 `→`，目标为
  `DestMark.point`（不是截图 rect 中心），仍不提供 `↗`。新增回放回归与单测。
- **UI 第三步：侧栏行统一**：注释行改为和 preview 行相同的整行 response +
  透明 `⋯` + 整行右键；hover 整行背景；不再只让左侧 label 响应。
- **UI 第四步：显式主题**：新增 `ui.dark_mode`（默认浅色），启动/切换时显式
  `ctx.set_theme(Theme::Light/Dark)`；顶栏「样式」菜单；自绘浮窗/临时预览使用
  `FloatPalette`（深浅两套），Markdown 局部 Visuals 随主题；`PDF_READER_DIAGNOSE=1`
  启动打印实际 panel/window/text 色。
- **图片可获取性重检**：文字浮窗引用的 `pd-img` 每次 owner 扫描都重新判定来源
  preview/dest 是否还在；来源被删时清掉 `ImageStore` 的旧 revision 并 `forget_image`，
  不再继续显示缓存里的已删除图片。
- **Markdown 空正文双击**：`detect_body_double_click` 不再对整个 Markdown 模式提前返回，
  只跳过**非空** Markdown 正文（它们有自己的链接/滚动控件），空正文仍走全窗双击层；
  新增无窗口双击回归测试。
- **文字浮窗 Markdown 阅读视图**：顶栏全局开关（`settings.json` 的 `ui.markdown_floats`）；
  注释/便签/文字 preview 用 `egui_commonmark` 渲染，正文可滚动、链接只提示；
  单换行按笔记语义保留为硬换行；代码块/表格/长 URL 会测最小内容宽度并横向滚动；
  标题栏 ✎/双击打开正式的可调整编辑窗口（高度跟随窗口、每次打开重置滚动、未保存时确认）；表格走完整渲染路径，避免视口切片把表格推到右侧；
  图片浮窗与 PDF 写回路径不变。多个浮窗重叠时滚轮只归 z 最高者。
  拖放打开在空状态也生效；Linux Wayland 受 winit 0.30 限制，可走 X11 并观察 `[drag]` 日志。
  注释坐标按渲染盒原点归一化，写回时加回；`Ctrl+S` 不再被输入焦点吞掉。
  Markdown 预处理（规范化/最小宽度/图片引用）按源文本哈希缓存；完整布局仍由 commonmark 每帧排版。
  内部图片 B1：sidecar v4 增加 `image_id`/`image_revision`（v5 起文字 preview 也用它作链接 id）；`ImageStore` + `pd-img` loader；
  只扫描打开的文字浮窗引用，缺图时由 preview/被引用点 owner 懒渲染；图片浮窗 @、引用点右键复制引用；
  编辑窗口「插入内部图片」；导出摘要时已缓存图片转 base64；同一来源的旧 rev 在渲染前规范化，
  重开 PDF / 重选目的地后旧引用也能显示当前图。顶栏「hover MD」开关持久化，
  开启后侧栏文字临时预览也走 Markdown/数学/图片引用。Markdown 锚点：`## 标题 {#id}` + `[文字](#id)` 在当前浮窗内滚动；每个浮窗独立缓存，多浮窗相同 id 不串；
  正文双击打开编辑器时按点击位置放置光标/滚动（纯文本精确，Markdown 近似）。
- **LaTeX 数学（P1.5）**：`MathBackend` trait + `latex-rust 3.0.1` SVG 后端（默认；PNG 可 env 回退）；按公式/行内/DPI
  缓存（64 条上限，超宽降采样/回退）；错误类型区分语法/字形/不支持/过大；
  解析失败或 `\text{中文}`/全角标点缺字形回退原文，状态栏提示（下一条消息可顶替）；
  显示公式块内空行在渲染前去重，`~` 在送入后端前转换为空格；保存正文后重新检查公式，
 提示可合并为“已更新内容；数学公式：…”。顶栏「?」同时说明 Markdown 与数学语法/后端能力/限制。
  SVG 按公式逻辑尺寸 × ppp × 质量倍率光栅化；行内公式显式同步行高；公式前后文本基线仍受 CommonMark 行布局限制。
  应用不自行设置系统缩放，ppp 由 eframe/winit 继承；应用级 UI 缩放/高 DPI 清晰度留待后续一并验证。
  数学纹理不持久化、不写回 PDF。
  工作区 MSRV 因此升到 1.99。
- **注释写回/读回**：对象级增量写回；读回配对 `Text(1)` 等 subtype；同一 id 多份时以最后一份为准；
 外部注释只读（可改正文/删除）。
- **便签**：`/Subtype /Text` 图标 + 正文；创建、编辑、删除、侧栏筛选、导出摘要。
- **墨迹平移**：Ctrl（或 Alt）+拖拽、右键「移动（拖动）」；一条撤销；写回重建对象。
- **保存语义合并**：保存 / 另存为 / 覆盖保存共用 `write_document`；顶栏只留一个「保存」；
 删除“保存自定义数据”按钮和“导出副本”；原地保存备份改为设置项（默认关闭）。
- **脏位**：历史 revision + 保存点；撤销变脏、重做回到保存点自动干净；预览编辑不弄脏 PDF。
- **未保存提示**：关闭窗口 / 最近打开 / 拖放在脏时提示；外部修改冲突时只能覆盖（另存为原路径）
 或另存为/不保存/取消；保存失败保留对话框。
- **设置**：`settings.json` 增加 `save.backup_in_place` 与五个顶栏开关。
- **旧格式移除**：不再读取/创建 `.annot.json`，也不会再产生 `.annot.json.bak-*`；遗留文件忽略。

## 3. 当前已知限制

- **Linux Wayland 外部文件拖放不可用**：winit 0.30 的 Wayland 后端不投递 DnD 事件；
  用 `env -u WAYLAND_DISPLAY` 走 X11/XWayland 可用。Windows 正常。
- 外部注释的几何/颜色/样式仍只读；改正文/删除会写回 PDF。
- 便签不支持拖动/缩放；墨迹平移没有吸附、页面边界夹取，浮窗不跟随。
- 页面文本搜索不索引注释正文；侧栏搜索覆盖注释正文/作者。
- 无头回放只覆盖主视图渲染主线；浮窗、侧栏、截图、便签、保存提示等仍需人工验收。
- 纹理层按条目数而非显存字节限额；极端长宽比页面的分块未按轴分级。
- 签名/加密 PDF 拒绝原地写入与整理；加密文档不能另存为。
- 未接 CI；性能基线只能在固定机器上比较。
- Adobe 对作者/时间的兼容性尚未人工验证（Okular 已验证）。

## 4. Markdown 模式计划完成情况

| 项目 | 状态 | 说明 |
| --- | --- | --- |
| 文字浮窗 Markdown 阅读视图开关 | 完成 | 顶栏全局开关，`settings.json` 持久化 |
| 单换行/块级公式空白规范化 | 完成 | 笔记语义硬换行；块内空行去重 |
| 正式正文编辑器 | 完成 | 标题栏 ✎、双击、光标定位、未保存确认；空正文 ✎ 可用 |
| LaTeX 数学后端抽象 | 完成 | `MathBackend`；`latex-rust` 默认 SVG，PNG 可 env 回退 |
| 数学错误/帮助 UX | 完成 | 语法/字形/不支持/过大分类，帮助窗口与状态栏提示 |
| 内部图片 B1 数据/解析 | 完成 | sidecar v4 `image_id`/`image_revision`、`ImageStore`、`pd-img` |
| 内部图片 B2 UI/导出 | 完成 | 插入引用、@ 复制、懒渲染、旧 rev 规范化、base64 导出 |
| Markdown 锚点 | 完成 | `## 标题 {#id}` + `[文字](#id)`，每浮窗独立缓存 |
| hover 临时预览 Markdown | 完成 | 顶栏 `hover MD`，默认关闭 |
| 侧栏 preview/注释 `⋯` 菜单 | 完成 | 复制引用链接、跳转、编辑/删除、复制图片引用；与右键同一菜单 |
| Markdown 预处理缓存 | 完成 | 规范化/最小宽度/图片引用按源文本哈希；完整布局仍每帧执行 |
| SVG 后端 | 完成，视觉验证暂缓 | 按 ppp/目标像素光栅化；等应用级 UI 缩放时一起验高 DPI |
| RaTeX 可选后端 | 完成（可选） | `math-ratex` feature 默认 RaTeX；不默认构建；CJK 依赖系统字体 |
| 行内公式行高/基线 | 部分完成 | 显式同步行高；前后文本基线仍受 CommonMark row 布局限制 |
| Markdown 空正文双击 | 完成 | 空正文走全窗双击层；带无窗口回归测试 |
| Markdown 完整布局复用 | 未完成 | `egui_commonmark` 不暴露布局缓存；需上游/自研渲染器 |

## 5. 代码地图（先看哪）

| 路径 | 内容 |
| --- | --- |
| `crates/pdfcore` | 不依赖 UI 的模型/会话/几何/布局/渲染调度；`#![forbid(unsafe_code)]` |
| `crates/backend-pdfium` | PDFium 适配、索引/读回、写回（子进程 FFI）、整理副本 |
| `crates/app/src/view.rs` | 可测的纯逻辑：坐标、画法、浮窗几何、摘要格式 |
| `crates/app/src/app.rs` | 状态/构造/`eframe::App::ui` 入口（~0.6k）；子模块见 `crates/app/src/app/` |
| `crates/app/src/app/model.rs` | `app` 共用类型/常量/纯函数 |
| `crates/app/src/app/canvas/` | 画布：输入与交互（`interact.rs`）、页面/叠加层绘制（`paint.rs`） |
| `crates/app/src/app/frame.rs` | 每帧编排（命令、pump、纹理、绘制顺序） |
| `crates/app/src/app/save.rs` / `document.rs` | 保存/脏位/写回编排；打开/读回合并/对象号对账 |
| `crates/app/src/app/tests.rs` | 原 `app.rs` 内联测试 |
| `crates/app/src/persist.rs` | `<pdf>.json` 自定义数据（手写 JSON + 版本迁移） |
| `crates/app/src/settings.rs` | `settings.json`（全局 UI + 每文件阅读状态 + 保存选项） |
| `crates/app/src/annotations_md.rs` | 注释摘要（Markdown） |
| `scripts/check.sh` | fmt → clippy → test → 回放基线 → 文档链接 |

## 6. 接手建议

1. 先读 [architecture.md](architecture.md) 的不变量清单，再读你准备改的模块对应的文档。
2. 改 UI/保存/注释行为前读 [app-ui.md](app-ui.md)；改写回/读回前读 [writeback.md](writeback.md)、
  [readback.md](readback.md)。
3. 未完成/暂缓事项集中在 [roadmap.md](roadmap.md)，不要凭印象加功能。
