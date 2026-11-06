# UI 层（现状）

> `app` crate = `view`（纯逻辑，可单测）+ `app`（egui 胶水，需真机）+ 存档/设置/写回/整理等无 UI 模块。
> 这里的每条"每帧顺序"与"层级"都直接对应代码；改动请同步本文。

## 1. 模块划分

| 模块 | 内容 | 怎么验证 |
| --- | --- | --- |
| [`view.rs`](../crates/app/src/view.rs) (~4.2k) | 坐标换算、输入映射、`KeyedStore`、画法决策（`plan_page`/`preview_source`/`underlay_source`）、快照拼合（`compose_snapshot`/`compose_region_snapshot`）、浮窗几何、持久化辅助 | `cargo test -p app --lib` |
| [`app.rs`](../crates/app/src/app.rs) (~0.6k) | `PdfReaderApp` 状态 + 构造 + `eframe::App::ui` 转发到 `app::frame::frame` | 人工 |
| [`app/model.rs`](../crates/app/src/app/model.rs) (~1.0k) | `app` 子模块共用类型/常量/纯函数（`Float`/`AppEdit`/`Tool`/`CaptureState`…） | `cargo test -p app --lib` |
| [`app/frame.rs`](../crates/app/src/app/frame.rs) (~0.5k) | 每帧编排：命令采集、pump、布局切换、纹理上传、绘制顺序 | 回放 + 人工 |
| `app/canvas/`（`mod.rs` ~0.1k、`paint.rs` ~0.6k、`interact.rs` ~0.5k） | 画布：输入与点击/拖动交互；页面/选区/链接/标注/引用点绘制 | 回放 + 人工 |
| [`app/toolbar.rs`](../crates/app/src/app/toolbar.rs) (~0.6k) | 顶栏三行（文件菜单 / 视图开关 / 页与缩放） | 人工 |
| [`app/sidebar.rs`](../crates/app/src/app/sidebar.rs) (~0.6k) | 搜索栏、目录、自定义 preview 列表 | 人工 |
| [`app/floats.rs`](../crates/app/src/app/floats.rs) (~1.1k) | 浮窗绘制/打开/关闭/正文内容 | 无窗口测试 + 人工 |
| [`app/hover.rs`](../crates/app/src/app/hover.rs) (~0.4k) | 悬停预览（含 hover MD） | 人工 |
| [`app/body_editor.rs`](../crates/app/src/app/body_editor.rs) (~0.5k) | 正文编辑器与未保存确认 | 无窗口测试 + 人工 |
| [`app/dialogs.rs`](../crates/app/src/app/dialogs.rs) (~0.8k) | 保存确认/未保存提示/整理/密码/数学帮助 | 人工 |
| [`app/save.rs`](../crates/app/src/app/save.rs) (~0.7k) | 脏位/写回/另存为/自定义数据/替换文件 | 集成测试 + 人工 |
| [`app/document.rs`](../crates/app/src/app/document.rs) (~0.5k) | 打开/读回合并/对象号对账 | 集成测试 |
| [`app/annotate.rs`](../crates/app/src/app/annotate.rs) (~0.7k) | 标注编辑/右键菜单/历史窗口 | 回放 + 人工 |
| [`app/modes.rs`](../crates/app/src/app/modes.rs) (~0.2k) | pin/墨迹平移/便签状态机 | 回放 + 人工 |
| [`app/capture.rs`](../crates/app/src/app/capture.rs) (~0.5k) | 截图/引用点/快照拼合 | 人工 |
| [`app/edit.rs`](../crates/app/src/app/edit.rs) (~0.4k) | 撤销/重做/视图记忆/settings flush | 单测 |
| [`app/textures.rs`](../crates/app/src/app/textures.rs) (~0.4k) | 纹理上传/预算/预览纹理 | 回放 |
| [`app/paint.rs`](../crates/app/src/app/paint.rs) (~0.3k) | 无状态绘制辅助（`paint_*`/坐标换算） | 人工 |
| [`app/tests.rs`](../crates/app/src/app/tests.rs) (~1.8k) | 原 `app.rs` 内联测试（56 个） | `cargo test -p app --lib` |
| [`app/replay.rs`](../crates/app/src/app/replay.rs) (+`replay/baseline.rs`、`replay/samples.rs`，合计 ~3.2k) | 无窗口回放：脚本驱动真实 `App::ui`，逐帧采集与结构基线 | `cargo run -p app -- --replay …` |
| [`math.rs`](../crates/app/src/math.rs) (~0.9k) | LaTeX 后端 trait、`latex-rust` SVG（PNG 可回退）实现、公式纹理缓存、错误类型 | `cargo test -p app --lib math::` |
| [`markdown.rs`](../crates/app/src/markdown.rs) (~0.3k) | 浮窗 Markdown 预处理（单换行→硬换行、最小内容宽度测量） | `cargo test -p app --lib markdown::` |
| [`image_store.rs`](../crates/app/src/image_store.rs) (~0.5k) | 内部图片 PNG 缓存（32 张/64MB）与 `pd-img` loader | `cargo test -p app --lib image_store::` |
| [`ink.rs`](../crates/app/src/ink.rs) (~0.24k) | 墨迹手势状态机（纯逻辑）：页面空间采样、0.5 抽稀、2000 点上限、收笔判定 | `cargo test -p app --lib ink::` |
| [`persist.rs`](../crates/app/src/persist.rs) (~1.6k) | 存档 JSON（手写序列化 + 极简 JSON 解析）+ 版本迁移 | `cargo test -p app --lib persist::` |
| [`settings.rs`](../crates/app/src/settings.rs) (~0.7k) | `settings.json`（缺字段默认、坏文件改名 `.bad-*`、原子写） | `cargo test -p app --lib settings::` |
| [`export.rs`](../crates/app/src/export.rs) (~1.0k) | 写回 job 组装、`--writeback` 子进程调用、校验与原子改名 | `cargo test -p app --test writeback_cli` |
| [`writeback.rs`](../crates/app/src/writeback.rs) (~0.5k) | 子进程侧 `--writeback` job 执行 | `cargo test -p app --test writeback_cli` |
| [`compact.rs`](../crates/app/src/compact.rs) (~0.2k) | 整理副本子进程入口与校验 | `cargo test -p app --test compact_cli` |
| [`history.rs`](../crates/app/src/history.rs) (~0.3k) | 撤销栈（带 `affects_pdf` 标记的修订号） | `cargo test -p app --lib history::` |
| [`annotations_md.rs`](../crates/app/src/annotations_md.rs) (~0.2k) | 注释摘要 Markdown 渲染 | `cargo test -p app --lib annotations_md::` |
| [`json.rs`](../crates/app/src/json.rs) (~0.3k) | 极简 JSON 解析/序列化 | `cargo test -p app --lib json::` |
| `clock.rs` / `diag.rs` (~0.2k 合计) | 可注入时钟（回放/测试）、诊断去处（stderr 或内存缓冲） | `cargo test -p app --lib clock::` |
| `main.rs` / `lib.rs` | 启动、子命令分发（`--writeback` / `--compact` / `--replay`）、模块导出 | 人工 |

