//! egui/eframe 外壳：把 [`DocumentSession`] 的"命令进、事件出"模型接到界面上。
//!
//! ## 本模块的定位
//! **只做胶水**：坐标换算、输入映射、纹理存储都在 [`crate::view`] 里（可单测）。
//! 剩下的部分需要真实窗口才能验证，按 agent.md 规则 2 **交由用户验收**。
//!
//! ## 每帧流程（与 `docs/architecture.md` §4.2 的契约一致）
//! 1. 采集输入 → 命令；2. `handle` 命令；3. `pump` 取回渲染结果；
//! 4. 画页面（就绪则贴纹理，未就绪则白底页框）；5. 画高亮；6. 请求可见页渲染。
//!
//! ## 与 egui 0.37 的适配
//! 0.37 的 `eframe::App` 只提供 `fn ui(&mut self, ui: &mut Ui, frame)`（不再给 `Context`），
//! 侧边/上下栏统一为 [`egui::Panel`]，因此本模块从 `ui.ctx()` 取 `Context` 做纹理与剪贴板操作。

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use egui::{
    Color32, CornerRadius, Id, Key, Modifiers, RichText, Sense, Stroke, StrokeKind, Vec2 as EVec2,
};
use pdfcore::annotation::{
    Annotation, AnnotationId, AnnotationKind, AnnotationStore, INK_COLOR, INK_OPACITY,
};
use pdfcore::prelude::*;

use crate::ink::InkStroke;
use crate::persist;

mod annotate;
mod body_editor;
mod capture;
mod dialogs;
mod document;
mod edit;
mod floats;
mod hover;
mod modes;
mod paint;
/// 无窗口回放器（脚本驱动真实 `App::ui`，用于回归断言与性能基线，见 D-103）。
pub mod replay;
mod save;
mod sidebar;
mod textures;
mod toolbar;

use crate::view::{
    body_size_changed, body_size_of, canvas_rect_for, command_for_key, command_for_scroll,
    command_for_zoom_delta, commands_for_destination, link_box_color, link_overlay_visible,
    link_rect_in_page_bounds, needs_underlay, outline_node_count, page_rect_to_screen, plan_page,
    tile_view_rect, view_rect_for, view_rect_to_page_rect, wheel_targets_canvas_with_float,
    Desired, DrawInput, KeyedStore, PageDraw, PlanStamp, Preview, FLOAT_TITLE_HEIGHT,
};
use paint::*;

/// 纹理层的**字节**预算（软上限，D-111）。
///
/// 256 MiB 的来历：本机 12 个回放脚本实测峰值 9–52 MB（A4 家庭）；
/// 极端情形（带鱼屏 5120×1440 + A0 在 800%）估算"当前视口"一类就要 68–206 MB。
/// 软上限的语义：**当前视口所需的那一类永不淘汰**，超出只打一行提示；
/// 只有"底料 / 旧内容"这些可再生的纹理才受它约束。
/// 可用 `PDF_READER_TEXTURE_BUDGET_MB` 覆盖（0 = 不限制）。
pub const TEXTURE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// 注释列表的视图：按页分组 / 最近修改（平铺，按修改时间倒序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AnnotationListView {
    /// 按页分组、页内自上而下（阅读顺序）。
    #[default]
    ByPage,
    /// 平铺，按修改时间倒序；没有时间的排最后。
    Recent,
}

impl AnnotationListView {
    fn label(self) -> &'static str {
        match self {
            Self::ByPage => "按页",
            Self::Recent => "最近修改",
        }
    }
}

/// 注释列表的一行（渲染前先收集、排序，避免边遍历边改 UI）。
struct AnnotationRow {
    page: PageIndex,
    id: AnnotationId,
    label: &'static str,
    readonly: bool,
    external: bool,
    summary: String,
    preview: String,
    author: Option<String>,
    created: Option<i64>,
    modified: Option<i64>,
    bounds: Option<Rect>,
}

/// 注释列表搜索：正文或作者包含查询词（忽略大小写）。
fn annotation_matches_query(annotation: &Annotation, query: &str) -> bool {
    let needle = query.to_lowercase();
    annotation
        .body
        .as_deref()
        .is_some_and(|body| body.to_lowercase().contains(&needle))
        || annotation
            .author
            .as_deref()
            .is_some_and(|author| author.to_lowercase().contains(&needle))
}

/// 按页视图：页号 → 页内自上而下 → 左 → id。
fn by_page_order(a: &AnnotationRow, b: &AnnotationRow) -> std::cmp::Ordering {
    a.page
        .get()
        .cmp(&b.page.get())
        .then_with(|| match (a.bounds, b.bounds) {
            (Some(left), Some(right)) => right
                .y
                .partial_cmp(&left.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    left.x
                        .partial_cmp(&right.x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }),
            _ => std::cmp::Ordering::Equal,
        })
        .then_with(|| a.id.get().cmp(&b.id.get()))
}

/// 最近修改视图：修改时间倒序（没有时间的排最后），再按页号稳定排序。
fn recent_order(a: &AnnotationRow, b: &AnnotationRow) -> std::cmp::Ordering {
    b.modified
        .unwrap_or(i64::MIN)
        .cmp(&a.modified.unwrap_or(i64::MIN))
        .then_with(|| a.page.get().cmp(&b.page.get()))
        .then_with(|| a.id.get().cmp(&b.id.get()))
}

/// 注释列表里的类型名；`visible` 恒为 true，预留给将来不可列类型。
fn annotation_kind_label(kind: &AnnotationKind) -> (&'static str, bool) {
    match kind {
        AnnotationKind::Highlight { .. } => ("高亮", true),
        AnnotationKind::Underline { .. } => ("下划线", true),
        AnnotationKind::StrikeOut { .. } => ("删除线", true),
        AnnotationKind::Ink { .. } => ("墨迹", true),
        AnnotationKind::Note { .. } => ("便签", true),
    }
}

/// 悬停预览：压平全部空白，最多 280 字。
fn body_tooltip(body: Option<&str>) -> String {
    let Some(body) = body else {
        return "无正文".to_string();
    };
    let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return "无正文".to_string();
    }
    let mut out: String = text.chars().take(280).collect();
    if text.chars().count() > 280 {
        out.push('…');
    }
    out
}

/// 正文摘要：压平空白、只取首行；没有正文时给明确占位。
fn body_summary(body: Option<&str>) -> String {
    let Some(body) = body else {
        return "无正文".to_string();
    };
    let line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "无正文".to_string()
    } else {
        collapsed
    }
}

/// 文字浮窗是否应提供正文编辑入口（空正文同样需要）。
fn text_float_is_editable(owner: FloatOwner, content: &FloatContent) -> bool {
    matches!(content, FloatContent::Text(_)) && !matches!(owner, FloatOwner::None)
}

/// 当前 Unix 秒（UTC）。
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// 注释摘要的默认路径：`<stem>.annotations.md`，已存在则递增 `-2`、`-3`…（不覆盖）。
fn annotation_summary_path(pdf: &std::path::Path) -> PathBuf {
    let stem = pdf
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".to_string());
    let mut index = 1u32;
    loop {
        let suffix = if index == 1 {
            String::new()
        } else {
            format!("-{index}")
        };
        let candidate = pdf.with_file_name(format!("{stem}.annotations{suffix}.md"));
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}

/// 设置里记录文件用的键：能规范化就规范化（相对路径 → 绝对路径）。/// 设置里记录文件用的键：能规范化就规范化（相对路径 → 绝对路径）。
fn settings_key(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 「另存为」的提示位；条件集中在这里，便于按真值表测试。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SaveAsWarnings {
    /// 新文件不会写入数据文件。
    new_missing_data: bool,
    /// 未保存数据未归档到新文件（但原文件已写）。
    unsaved_new: bool,
    /// 两个数据文件都不写。
    no_data_file: bool,
    /// 新文件写了，但原数据文件不更新。
    original_stale: bool,
}

fn save_as_warnings(
    write_original: bool,
    write_target: bool,
    data_dirty: bool,
    has_custom_data: bool,
) -> SaveAsWarnings {
    SaveAsWarnings {
        new_missing_data: !write_target && has_custom_data,
        unsaved_new: data_dirty && !write_target && write_original,
        no_data_file: data_dirty && !write_original && !write_target,
        original_stale: data_dirty && !write_original && write_target,
    }
}

/// 样式菜单里的颜色块。
fn color_swatch(ui: &mut egui::Ui, name: &str, rgb: [u8; 3], selected: bool) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(name).color(Color32::BLACK))
            .fill(Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
            .selected(selected)
            .min_size(EVec2::new(26.0, 20.0)),
    )
}

/// 一个纹理键"应当占多少字节"（**按几何算**，不看位图是否存在）。
///
/// 整页 = 渲染像素 × 4；块 = 该渲染比例下网格里的块像素 × 4。
/// 这样预算决策与"纹理现在有没有"无关，不会因为上传/删除而抖动（D-112）。
fn texture_key_bytes(page_size: Size, rotation: Rotation, max_side: u32, key: &RenderKey) -> u64 {
    let scale = key.scale_milli as f32 / 1000.0;
    let size = page_size.rotated(rotation);
    let grid = TileGrid::new_bounded(size, scale, max_side);
    let (width, height) = match key.tile {
        None => {
            let (width, height) = grid.render_px();
            (width as u64, height as u64)
        }
        Some(tile) => {
            let (_, _, width, height) = grid.tile_px(tile.col, tile.row);
            (width as u64, height as u64)
        }
    };
    width.saturating_mul(height).saturating_mul(4)
}

/// 是否显示"验收辅助入口"（截图 / 测试浮窗）。
///
/// 默认关闭：它们只服务于手工验收，不该占正式界面；设 `PDF_READER_DEV_ENTRY=1` 打开。
/// 用开关而不是删代码/加 `allow(dead_code)`：代码保持可编译、可调用，随时能用（D-114）。
fn dev_entries_enabled() -> bool {
    std::env::var_os("PDF_READER_DEV_ENTRY").is_some()
}

/// 从环境变量 `PDF_READER_TEXTURE_BUDGET_MB` 读预算（缺省用 [`TEXTURE_BUDGET_BYTES`]）。
fn texture_budget_bytes() -> u64 {
    std::env::var("PDF_READER_TEXTURE_BUDGET_MB")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(TEXTURE_BUDGET_BYTES, |mb| mb * 1024 * 1024)
}

/// 纹理缓存容量（条目数）。真正的淘汰依据是"是否可见 + 波段"（`retain_for_view`），
/// 容量只是兜底。
///
/// 128 的来历（D-96）：保留规则改成"当前波段 + 最近其它波段 + 当前以下最近的整页波段"后，
/// 高倍下 2 个可见页 × (当前 18 块 + 上一档 18 块 + 1 张整页) ≈ 74 条；64 会开始抖，
/// 旧档的块被 LRU 挤掉后预览又缺料。单块约 0.8 MB，上限约 100 MB 显存。
const TEXTURE_CAPACITY: usize = 128;

/// 侧栏宽度范围（内容空间像素）。宽度由 app 自己保存 —— 见 `sidebar_width` 的说明。
const SIDEBAR_MIN_WIDTH: f32 = 160.0;
const SIDEBAR_MAX_WIDTH: f32 = 640.0;
/// 侧栏拖拽条的宽度（由 app 自己处理拖拽，见 `draw_sidebar` 的说明）。
const SIDEBAR_HANDLE_WIDTH: f32 = 5.0;

/// 显示链接热区框所需的最低缩放：更小的时候热区框会糊成一片，反而干扰阅读（用户要求）。
const LINK_OVERLAY_MIN_ZOOM: f32 = 0.75;
/// 自定义**文字** preview 的默认正文尺寸（用户要求：初始化为一个默认值）。
const DEFAULT_CUSTOM_TEXT_BODY: egui::Vec2 = egui::Vec2::new(300.0, 170.0);

/// 侧栏条目被点击后，浮窗高亮的持续时间（一次性动作需要一个有限时长）。
const CLICK_HIGHLIGHT: std::time::Duration = std::time::Duration::from_millis(700);

/// 注释列表点击后，中性定位轮廓的持续时间。
const ANNOTATION_FOCUS: std::time::Duration = std::time::Duration::from_millis(1200);

/// 写回前发现 PDF 被外部修改时的统一提示。
const PDF_CONFLICT_MESSAGE: &str = "PDF 已被其他程序修改，已取消写入（原文件未改动）；当前修改仍在编辑器中。请丢弃修改后重新打开，或使用“另存为”保存到新文件。";
/// 滚动/缩放之后多久内不显示悬停预览（避免滚动时预览不断闪现）。
const HOVER_SCROLL_QUIET: std::time::Duration = std::time::Duration::from_millis(250);
/// 大纲每行的固定高度（虚拟化行渲染要求等行高）。
const OUTLINE_ROW_HEIGHT: f32 = 18.0;
/// 链接热区框的颜色（空心红线，用户指定）。
const LINK_OVERLAY_COLOR: Color32 = Color32::from_rgb(220, 40, 40);

/// 标注命中的容差（**屏幕像素**；命中时按缩放换算成页面单位，否则高倍下会变成"无边框的靶子"）。
const ANNOTATION_HIT_TOLERANCE_PX: f32 = 4.0;

/// 墨迹的默认线宽（**页面空间**单位；回放动作与 S4 的墨迹工具共用）。
const DEFAULT_INK_WIDTH: f32 = 2.0;
/// 新建便签的图标边长（页面空间 pt）。
const NOTE_ICON_SIZE: f32 = 18.0;

/// 历史窗口每行的高度（虚拟化列表要求等行高）。
const HISTORY_ROW_HEIGHT: f32 = 18.0;

/// 历史窗口列表的最大高度（超出滚动；虚拟化只排版可见行）。
const HISTORY_MAX_HEIGHT: f32 = 360.0;

// 链接框颜色（红/绿）由 `view::link_box_color` 决定，见 D-116。

/// 每帧最多上传几张纹理。
///
/// 一张整页位图的 RGBA 转换 + GPU 上传在毫秒量级；一帧内连传多张就会掉帧。
/// 设上限后开销被摊到多帧，视觉上是"相邻页稍后补齐"，比"翻页顿一下"好得多。
/// 每帧最多上传多少**张**纹理。
///
/// 原来是"每帧最多 2 **页**"（一个含糊的近似）；对账循环按"缺哪些键"逐张传，
/// 所以这里换成张数：40 张 ≈ 高倍下两页的可见块数量（12~18 块/页 × 2）。
const MAX_TEXTURE_UPLOADS_PER_FRAME: usize = 40;

/// 缩放/旋转后进入"预览窗口"的时长：这段时间内不提交新渲染、只显示旧图拉伸。
///
/// 取 160ms：比人手连续滚轮的间隔（约 50–120ms）略长，因此一次连续缩放只会
/// 在最后渲染一次；又不至于让单次缩放的清晰图来得太迟。
const ZOOM_PREVIEW_WINDOW: std::time::Duration = std::time::Duration::from_millis(160);

/// 拖拽中：区分"平移画面"与"选择文字"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    /// 拖动画面（起点没有命中文字）。
    Pan,
    /// 拖选文字（起点命中了字符）。
    Select,
    /// 正在画墨迹（S4b）：采样点累积在 `ink` 里，抬笔才变成标注。
    Ink,
    /// 正在平移一条墨迹（Alt+拖拽）：拖动中直接改坐标，松手记一条历史。
    MoveInk,
}

/// Alt+拖拽墨迹的瞬时状态：只记录起点和原始点集，拖动中直接改内存坐标。
struct InkMoveState {
    /// 被移动的注释。
    id: AnnotationId,
    /// 拖动前的快照（undo 用）。
    before: Annotation,
    /// 按下时指针在页面空间的位置。
    start: Point,
    /// 原始折线（每次拖动都从它出发，避免累积误差）。
    original: Vec<Point>,
}

/// 关闭/切换文件前挂起的动作。
enum PendingActionKind {
    Quit,
    Open(PathBuf),
}

/// 有未保存改动时，关闭窗口或切换文件前先问一句。
struct PendingAction {
    kind: PendingActionKind,
}

/// 正文编辑草稿未保存时，用户又要求关闭浮窗/切换文件，先挂起该动作。
#[derive(Clone, Debug)]
enum EditDiscardAction {
    /// 关闭正文编辑窗口
    CloseEditor,
    /// 关闭指定浮窗
    CloseFloat(u64),
    /// 打开另一个文件
    Open(PathBuf),
    /// 关闭整个应用
    Quit,
}

/// 一次写回请求：保存 / 另存为 / 覆盖保存共用。
struct WriteRequest {
    /// 目标地址（等于当前文档就是原地保存）。
    target: PathBuf,
    /// 目标已存在时是否允许覆盖（UI 已确认）。
    allow_overwrite: bool,
    /// 覆盖原文件时忽略外部修改（UI 已二次确认）。
    allow_conflict: bool,
    /// 是否写原文件的数据文件。
    write_source_data: bool,
    /// 是否写目标文件的数据文件。
    write_target_data: bool,
}

/// 鼠标拖拽的语义（第二轮验收：去掉不可靠的"自动"模式，只留两种明确行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    /// 总是平移画面（默认：拖拽永远不会意外改动选区）。
    #[default]
    Pan,
    /// 总是选字（拖到空白处不产生选区）。
    Select,
    /// 画墨迹（S4b）：拖拽画一笔，**抬起才入库**；不滚页也不选字。
    Ink,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "平移",
            Self::Select => "选字",
            Self::Ink => "墨迹",
        }
    }

    /// 脚本里的名字（回放动作 `set_tool`）。
    fn from_key(key: &str) -> Option<Self> {
        match key {
            "pan" => Some(Self::Pan),
            "select" => Some(Self::Select),
            "ink" => Some(Self::Ink),
            _ => None,
        }
    }
}

/// 打开时记录的 PDF 文件身份（修改时间 + 长度），用于写回前的外部修改检测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PdfFileStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
}

fn pdf_file_stamp(path: &std::path::Path) -> std::io::Result<PdfFileStamp> {
    let metadata = std::fs::metadata(path)?;
    Ok(PdfFileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// 同目录存档的读取结果。
enum Sidecar {
    /// 没有存档 / 解析失败（都当作"没有"）。
    Missing,
    /// 可用（可能刚从旧版本升级）。
    Ready(persist::Loaded),
    /// 由**更新版本**的程序写入：拒绝读取，并且**禁止写回**（D-107）。
    Locked { found: u32, supported: u32 },
}

/// 读取同目录存档。解析失败只打一行日志、当作没有；更新版本则返回 [`Sidecar::Locked`]。
fn load_sidecar(pdf: &std::path::Path) -> Sidecar {
    let target = persist::sidecar_path(pdf);
    let Ok(text) = std::fs::read_to_string(&target) else {
        return Sidecar::Missing;
    };
    match persist::load(&text) {
        Ok(loaded) => Sidecar::Ready(loaded),
        Err(persist::LoadError::NewerVersion { found, supported }) => {
            eprintln!(
                "[存档] {} 由更新版本写入（v{found} > v{supported}）：不加载，也不会覆盖它",
                target.display()
            );
            Sidecar::Locked { found, supported }
        }
        Err(error) => {
            eprintln!("[存档] 解析失败（忽略）：{}：{error}", target.display());
            Sidecar::Missing
        }
    }
}

/// 原位编辑的目标字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    /// 页码
    Page,
    /// 缩放比
    Zoom,
}

/// 缩放模式（决定缩放菜单的标题）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoomMode {
    /// 适应宽度
    FitWidth,
    /// 适应高度
    FitHeight,
    /// 100%
    Actual,
    /// 其它（手动输入或滚轮缩放）
    Custom,
}

impl ZoomMode {
    fn label(self) -> &'static str {
        match self {
            ZoomMode::FitWidth => "适应宽度",
            ZoomMode::FitHeight => "适应高度",
            ZoomMode::Actual => "100%",
            ZoomMode::Custom => "自定义",
        }
    }
}

/// 路径过长时中间截断（保留开头与文件名），如 `aa/…/bb.pdf`。
fn truncate_middle(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit || limit < 8 {
        return text.to_string();
    }
    let keep = limit - 1;
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// 侧栏的两个标签页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    /// 目录（书签）
    Outline,
    /// 自定义 preview 列表（Pin）
    Pins,
    /// 注释列表（P7）
    Annotations,
}

/// 自定义 preview 的两种类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustomKind {
    /// 文字预览（题目 + 内容）
    Text,
    /// 截图预览（题目 + 截图）
    Image,
}

/// 自定义 preview 的输入对话框（Pin 结束、点击页面后弹出）。
struct CustomDialog {
    kind: CustomKind,
    /// Pin 结束时点击的页号与页内坐标（PDF 页面空间）
    page: PageIndex,
    point: Point,
    title: String,
    content: String,
}

/// 新建便签的输入对话框。
struct NoteDraft {
    /// 所在页。
    page: PageIndex,
    /// 图标矩形（页面空间）。
    rect: Rect,
    /// 正文草稿。
    text: String,
}

/// 生成一个跨会话唯一的图片记录 id（无需额外依赖）。
fn new_image_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{seq:x}")
}

/// 每个浮窗缓存的 Markdown 预处理结果（不缓存 egui_commonmark 的排版）。
struct PreparedMarkdown {
    /// 源文本哈希；变化时重建。
    source_hash: u64,
    /// 换行/公式块规范化后的文本。
    normalized: String,
    /// 图片引用（用于 owner 懒渲染，不必每帧重新解析）。
    image_refs: Vec<crate::image_store::ImageRef>,
    /// 不可重排内容的最小宽度。
    min_width: f32,
}

/// Markdown 源文本哈希（预处理缓存 key）。
fn markdown_source_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// 在 TextEdit 当前光标/选区处插入文本，并把光标放到插入内容之后。
fn insert_at_text_cursor(ctx: &egui::Context, id: Id, text: &mut String, snippet: &str) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    let char_len = text.chars().count();
    let (start, end) = state
        .cursor
        .char_range()
        .map(|range| {
            let a = usize::from(range.primary.index).min(char_len);
            let b = usize::from(range.secondary.index).min(char_len);
            (a.min(b), a.max(b))
        })
        .unwrap_or((0, 0));
    let byte_at = |index: usize| {
        text.char_indices()
            .nth(index)
            .map(|(byte, _)| byte)
            .unwrap_or(text.len())
    };
    text.replace_range(byte_at(start)..byte_at(end), snippet);
    let next = (start + snippet.chars().count()).min(text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(next),
        )));
    state.store(ctx, id);
}

