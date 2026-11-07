# 架构

本文只描述现状与仍生效的约束。历史方案与逐条决策在 git 历史里。

## 1. 目标与边界

本地单文件 PDF 阅读器 + **注释写回**：

- 阅读：连续/横向滚动、缩放、旋转、单页/连续、选字复制、搜索、大纲、链接、引用点、浮窗截图、
 全屏、最近文件与阅读位置恢复。
- 注释：高亮 / 下划线 / 删除线 / 墨迹 / 便签；正文、作者、时间、颜色/透明度/线宽；
 撤销/重做；写入 PDF 后其它阅读器可见；从 PDF 读回。
- 自定义数据：preview（文字/图片）、链接、被引用点尺寸 → 同目录 `<pdf>.json`。
- 整理副本：去历史修订、生成更小 PDF；不覆盖源文件；拒绝签名/加密。
- 设置：`settings.json`（全局 UI、注释默认样式、每文件阅读状态、保存选项）。

不做：云端/账号、通用 PDF 编辑、把外部注释改成自有注释、旧格式迁移。

## 2. crate 与依赖方向

```text
    app（egui 外壳：胶水 + 纯逻辑 + 持久化/写回编排）
     │ 依赖
     ▼
    pdfcore（trait + 领域模型，零依赖，forbid(unsafe_code)）
     ▲
     │ 实现
  backend-pdfium（PDFium 适配；写回/整理用自带 FFI，只在子进程）
```

| crate | 角色 |
| --- | --- |
| `pdfcore` | 9 个 trait、领域类型、`DocumentSession`、几何/布局/分块/波段/缓存/调度/文本；`testkit` 假后端 |
| `backend-pdfium` | 文档几何、导航索引、渲染、字符级文本；`pdf_index`/`readback` 解析注释；`pdf_patch` 追加对象；`writeback`（子进程 FFI）；`pdf_compact` |
| `app` | `view`（纯逻辑，可测）+ `app`（egui 胶水、状态机）+ `persist`/`settings`（存档与设置）+ `export`/`writeback`/`compact`（写回与整理编排）+ `annotations_md` + `image_store` + `markdown`/`math` + `replay` |

## 3. 模块地图（按职责）

### pdfcore
- `geometry`：`Point`/`Rect`/`Size`/`Vec2`/`Rotation`/`Insets`，**坐标约定唯一实现处**。
- `layout`：`PageLayout`/`VerticalLayout`/`HorizontalLayout`/`PagePlacement`/`Viewport`/`ScrollAnchor`。
- `render`/`tiling`/`scheduler`/`cache`：渲染契约、渲染键、块几何、波段、后台调度、位图 LRU。
- `selection`/`text`：字符级文本、选区、搜索。
- `session`：命令进、事件出；`annotation`：标注模型与 `AnnotationStore`。
- `store`/`testkit`：少量持久化接口、内存假后端。

### backend-pdfium
- `document`/`raster`/`text`：PDFium 适配；`raster` 保证“块渲染 == 整页裁剪”。
- `pdf_index`：页面/Annots/对象号；`readback`：读注释（几何、颜色、正文、作者、时间）。
- `pdf_patch`：对象级增量（追加注释对象、正文更新、删除、内联注释更新）。
- `writeback`：写回任务编排；裸 FFI 在**子进程** `--writeback` 里执行。
- `pdf_compact`：xref 链解析 + 活对象表 + 重建 xref stream（`--compact` 子进程）。

### app
- `view.rs`：坐标换算、输入映射、`KeyedStore`、画法决策 `plan_page`、快照拼合、浮窗几何、
 摘要文案；**能脱离窗口验证的逻辑一律在这里**（当前 ~4.2k 行）。
- `app.rs`（~0.6k）：`PdfReaderApp` 状态/构造 + `eframe::App::ui` 转发到 `app::frame::frame`。
- `app/model.rs`（~1.0k）：`app` 子模块共用类型/常量/纯函数（`Float`/`AppEdit`/`Tool`/
  `CaptureState` 等；字段 `pub(super)`）。
