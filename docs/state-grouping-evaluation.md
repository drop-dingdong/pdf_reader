# Phase 3 评估：`PdfReaderApp` 状态分组

> 状态：2026-11-07 评估；**RuntimeState、SaveFlowState、MarkdownState、
> AnnotationPdfState 四组均已实施**。
> 当前行为以 [architecture.md](architecture.md) / [app-ui.md](app-ui.md) 为准。

## 1. 结论摘要

`PdfReaderApp` 当前约 **118 个字段**（RuntimeState 14 个、SaveFlowState 14 个、
MarkdownState 9 个、AnnotationPdfState 8 个），
但 `app.rs` 已经拆成 20 多个子模块；
真正的维护成本不在“单文件太长”，而在：

- PDF 对象号/读回/内联/写回簿记字段散落在注释、保存、文档和浮窗逻辑里；
- Markdown/数学/图片缓存有 10 多个字段，和浮窗/正文编辑器耦合；
- 保存/关闭/另存为/整理副本的对话框状态各自成组，但没有命名边界；
- 大量测试直接访问 `app.floats` / `app.annotations` / `app.custom_previews`。

**建议：只做“目标分组”（Runtime / SaveFlow / Markdown / AnnotationPdf 四组），
不做全量多级状态树。** 四组目标分组已全部完成。
全量分组的收益主要是字段数量好看，代价是大量 `self.` 路径、借用冲突和测试改动；
当前不划算。

如果继续，推荐顺序：

1. `RuntimeState`（诊断/时间/上一帧观察值）——最低风险；
2. `SaveFlowState`（另存为/整理副本/关闭确认）——低风险；
3. ~~`MarkdownState`（Markdown/数学/图片缓存）——已完成；~~
4. `AnnotationPdfState`（对象号/读回/内外注释簿记）——中等偏高，测试最多。

`session`、`floats`、`annotations`、`custom_previews`、`settings` 这些热点字段
**保持顶层**，避免把最频繁的借用路径藏进子结构。

## 2. 现状

| 指标 | 当前 |
| --- | --- |
| `PdfReaderApp` 字段 | 约 118（Runtime 14 + SaveFlow 14 + Markdown 9 + AnnotationPdf 8） |
| `app.rs` | 状态/构造/`App::ui` 入口，约 0.6k 行 |
| `app/*` 子模块 | 约 16.7k 行（含 tests/replay） |
| 最大业务模块 | `floats.rs` ~1.5k、`model.rs` ~1.1k、`dialogs.rs` ~0.8k |
| 测试 | `./scripts/check.sh` 523 个，回放基线 10 个 |
| 字段访问 | `message` 117、`floats` 77、`custom_previews` 50、`annotations` 50、`diag` 40… |

`app.rs` 的模块表已经按职责拆开，但状态仍是一个平面结构；`use super::*` 让
任何子模块都能直接读写任意字段。字段数量本身不是 bug，问题是：

- 读代码时无法从 `self.xxx` 看出它属于哪个生命周期；
- 新增功能容易把“会话内缓存 / 持久化数据 / 纯 UI 临时值”混在一起；
- 关闭/保存/迁移时，需要人工确认哪些字段该清空。

## 3. 字段聚类

### 3.1 文档与渲染核心（约 25 个）

`session`、`navigation`、`textures`、`band_since`、`last_plan`、`commands`、`events`、
`visible`、`last_visible`、`document_path`、`pdf_file_stamp`、`outline_rows`、
`link_marks`、`pending_view_restore`、`sidecar_migrated`、`sidecar_locked`、
`pdf_marked_annotations`、`reopening_after_write`、`pending_open` 等。

特点：生命周期跟文档；`session` 是外部 `DocumentSession`，不适合再包一层。
**保持顶层。**

### 3.2 注释与 PDF 簿记（约 24 个）

`annotations`、`external_ids`、`opened_pdf_refs`、`opened_pdf_bodies`、
`opened_id_by_ref`、`inline_refs`、`opened_inline_bodies`、`pdf_ref_ids`、
`pdf_dirty`、`saved_pdf_revision`、`history`、`hovered_annotation`、
`annotation_tool`、`pending_annotate`、`ink`、`ink_move`、`move_ink_armed`、
`note_pin`、`note_dialog`、`markup_color/opacity`、`ink_color/width`、
`tool_before_annotation`、`selection_started_on_text`。

`annotations` 是热点（测试/浮窗/侧栏都直接用），保持顶层；
`opened_*` / `pdf_ref_ids` / `inline_*` / `external_ids` / `pdf_marked_annotations`
可以组成 `AnnotationPdfState`。