/// 已保存的自定义 preview。
///
/// 按用户要求记录：Pin 结束时的**页号 + 页内坐标**、以该坐标为中心的**零尺寸矩形**
/// （与链接对应）、以及题目/内容或截图。位置类字段暂时只被"保存"而未被读取 ——
/// 它们是给"自定义 preview 与链接/被引用点挂接"的下一版准备的。
#[derive(Clone)]
struct CustomPreview {
    /// 会话内稳定 id：按页从 0 开始，删除不回收；不写进 sidecar。
    id: u64,
    page: PageIndex,
    /// 记录时的页内坐标。**只写不读**：当前只用于写回存档（D-49 的"位置类字段"），
    /// 后续"自定义 preview 与链接/被引用点挂接"会读它。
    #[allow(dead_code)]
    point: Point,
    /// 以 `point` 为中心、**零尺寸**的矩形（与链接对应用）
    rect: Rect,
    kind: CustomKind,
    title: String,
    /// 文字预览的内容
    text: Option<String>,
    /// 截图预览的图像
    snapshot: Option<egui::TextureHandle>,
    /// **浮窗正文尺寸**（egui 单位），语义同 `DestMark::body_size`。
    body_size: egui::Vec2,
    /// ▲v2 截图时的**显示比例**（`1.0` = 100%）：预览图按它请求渲染，
    /// 因此预览分辨率与"当前缩放"解耦（D-100）。文字预览用不到，填 1.0。
    scale: f32,
    /// ▲v4 稳定图片记录 id；文字 preview 为空。
    image_id: String,
    /// ▲v4 图片内容版本。
    image_revision: u64,
}

/// 截图模式产出的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CaptureTarget {
    /// 普通截图：只放进新浮窗
    #[default]
    Plain,
    /// 回写到某个被引用点
    DestMark(PageIndex, usize),
    /// 回写到某个自定义 preview（下标）
    Custom(usize),
}

/// 悬停预览浮窗：鼠标在页内链接上时出现，显示该链接目标的被引用点截图。
///
/// 只要指针还在**链接**或**这个浮窗**上就持续存在；点击它则转为永久浮窗（不受鼠标影响）。
struct HoverTip {
    /// 对应的被引用点 `(页号, 该页内下标)`
    target: (PageIndex, usize),
    /// 屏幕空间锚点（链接矩形的左下角附近）
    anchor: egui::Pos2,
    /// 上一帧实际绘制出来的矩形（用于判断"指针是否还在浮窗上"）
    rect: Option<egui::Rect>,
}

/// 一个**被引用点**（链接跳转目的地）及其可编辑的截图信息。
///
/// * `rect`：页面空间的一块矩形，初始化时是点周围的一小块；用户可在截图模式里改；
/// * `snapshot`：**初始为 `None`** —— 没有额外动作前不渲染任何截图（用户要求）；
///   只有"点击标识 → 截图模式 → Enter"之后才会变成 `Some`。
#[derive(Clone)]
struct DestMark {
    /// 页面空间的位置。
    point: Point,
    /// 页面空间的矩形（与当前页重叠的部分）。
    rect: Rect,
    /// 已构建的截图（自包含纹理）。
    snapshot: Option<egui::TextureHandle>,
    /// **浮窗正文尺寸**（egui 单位）：生成浮窗时正文按它显示；
    /// 用户改浮窗大小后回写。
    body_size: egui::Vec2,
    /// ▲v2 截图时的**显示比例**（`1.0` = 100%）：预览图按它请求渲染（D-100）。
    scale: f32,
    /// ▲v4 稳定图片记录 id。
    image_id: String,
    /// ▲v4 图片内容版本。
    image_revision: u64,
}

/// 截图模式的状态。
///
/// 进入后：主视图的拖动/滚动/缩放/链接跳转等**一律不响应**（瓦片加载仍继续），
/// 浮窗与所有叠层隐藏，主视图蒙上半透明亮灰；鼠标拖出矩形，矩形内透明（"挖洞"）。
/// `Enter` 退出并在终端输出区域信息，`Esc` 退出且无输出。
#[derive(Default)]
struct CaptureState {
    /// 已画出的选区（**屏幕**空间）；`None` = 尚未开始
    rect: Option<egui::Rect>,
    /// 正在拖拽的起点（屏幕空间）
    anchor: Option<egui::Pos2>,
    /// 这次截图的结果要回写到哪儿。
    target: CaptureTarget,
}

/// 当前“下一步把选区变成哪种文本标记”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkupTool {
    Highlight,
    Underline,
    StrikeOut,
}

impl MarkupTool {
    fn label(self) -> &'static str {
        match self {
            Self::Highlight => "高亮",
            Self::Underline => "下划线",
            Self::StrikeOut => "删除线",
        }
    }

    fn kind(self, quads: Vec<Rect>) -> AnnotationKind {
        match self {
            Self::Highlight => AnnotationKind::Highlight { quads },
            Self::Underline => AnnotationKind::Underline { quads },
            Self::StrikeOut => AnnotationKind::StrikeOut { quads },
        }
    }
}

type ReadAnnotationEntry<'a> = (
    &'a backend_pdfium::readback::ReadAnnotation,
    Option<pdfcore::annotation::PdfObjectRef>,
    Option<(PageIndex, usize)>,
);

/// 自定义 preview 的会话内稳定键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PreviewKey {
    page: PageIndex,
    id: u64,
}

/// 一条可撤销的编辑（S3）。
///
/// 命令里**持有旧值**（`Arc` 共享，不做深拷贝）：撤销时把它移回应用即可。
#[derive(Clone)]
enum AppEdit {
    /// 新增一条标注（快照含正文，撤销/重做天然带着走）。
    AddAnnotation {
        /// 标注本体（含 id 与正文，撤销后重做必须用同一个 id）。
        annotation: pdfcore::annotation::Annotation,
    },
    /// 删除一条标注（撤销 = 把快照原样放回）。
    DeleteAnnotation {
        /// 被删掉的标注（含删除那一刻的正文）。
        annotation: pdfcore::annotation::Annotation,
    },
    /// 修改已有标注的样式（颜色/透明度/墨迹线宽）：重建对象，旧对象由写回 deletes 删除。
    RestyleAnnotation {
        /// 改样式前快照。
        before: pdfcore::annotation::Annotation,
        /// 改样式后快照（id 可能不同）。
        after: pdfcore::annotation::Annotation,
    },
    /// 移动已有标注（目前只有墨迹）：原地改几何；写回时旧对象删除、新对象重建。
    MoveAnnotation {
        /// 移动前快照。
        before: pdfcore::annotation::Annotation,
        /// 移动后快照（同 id，`pdf_ref = None` 表示要重建对象）。
        after: pdfcore::annotation::Annotation,
    },
    /// 修改注释正文（一条命令一次提交，不按字符记历史）。
    SetBody {
        /// 注释 id。
        id: AnnotationId,
        /// 所在页（只用于描述）。
        page: PageIndex,
        /// 旧正文。
        before: Option<String>,
        /// 新正文。
        after: Option<String>,
    },
    /// 删除一条自定义 preview。
    DeletePreview {
        /// 稳定键。
        key: PreviewKey,
        /// 删除时的全局位置（用于插回）。
        position: usize,
        /// 被删掉的快照。
        preview: std::sync::Arc<CustomPreview>,
    },
    /// 放回一条自定义 preview（撤销删除，或新建时插入）。
    AddPreview {
        /// 稳定键。
        key: PreviewKey,
        /// 插入位置。
        position: usize,
        /// 内容。
        preview: std::sync::Arc<CustomPreview>,
    },
    /// 修改 preview 标题。
    SetPreviewTitle {
        key: PreviewKey,
        before: String,
        after: String,
    },
    /// 修改文字 preview 正文。
    SetPreviewBody {
        key: PreviewKey,
        before: Option<String>,
        after: Option<String>,
    },
    /// 修改 preview 浮窗正文尺寸。
    ResizePreview {
        key: PreviewKey,
        before: egui::Vec2,
        after: egui::Vec2,
    },
    /// 替换 preview 内容（截图重选/图片重选）。
    ReplacePreview {
        key: PreviewKey,
        before: std::sync::Arc<CustomPreview>,
        after: std::sync::Arc<CustomPreview>,
    },
    /// 调整 preview 顺序（当前 UI 尚无入口，先留统一命令）。
    MovePreview {
        key: PreviewKey,
        from: usize,
        to: usize,
    },
    /// 重选引用点（撤销 = 换回旧的矩形/截图/文字）。
    EditDestMark {
        /// 页号。
        page: PageIndex,
        /// 该页第几个被引用点。
        index: usize,
        /// 旧值。
        before: std::sync::Arc<DestMark>,
        /// 新值。
        after: std::sync::Arc<DestMark>,
    },
}

impl AppEdit {
    /// 这条编辑是否改变 PDF 里的内容（注释）。preview/引用点只写 `.json`。
    fn affects_pdf(&self) -> bool {
        matches!(
            self,
            Self::AddAnnotation { .. }
                | Self::DeleteAnnotation { .. }
                | Self::RestyleAnnotation { .. }
                | Self::MoveAnnotation { .. }
                | Self::SetBody { .. }
        )
    }

    /// 给人看的一句话（可视列表直接用它）。
    fn describe(&self) -> String {
        match self {
            Self::AddAnnotation { annotation } => {
                let page = annotation.page.display_number();
                match &annotation.kind {
                    // 墨迹/便签没有行矩形：按 quads 数描述会写成"高亮（第 N 页，0 行）"
                    AnnotationKind::Ink { .. } => format!("墨迹（第 {page} 页）"),
                    AnnotationKind::Note { .. } => format!("便签（第 {page} 页）"),
                    AnnotationKind::Underline { .. } => match annotation.kind.quads().len() {
                        1 => format!("下划线（第 {page} 页）"),
                        rows => format!("下划线（第 {page} 页，{rows} 行）"),
                    },
                    AnnotationKind::StrikeOut { .. } => match annotation.kind.quads().len() {
                        1 => format!("删除线（第 {page} 页）"),
                        rows => format!("删除线（第 {page} 页，{rows} 行）"),
                    },
                    AnnotationKind::Highlight { .. } => match annotation.kind.quads().len() {
                        1 => format!("高亮（第 {page} 页）"),
                        rows => format!("高亮（第 {page} 页，{rows} 行）"),
                    },
                }
            }
            Self::DeleteAnnotation { annotation } => {
                format!("删除标注（第 {} 页）", annotation.page.display_number())
            }
            Self::RestyleAnnotation { after, .. } => {
                format!("修改标注样式（第 {} 页）", after.page.display_number())
            }
            Self::MoveAnnotation { after, .. } => {
                format!("移动标注（第 {} 页）", after.page.display_number())
            }
            Self::SetBody { page, .. } => {
                format!("修改注释正文（第 {} 页）", page.display_number())
            }
            Self::DeletePreview { key, .. } => {
                format!(
                    "删除自定义preview（第 {} 页 #{}）",
                    key.page.display_number(),
                    key.id
                )
            }
            Self::AddPreview { key, .. } => {
                format!(
                    "添加自定义preview（第 {} 页 #{}）",
                    key.page.display_number(),
                    key.id
                )
            }
            Self::SetPreviewTitle { key, .. } => {
                format!("修改preview标题（第 {} 页）", key.page.display_number())
            }
            Self::SetPreviewBody { key, .. } => {
                format!("修改preview正文（第 {} 页）", key.page.display_number())
            }
            Self::ResizePreview { key, .. } => {
                format!("调整preview浮窗大小（第 {} 页）", key.page.display_number())
            }
            Self::ReplacePreview { key, .. } => {
                format!("替换preview内容（第 {} 页）", key.page.display_number())
            }
            Self::MovePreview { key, .. } => {
                format!("调整preview顺序（第 {} 页）", key.page.display_number())
            }
            Self::EditDestMark { page, .. } => {
                format!("重选引用点（第 {} 页）", page.display_number())
            }
        }
    }

    /// 估算权重：截图类命令持有位图（在 `Arc` 后面共享），按 1 MiB 估。
    fn weight(&self) -> usize {
        match self {
            Self::DeletePreview { .. } | Self::AddPreview { .. } | Self::ReplacePreview { .. } => {
                1024 * 1024
            }
            Self::SetPreviewTitle { .. }
            | Self::SetPreviewBody { .. }
            | Self::ResizePreview { .. }
            | Self::MovePreview { .. } => 512,
            Self::EditDestMark { .. } => 1024 * 1024,
            _ => 768,
        }
    }
}

/// 浮窗内容：纯文字，或一张**自包含的截图**。
enum FloatContent {
    /// 纯文字。
    Text(String),
    /// 截图（独立纹理，与主视图状态解耦；改窗口大小只改变显示尺寸，不改变内容）。
    Image(egui::TextureHandle),
}

/// 浮窗的来源：注释正文是一类独立对象，不与自定义 preview 混用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloatOwner {
    /// 注释的正文浮窗。
    Annotation(AnnotationId),
    /// 自定义文字/图片 preview 的正式浮窗。
    Custom(usize),
    /// 测试浮窗、链接预览、截图等。
    None,
}

/// 视图区域内的浮窗（始终在画布内部、可移动/改大小/关闭）。
struct Float {
    /// 唯一 id（用于 egui 的交互标识）。
    id: u64,
    /// 标题栏文字。
    title: String,
    /// 内容（文字或截图）。
    content: FloatContent,
    /// 在**视图区域**（画布）内的矩形；每帧会被夹取回区域内。
    rect: egui::Rect,
    /// 来源（注释 / 自定义 preview / 其它）。
    owner: FloatOwner,
    /// 正文尺寸的**权威值**：只在"创建"与"拖动右下角改大小"时写入。
    ///
    /// 不从 `rect` 每帧反算 —— 那样会带入换算与夹取的亚像素误差，
    /// 使"尺寸是否变化"的判断失效（D-73 的根因）。浮窗自己持有它，
    /// 回写时直接赋值，比较就是数值对数值（用户建议的模型）。
    body: egui::Vec2,
    /// 来源：由某个被引用点的悬停预览**固定**而来时记录 `(页号, 下标)`。
    /// 用于"同一目的地再次悬停时高亮并置顶这个浮窗，而不是再弹预览"。
    origin: Option<(PageIndex, usize)>,
}

impl Float {
    fn custom_index(&self) -> Option<usize> {
        match self.owner {
            FloatOwner::Custom(index) => Some(index),
            _ => None,
        }
    }
}