- `app/state.rs`：`RuntimeState`（诊断/时钟/上一帧观察值）、`SaveFlowState`
  （另存为/整理/关闭确认/待保存）与 `MarkdownState`（Markdown/数学/图片缓存）。
  三组都不持久化。
- `app/frame.rs`（~0.5k）：每帧编排（命令采集、pump、布局切换、纹理上传、绘制顺序）。
- `app/canvas/`：`mod.rs` 编排与输入；`paint.rs` 页面/选区/链接/标注/引用点；
  `interact.rs` 点击/拖动/右键/帧尾浮窗。画布内容的原始命中先按 canvas 裁剪。
- `app/floats.rs` 还负责 `pd-float://` 的复制/解析/打开（契约见
  [float-links.md](float-links.md)）与指针归属：`topmost_float_at`（z 序最上层浮窗）与
  `canvas_hover_allowed`（面板/画布本层之外的窗口/弹层/拖拽/浮窗排除，画布层由
  `canvas_layer` 记录）与 `pointer_over_overlay`（比较画布层，识别编辑窗口/菜单/模态）；每个浮窗还有一个只感知 drag 的全 rect 遮罩，
  防止拖动漏到下层浮窗或画布。
- `app/{toolbar,sidebar,hover,body_editor,dialogs,save,document,annotate,modes,capture,edit,textures,paint}.rs`：
  面板、浮窗、正文编辑器、保存/打开、标注与模式、截图、纹理、无状态绘制辅助。
- `app/tests.rs`：原 `app.rs` 内联测试（含浮窗链接/指针归属/注释 id 稳定性）。
- `markdown.rs`：浮窗 Markdown 预处理（单换行→硬换行、最小内容宽度测量）。
- `math.rs`：LaTeX `MathBackend` trait、SVG（PNG 可回退）后端、公式纹理缓存与错误类型。
- `persist.rs`：`.pdf.json` 手写 JSON + 版本迁移（未知字段不保留）。
- `settings.rs`：`settings.json`（缺字段默认、坏文件改名 `.bad-*`、原子写）。
- `export.rs`：写回 job 组装、`--writeback` 子进程调用、校验与原子改名。
- `writeback.rs`：子进程侧 `--writeback` job 执行。
- `compact.rs`：`--compact` 整理副本与校验。
- `image_store.rs`：内部图片 PNG 缓存与 `pd-img` loader。
- `history.rs`：撤销栈（带 `affects_pdf` 标记的修订号）。
- `annotations_md.rs`：注释摘要（Markdown）。
- `json.rs`：极简 JSON 解析/序列化。
- `clock.rs` / `diag.rs`：可注入时钟、诊断去处。
- `ink.rs`：墨迹手势状态机。
- `replay.rs`：脚本驱动真实 `App::ui`，逐帧采集 + 断言 + 结构基线。

## 4. 运行时数据流

### 4.1 打开文档

```text
app::open → backend.open(Source) → OpenedDocument{document,raster,text,capabilities}
     → DocumentSession::new(...)
       · document.page_geometries() ← 一次性批量表
       · readback::read(path)    ← 注释（几何/正文/作者/时间）
       · pdf_index::page_annotations ← 对象号配对
       · merge_readback / reconcile_history_refs
       · load_sidecar(<pdf>.json)  ← preview/链接/引用点
```

### 4.2 每帧（UI 线程）

```text
collect_commands → session.handle(命令…)   // 滚动/缩放/翻页/选择…
apply_and_pump  → scheduler.drain_events → cache.insert → SessionEvent
ensure_textures → 上传整页/块纹理 + 按波段裁剪
draw_*      → 布局矩形 + TileGrid 摆放绘制
RequestVisible  → 提交可见页/预取页渲染任务
```

### 4.3 一次渲染

```text
DocumentSession::request_page/request_tiles/request_region
 → RenderRequest{page, scale=渲染比例, rotation, tile, region, priority}
 → WorkerScheduler（PDFium 串行）
 → PdfiumRaster::render
 → RenderEvent::Rendered{key,image} → pump → RenderCache.insert
```

取消只在出队前生效；**取消必须补发 `RenderEvent::Cancelled`**。

### 4.4 注释编辑与撤销