约定：**能脱离窗口验证的逻辑一律放 `view.rs`**；`app.rs` 只留"读输入、调会话、画"。

## 2. 每帧顺序（`app::frame::frame`，由 `PdfReaderApp::ui` 转发）

```text
1. handle_dropped_files         拖入文件立刻打开（空状态/Windows 显式开启 drag-and-drop）
2. session.take()            借出会话，避免与其它字段借用冲突
3. collect_commands           键盘/滚轮 → ViewerCommand（截图模式下丢弃）
4. pending_layout → session.set_layout  纵向/横向切换
5. apply_and_pump            session.handle(命令) + pump 事件
6. 预览窗口判定（previewing）       缩放/旋转后 160ms 内不请求渲染
7. ensure_textures            上传整页/块纹理；裁剪到"当前+两档底料"
8. draw_toolbar             顶栏（三行；全屏时为悬浮 Area）
9. draw_sidebar(ui, &mut session)    侧栏（目录 / 自定义preview 两个标签页）
10. draw_status             底部状态栏
11. draw_canvas(ui, &mut session)    画布：输入(paint/interact) → 页面 → 高亮/链接/标注 → 浮窗 → 悬停预览
12. RequestVisible            提交渲染（预览窗口内跳过）
13. draw_password_prompt / draw_custom_dialog / draw_edit_dialogs
```