/// 应用状态。
pub struct PdfReaderApp {
    session: Option<DocumentSession>,
    navigation: Option<Arc<NavigationIndex>>,
    textures: KeyedStore<egui::TextureHandle>,
    /// D-96：每页"当前波段从何时开始"（`(scale_milli, 起始时刻)`），用于就绪门槛的超时兜底。
    band_since: std::collections::HashMap<PageIndex, (u32, std::time::Instant)>,
    /// 每页上一帧的画法（结构化）。既用于 `[画法]` 的边沿触发输出，也供回放器读取。
    last_plan: std::collections::HashMap<PageIndex, PlanStamp>,
    /// 本次打开是否发生过**存档版本迁移**（旧版本 → 当前版本）：是则置脏，提示保存。
    sidecar_migrated: bool,
    /// 同目录存档由**更新版本**写入：不加载、也不允许覆盖（`(存档版本, 本程序版本)`，D-107）。
    sidecar_locked: Option<(u32, u32)>,
    /// 是否已经提示过"当前视口的纹理需求超出预算"（只提示一次，D-111）。
    budget_warned: bool,
    /// 上一帧的保留统计（A/B/C 各占多少字节），诊断用（D-111）。
    last_retain: Desired,
    /// 每帧临时持有的命令与事件缓冲（避免反复分配）。
    commands: Vec<ViewerCommand>,
    events: Vec<SessionEvent>,
    visible: Vec<PageIndex>,
    drag: Option<DragMode>,
    /// 拖拽语义（工具栏可切换）。
    tool: Tool,
    /// 是否画出链接热区框（工具栏 checkbox，默认关闭）。
    show_links: bool,
    /// 是否绘制标注（D-118；默认开启）。
    show_annotations: bool,
    /// 页面标注（会话内；S2 才落盘）。
    annotations: AnnotationStore,
    /// 本会话里“来自 PDF、但不属于 overlay”的注释 id（不写回 `.annot.json`）。
    external_ids: std::collections::HashSet<AnnotationId>,
    /// 打开时 PDF 里所有四种注释的对象号（用于计算待删除集合）。
    opened_pdf_refs: std::collections::BTreeSet<pdfcore::annotation::PdfObjectRef>,
    /// 打开时每个对象号的正文（用于外部注释正文差分）。
    opened_pdf_bodies: std::collections::HashMap<pdfcore::annotation::PdfObjectRef, Option<String>>,
    /// 打开时对象号 → 标记 id（撤销/重做后快照可能暂时没有 pdf_ref）。
    opened_id_by_ref: std::collections::HashMap<pdfcore::annotation::PdfObjectRef, AnnotationId>,
    /// 外部内联注释：id → (页号, 该页可见支持注释中的位置)。
    inline_refs: std::collections::HashMap<AnnotationId, (PageIndex, usize)>,
    /// 打开时内联注释的正文，用于判断是否需要页级正文 patch。
    opened_inline_bodies: std::collections::HashMap<(PageIndex, usize), Option<String>>,
    /// 外部注释的对象号 → 稳定本地 id（写回重开时复用，撤销栈才能对上）。
    pdf_ref_ids: std::collections::HashMap<pdfcore::annotation::PdfObjectRef, AnnotationId>,
    /// 下一次 `open` 是写回后的重开：保留撤销栈与对象号映射。
    reopening_after_write: bool,
    /// 正在画的一笔墨迹（S4b）；**抬笔之前它不是标注**，取消只要丢掉它。
    ink: Option<InkStroke>,
    /// "下一步把当前选区变成标注"（快捷键一次性动作）。
    pending_annotate: Option<MarkupTool>,
    /// 当前持有哪种标记工具：点按钮进入，拖选文字后自动施加，Esc/再点退出。
    annotation_tool: Option<MarkupTool>,
    /// 本次拖选是否从文字上开始（避免在空白处拖拽时误用旧选区）。
    selection_started_on_text: bool,
    /// 新文本标记的颜色（样式栏）。
    markup_color: [u8; 3],
    /// 新高亮的透明度（下划线/删除线固定不透明）。
    markup_opacity: f32,
    /// 新墨迹的颜色。
    ink_color: [u8; 3],
    /// 进入标记模式前的工具，退出时恢复。
    tool_before_annotation: Option<Tool>,
    /// 是否正在等"原地写入 PDF"的确认（S5c）。
    pending_in_place: bool,
    /// 保存确认框里的错误；失败/冲突时对话框保留并显示它。
    in_place_error: Option<String>,
    /// 上次打开时 PDF 里带 `pd-fr:` 标记的注释数（决定空 overlay 能否写回/导出）。
    pdf_marked_annotations: usize,
    /// 原地写入重开后要恢复的视图（页、缩放、滚动、剩余帧数）。
    ///
    /// 延迟两帧：**先让新会话收到真实的视口尺寸**，否则 `SetScroll` 会被按旧/默认视口夹紧，
    /// 表现为"缩放对了、滚动偏了一点"（用户实测）。
    pending_view_restore: Option<(PageIndex, f32, Option<Vec2>, u8)>,
    /// 注释是否有未写入 PDF 的新增/正文/删除。
    pdf_dirty: bool,
    /// 上次成功写 PDF / 打开时的历史修订号（与当前修订号比较得出脏位）。
    saved_pdf_revision: u64,
    /// 原地保存前是否留一份 `.bak` 副本（设置项，默认关闭）。
    backup_before_inplace: bool,
    /// 关闭/切换文件前的未保存提示。
    pending_action: Option<PendingAction>,
    /// 正文编辑尚未保存时挂起的关闭/切换动作。
    pending_edit_action: Option<EditDiscardAction>,
    /// 提示里的错误（保存失败/冲突等）。
    pending_action_error: Option<String>,
    /// 用户已经决定关闭：下一次 close_requested 不再拦截。
    closing_after_decision: bool,
    /// 上次成功保存自定义数据 / 打开时自定义数据的指纹。
    data_clean_hash: Option<u64>,
    /// 编辑历史（注释 + 重选截图/引用点，单栈时间序）。
    history: crate::history::History<AppEdit>,
    /// 是否显示"编辑历史"可视列表（首次撤销时自动打开）。
    show_history: bool,
    /// 本帧指针悬停在哪个标注上（`Delete` 用它决定删谁）。
    hovered_annotation: Option<AnnotationId>,
    /// 大纲的**扁平行**（打开文档时摊平一次；侧栏用虚拟化行渲染，避免每帧布局上千行）。
    outline_rows: Vec<crate::view::OutlineRow>,
    /// 视图区域内的浮窗；**后加入的排在后面 → 绘制与交互都在更上层**（用户要求"后出现的浮窗更高"）。
    floats: Vec<Float>,
    /// 下一个浮窗 id。
    next_float_id: u64,
    /// 是否标注"被引用的点"（链接跳转目的地）。
    show_dest_points: bool,
    /// 每个目标页上被引用的点（按 0.5pt 去重）及其矩形/截图，下标 = 页号；打开文档时算一次。
    dest_marks: Vec<Vec<DestMark>>,
    /// 截图模式状态（`Some` = 处于该模式）。
    capture: Option<CaptureState>,
    /// 悬停预览浮窗（鼠标在页内链接上时出现）。
    hover_tip: Option<HoverTip>,
    /// 上一帧鼠标所在链接框的尺寸（仅用于计算临时浮窗位置）
    hover_link_size: egui::Vec2,
    /// 滚动/缩放"刚刚发生过"的截止时刻：这段时间内不显示悬停预览（用户要求）。
    scroll_quiet_until: Option<std::time::Instant>,
    /// 本帧需要**高亮**的浮窗 id（悬停到已固定的同一目的地时设置，仅维持一帧）。
    highlight_float: Option<u64>,
    /// 当前文档的路径（用于保存/查找同目录的存档文件）。
    document_path: Option<std::path::PathBuf>,
    /// 打开时记录的文件身份；写回前若变化则拒绝覆盖。
    pdf_file_stamp: Option<PdfFileStamp>,
    /// 另存为对话框中的目标路径；`None` = 未打开。
    save_as: Option<String>,
    /// 另存为对话框的错误提示。
    save_as_error: Option<String>,
    /// 另存为：同时写原 PDF 的数据文件。
    save_as_write_original: bool,
    /// 另存为：写新 PDF 的数据文件。
    save_as_write_target: bool,
    /// 另存为：目标已存在时是否覆盖（对话框里勾选）。
    save_as_overwrite: bool,
    /// 整理副本对话框里的目标路径；`None` = 未打开。
    compact_dialog: Option<String>,
    /// 整理副本的错误提示。
    compact_error: Option<String>,
    /// 便签模式：下一次点击页面时在该位置新建便签。
    note_pin: bool,
    /// 新建便签的输入对话框。
    note_dialog: Option<NoteDraft>,
    /// 正在 Alt/Ctrl+拖拽平移的墨迹。
    ink_move: Option<InkMoveState>,
    /// 右键「移动」选中的墨迹：下一次拖动它时不需要按修饰键。
    move_ink_armed: Option<AnnotationId>,
    /// 自定义 preview 的 Pin 状态（`Some` = 正在选点；此时不显示浮窗、不响应链接点击）。
    custom_pin: Option<CustomKind>,
    /// 自定义 preview 的输入对话框。
    custom_dialog: Option<CustomDialog>,
    /// 已保存的自定义 preview。
    custom_previews: Vec<CustomPreview>,
    /// 每页下一个可用的 custom preview id（会话内）。
    next_preview_id: Vec<u64>,
    /// 连续的 preview resize 合并窗口。
    last_preview_resize: Option<(PreviewKey, std::time::Instant)>,
    /// 自定义数据是否有未保存的修改（影响保存按钮文案）。
    data_dirty: bool,
    /// 页码原位编辑草稿（`Some` = 正在编辑）。
    page_edit: Option<String>,
    /// 缩放比原位编辑草稿。
    zoom_edit: Option<String>,
    /// 缩放菜单当前的模式标题。
    zoom_mode: ZoomMode,
    /// 哪个原位编辑框需要在**下一帧**获得焦点（双击后直接进入真编辑）。
    focus_edit: Option<EditField>,
    /// 正文原位编辑是否需要在当帧获得焦点（与 `focus_edit` 同理，但目标在浮窗里）。
    focus_body_edit: bool,
    /// 搜索框是否需要在当帧获得焦点（Ctrl+F 直接可输入）。
    focus_search: bool,
    /// 是否处于全屏模式（窗口最大化、去标题栏、顶栏改为悬浮显示）。
    fullscreen: bool,
    /// 全屏下顶栏是否**悬浮显示**（指针靠近窗口顶端时出现）。
    reveal_toolbar: bool,
    /// 是否横向浏览（页从左到右排成一行）。
    horizontal_layout: bool,
    /// 勾选后：**纵向滚动一律转为横向滚动**（原始横向分量丢弃）。
    horizontal_scroll: bool,
    /// 待应用的布局切换（`Some(true)` = 横向），在拿到 `&mut session` 时执行。
    pending_layout: Option<bool>,
    /// 底部搜索框是否显示（Ctrl+F 切换）。
    search_open: bool,
    /// 正在编辑题目 `(下标, 草稿)`。
    editing_title: Option<(usize, String)>,
    /// 正在编辑正文 `(下标, 草稿)`（文字预览：固定浮窗内双击正文进入）。
    editing_body: Option<(FloatOwner, String)>,
    /// **链接 → 被引用点** 的登记表（与 `navigation.links_by_page` 同构）：
    /// 下标与 `links_on(page)` 一一对应。打开文档时解析一次，
    /// 之后悬停与跳转都直接用它（用户建议：记住索引，而不是每次按位置查）。
    ///
    /// 前提：打开后 PDF 内容不再变化（不会插入新链接）—— 用户已确认该假设成立。
    link_marks: Vec<Vec<Option<(PageIndex, usize)>>>,
    /// 本帧累计的"像素格式转换"与"load_texture"耗时（诊断用，定位 150ms 到底在哪一段）。
    last_convert_ms: f64,
    last_upload_ms: f64,
    /// 侧栏宽度：**由 app 自己保存**。
    ///
    /// egui 的面板尺寸以 `parent_ui.scope_id().with_salt(...)` 为键持久化，
    /// 而 eframe 每帧给的是一个新的根 `Ui`，于是 egui 侧的持久化读不回来 ——
    /// 人工验收看到的"拖动边框、松手回弹"就是这个原因。改为每帧把上次观察到的宽度
    /// 作为 `default_size` 回填，尺寸便由我们自己保住。
    sidebar_width: f32,
    /// 用户设置（注释默认样式 / 每个文件的阅读状态 / 窗口尺寸）。
    settings: crate::settings::Settings,
    /// 设置文件路径；`None` = 不读写（无窗口/回放/测试）。
    settings_path: Option<PathBuf>,
    /// 设置是否有未落盘的修改。
    settings_dirty: bool,
    /// 首帧要恢复的窗口尺寸（`None` = 不动）。
    pending_window_size: Option<[f32; 2]>,
    /// 在绘制过程中请求打开的文件：必须等 `session` 放回 `self` 之后再打开，
    /// 否则 `ui()` 末尾会把旧 session 又塞回去（菜单里直接 `open` 的坑）。
    pending_open: Option<PathBuf>,
    /// 新墨迹的线宽（来自设置）。
    ink_width: f32,
    /// 待输入的密码（文档加密时）。
    password_for: Option<PathBuf>,
    password_input: String,
    search_input: String,
    /// 页码输入框（第二轮验收：支持键入页码跳转）。
    page_input: String,
    hits: Vec<SearchHit>,
    active_hit: Option<usize>,
    /// 侧栏是否显示（原"目录"按钮，现在叫"侧栏"）。
    show_outline: bool,
    /// 侧栏当前标签页。
    sidebar_tab: SidebarTab,
    /// 注释列表：按类型过滤。
    annotation_show_highlight: bool,
    annotation_show_underline: bool,
    annotation_show_strike: bool,
    annotation_show_ink: bool,
    annotation_show_note: bool,
    /// 注释列表：只看外部/只读注释。
    annotation_only_external: bool,
    annotation_only_readonly: bool,
    /// 注释列表视图（按页 / 最近修改）。
    annotation_view: AnnotationListView,
    /// 注释列表搜索词（正文/作者，忽略大小写）。
    annotation_search: String,
    /// 按页视图里被折叠的页。
    collapsed_annotation_pages: std::collections::HashSet<u32>,
    /// 注释列表点击后的中性定位轮廓（id + 截止时刻）。
    annotation_focus: Option<(AnnotationId, std::time::Instant)>,
    /// 画布右键菜单锚定的注释（菜单/子菜单打开期间保持）。
    context_annotation: Option<AnnotationId>,
    /// 自定义右键菜单的屏幕位置；`None` = 关闭。
    context_menu_pos: Option<egui::Pos2>,
    /// Pin 条目悬浮响应开关（勾选 = 悬浮时显示临时预览/高亮旧浮窗）。
    pin_hover_enabled: bool,
    /// 文字浮窗是否以 Markdown 渲染（`settings.json`，默认关闭）。
    markdown_floats: bool,
    /// 侧栏 hover 临时预览是否也按 Markdown 渲染（设置项）。
    hover_preview_markdown: bool,
    /// hover Markdown 专用的解析/标题缓存。
    hover_markdown_cache: egui_commonmark::CommonMarkCache,
    /// 当前 hover Markdown 对应的 preview 下标；变化时清缓存。
    hover_markdown_index: Option<usize>,
    /// egui_commonmark 的排版/图片缓存（每个浮窗用稳定的 `Id` 区分）。
    /// 每个浮窗独立的 Markdown 缓存：锚点位置不能跨浮窗共享。
    markdown_caches: std::collections::HashMap<u64, egui_commonmark::CommonMarkCache>,
    /// 每个 Markdown 浮窗上一帧的滚动偏移/内容尺寸，用于把双击位置映射回源码。
    markdown_viewports: std::collections::HashMap<u64, (egui::Vec2, egui::Vec2)>,
    /// 每个浮窗的 Markdown 预处理缓存（按源文本哈希失效）。
    markdown_prepared: std::collections::HashMap<u64, PreparedMarkdown>,
    /// 拖入文件 hover 日志只打一次，避免 Wayland 调度每帧刷屏。
    drag_hover_logged: bool,
    /// Ctrl+S 在 egui 帧内无法安全借 session；延后到 session 放回后执行。
    pending_save: bool,
    /// LaTeX 公式的 PNG 纹理缓存与内嵌字体（全局共用）。
    math_cache: std::rc::Rc<std::cell::RefCell<crate::math::MathCache>>,
    /// 数学公式支持/限制帮助窗口是否打开。
    math_help: bool,
    /// Markdown 内部图片的 PNG 字节缓存。
    images: crate::image_store::ImageStore,
    /// 保存正文后，下一帧把公式错误合并到“已更新内容”提示里。
    math_recheck_pending: bool,
    /// 正文编辑会话号：每次打开换一个 ScrollArea id，避免不同文本继承滚动位置。
    body_edit_session: u64,
    /// 首次打开编辑器时要放置的字符光标（双击位置映射；✎ 为 0）。
    body_edit_cursor: Option<usize>,
    /// 本帧是否有 Markdown 浮窗渲染过；用于把外部链接的 `OpenUrl` 改成状态栏提示。
    markdown_rendered: bool,
    /// 侧栏 Pin 列表里正在被悬浮的条目（下标 + 该行的屏幕矩形，用于定位临时浮窗）。
    custom_hover: Option<(usize, egui::Rect)>,
    /// 上一帧主视图（画布）的矩形：侧栏先于画布绘制，用它给浮窗定位。
    canvas_rect: egui::Rect,
    /// 点击 Pin 条目后的一次性高亮（浮窗 id + 截止时刻）。
    click_highlight: Option<(u64, std::time::Instant)>,
    /// 状态栏文本与错误提示。
    message: String,
    error: Option<String>,
    /// 运行时诊断输出（环境变量 `PDF_READER_DIAGNOSE=1` 开启）。
    ///
    /// 存在的理由：像"800% 时看不到内容"这类问题只在真实窗口 + 真实 GPU 上出现，
    /// 无头测试无法复现；把每帧的绘制决策打到 stderr，用户就能把实况数据交回来
    /// （agent.md 规则 2：UI 相关验证交给用户，但要让用户能看到可诊断的信息）。
    diag: crate::diag::Diagnostics,
    /// 行为性截止时间用的时钟（真实时间；回放器注入可推进的时钟）。
    clock: crate::clock::Clock,
    /// 上次诊断输出的时间（限流到每秒一次）。
    last_report: std::time::Instant,
    /// 缩放/旋转"刚变过"的截止时间：在此之前不提交新渲染，全部可见页统一显示旧图拉伸。
    ///
    /// 连续滚轮缩放会产生一串中间比例（100→120→140→150），逐个渲染既浪费时间
    /// 又每步都掉一次帧（第三轮验收"缩放总是有顿感"）。等用户停手再渲染最终比例，
    /// 中间的过场用旧图顶上 —— 这是"预览质量阶梯"的轻量版。
    preview_until: Option<std::time::Instant>,
    /// 上一帧观察到的缩放与旋转，用于检测"刚变过"。
    last_zoom: f32,
    last_rotation: Rotation,
    /// 进程启动时刻：所有时序诊断都按"相对它的秒数"输出（用户要求：秒 + 浮点）。
    started_at: std::time::Instant,
    /// 本次拖动开始时刻（拖动结束时清空）。
    drag_started_at: Option<std::time::Instant>,
    /// 上一次"拖动位移生效"的诊断时刻，用于给出帧间隔。
    last_pan_at: Option<std::time::Instant>,
    /// 上一次记录的可见页集，用于检测"因拖动而重新决定显示内容"。
    last_visible: Vec<PageIndex>,
}