```text
输入 → AppEdit（Add/Delete/Restyle/Move/SetBody/…）
   → apply_edit：改 AnnotationStore + history.push_tagged(affects_pdf)
   → refresh_dirty：PDF 修订号 vs 保存点
```

- 只有前五类 `affects_pdf`；preview/引用点编辑只影响 `.json` 脏位。
- 样式/几何编辑写回时需要重建对象：`pdf_ref = None`；旧对象由 `deletes` 清理。

### 4.5 保存 / 另存为 / 覆盖保存

统一入口 `write_document(WriteRequest { target, allow_overwrite, allow_conflict,
write_source_data, write_target_data })`：

```text
取快照（新注释/正文更新/删除/内联/数据）
 → 原地：写 .json（若脏）→ 子进程暂存 → 冲突复查 → 可选备份 → 原子替换 → 重开
 → 另存为：子进程写目标临时文件 → 原子替换 → 写源/目标 .json → 重开
 → 重开：读回注释 + 重新配对对象号；保留历史并 reconcile；恢复预览/视图/脏位
```

**写回 PDF 的裸 FFI 永远在子进程**（主进程有渲染线程）。注释对象使用
`/NM = pd-fr:<id>` 做幂等标记。

### 4.6 整理副本

```text
app::compact::run_child → app --compact <in> <out>
 → pdf_compact：xref 链（经典表/xref 流，/Prev、/XRefStm，PNG 预测器）
       → 活对象表（每对象号只留最后一次定义）
       → 重建 xref stream；保留对象字节与 ObjStm；去 /Linearized
 → 校验页数/注释数/首页像素；不更小不落盘；临时文件 + 原子改名
```

## 5. 坐标系

| 空间 | 原点 | y 轴 | 单位 | 谁换算 |
| --- | --- | --- | --- | --- |
| PDF 页面空间 | 页面左下角 | 向上 | point | 后端原始值 |
| 内容空间 | 文档左上角 | 向下 | 设备无关像素 | `PagePlacement` |
| 视图空间 | 视口左上角 | 向下 | 设备无关像素 | UI（减滚动量） |
| 渲染像素 | 页位图左上角 | 向下 | 像素 | `TileGrid` |

约束：
- 只有 `PagePlacement` 做 page ↔ content 换算；
- 旋转先于缩放；`Rotation::map_point` 在 0°/180° 也包含 y 翻转；
- 先减 `PageGeometry.origin` 再换算；
- **注释几何一律存未旋转页面空间**（便签 `/Rect`、文本 quads、墨迹点、线宽都是页面空间）。

## 6. 线程与 FFI

| 谁 | 跑什么 | 约束 |
| --- | --- | --- |
| UI 线程 | `handle/pump`、纹理上传、绘制 | 不直接碰后端 |
| 工作线程 ×N | `PageRaster::render` | `N = capabilities.max_concurrency`，PDFium 为 1；按 `RenderKey` 去重 |
| 子进程 | `--writeback` / `--compact` | 裸 FFI 不与渲染线程同进程；主进程只做编排/校验 |

## 7. 持久化分工

| 数据 | 位置 | 规则 |
| --- | --- | --- |
| 注释（几何/正文/作者/时间） | **PDF 注释对象** | `/NM`、`/Contents`、`/Rect`、`/QuadPoints`、`/AP`、`/T`、`/M`、`/CreationDate` |
| preview/链接/引用点尺寸 | `<pdf>.json` | 手写 JSON，版本迁移，未知字段不保留 |
| 阅读状态/最近文件/UI/默认样式/保存选项 | `settings.json` | 全局，缺字段默认，坏文件改名 `.bad-*` |
| 旧 `.annot.json` | — | **已完全不读不写**；遗留文件忽略 |

## 8. 不变量清单（改代码前先看）