画布内部的顺序（决定遮挡关系）：

```text
纸张(白底+页框) → 底料 → 主图层(Preview/Tiles) → 瓦片红线(可选诊断)
→ 选区/搜索高亮 → 链接框 → **标注叠加层（高亮/下划线/删除线/墨迹 + 正在画的一笔）**
→ 引用点 → 内容空间浮窗(draw_floats)
→ 正文原位编辑框 → 临时悬停预览(draw_hover_preview) → 侧栏悬浮浮窗
```

"后画的在上面"：临时预览必须在固定浮窗之后，才能与"点击取消"竞争时赢。

## 3. 面板结构

| 区域 | 内容 |
| --- | --- |
| 顶栏（`Panel::top`，全屏为 `Area`） | 第 0 行：文件路径（中间截断）+ **`文件` 菜单**（保存 / 另存为… / 整理副本… / 导出注释摘要… / 最近打开 / 原地保存前备份）；第 1 行：侧栏开关、＋文字/＋图片预览、Pin悬浮响应、**Markdown**；第 2 行：上一页/页码/下一页、缩放比与菜单、顺/逆时针、平移/选字、**注释工具 ▾**（高亮/下划线/删除线/墨迹/便签，弹出菜单）+ **样式** 按钮、纵向/横向、横向滚动、引用显示、全屏；Pin悬浮响应 / 横向滚动 / Markdown / hover MD / 链接框 / 引用点 / 标注七个开关写入 `settings.json` 并恢复 |
| 侧栏（拖拽条 + 自绘面板） | 标签页「目录」（虚拟化行）/「自定义preview」（行内 `→` `✎` `❎`） |
| 中央画布 | 见 §2 画布顺序；左侧 5px 拖拽条 |
| 底部搜索栏（中央区域 `Panel::bottom`） | 命中计数、上一条/下一条、输入框 |
| 状态栏 | 页码、缩放、消息、错误 |
| 浮窗层 | 固定浮窗（可拖动/改大小/关闭/点击置顶）+ 临时预览（最上层） |

## 4. 交互状态机

| 状态 | 进入 | 退出 | 行为 |
| --- | --- | --- | --- |
| `DragMode::Pan` / `Select` | 工具栏「平移/选字」 | 切换工具 | 平移 = `ScrollBy`；选字 = `SelectBegin/Extend`（起点不在字上时不做动作） |
| `DragMode::Ink`（墨迹工具） | 工具栏「墨迹」 | 抬笔入库；`Esc` 或切工具 → **丢弃整笔** | 按下（`press_origin`）起笔 → 拖动累积**页面空间**点（0.5 抽稀、2000 点上限）→ **抬笔时** `annotate_ink`（一笔 = 一条标注 = 一次撤销）。拖动中只画临时折线，不进标注集合、不产生命令 |
| Pin（自定义 preview 取点） | ＋文字/＋图片预览 | **只在切到选字/截图时退出** | 十字光标；点击页面 → 对话框 → 截图（图片预览） |
| 截图模式（`CaptureState`） | 截图按钮 / 图片 preview / 点击引用点 | `Enter` 保存、`Esc` 取消 | 主视图动作全部短路（但 `pump` 继续）；蒙层挖洞；`Enter` 后输出区域信息（stderr） |
| 悬停预览 | 指针在带落点的页内链接 / 侧栏 Pin 行上 | 指针离开 / 滚动静默窗口 / **正在画墨迹** | 显示目标区域截图；已有同来源浮窗时改为高亮置顶；画墨迹期间一律不弹 |
| 原位编辑 | 双击页码/缩放/浮窗标题 | 失焦或「完成」 | 编辑期间不采集视图键盘命令（`memory().focused()`） |
| 正文编辑 | 标题栏 **✎**（正文双击在能落到 body 时也可用） | 「保存」/「取消」/关闭窗口 | 打开可调整大小的正式 `Window`；内部 `ScrollArea` + 多行源码编辑器，不受浮窗尺寸限制；双击位置尽量把光标/滚动放到源码对应处（纯文本精确，Markdown 按纵向比例近似）；保存走原 `SetBody`/`SetPreviewBody` |
| Markdown 浮窗 | 顶栏「Markdown」开启（全局） | 关闭开关 | 文字浮窗改为 Markdown 阅读视图：标题栏拖动，正文内部滚动/点链接；外部链接只提示；图片浮窗不受影响 |
| Markdown/数学帮助 | 顶栏「?」 | 关闭窗口 | 列出 Markdown 与数学语法、当前后端能力、支持/不支持与限制；错误在状态栏提示 |
| 密码框 | `CoreError::PasswordRequired` | 输入正确密码重试 | 模态 |