impl PdfReaderApp {
    /// 构造；`initial` 为命令行传入的文件路径。
    ///
    /// 这里顺手关掉 egui 内建的"Ctrl/⌘ + `+`/`-`/`0` 缩放整个界面"快捷键
    /// （`Options::zoom_with_keyboard`）：否则按 `Ctrl`+`+` 会**同时**放大 UI 与文档页面
    /// （人工验收反馈的现象）。关掉之后这些键由本应用统一解释为**文档缩放**。
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        let settings_path = crate::settings::Settings::path();
        let had_settings = settings_path.as_ref().is_some_and(|path| path.exists());
        let (settings, warning) = crate::settings::Settings::load();
        let mut app = Self::with_clock(
            crate::clock::Clock::system(),
            crate::diag::Diagnostics::from_env(),
            initial,
            settings,
            settings_path,
            warning,
        );
        // 窗口系统在创建时还不知道设置；只有**已有设置文件**时才恢复尺寸，
        // 首次启动保持 main 的默认窗口大小。
        if had_settings {
            app.pending_window_size = Some(app.settings.ui.window_size);
        }
        app
    }

    /// 设置纹理层字节预算（0 = 不限制）。给回放/调参用；正常路径由构造时读取。
    pub fn set_texture_budget(&mut self, bytes: u64) {
        self.textures.set_max_bytes(bytes);
        self.budget_warned = false;
    }

    /// **无窗口**构造：自己提供时钟与诊断去处（回放器 / 集成测试用）。
    ///
    /// 与 [`PdfReaderApp::new`] 的唯一区别就是不碰 `CreationContext`；其余行为完全一致。
    pub fn new_headless(
        clock: crate::clock::Clock,
        diag: crate::diag::Diagnostics,
        initial: Option<PathBuf>,
    ) -> Self {
        let mut app = Self::with_clock(
            clock,
            diag,
            initial,
            crate::settings::Settings::default(),
            None,
            None,
        );
        // 无窗口时没有窗口尺寸，给一个明确的默认视口（回放脚本也可以覆盖它）。
        app.commands
            .push(ViewerCommand::ResizeViewport(Size::new(1100.0, 900.0)));
        app
    }

    fn with_clock(
        clock: crate::clock::Clock,
        diag: crate::diag::Diagnostics,
        initial: Option<PathBuf>,
        settings: crate::settings::Settings,
        settings_path: Option<PathBuf>,
        settings_warning: Option<String>,
    ) -> Self {
        // 诊断限流与"启动时刻"都取注入时钟的当前值（回放时它由脚本推进）。
        let boot = clock.now();
        let style = settings.annotation.clone();
        let sidebar_width = settings.ui.sidebar_width;
        let ui_settings = settings.ui.clone();
        let save_settings = settings.save.clone();
        let mut app = Self {
            session: None,
            navigation: None,
            textures: {
                let mut textures =
                    KeyedStore::with_bytes(TEXTURE_CAPACITY, crate::view::texture_bytes);
                textures.set_max_bytes(texture_budget_bytes());
                textures
            },
            band_since: std::collections::HashMap::new(),
            last_plan: std::collections::HashMap::new(),
            sidecar_migrated: false,
            sidecar_locked: None,
            budget_warned: false,
            last_retain: Desired::default(),
            commands: Vec::new(),
            events: Vec::new(),
            visible: Vec::new(),
            drag: None,
            tool: Tool::default(),
            show_links: ui_settings.show_links,
            show_annotations: ui_settings.show_annotations,
            annotations: AnnotationStore::new(),
            external_ids: std::collections::HashSet::new(),
            opened_pdf_refs: std::collections::BTreeSet::new(),
            opened_pdf_bodies: std::collections::HashMap::new(),
            opened_id_by_ref: std::collections::HashMap::new(),
            inline_refs: std::collections::HashMap::new(),
            opened_inline_bodies: std::collections::HashMap::new(),
            pdf_ref_ids: std::collections::HashMap::new(),
            reopening_after_write: false,
            ink: None,
            pending_annotate: None,
            annotation_tool: None,
            selection_started_on_text: false,
            markup_color: style.markup_color,
            markup_opacity: style.highlight_opacity,
            ink_color: style.ink_color,
            ink_width: style.ink_width,
            settings,
            settings_path,
            settings_dirty: false,
            pending_window_size: None,
            pending_open: None,
            tool_before_annotation: None,
            pending_in_place: false,
            in_place_error: None,
            pdf_marked_annotations: 0,
            pending_view_restore: None,
            pdf_dirty: false,
            saved_pdf_revision: 0,
            backup_before_inplace: save_settings.backup_in_place,
            pending_action: None,
            pending_edit_action: None,
            pending_action_error: None,
            closing_after_decision: false,
            data_clean_hash: None,
            history: crate::history::History::new(),
            show_history: false,
            hovered_annotation: None,
            outline_rows: Vec::new(),
            floats: Vec::new(),
            next_float_id: 1,
            show_dest_points: ui_settings.show_dest_points,
            dest_marks: Vec::new(),
            capture: None,
            hover_tip: None,
            hover_link_size: EVec2::new(160.0, 20.0),
            scroll_quiet_until: None,
            highlight_float: None,
            document_path: None,
            pdf_file_stamp: None,
            save_as: None,
            save_as_error: None,
            save_as_write_original: true,
            compact_dialog: None,
            compact_error: None,
            note_pin: false,
            note_dialog: None,
            ink_move: None,
            move_ink_armed: None,
            save_as_write_target: true,
            save_as_overwrite: false,
            custom_pin: None,
            custom_dialog: None,
            custom_previews: Vec::new(),
            next_preview_id: Vec::new(),
            last_preview_resize: None,
            data_dirty: false,
            page_edit: None,
            zoom_edit: None,
            zoom_mode: ZoomMode::Custom,
            focus_edit: None,
            focus_body_edit: false,
            focus_search: false,
            fullscreen: false,
            reveal_toolbar: false,
            horizontal_layout: false,
            horizontal_scroll: ui_settings.horizontal_scroll,
            pending_layout: None,
            search_open: false,
            editing_title: None,
            editing_body: None,
            link_marks: Vec::new(),
            last_convert_ms: 0.0,
            last_upload_ms: 0.0,
            sidebar_width,
            password_for: None,
            password_input: String::new(),
            search_input: String::new(),
            page_input: String::new(),
            hits: Vec::new(),
            active_hit: None,
            show_outline: true,
            sidebar_tab: SidebarTab::Outline,
            annotation_show_highlight: true,
            annotation_show_underline: true,
            annotation_show_strike: true,
            annotation_show_ink: true,
            annotation_show_note: true,
            annotation_only_external: false,
            annotation_only_readonly: false,
            annotation_view: AnnotationListView::default(),
            annotation_search: String::new(),
            collapsed_annotation_pages: std::collections::HashSet::new(),
            annotation_focus: None,
            context_annotation: None,
            context_menu_pos: None,
            pin_hover_enabled: ui_settings.pin_hover_enabled,
            markdown_floats: ui_settings.markdown_floats,
            hover_preview_markdown: ui_settings.hover_preview_markdown,
            hover_markdown_cache: egui_commonmark::CommonMarkCache::default(),
            hover_markdown_index: None,
            markdown_caches: std::collections::HashMap::new(),
            markdown_viewports: std::collections::HashMap::new(),
            markdown_prepared: std::collections::HashMap::new(),
            drag_hover_logged: false,
            pending_save: false,
            math_cache: std::rc::Rc::new(std::cell::RefCell::new(crate::math::MathCache::new())),
            math_help: false,
            images: crate::image_store::ImageStore::default(),
            math_recheck_pending: false,
            body_edit_session: 1,
            body_edit_cursor: None,
            markdown_rendered: false,
            custom_hover: None,
            canvas_rect: egui::Rect::NOTHING,
            click_highlight: None,
            message: String::new(),
            error: None,
            diag,
            clock,
            last_report: boot,
            preview_until: None,
            last_zoom: 1.0,
            last_rotation: Rotation::None,
            started_at: boot,
            drag_started_at: None,
            last_pan_at: None,
            last_visible: Vec::new(),
        };
        if let Some(warning) = settings_warning {
            app.message = warning;
        }
        app.saved_pdf_revision = app.history.current_pdf_revision();
        app.data_clean_hash = Some(app.data_fingerprint());
        if let Some(path) = initial {
            app.open(&path, None);
        }
        app
    }

    /// 采集输入 → 命令。
    fn collect_commands(&mut self, ctx: &egui::Context, session: &DocumentSession) {
        // 保存是全局动作：不能被“输入框有焦点”或截图模式吞掉。
        // 正文编辑有未提交草稿时先提示，避免 Ctrl+S 保存了旧内容却让用户以为草稿已落盘。
        let save_shortcut = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: Key::S,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.ctrl || modifiers.command
                )
            })
        });
        if save_shortcut {
            if self.editing_body.is_none() {
                // 本函数持有的是 `session` 的借用，而 `save_all` 需要 `self.session`
                // （此刻字段是 None）；延后到本帧末尾 session 放回后再写。
                self.pending_save = true;
            } else {
                self.message = "正文编辑器有未保存草稿：请用 Ctrl+Enter 保存正文".to_string();
            }
            return;
        }
        // **文本输入获得焦点时，键盘归输入框**：否则在编辑框里敲空格/方向键会同时滚动页面
        // （用户反馈）。`Memory::focused()` 在有 TextEdit 聚焦时返回其 Id，
        // 覆盖搜索框、题目/正文编辑框等所有输入控件。
        if ctx.memory(|memory| memory.focused().is_some()) {
            return;
        }
        // 正文编辑窗口打开时，键盘归编辑窗口（Esc 取消、Ctrl+Enter 保存）。
        if self.editing_body.is_some() {
            return;
        }
        // 截图模式下不采集任何"影响主视图显示内容"的命令（滚动/缩放/翻页/复制…）：
        // 键盘的 Enter/Esc 与框选都在 `draw_canvas` 里处理（那里才有画布矩形与滚动量）。
        if self.capture.is_some() {
            return;
        }
        let page_count = session.page_count();

        // 键盘
        let pressed: Vec<(Key, Modifiers)> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        // 复制：egui 把 Ctrl/Cmd+C 翻译成 `Event::Copy`（**不是** Key 事件），
        // 因此必须单独识别它 —— 这就是第二轮验收里"复制按钮可用但 Ctrl+C 无反应"的原因。
        let copy_event = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
        });
        if copy_event {
            self.commands.push(ViewerCommand::CopySelection);
        }
        for (key, modifiers) in pressed {
            // 已经由 Event::Copy 处理过时不重复处理（避免复制两次）
            if copy_event && key == Key::C {
                continue;
            }
            let command_modifier = modifiers.ctrl || modifiers.command;
            // 文本框有焦点时把按键让给文本框（它自己的撤销/删除更贴合当前输入）
            let text_focused = ctx.memory(|memory| memory.focused().is_some());
            if key == Key::Z && command_modifier && !text_focused {
                if modifiers.shift {
                    self.redo_edit();
                } else {
                    self.undo_edit();
                }
                continue;
            }
            if (key == Key::Delete || key == Key::Backspace)
                && !text_focused
                && self.delete_hovered_annotation()
            {
                continue;
            }
            if key == Key::H && !modifiers.any() {
                self.pending_annotate = Some(MarkupTool::Highlight);
                continue;
            }
            if key == Key::Escape && self.move_ink_armed.is_some() {
                self.move_ink_armed = None;
                self.message = "已取消移动".to_string();
                continue;
            }
            if key == Key::Escape && self.context_menu_pos.is_some() {
                self.context_annotation = None;
                self.context_menu_pos = None;
                continue;
            }
            if key == Key::Escape && self.annotation_tool.is_some() {
                self.exit_markup_tool();
                self.message = "已退出标记模式".to_string();
                self.drag = None;
                continue;
            }
            if key == Key::Escape && self.tool == Tool::Ink && self.ink.is_none() {
                let previous = self.tool_before_annotation.take().unwrap_or(Tool::Select);
                self.switch_tool(previous);
                self.message = "已退出墨迹模式".to_string();
                continue;
            }
            if key == Key::Escape && self.ink.is_some() {
                // 整笔丢弃：不进标注集合、不产生命令，所以撤销栈里不会留下半条
                self.ink = None;
                self.drag = None;
                self.message = "已取消这一笔".to_string();
                continue;
            }
            if let Some(command) = command_for_key(key, modifiers, page_count) {
                self.commands.push(command);
            }
        }

        // 滚轮与 Ctrl+滚轮的处理**不在这里**：它必须等侧栏等面板绘制完再读
        // （egui 的 ScrollArea 会消费滚动增量），并只在指针位于画布上时生效 ——
        // 否则"在侧栏里滚动，文档也跟着动"。见 `draw_canvas` 里的"滚轮"段。
    }

    /// 处理"把文件拖进窗口"。
    ///
    /// **必须在没有文档时也执行**：v1 把它放在"已有会话"的分支里，
    /// 导致启动后（空状态）拖入 PDF 毫无反应 —— 人工验收反馈的缺陷之一。
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let (hovered, dropped) = ctx.input(|input| {
            (
                input
                    .raw
                    .hovered_files
                    .iter()
                    .map(|file| (file.path.clone(), file.mime.clone()))
                    .collect::<Vec<_>>(),
                input
                    .raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_path_buf())
                    .collect::<Vec<_>>(),
            )
        });
        if !hovered.is_empty() && !self.drag_hover_logged {
            eprintln!("[drag] hovered files: {hovered:?}");
            self.drag_hover_logged = true;
        } else if hovered.is_empty() {
            self.drag_hover_logged = false;
        }
        if !dropped.is_empty() {
            eprintln!("[drag] dropped files: {dropped:?}");
        }
        if let Some(path) = dropped.into_iter().next() {
            self.request_open(path);
        }
    }

    /// 把命令交给会话，并把事件翻译成界面动作。
    fn apply_and_pump(&mut self, ctx: &egui::Context, session: &mut DocumentSession) {
        for command in std::mem::take(&mut self.commands) {
            session.handle(command, &mut self.events);
        }
        session.pump(&mut self.events);

        for event in std::mem::take(&mut self.events) {
            match event {
                SessionEvent::CopyToClipboard(text) => {
                    ctx.copy_text(text.clone());
                    self.message = format!("已复制 {} 个字符", text.chars().count());
                }
                SessionEvent::PageRenderFailed { page, error } => {
                    // 切布局/跳页会取消在途渲染；"任务已取消"不是失败，别弹给用户。
                    if !error.is_cancelled() {
                        self.error =
                            Some(format!("第 {} 页渲染失败：{error}", page.display_number()));
                    }
                }
                SessionEvent::Error(error) => self.error = Some(error.to_string()),
                SessionEvent::CurrentPageChanged(page) => {
                    // 页码输入框跟随真实当前页，避免显示过期数字
                    self.page_input = page.display_number().to_string();
                }
                _ => {}
            }
        }
    }

    /// 底部状态栏：页码与缩放已移到顶栏（用户要求），这里只留消息、选中字数与诊断。
    fn draw_status(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.separator();
                ui.separator();
                ui.label(format!("纹理 {} 个", self.textures.len()));
                if let Some(kind) = self.custom_pin {
                    ui.separator();
                    ui.colored_label(
                        Color32::from_rgb(230, 180, 60),
                        match kind {
                            CustomKind::Text => "当前自定义preview：文字（点击页面确定位置）",
                            CustomKind::Image => "当前自定义preview：图片（点击页面确定位置）",
                        },
                    );
                }
                if !self.message.is_empty() {
                    ui.separator();
                    // 状态栏最多一行：长文本用省略号，不能让消息把窗口撑高。
                    ui.add(egui::Label::new(&self.message).truncate());
                }
                if let Some(error) = &self.error {
                    ui.separator();
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error).color(Color32::from_rgb(200, 60, 60)),
                        )
                        .truncate(),
                    );
                }
            });
        });
    }

    /// 画布：页面（纹理或白底页框）+ 高亮 + 鼠标交互。
    fn draw_canvas(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        if let Some(tool) = self.pending_annotate.take() {
            self.annotate_selection(session, tool);
        }
        // 搜索栏放在**中央区域**的底部：只占画布宽度（不会跑到侧栏下方），
        // 且位于状态栏之上（用户建议）。
        self.draw_search_bar_in_center(ui, session);
        egui::CentralPanel::default().show(ui, |ui| {
            // 同时用"显式矩形 + interact"处理这两个区域，而**不是** `allocate_painter`：
            // 后者会按布局流占用空间 —— 上一版用 `allocate_painter(5px × 整高)` 画拖拽条，
            // 结果整高都被它吃掉，画布高度变成 0，界面看起来就是"分割线能拖但没有任何图像"。
            let full = ui.available_rect_before_wrap();
            let handle_width = if self.show_outline { SIDEBAR_HANDLE_WIDTH } else { 0.0 };
            let canvas = canvas_rect_for(full, handle_width);

            // 侧栏拖拽条：宽度完全由我们自己记（见 `draw_sidebar` 的说明）。
            if self.show_outline {
                let strip_rect = egui::Rect::from_min_size(
                    full.min,
                    EVec2::new(SIDEBAR_HANDLE_WIDTH, full.height()),
                );
                let strip_response =
                    ui.interact(strip_rect, Id::unique("sidebar-handle"), Sense::drag());
                let active = strip_response.hovered() || strip_response.dragged();
                if active {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter().rect_filled(
                    strip_rect,
                    CornerRadius::ZERO,
                    if active { Color32::from_gray(120) } else { Color32::from_gray(70) },
                );
                if strip_response.dragged() {
                    self.sidebar_width = (self.sidebar_width + strip_response.drag_delta().x)
                        .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                }
            }

            // 画布从拖拽条右侧开始（两者不重叠，拖拽不会被画布抢走）
            let response = ui.interact(canvas, Id::unique("pdf-canvas"), Sense::click_and_drag());
            let painter = ui.painter_at(canvas);

            // ---- 滚轮（在面板绘制之后读，且仅当指针在画布上）----
            //
            // 1. **必须等侧栏/工具栏绘制完再读**：egui 的 `ScrollArea` 会消费滚动增量，
            //    消费后 `smooth_scroll_delta` 归零；此前在面板绘制前读取，于是"侧栏滚动时文档也动"。
            // 2. 加指针位置判据，双保险（且行为可预期）。
            // 3. 当帧直接执行，滚轮少延迟一帧。
            let pointer_over_canvas = ui
                .ctx()
                .pointer_hover_pos()
                .is_some_and(|position| canvas.contains(position));
            // 浮窗是画布上的不透明覆盖层：指针在浮窗上时，滚轮/缩放只归浮窗内部
            // （Markdown 的 ScrollArea 会自己消费）。否则文档会跟着一起滚。
            let over_float = ui.ctx().pointer_hover_pos().is_some_and(|position| {
                self.floats
                    .iter()
                    .any(|window| window.rect.contains(position))
                    || self
                        .hover_tip
                        .as_ref()
                        .and_then(|tip| tip.rect)
                        .is_some_and(|rect| rect.contains(position))
            });
            // 指针在编辑窗口/帮助窗口等 Area 上时，滚轮归该窗口（例如编辑区的
            // ScrollArea），不能先被画布消费。`layer_id_at` 用的是上一帧的图层，
            // 对正在打开的窗口最多晚一帧，可接受。
            let over_window = ui.ctx().pointer_hover_pos().is_some_and(|position| {
                ui.ctx()
                    .layer_id_at(position)
                    .is_some_and(|layer| layer != egui::LayerId::background())
            });
            // Alt 悬停在墨迹上：抓手光标，提示可以拖动。
            let (alt, ctrl) = ui.ctx().input(|input| (input.modifiers.alt, input.modifiers.ctrl));
            if (alt || ctrl || self.move_ink_armed.is_some())
                && self.annotation_tool.is_none()
                && matches!(self.tool, Tool::Select | Tool::Pan)
            {
                if let Some(position) = ui.ctx().pointer_hover_pos() {
                    if canvas.contains(position) {
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
                        if self.annotation_at(session, content).is_some_and(|id| {
                            !self.external_ids.contains(&id)
                                && self
                                    .annotations
                                    .get(id)
                                    .is_some_and(|annotation| annotation.kind.ink().is_some())
                        }) {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                    }
                }
            }
            // 便签选点：十字光标 + Esc 取消
            if self.note_pin {
                if pointer_over_canvas {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                    self.exit_note_mode();
                }
            }
            // Pin 状态（自定义 preview 选点）：十字光标 + 状态栏提示
            if self.custom_pin.is_some() {
                if pointer_over_canvas {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                    self.custom_pin = None;
                    self.message = "已退出自定义preview".to_string();
                }
            }
            if self.capture.is_none()
                && wheel_targets_canvas_with_float(pointer_over_canvas && !over_window, over_float)
            {
                let (scroll_delta, zoom_delta, modifiers) = ui.input(|input| {
                    (input.smooth_scroll_delta, input.zoom_delta(), input.modifiers)
                });
                // 注意：**只有真的产生了滚动/缩放命令**才开启静默窗口。
                // 曾经把开窗写在"指针在画布上"的分支里，结果静止悬停时也每帧刷新，
                // 静默窗口永不结束 → 正常悬停也看不到预览。
                let mut view_changed = false;
                // 勾选"横向滚动"：纵向分量改作横向，横向分量丢弃（用户要求）；拖动不受影响
                let scroll_delta = if self.horizontal_scroll {
                    egui::vec2(scroll_delta.y, 0.0)
                } else {
                    egui::vec2(scroll_delta.x, scroll_delta.y)
                };
                let scroll_before = session.viewport().scroll;
                if let Some(command) = command_for_scroll(
                    scroll_delta,
                    modifiers.ctrl || modifiers.command,
                    session.state().zoom,
                ) {
                    session.handle(command, &mut self.events);
                    view_changed = true;
                    if self.diag.enabled() {
                        let after = session.viewport().scroll;
                        let viewport = session.viewport().size;
                        self.diag.line(format!(
                            "[滚动] 滚轮 delta=({:.1},{:.1}) scroll ({:.1},{:.1})→({:.1},{:.1}) 视口={}x{} 横向布局={}",
                            scroll_delta.x, scroll_delta.y, scroll_before.x, scroll_before.y,
                            after.x, after.y, viewport.width, viewport.height,
                            self.horizontal_layout
                        ));
                    }
                }
                if let Some(command) = command_for_zoom_delta(zoom_delta, session.state().zoom) {
                    session.handle(command, &mut self.events);
                    view_changed = true;
                }
                if view_changed {
                    // 视图刚被滚动/缩放 → 静默一段时间，期间不显示悬停预览（用户要求）
                    self.scroll_quiet_until = Some(self.clock.now() + HOVER_SCROLL_QUIET);
                    ui.ctx().request_repaint();
                }
            }

            // ---- 拖动**当帧生效** ----
            //
            // 拖动命令原先要等下一帧 `apply_and_pump` 才执行，于是画面永远落后指针一帧；
            // 快速拖动时就是上百像素的偏差（用户反馈"落后鼠标移动过多"）。
            // 这里在读取 scroll / 绘制**之前**先处理拖动，内容与指针同帧同步。
            let inverted_now = ui.input(|input| input.modifiers.shift);
            let select_now = self.annotation_tool.is_some()
                || match self.tool {
                    Tool::Select => !inverted_now,
                    Tool::Pan => inverted_now,
                    Tool::Ink => false,
                };
            // 墨迹工具独占指针：拖拽既不滚页也不选字（S4b）；便签模式下不画墨迹。
            if self.tool == Tool::Ink && !self.note_pin {
                self.handle_ink_pointer(ui, canvas, &response, session, session.viewport().scroll);
            } else if self.capture.is_none() && response.drag_started() && !self.note_pin {
                self.selection_started_on_text = false;
                self.drag_started_at = Some(self.clock.now());
                self.last_pan_at = None;
                if self.diag.enabled() {
                    self.diag.line(format!(
                        "[时序] 拖动开始 t={:.6}s",
                        self.started_at.elapsed().as_secs_f64()
                    ));
                }
                // Alt+拖拽墨迹 = 平移（v1 只支持墨迹）。命中才进入，否则照旧选字/平移。
                // Alt 在不少 Linux 桌面被窗口管理器抢去拖窗口，所以同时支持 Ctrl；
                // 也可以先用右键菜单「移动」选中，再直接拖。
                let (alt, ctrl) = ui.input(|input| (input.modifiers.alt, input.modifiers.ctrl));
                let armed = self.move_ink_armed;
                let mut moving = false;
                if (alt || ctrl || armed.is_some())
                    && self.annotation_tool.is_none()
                    && matches!(self.tool, Tool::Select | Tool::Pan)
                {
                    if let Some(position) = response.interact_pointer_pos() {
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
                        moving = self.begin_ink_move(session, content, armed);
                    }
                }
                self.move_ink_armed = None;
                if moving {
                    self.drag = Some(DragMode::MoveInk);
                } else if let Some(position) = response.interact_pointer_pos() {
                    let content =
                        canvas_to_content(canvas, position, session.viewport().scroll);
                    let hit = session.text_position_at(content);
                    match (select_now, hit) {
                        (true, Some(text_position)) => {
                            self.drag = Some(DragMode::Select);
                            self.selection_started_on_text = true;
                            session.handle(
                                ViewerCommand::SetSelectionGranularity(SelectionGranularity::Char),
                                &mut self.events,
                            );
                            session.handle(
                                ViewerCommand::SelectBegin(text_position),
                                &mut self.events,
                            );
                        }
                        // 选字模式下即使起点不在文字上，也**不做平移**（用户反馈）：
                        // 拖动过程中指针移到文字上时仍可继续扩选；不在文字上则什么也不发生。
                        (true, None) => self.drag = Some(DragMode::Select),
                        _ => self.drag = Some(DragMode::Pan),
                    }
                }
            }
            if self.capture.is_none() && response.dragged() {
                ui.ctx().request_repaint();
                match self.drag {
                    Some(DragMode::Pan) => {
                        let delta = response.drag_delta();
                        let before = session.viewport().scroll;
                        session.handle(
                            ViewerCommand::ScrollBy(Vec2::new(-delta.x, -delta.y)),
                            &mut self.events,
                        );
                        let after = session.viewport().scroll;
                        // 拖动也是"视图在动" → 同样静默，避免预览在拖动中闪现
                        self.scroll_quiet_until =
                            Some(self.clock.now() + HOVER_SCROLL_QUIET);
                        if self.diag.enabled() && after != before {
                            let now = self.clock.now();
                            let since_start = self
                                .drag_started_at
                                .map(|start| now.duration_since(start).as_secs_f64() * 1000.0)
                                .unwrap_or(0.0);
                            let since_last = self
                                .last_pan_at
                                .map(|last| now.duration_since(last).as_secs_f64() * 1000.0)
                                .unwrap_or(f64::NAN);
                            self.diag.line(format!(
                                "[时序] 拖动位移生效 t={:.6}s Δ起={:.1}ms Δ上次={:.1}ms scroll=({:.1},{:.1})",
                                self.started_at.elapsed().as_secs_f64(),
                                since_start,
                                since_last,
                                after.x,
                                after.y
                            ));
                            self.last_pan_at = Some(now);
                            // "因为拖动而重新决定显示内容"= 可见页集合发生变化（要换/补位图）
                            let mut visible = Vec::new();
                            session.visible_pages(&mut visible);
                            if visible != self.last_visible {
                                self.diag.line(format!(
                                    "[时序] 可见页集变化 t={:.6}s Δ起={:.1}ms 可见页={:?}",
                                    self.started_at.elapsed().as_secs_f64(),
                                    since_start,
                                    visible.iter().map(|page| page.display_number()).collect::<Vec<_>>()
                                ));
                                self.last_visible = visible;
                            }
                        }
                    }
                    Some(DragMode::Select) => {
                        if let Some(position) = response.interact_pointer_pos() {
                            let content =
                                canvas_to_content(canvas, position, session.viewport().scroll);
                            if let Some(text_position) = session.text_position_at(content) {
                                session.handle(
                                    ViewerCommand::SelectExtend(text_position),
                                    &mut self.events,
                                );
                            }
                        }
                    }
                    // 墨迹已在上面由 `handle_ink_pointer` 逐帧累积采样点
                    Some(DragMode::MoveInk) => {
                        if let Some(position) = response.interact_pointer_pos() {
                            let content =
                                canvas_to_content(canvas, position, session.viewport().scroll);
                            self.update_ink_move(session, content);
                        }
                    }
                    Some(DragMode::Ink) => {}
                    None => {}
                }
            }
            let size = canvas.size();

            // 视口变化 → 交给会话（会话据此夹取滚动、更新当前页）
            let viewport = *session.viewport();
            if (viewport.size.width - size.x).abs() > 0.5
                || (viewport.size.height - size.y).abs() > 0.5
            {
                self.commands
                    .push(ViewerCommand::ResizeViewport(Size::new(size.x, size.y)));
            }

            let scroll = session.viewport().scroll;

            // 1) 页面
            let visible = self.visible.clone();
            let previewing = self.previewing();
            let mut report: Vec<String> = Vec::new();
            for page in visible {
                let Some(placement) = session.placement(page).copied() else {
                    report.push(format!("p{}: 无布局", page.display_number()));
                    continue;
                };
                let page_rect = view_rect_for(&placement, scroll);
                let screen = egui::Rect::from_min_size(
                    canvas.min + EVec2::new(page_rect.x, page_rect.y),
                    EVec2::new(page_rect.width, page_rect.height),
                );
                // 先铺"纸张"：任何未就绪的区域露出的是白纸，而不是深色画布（D-06 的兜底）
                paint_placeholder(&painter, screen);

                // D-94：整页与瓦片的键都用**波段渲染比例**（与 session 完全一致）
                let render = session.render_scale(page);
                let whole_key = RenderKey::new(page, render, placement.rotation);
                // 可见范围与网格只取一次：决策与绘制必须用**同一份**，否则会出现
                // "永远差一块"→ 永远停在预览（见 view::ready_tiles_in_view 的说明）。
                let tile_area = match (session.tile_grid(page), session.visible_tiles(page)) {
                    (Some(grid), Some(range)) => Some((grid, range)),
                    _ => None,
                };
                let (ready, total) = match &tile_area {
                    Some((grid, range)) => self.textures.ready_tiles_in_view(
                        page,
                        placement.rotation,
                        render,
                        grid,
                        *range,
                    ),
                    None => (0, 0),
                };
                let preview =
                    self.textures.preview_source(page, placement.rotation, whole_key.scale_milli);
                // D-96：波段切换后"旧内容 vs 新块"的判据（缺一块就不翻页，另有超时兜底）
                let waited_ms = self.note_band_wait(page, whole_key.scale_milli, ready);
                // D-104：同一波段内已经切到瓦片就不再退回预览（否则波段内滚动会整页变糊）
                let showing_tiles = self.last_plan.get(&page).is_some_and(|stamp| {
                    stamp.band == whole_key.scale_milli
                        && matches!(stamp.plan, PageDraw::Tiles)
                });
                let plan = plan_page(&DrawInput {
                    tiled: tile_area.is_some(),
                    has_whole_current: self.textures.contains(&whole_key),
                    ready,
                    total,
                    preview,
                    waited_ms,
                    showing_tiles,
                });

                let mut drawn = (0u32, 0u32);
                // 这一帧实际画了几张旧内容（整页拉伸 = 1，旧块拼 = 块数，底料 = +1）。
                let mut layers = 0u32;

                // 底料（D-98/D-99）：**覆盖优先**的旧内容，保证"缺的地方绝不露白"。
                //
                // 为什么不只在 `Tiles` 分支铺：预览层自己也可能不完整 —— 连续放大时
                // 上一波段往往只画完一两块就被下一次缩放取消（`set_zoom` 会 `cancel_all`），
                // 这时 `TilePreview` 只覆盖页面的一小块，其余位置没有任何图层（实测白块）。
                // 底料是一张整页图（单一比例、单一映射，铺满整个页面矩形），
                // 与块/预览层都对齐到同一个页面矩形，因此只有清晰度差、没有几何错位。
                let underlay = if needs_underlay(plan, ready, total) {
                    self.textures.underlay_source(
                        page,
                        placement.rotation,
                        whole_key.scale_milli,
                    )
                } else {
                    Preview::None
                };
                if underlay != Preview::None {
                    layers += paint_preview(
                        &painter,
                        canvas.min,
                        &self.textures,
                        page,
                        &placement,
                        page_rect,
                        screen,
                        underlay,
                    );
                }

                match plan {
                    PageDraw::Whole => {
                        let texture = self.textures.get(&whole_key).expect("已判定存在");
                        paint_image(&painter, texture, screen);
                        drawn = (1, 1);
                    }
                    // 画的就是 plan 决定的那一层（比例原样透传，与 `preview` 同源）；
                    // 与底料相同就不必重复画一遍。
                    PageDraw::Stretch(scale_milli) => {
                        if underlay != Preview::Whole(scale_milli) {
                            layers += paint_preview(
                                &painter,
                                canvas.min,
                                &self.textures,
                                page,
                                &placement,
                                page_rect,
                                screen,
                                Preview::Whole(scale_milli),
                            );
                        }
                    }
                    PageDraw::TilePreview(scale_milli) => {
                        if underlay != Preview::Tiles(scale_milli) {
                            layers += paint_preview(
                                &painter,
                                canvas.min,
                                &self.textures,
                                page,
                                &placement,
                                page_rect,
                                screen,
                                Preview::Tiles(scale_milli),
                            );
                        }
                    }
                    PageDraw::Tiles => {
                        // 视口覆盖到的**全部**块都要画：就绪的贴图叠在底料上。
                        // 位置一律由 TileGrid 给出（`页.x + 块像素·u`），因此同一波段内
                        // 无论显示比例怎么变，块之间的接缝都不会变（D-94）。
                        // **缺块处不再补白**：底料已经铺在下面（D-98）。
                        // 只有显式设成 1/true 才描线：默认、空值、其它值都不画。
                        let tile_borders = std::env::var("PDF_READER_TILE_BORDERS")
                            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                            .unwrap_or(false);
                        if let Some((grid, (col0, row0, col1, row1))) = &tile_area {
                            for row in *row0..=*row1 {
                                for col in *col0..=*col1 {
                                    if !grid.contains(col, row) {
                                        continue;
                                    }
                                    let tile = TileSpec::new(col, row);
                                    let rect = tile_view_rect(
                                        grid, &placement, col, row, render.0, scroll,
                                    );
                                    let key =
                                        RenderKey::tiled(page, render, placement.rotation, tile);
                                    let tile_screen = egui::Rect::from_min_size(
                                        canvas.min + EVec2::new(rect.x, rect.y),
                                        EVec2::new(rect.width, rect.height),
                                    );
                                    if let Some(texture) = self.textures.get(&key) {
                                        paint_image(&painter, texture, tile_screen);
                                    }
                                    // 临时诊断（D-89）：给每块描 2px 红线，便于肉眼分辨块范围。
                                    // **默认关闭**；设 PDF_READER_TILE_BORDERS=1 开启。
                                    if tile_borders {
                                        painter.rect_stroke(
                                            tile_screen,
                                            CornerRadius::ZERO,
                                            Stroke::new(2.0, Color32::from_rgb(230, 30, 30)),
                                            StrokeKind::Inside,
                                        );
                                    }
                                }
                            }
                        }
                        drawn = (ready as u32, total as u32);
                    }
                    PageDraw::Blank => {
                        // 纸张（白底 + 页框）在循环开头已经铺好，这里无需再画任何东西 ——
                        // 尤其不要画块边框（那会变成满屏网格线）。
                    }
                }

                // 画法记账：**无条件**更新（很便宜），供 `[画法]` 边沿触发与回放断言共用。
                let stamp = PlanStamp {
                    plan,
                    band: whole_key.scale_milli,
                    partial: plan == PageDraw::Tiles && total > 0 && ready < total,
                    ready,
                    total,
                    waited_ms,
                    preview,
                };
                let previous = self.last_plan.insert(page, stamp).filter(|prev| *prev != stamp);
                if let Some(previous) = previous.filter(|_| self.diag.enabled()) {
                    // 边沿触发：画法一变就立刻打一行（不受 1 秒节流），
                    // 白块到底是"没渲染好"还是"被旧内容挤掉"一眼可见（D-96）。
                    let line = format!(
                        "[画法] p{} 波段={} 从={} 到={} 就绪={}/{} 等待={}ms 可见={:?} 预览={:?} 层={} 底料={:?}",
                        page.display_number(),
                        stamp.band,
                        previous.describe(),
                        stamp.describe(),
                        ready,
                        total,
                        waited_ms,
                        tile_area.as_ref().map(|(_, range)| *range),
                        preview,
                        layers,
                        underlay
                    );
                    self.diag.line(line);
                }
                if self.diag.enabled() {
                    report.push(format!(
                        "p{} 画法={} 块={}/{} 网格={} 渲染比例={:.3} 等待={}ms 预览={:?} 底料={:?} 视图矩形=({:.0},{:.0} {:.0}×{:.0})",
                        page.display_number(),
                        plan.label(),
                        drawn.0,
                        drawn.1,
                        tile_area.as_ref().map(|(grid, _)| grid.side).unwrap_or(0),
                        render.0,
                        waited_ms,
                        preview,
                        underlay,
                        page_rect.x,
                        page_rect.y,
                        page_rect.width,
                        page_rect.height
                    ));
                }
            }

            // 诊断输出（PDF_READER_DIAGNOSE=1）：每秒一次，足够定位"看不到内容"这类问题
            if self.diag.enabled()
                && self.clock.now().saturating_duration_since(self.last_report)
                    >= std::time::Duration::from_secs(1)
            {
                self.last_report = self.clock.now();
                let max_side = ui.ctx().input(|input| input.max_texture_side);
                self.diag.line(format!(
                    "[诊断] 缩放={:.0}% 视口={:.0}×{:.0} scroll=({:.1},{:.1}) 当前页={} 可见页={:?} \
                     纹理={}条/{:.1}MB(A{:.1}/B{:.1}/C{:.1}) 预算={:.0}MB 位图={}条/{:.1}MB 在飞={} 预览中={} max_texture_side={} | {}",
                    session.state().zoom * 100.0,
                    viewport.size.width,
                    viewport.size.height,
                    scroll.x,
                    scroll.y,
                    session.state().current_page.display_number(),
                    self.visible.iter().map(|page| page.display_number()).collect::<Vec<_>>(),
                    self.textures.len(),
                    self.textures.bytes() as f64 / (1024.0 * 1024.0),
                    self.last_retain.must_bytes as f64 / (1024.0 * 1024.0),
                    self.last_retain.bed_bytes as f64 / (1024.0 * 1024.0),
                    self.last_retain.old_bytes as f64 / (1024.0 * 1024.0),
                    self.textures.max_bytes() as f64 / (1024.0 * 1024.0),
                    session.cache_bytes().1,
                    session.cache_bytes().0 as f64 / (1024.0 * 1024.0),
                    session.pending_renders(),
                    previewing,
                    max_side,
                    report.join(" | ")
                ));
            }

            // 2) 高亮（选区 + 搜索命中）
            for highlight in session.highlight_selection() {
                paint_highlight(&painter, session, &highlight, scroll, canvas.min);
            }
            let active = self.active_hit;
            for highlight in session.highlights_for_search(&self.hits, active) {
                paint_highlight(&painter, session, &highlight, scroll, canvas.min);
            }

            // 链接热区诊断（PDF_READER_DIAGNOSE=1）：热区越界 = 坐标空间不一致
            if self.diag.enabled() {
                if let Some(navigation) = &self.navigation {
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else { continue };
                        let links = navigation.links_on(page);
                        if links.is_empty() {
                            continue;
                        }
                        let size = placement.page_size;
                        let first = links[0].rect;
                        self.diag.line(format!(
                            "[链接] p{} 页尺寸={:.1}×{:.1} 热区数={} 首个=({:.1},{:.1} {:.1}×{:.1}) 越界(粗查,只能发现大偏移)={}",
                            page.display_number(),
                            size.width,
                            size.height,
                            links.len(),
                            first.x,
                            first.y,
                            first.width,
                            first.height,
                            if link_rect_in_page_bounds(first, size) { "否" } else { "是" }
                        ));
                    }
                }
            }

            // 2b) 链接热区框（可开关；缩放过小时不画）
            if self.capture.is_none()
                && self.custom_pin.is_none()
                && link_overlay_visible(self.show_links, session.state().zoom, LINK_OVERLAY_MIN_ZOOM)
            {
                if let Some(navigation) = &self.navigation {
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else { continue };
                        for link in navigation.links_on(page) {
                            // 与高亮同一条换算链：PDF 页面空间 → 内容空间 → 视图空间
                            let content = placement.rect_to_content(link.rect);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                                EVec2::new(content.width, content.height),
                            );
                            painter.rect_stroke(
                                screen,
                                CornerRadius::ZERO,
                                Stroke::new(1.5, link_box_color(&link.destination)),
                                StrokeKind::Outside,
                            );
                        }
                    }
                }
            }

            // 2a'') 右键：记录指针下的注释；菜单由 draw_annotation_context_menu 绘制。
            // 用原始指针事件，不受 popup 捕获影响；在另一条注释上右键可直接切换。
            if self.capture.is_none() && ui.input(|i| i.pointer.secondary_clicked()) {
                if let Some(position) = ui.input(|i| i.pointer.interact_pos()) {
                    if canvas.contains(position) {
                        let content = canvas_to_content(canvas, position, scroll);
                        let id = self.annotation_at(session, content);
                        self.context_annotation = id;
                        self.context_menu_pos = id.map(|_| position);
                    }
                }
            }

            // 2b') 标注叠加层（D-118）：几何在页面空间，这里换算到屏幕
            if self.show_annotations && self.capture.is_none() {
                let pointer = ui.ctx().pointer_hover_pos();
                self.hovered_annotation = None; // 每帧重算（指针离开后立即失效）
                for page in self.visible.clone() {
                    let Some(placement) = session.placement(page).copied() else { continue };
                    for annotation in self.annotations.on_page(page) {
                        let alpha = (annotation.clamped_opacity() * 255.0).round() as u8;
                        let [r, g, b] = annotation.color;
                        let color = Color32::from_rgba_unmultiplied(r, g, b, alpha);
                        // 墨迹：折线的每个顶点都要换算（含旋转），所以不能走下面的"按矩形"循环。
                        // 线宽乘布局比例，高倍下才跟着变粗；悬停命中走与点击相同的折线距离判定。
                        if let Some((ink_points, ink_width)) = annotation.kind.ink() {
                            let stroke_width = (ink_width * placement.scale.0).max(1.0);
                            paint_ink(
                                &painter,
                                &placement,
                                canvas,
                                scroll,
                                ink_points,
                                stroke_width,
                                color,
                            );
                            if let Some(pointer) = pointer {
                                let content_point = Point::new(
                                    pointer.x - canvas.min.x + scroll.x,
                                    pointer.y - canvas.min.y + scroll.y,
                                );
                                let tolerance =
                                    ANNOTATION_HIT_TOLERANCE_PX / placement.scale.0.max(0.01);
                                if placement.rect.contains(content_point)
                                    && annotation.contains(
                                        placement.content_to_page(content_point),
                                        tolerance,
                                    )
                                {
                                    // 命中/删除只看几何，与"有没有正文"无关
                                    self.hovered_annotation = Some(annotation.id);
                                    if let Some(window) = self
                                        .floats
                                        .iter()
                                        .find(|window| {
                                            window.owner == FloatOwner::Annotation(annotation.id)
                                        })
                                    {
                                        self.highlight_float = Some(window.id);
                                    }
                                }
                            }
                            continue;
                        }
                        // 便签：没有 quads，单独画图标（图标 = 便签矩形；正文在浮窗里）。
                        if let AnnotationKind::Note { rect } = annotation.kind {
                            let content = placement.rect_to_content(rect);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                                EVec2::new(content.width, content.height),
                            );
                            paint_note_icon(&painter, screen, color);
                            if let Some(pointer) = pointer {
                                if screen.expand(2.0).contains(pointer) {
                                    self.hovered_annotation = Some(annotation.id);
                                    if let Some(window) = self
                                        .floats
                                        .iter()
                                        .find(|window| {
                                            window.owner == FloatOwner::Annotation(annotation.id)
                                        })
                                    {
                                        self.highlight_float = Some(window.id);
                                    }
                                }
                            }
                            continue;
                        }
                        for quad in annotation.kind.quads() {
                            let content = placement.rect_to_content(*quad);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                                EVec2::new(content.width, content.height),
                            );
                            let to_screen = |point: Point| {
                                let content = placement.page_to_content(point);
                                egui::pos2(
                                    canvas.min.x + content.x - scroll.x,
                                    canvas.min.y + content.y - scroll.y,
                                )
                            };
                            // 悬停：把对应浮窗高亮一帧（离开即熄灭；点击另有时长高亮）
                            if let Some(pointer) = pointer {
                                if screen.contains(pointer) {
                                    // 命中/删除只看几何，与"有没有正文"无关
                                    self.hovered_annotation = Some(annotation.id);
                                    if let Some(window) = self
                                        .floats
                                        .iter()
                                        .find(|window| {
                                            window.owner == FloatOwner::Annotation(annotation.id)
                                        })
                                    {
                                        self.highlight_float = Some(window.id);
                                    }
                                }
                            }
                            match annotation.kind {
                                AnnotationKind::Highlight { .. } => {
                                    painter.rect_filled(screen, CornerRadius::ZERO, color);
                                }
                                AnnotationKind::Underline { .. } => {
                                    let width = (pdfcore::annotation::TEXT_MARK_LINE_WIDTH
                                        * placement.scale.0)
                                        .max(1.0);
                                    // 端点逐个换算，旋转页上线条方向才不会错。
                                    let y = quad.y - pdfcore::annotation::UNDERLINE_GAP;
                                    painter.line_segment(
                                        [
                                            to_screen(Point::new(quad.x, y)),
                                            to_screen(Point::new(quad.x + quad.width, y)),
                                        ],
                                        Stroke::new(width, color),
                                    );
                                }
                                AnnotationKind::StrikeOut { .. } => {
                                    let width = (pdfcore::annotation::TEXT_MARK_LINE_WIDTH
                                        * placement.scale.0)
                                        .max(1.0);
                                    let y = quad.y
                                        + quad.height
                                            * pdfcore::annotation::STRIKE_FRACTION;
                                    painter.line_segment(
                                        [
                                            to_screen(Point::new(quad.x, y)),
                                            to_screen(Point::new(quad.x + quad.width, y)),
                                        ],
                                        Stroke::new(width, color),
                                    );
                                }
                                // 墨迹/便签在上面画完了，且没有 quads，不会进入本循环。
                                AnnotationKind::Ink { .. } | AnnotationKind::Note { .. } => {}
                            }
                        }
                    }
                }

                // 列表定位后的中性轮廓：不改注释颜色，只做短时定位反馈。
                if let Some((id, until)) = self.annotation_focus {
                    if self.clock.now() >= until {
                        self.annotation_focus = None;
                    } else if let Some(annotation) = self.annotations.get(id) {
                        if let (Some(bounds), Some(placement)) =
                            (annotation.bounds(), session.placement(annotation.page))
                        {
                            let content = placement.rect_to_content(bounds);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                                EVec2::new(content.width, content.height),
                            )
                            .expand(3.0);
                            painter.rect_stroke(
                                screen,
                                CornerRadius::same(2),
                                Stroke::new(2.0, Color32::WHITE),
                                StrokeKind::Outside,
                            );
                            painter.rect_stroke(
                                screen.shrink(1.0),
                                CornerRadius::same(2),
                                Stroke::new(1.0, Color32::from_rgb(30, 30, 30)),
                                StrokeKind::Inside,
                            );
                            ui.ctx().request_repaint();
                        }
                    }
                }

                // 正在画的一笔：还没进标注集合，但必须**跟手显示**（否则用户以为没反应）
                if let Some(stroke) = &self.ink {
                    if let Some(placement) = session.placement(stroke.page()).copied() {
                        let alpha = (INK_OPACITY * 255.0).round() as u8;
                        let color = Color32::from_rgba_unmultiplied(
                            INK_COLOR[0],
                            INK_COLOR[1],
                            INK_COLOR[2],
                            alpha,
                        );
                        let width = (stroke.width() * placement.scale.0).max(1.0);
                        paint_ink(
                            &painter,
                            &placement,
                            canvas,
                            scroll,
                            stroke.points(),
                            width,
                            color,
                        );
                    }
                }
            }

            // 2c) 被引用的点（链接跳转目的地）：按位置去重后画记号；
            //     与链接框同样在缩放过小时不显示（用户要求）。
            if self.capture.is_none()
                && self.custom_pin.is_none()
                && link_overlay_visible(
                    self.show_dest_points,
                    session.state().zoom,
                    LINK_OVERLAY_MIN_ZOOM,
                )
            {
                let mut clicked: Option<(PageIndex, usize)> = None;
                for page in self.visible.clone() {
                    let Some(placement) = session.placement(page).copied() else { continue };
                    let Some(marks) = self.dest_marks.get(page.get() as usize) else { continue };
                    for (index, mark) in marks.iter().enumerate() {
                        // PDF 页面空间 → 内容空间 → 视图空间
                        let content = placement.page_to_content(mark.point);
                        let center =
                            canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y);
                        // 有截图的点画实心（一眼可辨），没有的画空心
                        painter.circle_stroke(center, 5.0, Stroke::new(1.5, LINK_OVERLAY_COLOR));
                        if mark.snapshot.is_some() {
                            painter.circle_filled(center, 2.5, LINK_OVERLAY_COLOR);
                        } else {
                            painter.circle_filled(center, 1.5, LINK_OVERLAY_COLOR);
                        }
                        // 点击标识 → 进入截图模式，选区初值 = 该点已存的矩形
                        let hit = egui::Rect::from_center_size(center, EVec2::splat(14.0));
                        let response = ui.interact(
                            hit,
                            Id::unique(("dest-mark", page.get(), index)),
                            Sense::click(),
                        );
                        if response.clicked() {
                            clicked = Some((page, index));
                        }
                        if !mark.image_id.is_empty() {
                            let record_id = mark.image_id.clone();
                            let revision = mark.image_revision;
                            response.context_menu(|ui| {
                                if ui.button("复制图片引用").clicked() {
                                    ui.ctx().copy_text(crate::image_store::image_uri(
                                        &record_id, revision,
                                    ));
                                    ui.close();
                                }
                            });
                        }
                    }
                }
                if let Some(target) = clicked {
                    // 用该点已存矩形初始化选区（页面空间 → 屏幕空间）
                    let page = target.0;
                    let mut selection = None;
                    if let Some(placement) = session.placement(page).copied() {
                        if let Some(mark) = self
                            .dest_marks
                            .get(page.get() as usize)
                            .and_then(|marks| marks.get(target.1))
                        {
                            selection = Some(page_rect_to_screen(
                                &placement,
                                mark.rect,
                                scroll,
                                canvas.min,
                            ));
                        }
                    }
                    self.capture = Some(CaptureState {
                        rect: selection,
                        anchor: None,
                        target: CaptureTarget::DestMark(target.0, target.1),
                    });
                    self.message = format!(
                        "编辑被引用点（第 {} 页）：可拖动调整选区，Enter 保存截图，Esc 取消",
                        page.display_number()
                    );
                }
            }

            // 3) 交互
            //
            // 在画布上按下时主动结束文本输入：否则工具栏的搜索框仍持有焦点，
            // `Ctrl+C` 会被它吃掉，用户看到的现象是"选中了却复制不出来"（人工验收反馈）。
            if response.drag_started()
                || response.clicked()
                || response.double_clicked()
                || response.triple_clicked()
            {
                ui.memory_mut(|memory| memory.stop_text_input());
            }

            // 当前生效的拖拽语义：按住 Shift 临时**反选**工具，
            // 这样想顺手复制一句时不必先切换工具（"自动"模式已按反馈移除）。
            let inverted = ui.input(|input| input.modifiers.shift);
            let select_tool = match self.tool {
                Tool::Select => !inverted,
                Tool::Pan => inverted,
                Tool::Ink => false,
            };

            // drag_started / dragged 已在绘制之前处理（当帧生效），此处不重复
            if response.dragged() {
                // 拖动期间必须持续重绘：命令在**下一帧**才生效，
                // 若这一帧之后没有新的输入事件（鼠标停住不动），画面就不会更新 ——
                // 表现出来就是"拖动过程中视野不动、松手才跳一下"。
                ui.ctx().request_repaint();
                match self.drag {
                    Some(DragMode::Select) => {
                        if let Some(position) = response.interact_pointer_pos() {
                            let content = canvas_to_content(canvas, position, scroll);
                            if let Some(text_position) = session.text_position_at(content) {
                                self.commands.push(ViewerCommand::SelectExtend(text_position));
                            }
                        }
                    }
                    Some(DragMode::MoveInk) => {
                        if let Some(position) = response.interact_pointer_pos() {
                            let content =
                                canvas_to_content(canvas, position, session.viewport().scroll);
                            self.update_ink_move(session, content);
                        }
                    }
                    Some(DragMode::Pan) | Some(DragMode::Ink) => {
                        // 已在绘制之前当帧处理（见"拖动当帧生效"），这里不再重复提交
                    }
                    None => {}
                }
            }

            // 拖动过程诊断：把 egui 报的状态与下一帧要执行的滚动量打出来（PDF_READER_DIAGNOSE=1）
            if self.diag.enabled()
                && (response.drag_started()
                    || response.dragged()
                    || response.drag_stopped()
                    || response.clicked())
            {
                self.diag.line(format!(
                    "[拖动] started={} dragged={} stopped={} clicked={} 模式={:?} 指针={:?} delta={:?} 当前scroll=({:.1},{:.1})",
                    response.drag_started(),
                    response.dragged(),
                    response.drag_stopped(),
                    response.clicked(),
                    self.drag,
                    response.interact_pointer_pos().map(|p| (p.x, p.y)),
                    response.drag_delta(),
                    scroll.x,
                    scroll.y
                ));
            }
            if response.drag_stopped() {
                let apply_markup = self.drag == Some(DragMode::Select)
                    && self.selection_started_on_text
                    && self.annotation_tool.is_some();
                self.selection_started_on_text = false;
                if self.diag.enabled() {
                    let total = self
                        .drag_started_at
                        .map(|start| start.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or(0.0);
                    self.diag.line(format!(
                        "[时序] 拖动结束 t={:.6}s 全程={:.1}ms",
                        self.started_at.elapsed().as_secs_f64(),
                        total
                    ));
                }
                if self.drag == Some(DragMode::MoveInk) {
                    self.finish_ink_move();
                }
                self.drag = None;
                self.drag_started_at = None;
                self.last_pan_at = None;
                if apply_markup {
                    self.pending_annotate = self.annotation_tool;
                }
            }
            if self.capture.is_none() && response.double_clicked() {
                if let Some(position) = response.interact_pointer_pos() {
                    let content = canvas_to_content(canvas, position, scroll);
                    if let Some(text_position) = session.text_position_at(content) {
                        self.commands.push(ViewerCommand::SetSelectionGranularity(
                            SelectionGranularity::Word,
                        ));
                        self.commands.push(ViewerCommand::SelectBegin(text_position));
                    }
                }
            } else if self.capture.is_none() && self.note_pin && response.clicked() {
                if let Some(position) = response.interact_pointer_pos() {
                    self.finish_note_pin(session, canvas, scroll, position);
                }
            } else if self.capture.is_none() && self.custom_pin.is_some() && response.clicked() {
                // Pin 状态：点击页面某处 → 结束选点，弹出输入对话框
                if let Some(position) = response.interact_pointer_pos() {
                    self.finish_pin(session, canvas, scroll, position);
                }
            } else if self.capture.is_none() && response.clicked() {
                if let Some(position) = response.interact_pointer_pos() {
                    let content = canvas_to_content(canvas, position, scroll);
                    // 标注最先（它画在链接框之上，用户点到的就是它）；其次是链接热区
                    if let Some(id) = self.annotation_at(session, content) {
                        self.open_annotation_note(id, position);
                    } else if !self.follow_link(session, content) && select_tool {
                        // 选字工具下：点空白处清空选择，点到文字则重新落点。
                        // v1 无条件清空，于是"选完随手点一下"就会把选区清掉、复制扑空。
                        match session.text_position_at(content) {
                            Some(text_position) => {
                                self.commands.push(ViewerCommand::SetSelectionGranularity(
                                    SelectionGranularity::Char,
                                ));
                                self.commands.push(ViewerCommand::SelectBegin(text_position));
                            }
                            None => self.commands.push(ViewerCommand::SelectClear),
                        }
                    }
                }
            }

            // 记下画布矩形：侧栏（在该函数之前绘制）需要它来给浮窗定位
            self.canvas_rect = canvas;

            // 悬停预览：**先只做检测与状态更新**（含"同一目的地已固定 → 高亮 + 置顶"）
            if self.capture.is_none() && self.custom_pin.is_none() {
                self.update_hover_preview(ui, session, canvas, scroll);
            }

            // 浮窗尺寸变化 → 回写正文尺寸到来源数据（用户要求：改大小即记住正文面积）
            // 先收集，再统一走 apply_edit，保证一次 resize 进入撤销栈。
            let mut preview_resizes: Vec<(PreviewKey, egui::Vec2, egui::Vec2)> = Vec::new();
            let mut dest_resizes: Vec<(PageIndex, usize, DestMark, DestMark)> = Vec::new();
            for window in std::mem::take(&mut self.floats) {
                let body = window.body;
                if let Some((page, index)) = window.origin {
                    if let Some(mark) = self
                        .dest_marks
                        .get(page.get() as usize)
                        .and_then(|marks| marks.get(index))
                    {
                        if body_size_changed(mark.body_size, body) {
                            let before = mark.clone();
                            let mut after = before.clone();
                            after.body_size = body;
                            dest_resizes.push((page, index, before, after));
                        }
                    }
                }
                if let Some(index) = window.custom_index() {
                    if let Some(preview) = self.custom_previews.get(index) {
                        if body_size_changed(preview.body_size, body) {
                            preview_resizes.push((
                                PreviewKey {
                                    page: preview.page,
                                    id: preview.id,
                                },
                                preview.body_size,
                                body,
                            ));
                        }
                    }
                }
                self.floats.push(window);
            }
            for (key, before, after) in preview_resizes {
                self.apply_preview_resize(key, before, after);
            }
            for (page, index, before, after) in dest_resizes {
                self.apply_edit(
                    AppEdit::EditDestMark {
                        page,
                        index,
                        before: std::sync::Arc::new(before),
                        after: std::sync::Arc::new(after),
                    },
                    true,
                );
            }

            // 固定浮窗：绘制与注册都在此；后加入的在更上层（见 draw_floats）
            if self.capture.is_none() && self.custom_pin.is_none() {
                // 正在显示、但还没拿到预览图的自定义图片 preview：每帧重试一次。
                // 旧实现只在"悬浮/点击侧栏条目"时重试，于是"点了但渲染还没好"的浮窗
                // 会一直空着（D-101 把初次生成改成了"提交渲染 + 等待"，必须有重试入口）。
                self.ensure_pending_custom_snapshots(ui, session);
                self.ensure_markdown_image_owners(ui, session);
                self.draw_floats(ui, canvas);
                // 正文编辑：只检测双击并打开正式编辑窗口（编辑窗口在 draw_edit_dialogs 里）
                self.detect_body_double_click(ui);
                // 临时预览**最后**绘制 → 真正位于最上层（用户要求）
                self.draw_hover_preview(ui, canvas);
                // 侧栏条目悬浮 → 主视图右侧的临时浮窗
                self.draw_sidebar_hover_float(ui, canvas);
                // 公式失败只做短暂状态提示；下一条消息会直接顶替。
                self.take_math_error();
                // Markdown 链接只提示，不打开浏览器（与 PDF URI 的行为一致）。
                self.consume_markdown_links(ui.ctx());
            } else if self.capture.is_some() {
                self.draw_capture(ui, canvas, session, scroll);
            }
        });
        self.draw_annotation_context_menu(ui.ctx());
    }

    /// 添加一个"测试浮窗"（带任意文字），位置按已有浮窗数量级联偏移。
    ///
    /// 入口在顶栏第二行（「测试浮窗」按钮）；用于不依赖文档内容地验证浮窗的
    /// 拖动/改大小/关闭/层级。
    fn add_test_float(&mut self) {
        let id = self.next_float_id;
        self.next_float_id += 1;
        let cascade = ((id as f32 - 1.0) % 6.0) * 24.0;
        let rect = egui::Rect::from_min_size(
            egui::pos2(24.0 + cascade, 24.0 + cascade),
            EVec2::new(280.0, 150.0),
        );
        self.floats.push(Float {
            id,
            title: format!("测试浮窗 #{id}"),
            content: FloatContent::Text(format!(
                "这是第 {id} 个测试浮窗。\n目前只显示文字。\n\n可以拖动标题栏移动、\n拖右下角改变大小、\n点右上角 ✕ 关闭。"
            )),
            origin: None,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            rect,
        });
        self.message = format!("已添加测试浮窗 #{id}");
    }

    /// 安装 `pd-img` bytes loader 与 PNG image loader。
    fn ensure_image_loaders(&self, ctx: &egui::Context) {
        if !ctx.is_loader_installed(crate::image_store::InternalImageLoader::ID) {
            ctx.add_bytes_loader(std::sync::Arc::new(
                crate::image_store::InternalImageLoader::new(self.images.clone()),
            ));
        }
        egui_extras::install_image_loaders(ctx);
    }

    fn run_edit_action(&mut self, action: EditDiscardAction, ctx: &egui::Context) {
        match action {
            EditDiscardAction::CloseEditor => self.editing_body = None,
            EditDiscardAction::CloseFloat(id) => self.close_float(id),
            EditDiscardAction::Open(path) => self.request_open(path),
            EditDiscardAction::Quit => {
                if self.is_dirty() {
                    // 正文保存后可能让文档变脏；交给已有的未保存文档提示。
                    self.pending_action = Some(PendingAction {
                        kind: PendingActionKind::Quit,
                    });
                } else {
                    self.closing_after_decision = true;
                    self.remember_document_view();
                    self.flush_settings(Some(ctx));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    /// 把截图结果存进某个**自定义图片预览**。
    ///
    /// **不再**自动生成浮窗（用户要求）：浮窗只在用户从侧栏悬浮/点击该条目时出现。
    fn commit_custom_image(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        index: usize,
        content_rect: Rect,
    ) {
        // **只保留重叠面积最大的那一页**（用户要求：跨 a-b 两页时取交叠更多的页）
        let all_pages: Vec<(usize, Rect)> = (0..session.page_count() as usize)
            .filter_map(|page_index| {
                let page = PageIndex::new(page_index as u32);
                session
                    .placement(page)
                    .map(|placement| (page_index, placement.rect))
            })
            .collect();
        // 必须与本 preview **自己所在的页**求交：否则 rect 与 page 不匹配，
        // 重新加载后会去错误的页取图（用户要求强限制）
        let Some(preview_page) = self.custom_previews.get(index).map(|preview| preview.page) else {
            return;
        };
        let Some((_, page_rect)) = all_pages
            .iter()
            .find(|(page_index, _)| *page_index == preview_page.get() as usize)
            .copied()
        else {
            self.message = "找不到该 preview 所在页".to_string();
            return;
        };
        let Some(overlap) = page_rect.intersection(content_rect) else {
            self.message = "截图区域与该 preview 所在页没有重叠，未保存".to_string();
            return;
        };
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            self.message = "截图区域与该 preview 所在页没有重叠，未保存".to_string();
            return;
        }
        let page = preview_page;
        let Some(snapshot) = self.snapshot_for(ui, session, overlap) else {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[图片预览] 合成失败：页={:?} 选区={:?} 缩放={:.3} 可见页={:?}",
                    page,
                    overlap,
                    session.state().zoom,
                    self.visible
                ));
            }
            self.message = "该区域还没有已渲染的像素（请稍候再试）".to_string();
            return;
        };
        let png = crate::image_store::encode_png(&snapshot);
        let texture = ui.ctx().load_texture(
            format!("custom-image-{index}"),
            snapshot,
            egui::TextureOptions::LINEAR,
        );
        // 记录交叠部分的矩形（页面空间）与它在**当前缩放比**下的显示尺寸
        let page_rect = session
            .placement(page)
            .and_then(|placement| view_rect_to_page_rect(placement, overlap, Vec2::ZERO))
            .map(|rect| {
                Rect::new(
                    rect.x.round(),
                    rect.y.round(),
                    rect.width.round(),
                    rect.height.round(),
                )
            })
            .unwrap_or(overlap);
        let Some(before) = self.custom_previews.get(index).cloned() else {
            return;
        };
        let key = PreviewKey {
            page: before.page,
            id: before.id,
        };
        let mut after = before.clone();
        after.snapshot = Some(texture);
        after.rect = page_rect;
        after.body_size = egui::vec2(overlap.width, overlap.height);
        after.image_revision = before.image_revision.saturating_add(1);
        if let Some(png) = png {
            let image_ref = crate::image_store::ImageRef {
                record_id: after.image_id.clone(),
                revision: after.image_revision,
            };
            self.images.insert(&image_ref, png);
            ui.ctx().forget_image(&crate::image_store::image_uri(
                &image_ref.record_id,
                image_ref.revision,
            ));
        }
        // ▲v2：记录这张图的分辨率（截图时的显示比例）—— 之后重建预览时按它请求渲染
        after.scale = session.state().zoom;
        self.apply_edit(
            AppEdit::ReplacePreview {
                key,
                before: std::sync::Arc::new(before),
                after: std::sync::Arc::new(after),
            },
            true,
        );
        self.message = format!(
            "已保存图片预览（取第 {} 页，交叠 {:.0}×{:.0}）",
            page.display_number(),
            overlap.width,
            overlap.height
        );
    }

    /// 取走公式后端的首次失败错误，写入状态栏（短暂提示，后续消息可顶替）。
    fn take_math_error(&mut self) {
        let after_save = std::mem::take(&mut self.math_recheck_pending);
        if let Some(error) = self.math_cache.borrow_mut().take_error() {
            let line = error.status_line();
            self.message = if after_save {
                format!("已更新内容；{line}")
            } else {
                line
            };
        }
    }

    /// 取走本帧 Markdown 链接产生的 `OpenUrl` 命令，改为状态栏提示。
    ///
    /// `egui_commonmark` 的外部链接最终走 `ui.hyperlink_to` → `ctx.open_url`；
    /// 项目当前对 PDF URI 也只提示、不打开浏览器（见 `follow_link`）。在
    /// `egui_commonmark` 没有链接钩子的情况下，这是官方文档给出的检查方式
    /// （`ctx.output_mut` 遍历 `OutputCommand::OpenUrl`）。
    fn consume_markdown_links(&mut self, ctx: &egui::Context) {
        if !self.markdown_rendered {
            return;
        }
        self.markdown_rendered = false;
        let mut opened = Vec::new();
        ctx.output_mut(|output| {
            output.commands.retain(|command| match command {
                egui::OutputCommand::OpenUrl(open) => {
                    opened.push(open.url.clone());
                    false
                }
                _ => true,
            });
        });
        if let Some(url) = opened.last() {
            self.message = format!("外部链接：{url}（打开浏览器暂未实现）");
        }
    }

    /// 命中当前页的链接热区则发出跳转命令，返回是否命中。
    ///
    /// 热区判断（点在哪个链接里）在核心层：[`DocumentSession::link_at`]；
    /// 这里只负责"把内容空间的点交给它"，以及把结果翻译成命令
    /// （[`crate::view::commands_for_destination`]：有落点 → 居中，无落点 → 顶部对齐）。
    fn follow_link(&mut self, session: &DocumentSession, content: Point) -> bool {
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let page_point = placement.content_to_page(content);
            // 找出命中的链接下标（登记表与 links_on 同构，因此能直接查）
            let Some(navigation) = self.navigation.clone() else {
                return false;
            };
            let hit = navigation
                .links_on(page)
                .iter()
                .position(|link| link.rect.contains(page_point));
            let Some(index) = hit else { return false };
            let destination = navigation.links_on(page)[index].destination.clone();
            match &destination {
                Destination::Uri(uri) => {
                    self.message = format!("外部链接：{uri}（打开浏览器暂未实现）");
                }
                Destination::Page { .. } => {
                    // 优先：跳到**被引用点矩形的中心**（用户要求；登记表在打开时解析）
                    let target = self
                        .link_marks
                        .get(page.get() as usize)
                        .and_then(|registered| registered.get(index))
                        .copied()
                        .flatten();
                    let mark_center = target.and_then(|(mark_page, mark_index)| {
                        self.dest_marks
                            .get(mark_page.get() as usize)?
                            .get(mark_index)
                            .map(|mark| (mark_page, mark.rect.center()))
                    });
                    match mark_center {
                        Some((mark_page, center)) => {
                            self.commands.push(ViewerCommand::GoToPage(mark_page));
                            self.commands.push(ViewerCommand::CenterOnPagePoint {
                                page: mark_page,
                                point: center,
                            });
                        }
                        // 没有登记被引用点（例如仅 /Fit 的目标）→ 退回原有语义
                        None => self.commands.extend(commands_for_destination(&destination)),
                    }
                }
            }
            return true;
        }
        false
    }
}