| # | 不变量 | 守它的测试/位置 |
| --- | --- | --- |
| INV-1 | 只有 `PagePlacement` 做 page↔content 换算 | `layout` 单测 |
| INV-2 | 后端只产出 PDF 页面空间；文本/链接减渲染盒原点 | `real_pdf.rs` |
| INV-3 | 一页一套网格；块边界整数、相邻严格相接 | `tiling::tests` |
| INV-4 | 渲染键比例 = 真正用于渲染的比例（波段上界） | `zooming_inside_a_band…` |
| INV-5 | 位图尺寸唯一来源：`TileGrid::render_px`/`tile_px` | `tiling_policy…` |
| INV-6 | 块渲染 == 整页裁剪（逐像素），含 `/Rotate` | `tiles_match_the_crop…` |
| INV-7 | `u=显示/渲染` 作用在摆放上 | `tile_view_rect_uses…` |
| INV-8 | 画法整页级降级，不逐块混比例 | `view::tests::plan_page_*` |
| INV-9 | 预览图不存像素，只存元数据+分辨率 | `persist::tests`、`region_render…` |
| INV-10 | UI 线程不直接调后端；渲染只经调度器 | 结构约束 |
| INV-11 | 取消必须补发 `Cancelled` | `scheduler::tests` |
| INV-12 | 页面 `/Rotate` 只应用一次（减去 intrinsic） | `intrinsic_rotation_is_applied_exactly_once` |
| INV-13 | `encrypted` 只在 security handler 非 Unprotected 时为真 | `unencrypted_documents…` |
| INV-14 | 页面位图不含 PDF 注释（注释只由应用叠层绘制） | `annotations_are_not_baked…` |
| INV-15 | 注释几何存未旋转页面空间；旋转只在显示/写回边界 | `readback`/`writeback` 测试 |
| INV-16 | 写回幂等：`/NM = pd-fr:<id>`；同一 id 多份以最后一份为准 | `annotation_meta.rs`、`merge_readback` 测试 |
| INV-17 | `pdf_ref = None` 表示“下次保存重建对象”；样式/几何快照写回重开后都按重建 | `undo_after_writeback_rebuilds_moved_ink` |
| INV-18 | 脏位 = PDF 修订号 vs 保存点；preview 编辑不推进 PDF 修订号 | `redo_back_to_the_save_point_is_clean`、`preview_edits…` |
| INV-19 | 设置恢复顺序：`GoToPage` → `SetZoom` → 可选 `SetScroll(anchor)` | `settings` 单测 + 人工验收 |
| INV-20 | 原地写入前检查 `pdf_file_stamp`；只写 `.json` 不检查 | `external_change_is_detected…` |
| INV-21 | 整理副本不覆盖源文件；签名/加密拒绝；不更小不落盘 | `tests/compact.rs` |
| INV-22 | 外部注释只读几何/颜色；改正文/删除可以写回 | `readback`/UI 人工验收 |
| INV-23 | Markdown 只影响文字浮窗的显示：不改 `Float.body`、不改源文本/持久化/写回；LaTeX 纹理只是临时缓存；滚轮不穿透浮窗；外部链接不打开浏览器 | `markdown`/`math` 单测 + 浮窗人工验收 |
| INV-24 | 浮窗链接“要么正确、要么断链”：内部注释 id 不复用且除删除外的编辑保持 id；preview 用稳定记录 id；外部注释仅会话 token/校验后可用；不可验证时提示而不按 id 猜 | `float_link_tests`、`annot_roundtrip`、人工验收 |

## 9. 架构层面的已知限制

- 纹理层按条目数限额，没有显存字节预算；上传/像素转换没有字节预算。
- `app.rs` 已拆成 `app/` 子模块（~0.6k 行只剩状态/构造/入口）；`PdfReaderApp` 仍有
  约 125 个字段，`RuntimeState`、`SaveFlowState`、`MarkdownState` 已分组；其余 Phase 3 评估见
  [state-grouping-evaluation.md](state-grouping-evaluation.md)（AnnotationPdf 未实施）；`view.rs` 仍有 ~4.2k 行
  （拆分的可行性评估见 [roadmap.md](roadmap.md)）。
- 无头回放只覆盖主视图渲染主线；UI 对话框、浮窗、便签、保存提示依赖人工验收。
- `.pdf.json` 不保留未知字段；新旧版本互读会有字段丢失（只做加法迁移，不做无损往返）。
- 性能基线只能在同类机器上比较，未接 CI。