### 3.3 浮窗、preview、截图（约 30 个）

`floats`、`next_float_id`、`dest_marks`、`capture`、`hover_tip`、`hover_link_size`、
`scroll_quiet_until`、`highlight_float`、`click_highlight`、`custom_previews`、
`next_preview_id`、`custom_pin`、`custom_dialog`、`last_preview_resize`、
`last_dest_resize`、`editing_title`、`editing_body`、`custom_hover`、
`canvas_rect`、`canvas_layer`。

`floats` / `custom_previews` 是热点，保持顶层；`capture` + `custom_pin` +
`custom_dialog` + resize 合并窗口可以组成 `PreviewFlowState`，但它们和
`floats`/`dest_marks` 的借用交织，收益不如其它组明显。

### 3.4 Markdown / 数学 / 图片缓存（约 12 个）

`markdown_floats`、`hover_preview_markdown`、`hover_markdown_cache`、
`hover_markdown_index`、`markdown_caches`、`markdown_viewports`、
`markdown_prepared`、`markdown_rendered`、`math_cache`、`math_recheck_pending`、
`images`、`body_edit_session`、`body_edit_cursor`。

这组是“会话内派生缓存”，生命周期清晰、跨模块访问相对集中，适合
`MarkdownState`。注意 `markdown_floats` / `hover_preview_markdown` 是设置值，
也可以留在 `SettingsState`。

### 3.5 保存、关闭、对话框（约 18 个）

`save_as`、`save_as_error`、`save_as_write_original`、`save_as_write_target`、
`save_as_overwrite`、`compact_dialog`、`compact_error`、`pending_in_place`、
`in_place_error`、`pending_action`、`pending_edit_action`、`pending_action_error`、
`closing_after_decision`、`pending_save`、`password_for`、`password_input`、
`pending_external_link`、`math_help`。

适合 `SaveFlowState`（或 `DialogsState`），但 `pending_external_link` 属于
链接确认，可留在链接/注释组。

### 3.6 视图/侧栏/搜索（约 25 个）

`fullscreen`、`reveal_toolbar`、`horizontal_layout`、`horizontal_scroll`、
`pending_layout`、`sidebar_width`、`show_outline`、`sidebar_tab`、
`annotation_show_*`、`annotation_only_*`、`annotation_view`、`annotation_search`、
`collapsed_annotation_pages`、`annotation_focus`、`context_annotation`、
`context_menu_pos`、`search_open`、`search_input`、`page_input`、`hits`、
`active_hit`、`zoom_mode`、`page_edit`、`zoom_edit`、`focus_edit`、
`focus_body_edit`、`focus_search`、`show_links`、`show_dest_points`、
`show_annotations`、`pin_hover_enabled`。

分布很广，且大量是 egui 当帧状态；不建议现在整体打包。

### 3.7 设置/运行时（约 20 个）

`settings`、`settings_path`、`settings_dirty`、`pending_window_size`、
`allow_external_annotation_links`、`document_load_token`、
`backup_before_inplace`、`diag`、`clock`、`started_at`、`last_report`、
`last_convert_ms`、`last_upload_ms`、`preview_until`、`last_zoom`、
`last_rotation`、`drag_started_at`、`last_pan_at`、`last_visible`、
`budget_warned`、`last_retain`、`message`、`error`。

`RuntimeState` 适合放诊断/时间/上一帧观察值；`settings` 保持顶层；
`message` / `error` 也可以留在顶层，避免日志路径改动过大。

## 4. 方案比较

| 方案 | 内容 | 收益 | 风险/成本 |
| --- | --- | --- | --- |
| A. 维持现状 | 不改 | 零回归 | 字段继续增长；新功能继续往平面结构里塞 |
| B. 目标分组（推荐） | 4 个子结构：Runtime / SaveFlow / Markdown / AnnotationPdf | 约减 40–50 个顶层字段；生命周期清晰；改动集中 | 3–5 天；主要是机械路径/测试改动；借用风险中等 |
| C. 全量多级状态 | 10+ 子结构，所有字段归组 | 顶层字段最少 | 1–2 周；借用/测试/模块边界大改；收益主要是形式 |

### 4.1 推荐目标结构