impl eframe::App for PdfReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ensure_image_loaders(&ctx);
        self.apply_pending_window_size(&ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.closing_after_decision {
                self.remember_document_view();
                self.flush_settings(Some(&ctx));
            } else if self.body_edit_dirty() {
                // 正文窗口里有未保存草稿：先取消关闭，交给编辑确认框。
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.pending_edit_action.is_none() {
                    self.pending_edit_action = Some(EditDiscardAction::Quit);
                }
            } else if !self.is_dirty() {
                self.remember_document_view();
                self.flush_settings(Some(&ctx));
            } else {
                // 有未保存改动：取消这次关闭；提示已经开着也不能放行。
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.pending_action.is_none() {
                    self.pending_action = Some(PendingAction {
                        kind: PendingActionKind::Quit,
                    });
                    self.pending_action_error = None;
                }
            }
        }

        // 拖放打开：**空状态下也必须处理**（v1 只在已有会话时读取，导致启动后拖入无反应）
        self.handle_dropped_files(&ctx);
        // 原地写入的确认框必须在借用 session 之前处理：确认后要**关掉会话**才能替换文件
        self.draw_pending_action_dialog(&ctx);
        self.draw_in_place_dialog(&ctx);
        self.draw_save_as_dialog(&ctx);
        self.draw_compact_dialog(&ctx);
        self.apply_pending_view_restore(&ctx);
        self.draw_history_window(&ctx);

        // 拖放/菜单请求的文件必须在“空状态提前返回”之前真正打开；否则
        // pending_open 只会在已有 session 的帧末尾消费，启动后拖入 PDF 没反应。
        if let Some(path) = self.pending_open.take() {
            self.open(&path, None);
            ctx.request_repaint();
        }
        // 把会话临时取出，避免与 `self` 的其它字段发生借用冲突
        let Some(mut session) = self.session.take() else {
            self.draw_empty(ui);
            self.draw_password_prompt(&ctx);
            return;
        };

        let frame_start = std::time::Instant::now();
        // 高亮只维持一帧：由本帧的悬停判定重新设置
        self.highlight_float = None;
        // `hovered_annotation` **不在这里清空**：按键处理（`collect_commands`）在本帧更早的位置
        // 读取它，清空会让"悬停 + Delete"永远看不到目标。它由标注绘制那段每帧重算，
        // 指针离开标注时自然变成 `None`。
        // 点击产生的一次性高亮（D-118）：在计时内**每帧**生效，并请求"到期时重绘"。
        // 少了这次请求，空闲时 egui 不会再画一帧 —— 高亮会一直停在屏幕上，
        // 直到下一次鼠标移动之类的事件（用户实测"移开指针后高亮不定长地持续"）。
        if let Some((id, until)) = self.click_highlight {
            let now = self.clock.now();
            if now < until {
                self.highlight_float = Some(id);
                ctx.request_repaint_after(until - now);
            } else {
                self.click_highlight = None;
            }
        }
        self.collect_commands(&ctx, &session);
        if self.capture.is_some() {
            // 丢弃工具栏等来源的命令：截图期间不改动主视图内容；
            // 注意仍然要跑 `apply_and_pump` —— 它负责 `pump`，瓦片加载得以继续。
            self.commands.clear();
        }
        // 注意：**不再**按命令白名单退出 Pin。用户最终要求是"只有切到选字/截图模式才退出"；
        // 缩放（含适应宽度、100%）、翻页、侧栏开关、checkbox 都不影响 Pin。
        // Pin 的退出由工具切换与截图按钮显式触发（见 draw_toolbar）。
        // 布局切换（纵向/横向）：拿到 &mut session 才能替换布局实现
        if let Some(horizontal) = self.pending_layout.take() {
            if horizontal {
                session.set_layout(Box::new(pdfcore::layout::HorizontalLayout));
            } else {
                session.set_layout(Box::new(pdfcore::layout::VerticalLayout));
            }
            session.handle(ViewerCommand::RequestVisible, &mut self.events);
        }
        // 新会话默认是纵向布局：若 UI 仍标记为"横向"，这里补上，
        // 否则会出现"菜单显示横向、实际纵向"的错位（用户反馈）。
        if self.horizontal_layout {
            session.set_layout(Box::new(pdfcore::layout::HorizontalLayout));
            session.handle(ViewerCommand::RequestVisible, &mut self.events);
        }
        self.apply_and_pump(&ctx, &mut session);
        let after_input = frame_start.elapsed();

        // 检测"缩放/旋转刚刚变过"：连续缩放期间不提交渲染，只显示旧图拉伸（见 ZOOM_PREVIEW_WINDOW）
        let zoom_now = session.state().zoom;
        let rotation_now = session.state().user_rotation;
        if (zoom_now - self.last_zoom).abs() > f32::EPSILON || rotation_now != self.last_rotation {
            self.last_zoom = zoom_now;
            self.last_rotation = rotation_now;
            self.preview_until = Some(self.clock.now() + ZOOM_PREVIEW_WINDOW);
        }
        let previewing = self.previewing();

        self.last_convert_ms = 0.0;
        self.last_upload_ms = 0.0;
        // 拖动期间**暂缓**转换与上传：把"新页进入"那一帧的逐像素开销挪到松手之后，
        // 拖动本身只需保证画面跟着走（未就绪的区域显示旧图或白纸）。
        let dragging = self.drag.is_some();
        let uploaded = self.ensure_textures(&ctx, &mut session, !dragging);
        let after_textures = frame_start.elapsed();
        if uploaded > 0 {
            // 本帧上传过 → 继续重绘，把"松手后补齐"的过程走完（拖动期间暂缓的上传会在这里补上）
            ctx.request_repaint();
        }

        if self.fullscreen {
            // 指针靠近窗口顶端才显示顶栏；离开顶栏区域即隐藏（显示区域本身不变）
            let pointer_y = ctx.pointer_hover_pos().map(|position| position.y);
            match pointer_y {
                Some(y) if y < 6.0 => self.reveal_toolbar = true,
                Some(y) if y > 120.0 => self.reveal_toolbar = false,
                _ => {}
            }
        }
        self.draw_toolbar(ui, &session);
        // Ctrl+F 切换底部搜索框；Esc 关闭
        // F11 切换全屏
        if ctx.input(|input| input.key_pressed(Key::F11)) {
            self.toggle_fullscreen(&ctx);
        }
        if ctx.input(|input| input.modifiers.command && input.key_pressed(Key::F)) {
            self.search_open = !self.search_open;
            // 呼出时直接聚焦输入框，省掉"再点一下"（用户要求）
            self.focus_search = self.search_open;
        }
        if self.search_open && ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.search_open = false;
        }
        self.draw_sidebar(ui, &mut session);
        self.draw_status(ui);
        self.draw_canvas(ui, &mut session);
        let after_draw = frame_start.elapsed();

        // 每帧耗时分解：拖动期间（或单帧明显慢时）打印，用来定位"不跟手"到底花在哪
        if self.diag.enabled() {
            let total_ms = after_draw.as_secs_f64() * 1000.0;
            if self.drag.is_some() || total_ms > 25.0 {
                self.diag.line(format!(
                    "[帧] 总={:.1}ms 输入+pump={:.1}ms 纹理={:.1}ms(转换={:.1} 上传={:.1}) 绘制={:.1}ms 纹理数={} 命中={} 可见页={} 大纲={} 在飞={}",
                    total_ms,
                    after_input.as_secs_f64() * 1000.0,
                    (after_textures - after_input).as_secs_f64() * 1000.0,
                    self.last_convert_ms,
                    self.last_upload_ms,
                    (after_draw - after_textures).as_secs_f64() * 1000.0,
                    self.textures.len(),
                    self.hits.len(),
                    self.visible.len(),
                    self.navigation
                        .as_ref()
                        .map(|navigation| outline_node_count(&navigation.outline))
                        .unwrap_or(0),
                    session.pending_renders()
                ));
            }
        }

        let pending = if previewing {
            // 预览窗口内**不请求渲染**：把这一串中间比例的钱省下来，只画最终那一次。
            // 会话在 set_zoom 里已经 cancel_all，所以队列此刻是空的，不会白等。
            0
        } else {
            session.handle(ViewerCommand::RequestVisible, &mut self.events);
            self.events.clear();
            session.pending_renders()
        };
        self.session = Some(session);

        // 预览窗口结束后必须自己醒一次，去请求最终比例的渲染
        if previewing {
            if let Some(deadline) = self.preview_until {
                ctx.request_repaint_after(deadline.saturating_duration_since(self.clock.now()));
            }
        } else if pending > 0 {
            ctx.request_repaint();
        }
        self.draw_password_prompt(&ctx);
        self.draw_note_dialog(&ctx);
        self.draw_custom_dialog(&ctx);
        self.draw_edit_dialogs(&ctx);
        // 样式/视图改动不抢在拖动中写盘：指针空闲时再落一次。
        // 这里**不要**再 remember_document_view：刚打开新文档时视图还在等两帧恢复，
        // 此时记下来会把"第 1 页"写进设置，反而把上次位置覆盖掉。
        if self.settings_dirty && !ctx.input(|input| input.pointer.any_down()) {
            self.flush_settings(Some(&ctx));
        }
        // Ctrl+S 同样要等 session 放回；这是它之前“按了没反应”的根因。
        if std::mem::take(&mut self.pending_save) {
            self.save_all();
            ctx.request_repaint();
        }
        // 菜单里请求的"打开另一个文件"放在最后：此时 `self.session` 已经放回，
        // 打开新文档不会被旧 session 覆盖，也不会把新文件的阅读状态写坏。
        if let Some(path) = self.pending_open.take() {
            self.open(&path, None);
            ctx.request_repaint();
        }
    }
}