### 4.1 标注

* **几何只在未旋转页面空间**；叠加层在画布上按布局换算后绘制，**不进渲染管线**（不产生 `RenderKey`、不改瓦片）。
* 种类：高亮 / 下划线 / 删除线（文本行矩形）+ **墨迹**（折线 + 页面空间线宽）。新建时文本类默认半透明黄、
 墨迹默认不透明深色（`pdfcore::annotation::{TEXT_MARK_*, INK_*}`）。
* 墨迹绘制（`paint_ink`）：**按页面矩形裁剪**（跨页拖拽时不会画到相邻页的空白上，与 PDF 阅读器一致）；
 折角补半径 = 半线宽的圆点（egui 的折线逐段四边形，直角处会漏缝）。
* 空正文浮窗仍保留标题栏 **✎** 和正文双击入口，不会因为 `Text("")` 省掉命中区域；
  Markdown 模式空正文不创建 ScrollArea，直接用一个正文点击层接收双击。
* 拖放打开前会打印 `[drag] hovered/dropped files: ...`；Linux Wayland 的 winit 0.30
  后端不实现外部文件 DnD（指针显示禁止）。实测可用 `env -u WAYLAND_DISPLAY cargo run -p app`
  走 X11/XWayland；`WINIT_UNIX_BACKEND=x11` 在本环境不可靠。
* 保存快捷键 `Ctrl+S` 在焦点/截图判断之前处理；正文编辑器有草稿时提示用 `Ctrl+Enter`。
* 注释几何进入模型前按页面的 `MediaBox ∩ CropBox` 原点归一化；写回 job 再带回该原点，
  避免原点非 0 的 PDF 上注释整体偏移。
* 注释正文**直接属于注释**（`Annotation.body`），点击注释打开独立浮窗（`FloatOwner::Annotation`），
 双击正文/✎ 打开正式编辑窗口，显式保存为一条 `SetBody` 撤销命令；不进侧栏、不写 `.json`。
* 「保存」是一个入口：PDF 脏就写回 PDF，自定义数据脏就写 `.json`；星号是两者合并的脏位；
 Ctrl+S 同路径；旧 `.annot.json` 不读取、不生成。
 撤销/重做回到保存点时对应脏位会清零；未保存的新注释关闭/切换文件即丢弃。
 外部注释已被写回删除后，撤销会把它作为新注释重新写回（不再是原外部对象）。
* 外部注释也进入主集合：点击打开正文浮窗、可改正文/删除；几何/颜色仍只读。
 正文修改/删除**只存内存**，由「文件 → 保存」按对象号落盘。
* 悬停/右键删除**只看几何**，与“有没有正文”无关。
* 注释的正文 / 作者 / 时间**只存 PDF**（旧 `.annot.json` 已完全不读不写）：
 平时只存内存，写进 PDF 后是 `/Contents`、`/T`、`/M`、`/CreationDate`；重开时由读回恢复
 （时间按本地时区显示）。点标注打开浮窗；正文缺失时显示"正文已丢失"但不影响加载。