```rust
pub struct PdfReaderApp {
    // 热点/生命周期核心，保持顶层
    session: Option<DocumentSession>,
    navigation: Option<Arc<NavigationIndex>>,
    annotations: AnnotationStore,
    floats: Vec<Float>,
    custom_previews: Vec<CustomPreview>,
    settings: crate::settings::Settings,

    // 目标分组
    runtime: RuntimeState,          // 诊断、时钟、上一帧观察值
    save_flow: SaveFlowState,       // 另存为/整理/关闭确认
    markdown: MarkdownState,        // Markdown/数学/图片缓存
    annotation_pdf: AnnotationPdfState, // 对象号/读回/内外注释簿记（已实施）
}
```

不要用 `Deref`/`DerefMut` 把组“伪装成”扁平字段；显式 `self.runtime.diag`
能让借用边界和生命周期可见。

### 4.2 不建议现在动的字段

- `session`：外部类型，很多方法以参数传入，包一层没有收益；
- `floats` / `annotations` / `custom_previews`：热点 + 测试直接访问；
- `settings`：已经是一个独立结构；
- `message` / `error`：被几乎所有模块写，保持简单。

## 5. 借用与测试风险

### 5.1 借用

当前代码已经有一些规避借用冲突的手法：

- UI 方法把 `session: &mut DocumentSession` 单独传入；
- `finish_canvas_frame` 用 `std::mem::take(&mut self.floats)` 避免同时借 `floats` 和 `self`；
- `markdown`/`images` 在 `draw_floats` 里以字段形式访问。

分组后：

- 两个**不同组**的字段仍可同时可变借用（Rust 字段级借用）；
- 但“组 + `session` + 另一个组”的方法如果写成 `&mut self`，会冲突；
  需要改成自由函数或把方法挪到子结构上；
- 不要为了消除冲突引入 `RefCell`；会把编译期错误变成运行时错误。

### 5.2 测试

大量测试直接写：

```rust
app.annotations...
app.floats...
app.custom_previews...
app.markdown_floats...
```

分组后这些路径要么机械更新，要么保留顶层热点字段。建议先做
`RuntimeState` / `SaveFlowState`（测试访问少），把 `MarkdownState` /
`AnnotationPdfState` 放后面。

### 5.3 回归信号

- `./scripts/check.sh`（523 测试）必须全绿；
- 10 个回放基线必须一致；
- 不新增 `unsafe`、不引入 `Deref`、不改 PDF 写回语义；
- 每个组独立提交，便于 `git revert`。

## 6. 迁移步骤（若采纳 B）

1. ~~`RuntimeState`：只加结构 + 搬诊断/时钟/上一帧观察值；更新测试并提交。~~
   已完成：`app/state.rs`，顶层字段 159 → 146，523 测试通过。
2. ~~`SaveFlowState`：另存为/整理/关闭确认；确认 `Settings` 序列化不变。~~
   已完成：`app/state.rs`，顶层字段 146 → 133，523 测试通过。
3. ~~`MarkdownState`：搬 Markdown/数学/图片缓存，保留 `markdown_floats` 等设置值在顶层；
   处理 `ensure_markdown_image_owners` 的借用。~~
   已完成：`MarkdownState`，顶层字段 133 → 125，523 测试通过。
4. ~~`AnnotationPdfState`：对象号/读回/内联/`pdf_ref_ids`；`merge_readback`、
   `pdf_ref_changes` 相关测试全部跑。~~
   已完成：`AnnotationPdfState`，顶层字段 125 → 118，523 测试通过。
5. 每步只做字段移动，不顺手改行为；文档随该步更新状态。

## 7. 决策与触发条件

**当前建议：四组目标分组已完成；不做全量状态树。**
后续只在字段继续增长、或出现“关闭/保存漏清缓存”类 bug 时重新评估。

触发条件：

- 新功能需要同时理解 4 个以上字段簇，且找不到入口；
- 出现“关闭/保存时漏清某个缓存字段”的实际 bug；
- `app.rs` 字段继续增长到 ~200，或 `use super::*` 的跨模块访问明显增加；
- 用户/维护者明确希望降低顶层字段数量。

不做全量的理由：

- 模块边界已经存在，当前主要成本是**语义分散**，不是文件长度；
- 全量分组会带来大量机械 diff，回归风险高于收益；
- 热点字段仍需顶层，最终结构不会变成“几个完美对象”。

## 8. 附：字段清单来源

字段定义见 [`crates/app/src/app.rs`](../crates/app/src/app.rs) 的 `pub struct PdfReaderApp`；
模块划分见 [architecture.md](architecture.md) §3 与 [app-ui.md](app-ui.md) §1。
本文的计数以当前工作树为准，后续字段变化时以代码为准。