impl PdfReaderApp {
    /// 是否正处于"缩放/旋转预览窗口"内（此刻只显示旧图，不提交渲染）。
    /// 返回"该页当前波段**第一块就绪**以来"的毫秒数（0 = 还没有任何块就绪）。
    ///
    /// 计时起点刻意选在"第一块就绪"而不是"波段变化"：
    /// * 慢手势（滚轮按住 1 秒以上）期间不断改波段，若从波段变化开始计时，
    ///   等渲染真正开始时早已超时 —— 第一块一到就触发兜底，白块又回来了；
    /// * 从"第一块"开始计时，语义正好是"用户已经看到半张图多久了"。
    ///
    /// 波段不变就不重置：同波段内滚动时新块本来就要现渲染，
    /// 等过 [`view::READY_TIMEOUT_MS`] 后应显示"已就绪 + 补白"，而不是一直糊着（D-96）。
    fn note_band_wait(&mut self, page: PageIndex, scale_milli: u32, ready: usize) -> u64 {
        let now = self.clock.now();
        if ready == 0 {
            self.band_since.remove(&page);
            return 0;
        }
        match self.band_since.get(&page) {
            Some((milli, since)) if *milli == scale_milli => {
                now.saturating_duration_since(*since).as_millis() as u64
            }
            _ => {
                self.band_since.insert(page, (scale_milli, now));
                0
            }
        }
    }