* 注释列表 tooltip：正文摘要 + `类型 · 作者 · 自有|外部 · 内联`，**第二行**是
 `创建 … · 修改 …`（两者相同只显示"创建"；没有时间就不显示）。作者替代了原来的"第 N 页"——
 列表本来就按页分组。作者缺失显示"未署名"。
* 注释列表顶部：**视图**（按页 / 最近修改）、**搜索**（正文/作者，忽略大小写）、**筛选**菜单
 （类型、仅外部、仅只读）。按页视图的页头可**折叠**并显示该页条数；最近修改是平铺列表，
 按修改时间倒序（没有时间的排最后）。
* 删除：悬停 + `Delete`，或右键菜单「删除标注」；两者都可 `Ctrl+Z` 撤销。
* **便签**：`注释工具 → 便签` → 点击页面位置 → 输入正文 →「添加」（一条撤销记录）。
 便签模式与四种标记模式互斥：进入任一方都会退出另一方（不再出现"两种同时选中、Esc 回到便签"）。
 与墨迹一样，**便签不改变阅读模式、也不高亮平移/选字**；进入时记住当前阅读模式 A，
 `Esc` 回到 A；点任一阅读模式则直接切换并丢弃 A。
 在注释模式之间切换（高亮↔墨迹↔便签）**不会改写 A**，Esc 始终回到最初进入注释前的阅读模式。
 画布上显示带折角的小图标；点击打开正文浮窗，双击正文沿用浮窗编辑；侧栏类型筛选有「便签」。
 暂不支持拖动/缩放。
* **移动墨迹**：**平移工具或选择工具**下 `Ctrl+拖拽` 或 `Alt+拖拽` 一条自有墨迹（悬停时指针变抓手）→
 平移；也可以右键「移动（拖动）」选中后直接拖（不依赖修饰键）。松手提交，`Ctrl+Z` 回原位；
 外部墨迹拒绝（会提示）。写回时旧对象删除、按新几何重建。
 （`Alt+拖拽` 在部分 Linux 桌面上会被窗口管理器抢去拖窗口，所以主推 `Ctrl+拖拽` 或右键入口。）
* 已有注释改样式（自有注释）：右键「颜色 / 透明度（高亮）/ 线宽（墨迹）」，可撤销；
 外部注释只读（不重建，避免变成自有对象）。新注释的默认样式来自 `settings.json` 的
 `annotation_defaults`（[persistence.md](persistence.md) §6）。
* 撤销：全局单栈（`Ctrl+Z` / `Ctrl+Shift+Z`）+ 可视历史窗口（点击行只定位，不做状态跳转）；
 历史窗口**虚拟化**（`ScrollArea::show_rows`，行高 18、最大高 360）—— 1000 条编辑时也只排版可见行
 。
* 压力门槛：回放 `ink_many.json`（1000 条）+ 结构基线保证"标注不进渲染管线"；
 时间上限只在测试里（debug 宽松值），见 [`testing.md`](testing.md)。
* **带注释的副本**：用「文件 → 另存为…」；另存为允许覆盖已存在的目标（对话框里勾选）。
* **未保存提示**：关闭窗口 / 最近打开 / 拖放切换文件时，脏状态先弹提示：
 「保存并继续 / 另存为… / 不保存 / 取消」；原 PDF 被外部修改时「覆盖保存…」替换「保存并继续」。
 只写自定义数据（PDF 不脏）的保存不检查 PDF 冲突。
 写回细节与排查见 [writeback.md](writeback.md)。
* **导出注释摘要**：`文件 → 导出注释摘要…` → 同目录 `<stem>.annotations.md`（不覆盖，递增）。
 内容：# 文件名 / 共 N 条（含外部 M 条）/ 按页分组；每条标题 `序号. 类型 · 作者`，
 下一行 `创建 … · 修改 …`，正文用引用块（空正文写"（无正文）"）。
 渲染是纯函数 `annotations_md::render`（有单测）。