    fn previewing(&self) -> bool {
        self.preview_until
            .is_some_and(|deadline| self.clock.now() < deadline)
    }

    /// 没有打开文档时的空状态界面。
    fn draw_empty(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(
                        "把 PDF 文件拖到这里，或用命令行参数打开：\n\n    cargo run -p app -- <文件路径>",
                    )
                    .size(18.0)
                    .color(Color32::GRAY),
                );
            });
        });
        if let Some(error) = &self.error {
            egui::Panel::bottom("empty-status").show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(error).color(Color32::from_rgb(200, 60, 60)),
                    )
                    .truncate(),
                );
            });
        }
    }
}

/// 画"白底页框"占位（D-06：位图未就绪时用它；D-98 起缺块处优先铺整页底料，不再补白）。
/// 用临时文件替换目标：优先 rename（Unix 原子覆盖）；失败且目标存在时退化为
/// 删掉目标再 rename（Windows）。失败时临时文件保留，便于排查。
fn replace_file(
    temporary: &std::path::Path,
    target: &std::path::Path,
) -> std::result::Result<(), String> {
    match std::fs::rename(temporary, target) {
        Ok(()) => Ok(()),
        Err(first) => {
            if target.exists() {
                std::fs::remove_file(target).map_err(|error| {
                    format!(
                        "替换 {} 失败：{error}（临时文件保留：{}）",
                        target.display(),
                        temporary.display()
                    )
                })?;
                std::fs::rename(temporary, target).map_err(|error| {
                    format!(
                        "改名到 {} 失败：{error}（临时文件保留：{}）",
                        target.display(),
                        temporary.display()
                    )
                })
            } else {
                Err(format!(
                    "改名到 {} 失败：{first}（临时文件保留：{}）",
                    target.display(),
                    temporary.display()
                ))
            }
        }
    }
}

#[cfg(test)]
mod edit_describe_tests {
    use super::*;

    fn ink() -> pdfcore::annotation::Annotation {
        pdfcore::annotation::Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)],
                width: 2.0,
            },
        )
        .expect("有效墨迹")
    }

    #[test]
    fn undo_list_calls_an_ink_stroke_ink_not_a_zero_row_highlight() {
        let edit = AppEdit::AddAnnotation { annotation: ink() };
        let description = edit.describe();
        assert!(
            description.starts_with("墨迹"),
            "历史列表必须说人话：实际 {description}"
        );
        assert!(
            !description.contains("0 行"),
            "墨迹没有行矩形，不能按行数描述：实际 {description}"
        );
    }

    #[test]
    fn undo_list_keeps_the_row_count_for_text_marks() {
        let annotation = pdfcore::annotation::Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![
                    Rect::new(0.0, 0.0, 10.0, 2.0),
                    Rect::new(0.0, 4.0, 10.0, 2.0),
                ],
            },
        )
        .expect("有效高亮");
        let edit = AppEdit::AddAnnotation { annotation };
        assert_eq!(edit.describe(), "高亮（第 1 页，2 行）");
    }
}

#[cfg(test)]
mod annotation_body_tests {
    use super::*;