* **保存**：`文件 → 保存`（或 `Ctrl+S`）→ 按实际写入内容显示确认框（PDF / `.json` / 两者，
 注释计数、备份说明）→ 子进程按 `/NM` 同步写临时文件 → 关闭会话（释放文件句柄）→
 可选的 `.bak` 备份 → 原子替换 → 重开并恢复视图。失败/冲突时对话框保留并显示错误。
 允许**空标注**写回：把 PDF 里我们写过的注释全部删掉（界面上删光之后按钮仍可用，
 条件见 `pdf_marked_annotations`）。
* **整理副本**：`文件 → 整理副本…` → 对话框（默认同目录 `<stem>.compact.pdf`，已存在则递增）→
 **子进程**跑 `--compact`（与导出/写回同一条隔离策略：主进程有渲染线程，校验会碰裸 FFI）→
 状态栏报告 `已整理：<原> → <新>（-33%，丢弃 N 个旧对象）`。
 只写新文件、**不切换当前文档**、不动 `.json`。
 `pdf_dirty`（注释还没写进 PDF）时对话框显示红字"当前注释改动尚未写入 PDF，不会被整理进副本"，
 但**不阻止**整理；目标已存在、加密、带签名、已经很紧凑都会拒绝并给出原因。
 算法与边界见 [architecture.md](architecture.md) §4.6 与 `backend-pdfium/src/pdf_compact.rs`。

* **重开即从 PDF 导入**：打开文件就会按 PDF 读回注释；
 所以"丢弃内存改动、以 PDF 为准"不需要单独入口：**「从 PDF 重建」已移除**。
 未写回 PDF 的注释在关闭/重开时会丢，这是既有语义。
* **最近打开**：`文件 → 最近打开` 列出 `settings.json` 里的 `documents`（最多 8 条，
 显示文件名与上次页码）；文件不存在时禁用并提示。打开后会按该文件的上次页码/缩放恢复。

## 5. 浮窗（`Float`）

* 内容：`FloatContent::{Text, Image}`；`custom: Option<usize>` 记录来源（侧栏 Pin 条目）；
 `origin: Option<(PageIndex, usize)>` 记录来源（链接目的地）。
* **尺寸权威来自浮窗自身**（`Float.body`）：只在创建与拖右下角改大小时写入，
 回写到记录（`body_size`），**不从矩形反算**（避免亚像素误差逐帧漂移）。
* 层级：`floats` 的顺序即 z 序；点击/标题栏点击 → `move_to_last`；临时预览最后绘制。
* 关闭：右上 ✕（有可见记号）；`Esc` 关闭最上层（截图模式除外）。
* **Markdown 阅读视图**：顶栏「Markdown」是**全局开关**（`settings.json` 的 `ui.markdown_floats`，
 默认关闭）。开启后所有**固定文字浮窗**（自定义文字 preview、注释/便签正文）改用
 `egui_commonmark` 渲染；图片浮窗、侧栏悬浮的临时预览不变。
* Markdown 只改变**显示**：源文本仍是 `CustomPreview.text` / `Annotation.body`，保存语义、撤销、
 写回 `/Contents` 全部不变；`Float.body` 不因开关变化。
 正文默认允许选词/复制，因此双击文字可能先选中词；**标题栏 ✎** 是稳定入口，打开统一的正式编辑窗口。
 编辑器高度跟随窗口、按钮固定在底部；每次打开换一个滚动会话（不同文本不继承滚动位置），
 窗口大小仍由 egui 记忆。编辑草稿与已保存内容不同时，关闭浮窗/切换文件会先确认。
* 正文内是独立的 `ScrollArea`，且**填满 body 高度**：短内容也把横向滚动条放在浮窗底部；
 改变浮窗大小只需重新排版（正文每帧渲染，不依赖旧尺寸缓存）。
 标题栏拖动窗口，正文滚动/点链接；右下角缩放手柄与滚动条错开。
 滚轮/缩放只在指针**不在任何浮窗上**时驱动 PDF；在浮窗上时归浮窗内部。