    fn app() -> PdfReaderApp {
        PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        )
    }

    #[test]
    fn editor_applies_requested_initial_cursor() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        app.open_body_editor(
            FloatOwner::Annotation(id),
            "hello world".to_string(),
            Some(5),
            &ctx,
        );
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(700.0, 500.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            let output = ctx.run_ui(input(), |ui| {
                let _ = ui;
                app.draw_body_editor(&ctx);
            });
            output.drop_without_applying_deltas();
        }
        let text_id = Id::unique(("edit-body-text", app.body_edit_session));
        let state = egui::text_edit::TextEditState::load(&ctx, text_id).expect("编辑器状态");
        let range = state.cursor.char_range().expect("光标范围");
        assert_eq!(usize::from(range.primary.index), 5);
    }

    #[test]
    fn body_cursor_mapping_moves_from_start_to_end() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            let body = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(300.0, 120.0));
            let start = body_cursor_from_pointer(
                ui.ctx(),
                "hello\nworld",
                body,
                body.left_top(),
                false,
                egui::Vec2::ZERO,
                None,
            );
            let end = body_cursor_from_pointer(
                ui.ctx(),
                "hello\nworld",
                body,
                body.right_bottom(),
                false,
                egui::Vec2::ZERO,
                None,
            );
            assert_eq!(start, 0);
            assert!(end > start, "底端应映射到后面的字符");
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn markdown_cursor_mapping_accounts_for_scroll_offset() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            let body = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(300.0, 100.0));
            let source = "## 标题\n\n第一段\n\n第二段\n\n第三段\n\n第四段";
            let top = body_cursor_from_pointer(
                ui.ctx(),
                source,
                body,
                body.left_top(),
                true,
                egui::Vec2::ZERO,
                Some(egui::vec2(300.0, 400.0)),
            );
            let scrolled = body_cursor_from_pointer(
                ui.ctx(),
                source,
                body,
                body.left_top(),
                true,
                egui::vec2(0.0, 200.0),
                Some(egui::vec2(300.0, 400.0)),
            );
            assert!(scrolled > top, "滚动后同一屏幕位置应映射到更后面的源码");
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn internal_image_inserts_at_editor_cursor() {
        let ctx = egui::Context::default();
        let id = Id::unique("cursor-insert-test");
        let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(5),
            )));
        state.store(&ctx, id);
        let mut text = "hello world".to_string();
        insert_at_text_cursor(&ctx, id, &mut text, "![图](pd-img://x?rev=1)");
        assert_eq!(text, "hello![图](pd-img://x?rev=1) world");
        let state = egui::text_edit::TextEditState::load(&ctx, id).expect("state");
        let cursor = state.cursor.char_range().expect("cursor");
        assert_eq!(
            usize::from(cursor.primary.index),
            5 + "![图](pd-img://x?rev=1)".chars().count()
        );
    }

    #[test]
    fn hover_preview_can_render_markdown_when_enabled() {
        let mut app = app();
        app.markdown_floats = true;
        app.hover_preview_markdown = true;
        app.custom_previews.push(CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "hover".to_string(),
            text: Some("## 标题\n\n[跳转](#h)\n\n## 标题 {#h}".to_string()),
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        });
        app.custom_hover = Some((
            0,
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(160.0, 20.0)),
        ));
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_sidebar_hover_float(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert!(app.markdown_rendered, "开启后 hover 临时预览应走 Markdown");
    }

    #[test]
    fn image_choices_include_previews_and_dest_marks() {
        let mut app = app();
        app.custom_previews.push(CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            kind: CustomKind::Image,
            title: "截图".to_string(),
            text: None,
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: "custom-1".to_string(),
            image_revision: 2,
        });
        app.dest_marks = vec![vec![DestMark {
            point: Point::new(10.0, 10.0),
            rect: Rect::new(5.0, 5.0, 10.0, 10.0),
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: "dest-1".to_string(),
            image_revision: 3,
        }]];
        let choices = app.image_choices();
        assert!(choices
            .iter()
            .any(|(_, uri)| uri.contains("custom-1?rev=2")));
        assert!(choices.iter().any(|(_, uri)| uri.contains("dest-1?rev=3")));
    }

    #[test]
    fn image_ref_button_does_not_use_custom_index_for_float_lookup() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let texture = ctx.load_texture(
            "test-image",
            egui::ColorImage::new([2, 2], vec![egui::Color32::RED; 4]),
            egui::TextureOptions::LINEAR,
        );
        let mut app = app();
        for index in 0..5 {
            app.custom_previews.push(CustomPreview {
                id: index as u64,
                page: PageIndex::FIRST,
                point: Point::new(0.0, 0.0),
                rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                kind: CustomKind::Text,
                title: format!("p{index}"),
                text: Some(String::new()),
                snapshot: None,
                body_size: DEFAULT_CUSTOM_TEXT_BODY,
                scale: 1.0,
                image_id: format!("custom-{index}"),
                image_revision: 0,
            });
        }
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "图片".to_string(),
            content: FloatContent::Image(texture),
            rect,
            owner: FloatOwner::Custom(4),
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn markdown_float_renders_inside_a_scroll_area() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "Markdown 测试".to_string(),
            content: FloatContent::Text("# 标题\n\n- a\n- b\n\n正文".to_string()),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert!(app.markdown_rendered, "文字浮窗应走 Markdown 渲染分支");
    }

    #[test]
    fn empty_text_float_still_has_an_edit_affordance() {
        assert!(text_float_is_editable(
            FloatOwner::Custom(0),
            &FloatContent::Text(String::new())
        ));
        assert!(text_float_is_editable(
            FloatOwner::Annotation(AnnotationId(1)),
            &FloatContent::Text(String::new())
        ));
        assert!(!text_float_is_editable(
            FloatOwner::None,
            &FloatContent::Text(String::new())
        ));
    }

    #[test]
    fn readback_geometry_is_normalized_to_the_rendered_box() {
        use backend_pdfium::readback::{ReadAnnotation, Readback};
        use pdfcore::annotation::AnnotationKind;
        let mut readback = Readback {
            annotations: vec![ReadAnnotation {
                page: PageIndex::FIRST,
                id: Some(1),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(179.0, 572.0, 173.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: None,
                author: None,
                created: None,
                modified: None,
                ink_strokes: Vec::new(),
            }],
            skipped: 0,
            extra_ink_strokes: 0,
        };
        PdfReaderApp::normalize_readback_geometry(&mut readback, &[Point::new(36.0, 36.0)]);
        let quad = readback.annotations[0].kind.quads()[0];
        assert!((quad.x - 143.0).abs() < 0.01);
        assert!((quad.y - 536.0).abs() < 0.01);
        assert!((quad.width - 173.0).abs() < 0.01);
    }

    #[test]
    fn markdown_preprocessing_is_cached_by_source() {
        let mut app = app();
        app.markdown_floats = true;
        let source = "第一行\n\n$$\n\n\\frac{1}{2}\n$$\n\n![图](pd-img://x?rev=1)";
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "缓存".to_string(),
            content: FloatContent::Text(source.to_string()),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        let prepared = app.markdown_prepared.get(&1).expect("应缓存预处理结果");
        assert!(
            !prepared.normalized.contains("\n\n\\frac"),
            "块内空行应已规范化"
        );
        assert_eq!(prepared.image_refs.len(), 1);
    }

    #[test]
    fn markdown_heading_cache_is_isolated_per_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 600.0));
        for (id, x) in [(1u64, 20.0f32), (2, 480.0)] {
            let rect = egui::Rect::from_min_size(egui::pos2(x, 30.0), egui::vec2(320.0, 240.0));
            app.floats.push(Float {
                id,
                title: format!("浮窗 {id}"),
                content: FloatContent::Text(format!("## 标题 {id} {{#h{id}}}\n\n[跳转](#h{id})")),
                rect,
                owner: FloatOwner::None,
                body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
                origin: None,
            });
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert_eq!(app.markdown_caches.len(), 2, "每个浮窗必须有独立缓存");
    }

    #[test]
    fn markdown_table_stays_inside_the_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "表格".to_string(),
            content: FloatContent::Text("|a|b|c|\n|:---|:--|:--|\n|1|1|2|".to_string()),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        let mut found = false;
        let mut max_x = f32::NEG_INFINITY;
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                found = true;
                max_x = max_x.max(text.pos.x + text.galley.size().x);
            }
        }
        output.drop_without_applying_deltas();
        assert!(found, "表格应至少绘制出一个文字 mesh");
        assert!(
            max_x <= rect.right() + 2.0,
            "表格不应被推到浮窗右侧：max_x={max_x}, rect_right={}",
            rect.right()
        );
    }

    #[test]
    fn body_editor_commit_writes_annotation_and_is_undoable() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        app.commit_body_edit(FloatOwner::Annotation(id), "正式窗口内容".to_string());
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("正式窗口内容")
        );
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            None
        );
    }

    #[test]
    fn dirty_body_editor_blocks_switching_files_until_decided() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        app.editing_body = Some((FloatOwner::Annotation(id), "未保存".to_string()));
        assert!(app.body_edit_dirty());
        app.request_open(PathBuf::from("/tmp/another.pdf"));
        assert!(
            matches!(app.pending_edit_action, Some(EditDiscardAction::Open(_))),
            "有未保存正文时应先挂起切换动作"
        );
    }

    #[test]
    fn markdown_math_renders_into_the_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "数学".to_string(),
            content: FloatContent::Text(
                "行内 $x^2 + y^2$ 与块级：\n\n$$\\frac{1}{2}$$".to_string(),
            ),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert!(
            !app.math_cache.borrow().is_empty(),
            "公式应生成至少一张纹理或失败占位"
        );
    }

    #[test]
    fn body_edit_is_undoable_and_does_not_create_previews() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        assert!(app.custom_previews.is_empty(), "注释正文不再创建 preview");
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: None,
                after: Some("第一版".to_string()),
            },
            true,
        );
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("第一版")
        );
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            None
        );
        app.redo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("第一版")
        );
        assert!(app.custom_previews.is_empty());
    }

    #[test]
    fn recoloring_new_annotation_is_undoable() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert!(app.recolor_annotation(id, [235, 70, 70]));
        let recolor_id = app.annotations.iter().next().unwrap().id;
        assert_eq!(
            app.annotations.get(recolor_id).unwrap().color,
            [235, 70, 70]
        );
        app.undo_edit();
        assert_eq!(app.annotations.get(id).unwrap().color, [255, 214, 0]);
        app.redo_edit();
        assert_eq!(
            app.annotations.get(recolor_id).unwrap().color,
            [235, 70, 70]
        );
    }

    /// 合成读回结果 + 页对象条目：便签必须按对象号配对，外部便签不能被判成"内联只读"；
    /// 同一 id 出现多次时以最后一版为准。
    #[test]
    fn notes_pair_with_objects_and_duplicates_take_the_last() {
        let mut app = app();
        let note = |page: PageIndex, id: Option<u64>, body: &str| {
            backend_pdfium::readback::ReadAnnotation {
                page,
                id,
                kind: AnnotationKind::Note {
                    rect: Rect::new(10.0, 20.0, 18.0, 18.0),
                },
                color: [255, 214, 0],
                opacity: 1.0,
                body: Some(body.to_string()),
                author: None,
                created: None,
                modified: None,
                ink_strokes: Vec::new(),
            }
        };
        let entry = |num: u32| backend_pdfium::pdf_index::PageAnnot {
            reference: Some(backend_pdfium::pdf_index::PdfObjectRef { num, gen: 0 }),
            subtype: Some(1),
            popup: None,
            hidden: false,
        };
        let readback = backend_pdfium::readback::Readback {
            annotations: vec![
                note(PageIndex::FIRST, Some(5), "旧正文"),
                note(PageIndex::FIRST, Some(5), "新正文"),
                note(PageIndex::new(1), None, "外部便签"),
            ],
            skipped: 0,
            extra_ink_strokes: 0,
        };
        let refs = vec![vec![entry(50), entry(60)], vec![entry(70)]];
        let _ = app.merge_readback(&readback, &refs);

        let own = app
            .annotations
            .iter()
            .find(|annotation| annotation.id == AnnotationId(5))
            .expect("自有便签");
        assert_eq!(own.body.as_deref(), Some("新正文"), "重复 id 取最后一版");
        assert_eq!(
            own.pdf_ref,
            Some(pdfcore::annotation::PdfObjectRef { num: 60, gen: 0 }),
            "应配对到最新的对象"
        );

        let external = app
            .annotations
            .iter()
            .find(|annotation| annotation.id != AnnotationId(5))
            .expect("外部便签");
        assert_eq!(
            external.pdf_ref,
            Some(pdfcore::annotation::PdfObjectRef { num: 70, gen: 0 }),
            "外部便签也要拿到对象号"
        );
        assert!(
            !(app.external_ids.contains(&external.id) && external.pdf_ref.is_none()),
            "外部便签不能被判成内联只读"
        );
    }

    #[test]
    fn note_mode_and_markup_mode_are_exclusive() {
        let mut app = app();
        app.note_pin = true;
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert!(!app.note_pin, "进入标记模式应清掉便签选点");
        assert_eq!(app.annotation_tool, Some(MarkupTool::Highlight));

        app.add_note_annotation(PageIndex::FIRST, Rect::new(0.0, 0.0, 18.0, 18.0), "x");
        app.enter_note_mode();
        assert!(app.note_pin);
        assert_eq!(app.annotation_tool, None, "进入便签模式应退掉标记模式");
    }

    #[test]
    fn annotation_modes_keep_the_original_reading_mode() {
        let mut app = app();
        app.switch_tool(Tool::Pan);
        // 阅读模式（平移）→ 高亮：记录 A=平移
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool, Tool::Select);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        // 高亮 → 墨迹：A 不变
        app.enter_ink_mode();
        assert_eq!(app.tool, Tool::Ink);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        // 墨迹 → 删除线：A 仍不变
        app.toggle_markup_tool(MarkupTool::StrikeOut);
        assert_eq!(app.tool, Tool::Select);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        // Esc 退出标记：回到最初的平移
        app.exit_markup_tool();
        assert_eq!(app.tool, Tool::Pan);
        assert_eq!(app.tool_before_annotation, None);

        // 便签同样不改变阅读模式，Esc 回原模式
        app.switch_tool(Tool::Select);
        app.enter_note_mode();
        assert_eq!(app.tool, Tool::Select, "便签不切换阅读模式");
        assert_eq!(app.tool_before_annotation, Some(Tool::Select));
        app.exit_note_mode();
        assert_eq!(app.tool, Tool::Select);

        // 从标记模式进便签：保留更早的 A（平移）
        app.switch_tool(Tool::Pan);
        app.toggle_markup_tool(MarkupTool::Underline);
        app.enter_note_mode();
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        app.exit_note_mode();
        assert_eq!(app.tool, Tool::Pan);

        // 便签 → 标记：也不能把 A 改写成选字，Esc 仍回平移
        app.switch_tool(Tool::Pan);
        app.enter_note_mode();
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        app.exit_markup_tool();
        assert_eq!(app.tool, Tool::Pan);
    }

    #[test]
    fn undo_after_writeback_rebuilds_moved_ink() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let old_ref = pdfcore::annotation::PdfObjectRef { num: 7, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(old_ref);
        }
        app.opened_pdf_refs.insert(old_ref);

        let before = app.annotations.get(id).cloned().unwrap();
        let mut after = before.clone();
        after.kind = AnnotationKind::Ink {
            points: vec![Point::new(50.0, 60.0), Point::new(70.0, 70.0)],
            width: 2.0,
        };
        after.pdf_ref = None;
        app.apply_edit(AppEdit::MoveAnnotation { before, after }, true);

        // 模拟"写入成功并重开"：该 id 在文件里的对象号变成 new_ref。
        let new_ref = pdfcore::annotation::PdfObjectRef { num: 99, gen: 0 };
        app.opened_pdf_refs.insert(new_ref);
        app.opened_id_by_ref.insert(new_ref, id);
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(new_ref);
        }
        app.reconcile_history_refs();

        // 撤销：几何回原位，且 pdf_ref 为空（下次保存按重建写）。
        app.undo_edit();
        let restored = app.annotations.get(id).expect("撤销后仍在");
        assert_eq!(restored.kind.ink().unwrap().0[0], Point::new(10.0, 10.0));
        assert!(restored.pdf_ref.is_none(), "撤销后不能带着已删除的旧对象号");
        let (_, deletes) = app.pdf_ref_changes();
        assert!(deletes.contains(&new_ref), "当前对象应进 deletes 等待重建");
    }

    #[test]
    fn external_change_is_detected_for_conflict_banner() {
        let path = std::env::temp_dir().join(format!(
            "pdf-reader-conflict-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, b"pdf").expect("写");
        let stamp = pdf_file_stamp(&path).expect("stamp");
        let mut app = app();
        app.document_path = Some(path.clone());
        app.pdf_file_stamp = Some(stamp);
        assert!(!app.pdf_conflict(), "未改动时不应报冲突");
        std::fs::write(&path, b"pdf changed externally").expect("改");
        assert!(app.pdf_conflict(), "外部修改后提示框应能提前发现冲突");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn opening_a_clean_sidecar_stays_clean() {
        let mut app = app();
        app.document_path = Some(PathBuf::from("/tmp/with-sidecar.pdf"));
        app.data_dirty = false;
        app.data_clean_hash = None; // 打开时还没建立基线
        app.settle_dirty_after_open();
        assert!(!app.data_dirty, "带 .json 的干净文件打开后不应变脏");

        // 迁移/升级场景：显式置脏后必须保留脏位。
        app.data_dirty = true;
        app.data_clean_hash = None;
        app.settle_dirty_after_open();
        assert!(app.data_dirty, "迁移中的数据仍应标记为脏");
    }

    #[test]
    fn preview_edits_do_not_dirty_the_pdf() {
        let mut app = app();
        app.document_path = Some(PathBuf::from("/tmp/preview-only.pdf"));
        app.mark_pdf_saved();
        app.apply_edit(
            AppEdit::MovePreview {
                key: PreviewKey {
                    page: PageIndex::FIRST,
                    id: 0,
                },
                from: 0,
                to: 1,
            },
            true,
        );
        assert!(!app.pdf_dirty, "preview 编辑只写 .json，不应弄脏 PDF");
        let annotation = Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(1.0, 1.0), Point::new(2.0, 2.0)],
                width: 2.0,
            },
        )
        .unwrap();
        app.apply_edit(AppEdit::AddAnnotation { annotation }, true);
        assert!(app.pdf_dirty, "注释编辑应弄脏 PDF");
    }

    #[test]
    fn redo_back_to_the_save_point_is_clean() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        app.mark_pdf_saved();
        assert!(!app.pdf_dirty, "保存点之后不应脏");
        app.undo_edit();
        assert!(app.pdf_dirty, "撤销离开保存点应变脏");
        app.redo_edit();
        assert!(!app.pdf_dirty, "重做回到保存点应变干净");
    }

    #[test]
    fn moving_ink_is_undoable_and_drops_the_old_object() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 7, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
        }
        app.opened_pdf_refs.insert(reference);
        app.opened_id_by_ref.insert(reference, id);

        let before = app.annotations.get(id).cloned().unwrap();
        let mut after = before.clone();
        after.kind = AnnotationKind::Ink {
            points: vec![Point::new(50.0, 60.0), Point::new(70.0, 70.0)],
            width: 2.0,
        };
        after.pdf_ref = None;
        app.apply_edit(AppEdit::MoveAnnotation { before, after }, true);

        let moved = app
            .annotations
            .get(id)
            .expect("同一条注释")
            .kind
            .ink()
            .unwrap();
        assert_eq!(moved.0[0], Point::new(50.0, 60.0));
        assert!(app.annotations.get(id).unwrap().pdf_ref.is_none());

        // 旧对象要进 deletes（写回时删掉、再按新几何重建）。
        let (_, deletes) = app.pdf_ref_changes();
        assert!(deletes.contains(&reference), "旧对象应被删除");

        app.undo_edit();
        let restored = app
            .annotations
            .get(id)
            .expect("撤销后仍在")
            .kind
            .ink()
            .unwrap();
        assert_eq!(restored.0[0], Point::new(10.0, 10.0));
    }

    #[test]
    fn adding_a_note_is_one_undo_step() {
        let mut app = app();
        let rect = Rect::new(10.0, 20.0, 18.0, 18.0);
        assert!(app.add_note_annotation(PageIndex::FIRST, rect, "便签正文"));
        let note = app.annotations.iter().next().expect("一条便签");
        assert!(matches!(note.kind, AnnotationKind::Note { .. }));
        assert_eq!(note.body.as_deref(), Some("便签正文"));
        // 作者来自环境变量/系统用户；测试环境可能没有，只要求时间已填。
        assert!(note.modified.is_some());
        assert!(note.created.is_some());
        app.undo_edit();
        assert!(
            app.annotations.iter().next().is_none(),
            "一条撤销应删掉整条便签"
        );
    }

    #[test]
    fn restyling_opacity_and_ink_width_is_undoable() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert!(app.restyle_annotation(id, None, Some(0.5), None));
        let new_id = app.annotations.iter().next().unwrap().id;
        assert!((app.annotations.get(new_id).unwrap().opacity - 0.5).abs() < 1e-6);
        app.undo_edit();
        assert!((app.annotations.get(id).unwrap().opacity - 0.35).abs() < 1e-6);

        // 墨迹线宽是 kind 的一部分，改完也要能撤销回 2.0。
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        let ink_id = app
            .annotations
            .iter()
            .find(|annotation| matches!(annotation.kind, AnnotationKind::Ink { .. }))
            .unwrap()
            .id;
        assert!(app.restyle_annotation(ink_id, None, None, Some(4.0)));
        let width = app
            .annotations
            .iter()
            .find(|annotation| matches!(annotation.kind, AnnotationKind::Ink { .. }))
            .and_then(|annotation| annotation.kind.ink())
            .map(|(_, width)| width)
            .unwrap();
        assert!((width - 4.0).abs() < 1e-6);
        app.undo_edit();
        let width = app
            .annotations
            .get(ink_id)
            .and_then(|annotation| annotation.kind.ink())
            .map(|(_, width)| width)
            .unwrap();
        assert!((width - 2.0).abs() < 1e-6);
    }

    #[test]
    fn recoloring_saved_annotation_replaces_the_old_object() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Underline {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let old_id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 42, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(old_id) {
            annotation.pdf_ref = Some(reference);
        }
        app.opened_pdf_refs.insert(reference);
        assert!(app.recolor_annotation(old_id, [70, 180, 90]));
        let current = app.annotations.iter().next().expect("新注释");
        assert_eq!(current.color, [70, 180, 90]);
        assert_eq!(current.pdf_ref, None, "改色必须重新生成对象");
        let (_, deletes) = app.pdf_ref_changes();
        assert!(deletes.contains(&reference), "旧对象应进入 deletes");
    }

    #[test]
    fn external_annotation_cannot_be_recolored() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        app.external_ids.insert(id);
        assert!(!app.recolor_annotation(id, [235, 70, 70]));
        assert_eq!(app.annotations.get(id).unwrap().color, [255, 214, 0]);
    }

    #[test]
    fn annotation_summary_path_does_not_overwrite() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-summary-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("a.b.pdf");
        let first = annotation_summary_path(&pdf);
        assert_eq!(first, dir.join("a.b.annotations.md"));
        std::fs::write(&first, "x").unwrap();
        let second = annotation_summary_path(&pdf);
        assert_eq!(second, dir.join("a.b.annotations-2.md"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn annotation_search_matches_body_and_author() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(0.0, 0.0, 10.0, 10.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.body = Some("研究 笔记".to_string());
            annotation.author = Some("张三".to_string());
        }
        let annotation = app.annotations.get(id).unwrap();
        assert!(annotation_matches_query(annotation, "笔记"));
        assert!(annotation_matches_query(annotation, "张"));
        assert!(!annotation_matches_query(annotation, "李四"));
    }

    #[test]
    fn recent_order_puts_newest_first_and_unknown_last() {
        let row = |id: u64, modified: Option<i64>| AnnotationRow {
            page: PageIndex::FIRST,
            id: AnnotationId(id),
            label: "高亮",
            readonly: false,
            external: false,
            summary: String::new(),
            preview: String::new(),
            author: None,
            created: None,
            modified,
            bounds: None,
        };
        let mut rows = [row(1, Some(100)), row(2, None), row(3, Some(300))];
        rows.sort_by(recent_order);
        let ids: Vec<u64> = rows.iter().map(|row| row.id.get()).collect();
        assert_eq!(ids, vec![3, 1, 2], "按修改时间倒序，未知时间在最后");
    }

    #[test]
    fn annotation_list_delete_is_undoable_for_inline_external() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert!(app.delete_annotation(id));
        assert!(app.annotations.get(id).is_none());
        app.undo_edit();
        assert!(app.annotations.get(id).is_some());

        let id = app.annotations.iter().next().unwrap().id;
        app.external_ids.insert(id);
        app.inline_refs.insert(id, (PageIndex::FIRST, 2));
        app.opened_inline_bodies
            .insert((PageIndex::FIRST, 2), Some("旧正文".to_string()));
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = None;
            annotation.body = Some("新正文".to_string());
        }
        let (updates, deletes) = app.inline_changes();
        assert_eq!(updates.len(), 1);
        assert!(deletes.is_empty());
        assert!(app.delete_annotation(id));
        assert!(app.annotations.get(id).is_none());
        let (_, deletes) = app.inline_changes();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].position, 2);
    }

    #[test]
    fn conflict_message_mentions_unsaved_edits_and_save_as() {
        assert!(PDF_CONFLICT_MESSAGE.contains("当前修改仍在编辑器中"));
        assert!(PDF_CONFLICT_MESSAGE.contains("另存为"));
    }

    #[test]
    fn pdf_file_stamp_detects_external_size_change() {
        let path = std::env::temp_dir().join(format!(
            "pdf-reader-stamp-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, b"pdf").expect("写");
        let before = pdf_file_stamp(&path).expect("stamp");
        std::fs::write(&path, b"pdf changed").expect("改");
        let after = pdf_file_stamp(&path).expect("stamp2");
        assert_ne!(before, after);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_as_warning_truth_table() {
        let table = |original, target, dirty| save_as_warnings(original, target, dirty, true);
        assert_eq!(
            table(false, false, false),
            SaveAsWarnings {
                new_missing_data: true,
                ..Default::default()
            }
        );
        assert_eq!(table(false, true, false), SaveAsWarnings::default());
        assert_eq!(
            table(true, false, false),
            SaveAsWarnings {
                new_missing_data: true,
                ..Default::default()
            }
        );
        assert_eq!(table(true, true, false), SaveAsWarnings::default());
        assert_eq!(
            table(false, false, true),
            SaveAsWarnings {
                new_missing_data: true,
                no_data_file: true,
                ..Default::default()
            }
        );
        assert_eq!(
            table(false, true, true),
            SaveAsWarnings {
                original_stale: true,
                ..Default::default()
            }
        );
        assert_eq!(
            table(true, false, true),
            SaveAsWarnings {
                new_missing_data: true,
                unsaved_new: true,
                ..Default::default()
            }
        );
        assert_eq!(table(true, true, true), SaveAsWarnings::default());
    }

    #[test]
    fn annotation_list_summary_and_tooltip_are_compact() {
        assert_eq!(body_summary(None), "无正文");
        assert_eq!(
            body_summary(Some("  \n  first   line \nsecond")),
            "first line"
        );
        assert_eq!(body_tooltip(Some("a\nb")), "a b");
        let long = "字".repeat(300);
        let tooltip = body_tooltip(Some(&long));
        assert_eq!(tooltip.chars().count(), 281);
        assert!(tooltip.ends_with('…'));
    }

    #[test]
    fn preview_title_body_and_resize_are_undoable() {
        let mut app = app();
        let preview = CustomPreview {
            id: 7,
            page: PageIndex::FIRST,
            point: Point::new(10.0, 10.0),
            rect: Rect::new(10.0, 10.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "旧标题".to_string(),
            text: Some("旧正文".to_string()),
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        };
        let key = PreviewKey {
            page: preview.page,
            id: preview.id,
        };
        app.apply_edit(
            AppEdit::AddPreview {
                key,
                position: 0,
                preview: std::sync::Arc::new(preview),
            },
            true,
        );
        app.apply_edit(
            AppEdit::SetPreviewTitle {
                key,
                before: "旧标题".to_string(),
                after: "新标题".to_string(),
            },
            true,
        );
        app.apply_edit(
            AppEdit::SetPreviewBody {
                key,
                before: Some("旧正文".to_string()),
                after: Some("新正文".to_string()),
            },
            true,
        );
        app.apply_edit(
            AppEdit::ResizePreview {
                key,
                before: DEFAULT_CUSTOM_TEXT_BODY,
                after: EVec2::new(400.0, 200.0),
            },
            true,
        );
        assert_eq!(app.custom_previews[0].title, "新标题");
        assert_eq!(app.custom_previews[0].text.as_deref(), Some("新正文"));
        assert_eq!(app.custom_previews[0].body_size, EVec2::new(400.0, 200.0));
        app.undo_edit();
        assert_eq!(app.custom_previews[0].body_size, DEFAULT_CUSTOM_TEXT_BODY);
        app.undo_edit();
        assert_eq!(app.custom_previews[0].text.as_deref(), Some("旧正文"));
        app.undo_edit();
        assert_eq!(app.custom_previews[0].title, "旧标题");
    }

    #[test]
    fn consecutive_preview_resizes_merge_into_one_undo() {
        let mut app = app();
        let preview = CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "t".to_string(),
            text: None,
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        };
        let key = PreviewKey {
            page: preview.page,
            id: preview.id,
        };
        app.apply_edit(
            AppEdit::AddPreview {
                key,
                position: 0,
                preview: std::sync::Arc::new(preview),
            },
            true,
        );
        app.apply_preview_resize(key, DEFAULT_CUSTOM_TEXT_BODY, EVec2::new(400.0, 200.0));
        app.apply_preview_resize(key, EVec2::new(400.0, 200.0), EVec2::new(500.0, 250.0));
        assert_eq!(app.history.len(), 2, "创建 + 合并后的 resize 应只有两条");
        assert_eq!(app.custom_previews[0].body_size, EVec2::new(500.0, 250.0));
        app.floats.push(Float {
            id: 1,
            title: "t".to_string(),
            content: FloatContent::Text(String::new()),
            rect: egui::Rect::from_min_size(
                egui::pos2(10.0, 10.0),
                crate::view::float_size_for_body(EVec2::new(500.0, 250.0), FLOAT_TITLE_HEIGHT),
            ),
            owner: FloatOwner::Custom(0),
            body: EVec2::new(500.0, 250.0),
            origin: None,
        });
        app.undo_edit();
        assert_eq!(app.custom_previews[0].body_size, DEFAULT_CUSTOM_TEXT_BODY);
        assert_eq!(app.floats[0].body, DEFAULT_CUSTOM_TEXT_BODY);
    }

    #[test]
    fn preview_move_interface_keeps_stable_key() {
        let mut app = app();
        for (index, title) in ["A", "B"].iter().enumerate() {
            let preview = CustomPreview {
                id: index as u64,
                page: PageIndex::FIRST,
                point: Point::new(index as f32, 0.0),
                rect: Rect::new(index as f32, 0.0, 0.0, 0.0),
                kind: CustomKind::Text,
                title: (*title).to_string(),
                text: Some(String::new()),
                snapshot: None,
                body_size: DEFAULT_CUSTOM_TEXT_BODY,
                scale: 1.0,
                image_id: String::new(),
                image_revision: 0,
            };
            app.apply_edit(
                AppEdit::AddPreview {
                    key: PreviewKey {
                        page: preview.page,
                        id: preview.id,
                    },
                    position: app.custom_previews.len(),
                    preview: std::sync::Arc::new(preview),
                },
                true,
            );
        }
        let key = PreviewKey {
            page: PageIndex::FIRST,
            id: 0,
        };
        app.move_custom_preview(key, 1);
        assert_eq!(app.custom_previews[0].id, 1);
        assert_eq!(app.custom_previews[1].id, 0);
        app.undo_edit();
        assert_eq!(app.custom_previews[0].id, 0);
    }

    #[test]
    fn markup_tool_mode_enters_select_and_toggles_off() {
        let mut app = app();
        app.toggle_markup_tool(MarkupTool::StrikeOut);
        assert_eq!(app.annotation_tool, Some(MarkupTool::StrikeOut));
        assert_eq!(app.tool, Tool::Select, "标记模式应自动切到选字");
        app.toggle_markup_tool(MarkupTool::StrikeOut);
        assert_eq!(app.annotation_tool, None);
        assert_eq!(app.tool, Tool::Pan, "退出后应恢复进入前的工具");
    }

    #[test]
    fn switching_between_annotation_modes_keeps_the_reading_mode() {
        // D-214 起取代 D-174 的"回到墨迹"：注释模式之间切换不改写 A，
        // 退出时回到进入注释模式之前的那一个**阅读模式**。
        let mut app = app();
        app.switch_tool(Tool::Pan);
        app.enter_ink_mode();
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool, Tool::Select);
        app.exit_markup_tool();
        assert_eq!(app.annotation_tool, None);
        assert_eq!(app.tool, Tool::Pan, "Esc/退出应回到最初的阅读模式");
    }

    #[test]
    fn ink_uses_the_style_color() {
        let mut app = app();
        app.ink_color = [200, 30, 30];
        assert!(app.annotate_ink(
            PageIndex::FIRST,
            &[Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
            2.0,
        ));
        let annotation = app.annotations.iter().next().expect("墨迹");
        assert_eq!(annotation.color, [200, 30, 30]);
        assert_eq!(annotation.opacity, INK_OPACITY);
    }

    #[test]
    fn styled_annotation_keeps_style_through_undo_redo() {
        let mut app = app();
        assert!(app.add_annotation_styled(
            PageIndex::FIRST,
            AnnotationKind::Underline {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
            [235, 70, 70],
            1.0,
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert_eq!(app.annotations.get(id).unwrap().color, [235, 70, 70]);
        app.undo_edit();
        app.undo_edit();
        app.redo_edit();
        assert_eq!(
            app.annotations.get(id).unwrap().color,
            [235, 70, 70],
            "撤销/重做必须保留样式"
        );
    }

    #[test]
    fn three_markup_buttons_map_to_three_annotation_kinds() {
        let quads = vec![Rect::new(10.0, 20.0, 30.0, 8.0)];
        for (tool, label) in [
            (MarkupTool::Highlight, "高亮"),
            (MarkupTool::Underline, "下划线"),
            (MarkupTool::StrikeOut, "删除线"),
        ] {
            assert_eq!(tool.label(), label);
            match tool.kind(quads.clone()) {
                AnnotationKind::Highlight { .. } => assert_eq!(label, "高亮"),
                AnnotationKind::Underline { .. } => assert_eq!(label, "下划线"),
                AnnotationKind::StrikeOut { .. } => assert_eq!(label, "删除线"),
                _ => panic!("不应生成墨迹"),
            }
        }
    }

    #[test]
    fn undo_back_to_saved_state_clears_the_right_dirty_flag() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        assert!(app.pdf_dirty, "新增注释是 PDF 脏");
        assert!(!app.data_dirty, "新增注释不是自定义数据脏");
        app.undo_edit();
        assert!(!app.pdf_dirty, "撤销回到干净点应清 PDF 脏");
        assert!(!app.data_dirty);
    }

    #[test]
    fn deleting_and_undoing_annotation_keeps_its_body() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: None,
                after: Some("正文".to_string()),
            },
            true,
        );
        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        assert!(app.annotations.get(id).is_none());
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("正文")
        );
        assert!(app.custom_previews.is_empty());
    }
}

#[cfg(test)]
mod external_annotation_tests {
    use super::*;

    fn app_with_external() -> (
        PdfReaderApp,
        AnnotationId,
        pdfcore::annotation::PdfObjectRef,
    ) {
        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 42, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
            annotation.body = Some("旧".to_string());
        }
        app.external_ids.insert(id);
        app.opened_pdf_refs.insert(reference);
        app.opened_pdf_bodies
            .insert(reference, Some("旧".to_string()));
        app.saved_pdf_revision = app.history.current_pdf_revision();
        app.refresh_dirty();
        (app, id, reference)
    }

    #[test]
    fn undoing_own_delete_after_save_requeues_it_as_new() {
        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 37, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
        }
        app.opened_pdf_refs.insert(reference);
        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        // 模拟删除已经写入 PDF：当前文件里没有这个对象
        app.opened_pdf_refs.clear();
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.pdf_ref),
            None,
            "自有注释的旧 ref 失效后也要按新注释写回"
        );
    }

    #[test]
    fn own_pdf_annotation_body_update_is_a_ref_update() {
        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 37, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
            annotation.body = Some("旧".to_string());
        }
        app.opened_pdf_refs.insert(reference);
        app.opened_pdf_bodies
            .insert(reference, Some("旧".to_string()));
        app.saved_pdf_revision = app.history.current_pdf_revision();
        app.refresh_dirty();
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: Some("旧".to_string()),
                after: Some("新".to_string()),
            },
            true,
        );
        let (updates, deletes) = app.pdf_ref_changes();
        assert_eq!(updates.len(), 1, "自有注释改正文也要走对象级更新");
        assert_eq!(updates[0].reference, reference);
        assert!(deletes.is_empty());
    }

    #[test]
    fn undoing_external_delete_after_save_requeues_it_as_new() {
        let (mut app, id, _reference) = app_with_external();
        // 模拟“删除已经写入 PDF”：当前 PDF 里已经没有这个对象
        app.opened_pdf_refs.clear();
        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        app.undo_edit();
        assert!(
            !app.external_ids.contains(&id),
            "恢复的注释不能继续当外部对象"
        );
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.pdf_ref),
            None,
            "要作为新注释重新写回"
        );
    }

    #[test]
    fn marked_ids_are_reserved_before_external_annotations() {
        use backend_pdfium::pdf_index::{PageAnnot, PdfObjectRef};
        use backend_pdfium::readback::{ReadAnnotation, Readback};

        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        let annotation = |id: Option<u64>, body: &str, reference: PdfObjectRef| {
            (
                ReadAnnotation {
                    page: PageIndex::FIRST,
                    id,
                    kind: AnnotationKind::Highlight {
                        quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
                    },
                    color: [255, 255, 0],
                    opacity: 0.35,
                    body: Some(body.to_string()),
                    author: None,
                    created: None,
                    modified: None,
                    ink_strokes: Vec::new(),
                },
                PageAnnot {
                    reference: Some(reference),
                    subtype: Some(9),
                    popup: None,
                    hidden: false,
                },
            )
        };
        let entries = [
            annotation(None, "外1", PdfObjectRef { num: 11, gen: 0 }),
            annotation(None, "外2", PdfObjectRef { num: 12, gen: 0 }),
            annotation(Some(1), "自有", PdfObjectRef { num: 13, gen: 0 }),
        ];
        let readback = Readback {
            annotations: entries.iter().map(|(item, _)| item.clone()).collect(),
            ..Readback::default()
        };
        let refs = vec![entries.iter().map(|(_, entry)| *entry).collect()];
        let _ = app.merge_readback(&readback, &refs);
        assert_eq!(app.annotations.len(), 3, "三条都要存在，不能被 id 覆盖");
        assert_eq!(app.external_ids.len(), 2, "只有无标记的两条是外部");
        assert_eq!(
            app.annotations
                .get(AnnotationId(1))
                .and_then(|a| a.body.as_deref()),
            Some("自有")
        );
    }

    #[test]
    fn external_body_edit_and_delete_become_ref_changes() {
        let (mut app, id, reference) = app_with_external();
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: Some("旧".to_string()),
                after: Some("新".to_string()),
            },
            true,
        );
        let (updates, deletes) = app.pdf_ref_changes();
        assert_eq!(updates.len(), 1, "正文变化要按对象号更新");
        assert_eq!(updates[0].reference, reference);
        assert_eq!(updates[0].body.as_deref(), Some("新"));
        assert!(deletes.is_empty());

        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        let (_, deletes) = app.pdf_ref_changes();
        assert_eq!(deletes, vec![reference], "删除要按对象号删");
        assert!(app.pdf_dirty, "删除/改正文都要写 PDF");
    }
}