* 代码块/表格/长 ASCII token 会按真实字体测一个最小内容宽度；超过 body 时由横向滚动条承载，
 `Float.body` 仍不变。表格使用 `CommonMarkViewer::show` + 外层 `ScrollArea` 的完整渲染路径；
 `show_scrollable` 的视口切片路径在 0.26.0 会把表格排到视口右侧，因此不采用。
* 外部链接**只提示**（状态栏显示 URL），不打开浏览器；与 PDF URI 的现有行为一致。
* 内部图片用 `pd-img` 伪协议：图片 preview 的固定浮窗点 @，引用点标记右键「复制图片引用」；
  正文编辑窗口也可从「插入内部图片」列表追加。临时预览本身可点击或拖动固定。
  导出 Markdown 摘要时，已缓存的内部图片替换成 `data:image/png;base64,...`；内部存档仍保留引用。
  只有打开的文字浮窗会解析/渲染引用；关闭后不再检测图片变化。sidecar v4 保存
  `image_id`/`revision`，PNG 字节按需缓存（32 张/64MB 上限）。不加载本地/网络图片。
* 滚轮/缩放在指针位于任意 Window（如正文编辑、帮助窗口）上时不驱动 PDF；
 多个浮窗重叠时，滚轮只归 z 轴最高的那个浮窗；画布拖动/点击仍可按原方式操作（编辑器窗口不是模态）。
* **LaTeX 数学**：`$...$` / `$$...$$` 经 `MathBackend` trait（当前默认实现 `latex-rust` SVG）
  生成矢量 SVG，再按目标像素光栅化成 egui 纹理；按公式/行内/DPI 缓存（最多 64 条，超宽降采样/回退）。
  目标像素由公式逻辑尺寸 × `pixels_per_point` × 质量倍率决定；高 DPI/缩放更清晰。
  `PDF_READER_MATH_BACKEND=png` 可临时回退旧 PNG 后端做对照；
  默认构建是 `latex-rust` SVG；`./scripts/build.sh --backend latex-png` 可编译默认 PNG 的二进制，
  `--backend ratex` 可编译默认 RaTeX 的二进制（可选 feature，不进入默认构建）。
  运行时仍可用 `PDF_READER_MATH_BACKEND=svg|png` 覆盖。
  RaTeX 的 `\text{中文}` 依赖系统 Unicode/CJK 字体；没有系统字体时设置
  `RATEX_UNICODE_FONT=/path/to/CJK.otf`。若字体不覆盖汉字，会回退原文并提示，
  不再用缺字形占位把同一行文字拉宽。
  应用**不自行**设置系统缩放/UI zoom，`pixels_per_point` 由 eframe/winit 从窗口系统继承；
  Linux 可用 `WINIT_X11_SCALE_FACTOR=2` 验证高 DPI 路径。
  行内公式会显式把当前行高设为“正文行高与公式高度取大”；同一行内公式前后文本的基线
  仍受 `egui_commonmark` 行布局限制，待后续 patch 行布局根治。
  解析失败、缺字形（如 STIX 没有 CJK，`\text{中文}` 或全角标点 `，`）、超长/超纹理限制
  都会回退原文，并在状态栏提示原因（下一条消息可直接顶替）；顶栏「?」有支持/限制说明。
  显示公式 `$$` 块内的空行会在渲染前去掉（CommonMark 空行会断段）；`~` 作为不断行空格会在渲染前转换为空格（后端本身把 `~` 当字面量）。
  如果公式没有正确识别，应把行内 `$...$` 换成块级 `$$...$$` 重试。
  浮窗内锚点：标题写 `## 标题 {#id}`，链接写 `[文字](#id)`；每个浮窗使用独立 Markdown 缓存，点击后在当前浮窗内滚动到标题。
  数学纹理只是临时缓存，不持久化、不写回 PDF。
* **换行策略**（本应用对笔记正文的约定）：代码块之外，单个换行按硬换行渲染（保留用户按 Enter
 写的行结构），空行仍是段落分隔；源文本不被改写，只影响渲染前的预处理。

## 6. 侧栏

* 目录：打开文档时 `view::flatten_outline` 摊平成行，`ScrollArea::show_rows` 只布局可见行。
* 自定义preview：每行 = 类型符号 + 标题（截断）+ 页码 + `→`（跳转）+ `✎`（改题目）+ `❎`（删除）；
 行为**先占位 → 绘制 → 最后在同一矩形上 `ui.interact`**（否则 hover/click 全不生效）。
* 悬浮：有正式浮窗 → 只高亮置顶；没有 → 显示临时预览（可被「Pin悬浮响应」开关关闭）。
  临时预览默认是**轻量缩略**：文字直接按纯文本截断绘制，图片直接画纹理；不跑 Markdown/数学/
  内部图片引用。开启顶栏「hover MD」后才走完整 Markdown/数学/图片引用（会触发 owner 渲染），
  并可能引入滚动/链接交互；点击固定后始终进入完整 Markdown 渲染路径。

## 7. 纹理上传与裁剪

```text
ensure_textures(allow_upload)
 targets = view::texture_pages(可见页 ±1 + 当前页)   // 上传与裁剪用同一集合
 for page in targets（最多 MAX_UPLOADS_PER_FRAME=2 页）:
   upload_page(page)                // 整页图 + 该页所有已缓存块
 textures.retain_preview_scales(&pages, 当前渲染比例) // 每页三档
```

`allow_upload = false`（拖动期间）只做裁剪，松手后补齐。

## 8. 与核心的接口（只在会话上）

| 用途 | 调用 |
| --- | --- |
| 布局/视口 | `layout()` / `placement(page)` / `visible_pages(&mut out)` |
| 渲染 | `RequestVisible` 命令、`pending_renders()`、`pump` 由 `apply_and_pump` 负责 |
| 纹理 | `image_for(page)` / `tiles_for(page)` / `tile_grid(page)` / `render_scale(page)` |
| 预览截图 | `region_tiles` / `request_region`（见 [`persistence.md`](persistence.md) §4） |
| 文本 | `text_position_at` / `highlight_selection` / `selected_text` / `search` |
| 导航 | `navigation()`（打开时已解析）、`link_at` |

## 9. 可测试性接缝

| 接缝 | 作用 |
| --- | --- |
| `PdfReaderApp::new_headless(clock, diag, initial)` | 无 `CreationContext` 也能构造（回放/测试） |
| `Clock`（`clock.rs`） | 行为性截止时间（160ms 预览窗口、400ms 兜底、250ms 静默、高亮消退）统一读它；测试注入可推进的手动时钟。**耗时测量**仍用真实 `Instant::now()` |
| `Diagnostics`（`diag.rs`） | 诊断去处：stderr 或内存缓冲（按动作分组） |
| `last_plan: HashMap<PageIndex, PlanStamp>` | 每帧每页的画法快照（无条件记录），`[画法]` 边沿触发与回放断言共用 |
| `KeyedStore::iter()` | 把 egui 的 `TextureId` 反查成 `RenderKey`（回放器据此判断"哪些格子是清晰层"） |
| `view::region_device_rect` | 预览区域的像素矩形（唯一来源：拼图与断言共用） |

## 10. 已知债务

* `app.rs` 已拆成 `app/` 子模块（~0.6k 行只剩状态/构造/入口）；`view.rs` 仍有 ~4.2k 行，拆分可行性见 [roadmap.md](roadmap.md)。
 新逻辑优先落 `view.rs`；`app.rs` 拆分见 [roadmap.md](roadmap.md)。
* 上传与像素转换没有字节预算：`MAX_UPLOADS_PER_FRAME` 按**页**计，一页可能一次传几十块。
* 纹理层按条目数限额（128），没有显存预算。
* "决策 → 绘制"没有自动化测试（见 [`testing.md`](testing.md) §5）。
