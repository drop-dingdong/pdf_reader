//! egui/eframe 外壳：把 [`DocumentSession`] 的"命令进、事件出"模型接到界面上。
//!
//! ## 本模块的定位
//! **只做胶水**：坐标换算、输入映射、纹理存储都在 [`crate::view`] 里（可单测）。
//! 剩下的部分需要真实窗口才能验证，按 agent.md 规则 2 **交由用户验收**。
//!
//! ## 每帧流程（与 `docs/current/architecture.md` §4.2 的契约一致）
//! 1. 采集输入 → 命令；2. `handle` 命令；3. `pump` 取回渲染结果；
//! 4. 画页面（就绪则贴纹理，未就绪则白底页框）；5. 画高亮；6. 请求可见页渲染。
//!
//! ## 与 egui 0.37 的适配
//! 0.37 的 `eframe::App` 只提供 `fn ui(&mut self, ui: &mut Ui, frame)`（不再给 `Context`），
//! 侧边/上下栏统一为 [`egui::Panel`]，因此本模块从 `ui.ctx()` 取 `Context` 做纹理与剪贴板操作。

use std::path::PathBuf;
use std::sync::Arc;

use backend_pdfium::PdfiumBackend;
use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, Id, Key, Modifiers, RichText, Sense, Stroke, StrokeKind,
    Vec2 as EVec2,
};
use pdfcore::annotation::{
    Annotation, AnnotationId, AnnotationKind, AnnotationStore, INK_COLOR, INK_OPACITY,
    TEXT_MARK_COLOR, TEXT_MARK_OPACITY,
};
use pdfcore::prelude::*;

use crate::ink::InkStroke;
use crate::persist;

/// 无窗口回放器（脚本驱动真实 `App::ui`，用于回归断言与性能基线，见 D-103）。
pub mod replay;
use crate::view::{
    body_size_changed, body_size_of, canvas_rect_for, checkbox_row, clamp_float_rect, color_image,
    command_for_key, command_for_scroll, command_for_zoom_delta, commands_for_destination,
    compose_region_snapshot, compose_snapshot, default_dest_rect, desired_textures,
    destination_points, drag_float_rect, find_dest_index, find_float_for_custom,
    find_float_for_target, fit_height_zoom, fit_inside, flatten_outline, float_rect_at_row,
    float_size_for_body, hover_preview_allowed, link_box_color, link_overlay_visible,
    link_rect_in_page_bounds, move_to_last, needs_underlay, outline_node_count, overflow_layout,
    page_index_from_text, page_rect_to_screen, pages_intersecting, parse_zoom_percent,
    pin_exits_on_mode_change, plan_page, pointer_over_any, save_button_label,
    screen_rect_to_content, texture_pages, tile_view_rect, tile_view_rect_scaled, tip_rect_for,
    veil_bands, view_rect_for, view_rect_to_page_rect, view_to_content, wheel_targets_canvas,
    zero_rect_at, zoom_percent_text, Desired, DrawInput, KeyedStore, PageDraw, PageFacts,
    PageShape, PlanStamp, Preview, SnapshotLayer, SnapshotPage, CUSTOM_SYMBOL_DELETE,
    CUSTOM_SYMBOL_EDIT, CUSTOM_SYMBOL_IMAGE, CUSTOM_SYMBOL_TEXT, FLOAT_HANDLE_SIZE, FLOAT_MIN_SIZE,
    FLOAT_TITLE_HEIGHT,
};

/// 纹理层的**字节**预算（软上限，D-111）。
///
/// 256 MiB 的来历：本机 12 个回放脚本实测峰值 9–52 MB（A4 家庭）；
/// 极端情形（带鱼屏 5120×1440 + A0 在 800%）估算"当前视口"一类就要 68–206 MB。
/// 软上限的语义：**当前视口所需的那一类永不淘汰**，超出只打一行提示；
/// 只有"底料 / 旧内容"这些可再生的纹理才受它约束。
/// 可用 `PDF_READER_TEXTURE_BUDGET_MB` 覆盖（0 = 不限制）。
pub const TEXTURE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// 注释列表的一行（渲染前先收集、排序，避免边遍历边改 UI）。
struct AnnotationRow {
    page: PageIndex,
    id: AnnotationId,
    label: &'static str,
    readonly: bool,
    external: bool,
    summary: String,
    preview: String,
    bounds: Option<Rect>,
}

/// 注释列表里的类型名；`visible` 恒为 true，预留给将来不可列类型。
fn annotation_kind_label(kind: &AnnotationKind) -> (&'static str, bool) {
    match kind {
        AnnotationKind::Highlight { .. } => ("高亮", true),
        AnnotationKind::Underline { .. } => ("下划线", true),
        AnnotationKind::StrikeOut { .. } => ("删除线", true),
        AnnotationKind::Ink { .. } => ("墨迹", true),
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

/// 已保存的自定义 preview。
///
/// 按用户要求记录：Pin 结束时的**页号 + 页内坐标**、以该坐标为中心的**零尺寸矩形**
/// （与链接对应）、以及题目/内容或截图。位置类字段暂时只被"保存"而未被读取 ——
/// 它们是给"自定义 preview 与链接/被引用点挂接"的下一版准备的。
#[derive(Clone)]
struct CustomPreview {
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
    /// 删除一条自定义 preview（撤销 = 按原下标放回）。
    ///
    /// 备注：自定义 preview 的"重新截图"入口**目前不存在**（截图入口只用于新建），
    /// 所以这一类编辑现在只有"删除"。
    DeletePreview {
        /// 原来在下标里的位置。
        index: usize,
        /// 被删掉的那一条（`Arc` 共享，不深拷贝）。
        preview: std::sync::Arc<CustomPreview>,
    },
    /// 放回一条自定义 preview（撤销"删除"时用；`index` 是原来的位置）。
    AddPreview {
        /// 原位置。
        index: usize,
        /// 那一条。
        preview: std::sync::Arc<CustomPreview>,
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
    /// 给人看的一句话（可视列表直接用它）。
    fn describe(&self) -> String {
        match self {
            Self::AddAnnotation { annotation } => {
                let page = annotation.page.display_number();
                match &annotation.kind {
                    // 墨迹没有行矩形：按 quads 数描述会写成"高亮（第 N 页，0 行）"
                    AnnotationKind::Ink { .. } => format!("墨迹（第 {page} 页）"),
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
            Self::SetBody { page, .. } => {
                format!("修改注释正文（第 {} 页）", page.display_number())
            }
            Self::DeletePreview { index, .. } => format!("删除自定义preview #{index}"),
            Self::AddPreview { index, .. } => format!("恢复自定义preview #{index}"),
            Self::EditDestMark { page, .. } => {
                format!("重选引用点（第 {} 页）", page.display_number())
            }
        }
    }

    /// 估算权重：截图类命令持有位图（在 `Arc` 后面共享），按 1 MiB 估。
    fn weight(&self) -> usize {
        match self {
            Self::DeletePreview { .. } | Self::AddPreview { .. } => 512,
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
    /// 是否正在等"从 PDF 重建标注"的确认（S6c）。
    pending_rebuild: bool,
    /// 上次打开时 PDF 里带 `pd-fr:` 标记的注释数（决定空 overlay 能否写回/导出）。
    pdf_marked_annotations: usize,
    /// 原地写入重开后要恢复的视图（页、缩放、滚动、剩余帧数）。
    ///
    /// 延迟两帧：**先让新会话收到真实的视口尺寸**，否则 `SetScroll` 会被按旧/默认视口夹紧，
    /// 表现为"缩放对了、滚动偏了一点"（用户实测）。
    pending_view_restore: Option<(PageIndex, f32, Vec2, u8)>,
    /// 注释是否有未写入 PDF 的新增/正文/删除。
    pdf_dirty: bool,
    /// 上次成功写 PDF / 打开时注释状态的指纹（`None` = 迁移中，保持脏）。
    pdf_clean_hash: Option<u64>,
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
    /// 自定义 preview 的 Pin 状态（`Some` = 正在选点；此时不显示浮窗、不响应链接点击）。
    custom_pin: Option<CustomKind>,
    /// 自定义 preview 的输入对话框。
    custom_dialog: Option<CustomDialog>,
    /// 已保存的自定义 preview。
    custom_previews: Vec<CustomPreview>,
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
    /// 注释列表：只看外部/只读注释。
    annotation_only_external: bool,
    annotation_only_readonly: bool,
    /// 注释列表点击后的中性定位轮廓（id + 截止时刻）。
    annotation_focus: Option<(AnnotationId, std::time::Instant)>,
    /// Pin 条目悬浮响应开关（勾选 = 悬浮时显示临时预览/高亮旧浮窗）。
    pin_hover_enabled: bool,
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
        Self::with_clock(
            crate::clock::Clock::system(),
            crate::diag::Diagnostics::from_env(),
            initial,
        )
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
        let mut app = Self::with_clock(clock, diag, initial);
        // 无窗口时没有窗口尺寸，给一个明确的默认视口（回放脚本也可以覆盖它）。
        app.commands
            .push(ViewerCommand::ResizeViewport(Size::new(1100.0, 900.0)));
        app
    }

    fn with_clock(
        clock: crate::clock::Clock,
        diag: crate::diag::Diagnostics,
        initial: Option<PathBuf>,
    ) -> Self {
        // 诊断限流与"启动时刻"都取注入时钟的当前值（回放时它由脚本推进）。
        let boot = clock.now();
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
            show_links: false,
            show_annotations: true,
            annotations: AnnotationStore::new(),
            external_ids: std::collections::HashSet::new(),
            opened_pdf_refs: std::collections::BTreeSet::new(),
            opened_pdf_bodies: std::collections::HashMap::new(),
            opened_id_by_ref: std::collections::HashMap::new(),
            pdf_ref_ids: std::collections::HashMap::new(),
            reopening_after_write: false,
            ink: None,
            pending_annotate: None,
            annotation_tool: None,
            selection_started_on_text: false,
            markup_color: TEXT_MARK_COLOR,
            markup_opacity: TEXT_MARK_OPACITY,
            ink_color: INK_COLOR,
            tool_before_annotation: None,
            pending_in_place: false,
            pending_rebuild: false,
            pdf_marked_annotations: 0,
            pending_view_restore: None,
            pdf_dirty: false,
            pdf_clean_hash: None,
            data_clean_hash: None,
            history: crate::history::History::new(),
            show_history: false,
            hovered_annotation: None,
            outline_rows: Vec::new(),
            floats: Vec::new(),
            next_float_id: 1,
            show_dest_points: false,
            dest_marks: Vec::new(),
            capture: None,
            hover_tip: None,
            hover_link_size: EVec2::new(160.0, 20.0),
            scroll_quiet_until: None,
            highlight_float: None,
            document_path: None,
            custom_pin: None,
            custom_dialog: None,
            custom_previews: Vec::new(),
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
            horizontal_scroll: false,
            pending_layout: None,
            search_open: false,
            editing_title: None,
            editing_body: None,
            link_marks: Vec::new(),
            last_convert_ms: 0.0,
            last_upload_ms: 0.0,
            sidebar_width: 260.0,
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
            annotation_only_external: false,
            annotation_only_readonly: false,
            annotation_focus: None,
            pin_hover_enabled: true,
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
        app.pdf_clean_hash = Some(app.pdf_fingerprint());
        app.data_clean_hash = Some(app.data_fingerprint());
        if let Some(path) = initial {
            app.open(&path, None);
        }
        app
    }

    /// 跳到第 `index` 条搜索命中。
    ///
    /// **把命中位置放到视口中心**（第二轮验收的统一跳转语义）：
    /// 跳页是"顶部对齐"，搜索命中则是"居中"，因为用户关心的是那一行文字而不是页首。
    fn goto_hit(&mut self, index: usize) {
        let Some(hit) = self.hits.get(index) else {
            return;
        };
        self.active_hit = Some(index);
        let page = hit.page;
        self.commands.push(ViewerCommand::GoToPage(page));
        if let Some(rect) = hit.rects.first() {
            self.commands.push(ViewerCommand::CenterOnPagePoint {
                page,
                point: rect.center(),
            });
        }
    }

    /// 打开文档（装配会话：LRU 缓存 + 后台调度器 + 纵向布局 + 选区模型）。
    /// 应用一条编辑（`record = true` 时同时记入历史）。
    ///
    /// 只有这一处修改"标注 + 配对正文"，撤销/重做因此共用同一条代码路径 ——
    /// 否则"删除标注忘了删正文"这类不对称会从两个方向各犯一次。
    fn apply_edit(&mut self, edit: AppEdit, record: bool) {
        let description = edit.describe();
        match &edit {
            AppEdit::AddAnnotation { annotation } => {
                self.annotations.insert(annotation.clone());
                // 外部注释被删除并写回后，对象已不在 PDF 里；撤销恢复时必须
                // 当作新注释重新写回，而不是继续按旧对象号做“外部更新”。
                // 删除并写回后，旧对象已不在 PDF；撤销恢复时必须按新注释写回。
                // 这不只对外部注释成立：自有注释的旧 pdf_ref 同样会失效。
                if let Some(reference) = annotation.pdf_ref {
                    if !self.opened_pdf_refs.contains(&reference) {
                        self.external_ids.remove(&annotation.id);
                        if let Some(restored) = self.annotations.get_mut(annotation.id) {
                            restored.pdf_ref = None;
                        }
                    }
                }
            }
            AppEdit::DeleteAnnotation { annotation } => {
                self.annotations.remove(annotation.id);
                // 关闭这条注释的正文浮窗（不会碰 CustomPreview）
                self.floats
                    .retain(|window| window.owner != FloatOwner::Annotation(annotation.id));
            }
            AppEdit::SetBody { id, after, .. } => {
                self.annotations.set_body(*id, after.clone());
            }
            AppEdit::DeletePreview { index, .. } => {
                if *index < self.custom_previews.len() {
                    self.custom_previews.remove(*index);
                    self.shift_float_custom_indices(*index);
                }
            }
            AppEdit::AddPreview { index, preview } => {
                let at = (*index).min(self.custom_previews.len());
                self.custom_previews.insert(at, (**preview).clone());
            }
            AppEdit::EditDestMark {
                page, index, after, ..
            } => {
                if let Some(slot) = self
                    .dest_marks
                    .get_mut(page.get() as usize)
                    .and_then(|marks| marks.get_mut(*index))
                {
                    *slot = (**after).clone();
                }
            }
        }
        if record {
            let weight = edit.weight();
            self.history.push(description, edit, weight);
        }
        self.refresh_dirty();
    }

    /// 撤销一步（反向应用栈顶命令）。
    fn undo_edit(&mut self) {
        let Some(entry) = self.history.undo() else {
            self.message = "没有可撤销的编辑".to_string();
            return;
        };
        let inverse = match entry.command {
            AppEdit::AddAnnotation { annotation } => AppEdit::DeleteAnnotation { annotation },
            AppEdit::DeleteAnnotation { annotation } => AppEdit::AddAnnotation { annotation },
            AppEdit::SetBody {
                id,
                page,
                before,
                after,
            } => AppEdit::SetBody {
                id,
                page,
                before: after,
                after: before,
            },
            // 这两个的"逆"由 `apply_edit` 自身根据当前状态判别（见各自的注释）
            AppEdit::DeletePreview { index, preview } => AppEdit::AddPreview { index, preview },
            AppEdit::AddPreview { index, preview } => AppEdit::DeletePreview { index, preview },
            AppEdit::EditDestMark {
                page,
                index,
                before,
                after,
            } => AppEdit::EditDestMark {
                page,
                index,
                before: after,
                after: before,
            },
        };
        self.apply_edit(inverse, false);
        self.message = format!("已撤销：{}", entry.description);
        self.show_history = true;
    }

    /// 重做一步。
    fn redo_edit(&mut self) {
        let Some(entry) = self.history.redo() else {
            self.message = "没有可重做的编辑".to_string();
            return;
        };
        self.apply_edit(entry.command, false);
        self.message = format!("已重做：{}", entry.description);
        self.show_history = true;
    }

    /// `Ctrl+S`：保存**所有**有脏标记的文件（主存档 + 标注图层）。
    ///
    /// 顺序固定：先主存档（正文在里面）、再图层；两份各自原子写，互不依赖，
    /// 所以即使图层写失败，正文也已经落盘（加载时缺图层只是"没有标注"，不会崩）。
    /// 原地写入重开之后，**等视口尺寸生效**再恢复页/缩放/滚动。
    fn apply_pending_view_restore(&mut self, ctx: &egui::Context) {
        let Some((page, zoom, scroll, remaining)) = self.pending_view_restore else {
            return;
        };
        if self.session.is_none() {
            self.pending_view_restore = None;
            return;
        }
        if remaining == 0 {
            self.commands.push(ViewerCommand::GoToPage(page));
            self.commands.push(ViewerCommand::SetZoom(zoom));
            self.commands.push(ViewerCommand::SetScroll(scroll));
            self.pending_view_restore = None;
        } else {
            self.pending_view_restore = Some((page, zoom, scroll, remaining - 1));
            ctx.request_repaint();
        }
    }

    /// 「从 PDF 重建标注」确认框（S6c）。
    fn draw_rebuild_dialog(&mut self, ctx: &egui::Context) {
        if !self.pending_rebuild {
            return;
        }
        let overlay = self.annotations.len();
        let bodies = self
            .custom_previews
            .iter()
            .filter(|preview| preview.title.starts_with("#annot:"))
            .count();
        let marked = self.pdf_marked_annotations;
        let mut decision: Option<bool> = None;
        egui::Modal::new(Id::unique("rebuild-from-pdf")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("从 PDF 重建标注");
            ui.label(format!(
                "PDF 里有 {marked} 条带标记的注释。将先备份 `.annot.json` / `.json`，                 然后**丢弃当前 overlay**，按 PDF 重建："
            ));
            ui.label(format!(
                "当前 overlay：{overlay} 条标注、{bodies} 条正文，都会被替换。"
            ));
            ui.label("没有被写进 PDF 的标注（以及它们独有的正文）会丢失。");
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    decision = Some(false);
                }
                if ui.button("备份并重建").clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(false) => self.pending_rebuild = false,
            Some(true) => {
                self.pending_rebuild = false;
                self.perform_rebuild_from_pdf();
            }
            None => {}
        }
    }

    /// 以 PDF 为准重建：备份 → 删 A/J 里的注释正文 → 重开（S6a 会从 PDF 导入）→ 落盘。
    fn perform_rebuild_from_pdf(&mut self) {
        let Some(pdf) = self.document_path.clone() else {
            self.message = "先打开一个 PDF".to_string();
            return;
        };
        if self.pdf_marked_annotations == 0 {
            self.message = "PDF 里没有可用来重建的标记注释".to_string();
            return;
        }
        if self.sidecar_locked.is_some() {
            self.message = "主存档由更新版本写入，拒绝重建（否则会破坏它）".to_string();
            return;
        }
        let Some((page, zoom, scroll)) = self.session.as_ref().map(|session| {
            (
                session.state().current_page,
                session.state().zoom,
                session.viewport().scroll,
            )
        }) else {
            self.message = "没有打开的文档".to_string();
            return;
        };

        // 1) 备份 A/J（存在才备份）
        let annot_path = persist::annot_path(&pdf);
        let sidecar_path = persist::sidecar_path(&pdf);
        let mut backups: Vec<std::path::PathBuf> = Vec::new();
        for path in [&annot_path, &sidecar_path] {
            if path.exists() {
                let backup = crate::export::backup_path(path);
                if let Err(error) = std::fs::copy(path, &backup) {
                    self.message = format!("备份 {} 失败，已取消：{error}", path.display());
                    return;
                }
                backups.push(backup);
            }
        }

        // 2) 删 A；保留 J 里的链接/自定义 preview，只删 `#annot:` 正文条目
        if annot_path.exists() {
            if let Err(error) = std::fs::remove_file(&annot_path) {
                self.message = format!("删除 {} 失败，已取消：{error}", annot_path.display());
                return;
            }
        }
        self.custom_previews
            .retain(|preview| !preview.title.starts_with("#annot:"));
        self.data_dirty = true;
        self.save_custom_data();
        if self.data_dirty {
            // J 没写成：把 A 恢复回去（有备份的话），避免 A 丢了而 J 也缺正文
            if let Some(backup) = backups
                .iter()
                .find(|path| path.to_string_lossy().contains(".annot.json.bak-"))
            {
                let _ = std::fs::copy(backup, &annot_path);
            }
            self.message = format!("保存主存档失败，已取消重建：{}", self.message);
            return;
        }

        // 3) 重开：A 不在 → S6a 从 PDF 导入标注，正文从 /Contents 重建
        self.history.clear();
        self.open(&pdf, None);
        if self.session.is_some() {
            self.pending_view_restore = Some((page, zoom, scroll, 2));
            self.save_all();
            let names = backups
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("、");
            self.message = if names.is_empty() {
                format!("已从 PDF 重建 {} 条标注", self.pdf_marked_annotations)
            } else {
                format!(
                    "已从 PDF 重建 {} 条标注（备份：{names}）",
                    self.pdf_marked_annotations
                )
            };
        } else {
            self.message = format!(
                "重建后重新打开失败：{}（备份：{}）",
                pdf.display(),
                backups
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("、")
            );
        }
    }

    /// 原地写入的确认框（S5c）。确认后覆盖原文件；取消则什么都不做。
    fn draw_in_place_dialog(&mut self, ctx: &egui::Context) {
        if !self.pending_in_place {
            return;
        }
        let path = self
            .document_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let mut decision: Option<bool> = None;
        egui::Modal::new(Id::unique("in-place-confirm")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("写入 PDF（原地覆盖）");
            ui.label(format!("将把当前标注写入：{path}"));
            ui.label("写入前会先备份原文件为 `<文件名>.bak-<时间>`；`.annot.json` 保留。");
            ui.label("重复写入是幂等的：按注释里的 /NM 标记同步，不会叠加。");
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    decision = Some(false);
                }
                if ui.button("备份并写入").clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(false) => self.pending_in_place = false,
            Some(true) => {
                self.pending_in_place = false;
                self.perform_in_place_writeback();
            }
            None => {}
        }
    }

    /// 原地写入（S5c）：暂存 → 关闭会话 → 备份 → 原子替换 → 重开并恢复视图。
    ///
    /// 注释不删 overlay（S6 才能读回 PDF 注释；删了本程序就看不到/改不了它们了）。
    /// 写回本身在子进程里（见 `docs/current/writeback.md`）。
    fn perform_in_place_writeback(&mut self) {
        let Some(pdf) = self.document_path.clone() else {
            self.message = "先打开一个 PDF 再写入".to_string();
            return;
        };
        // 一次性取出快照，随后要释放会话的借用
        let Some((page, zoom, scroll, pages)) = self.session.as_ref().map(|session| {
            (
                session.state().current_page,
                session.state().zoom,
                session.viewport().scroll,
                session.page_count(),
            )
        }) else {
            self.message = "没有打开的文档".to_string();
            return;
        };
        {
            let Some(session) = self.session.as_ref() else {
                return;
            };
            if session.opened().document.info().encrypted {
                self.message = "加密文档暂不支持原地写入".to_string();
                return;
            }
        }
        let (ref_updates, ref_deletes) = self.pdf_ref_changes();
        if self.annotations.is_empty()
            && self.pdf_marked_annotations == 0
            && ref_updates.is_empty()
            && ref_deletes.is_empty()
        {
            self.message = "当前文档没有标注".to_string();
            return;
        }
        // P5：pages 只放“新注释”（没有 PDF 对象号的注释）；已有注释的正文
        // 全部走 updates，不再靠 marker 差分删除/保留。
        let items: Vec<crate::export::ExportItem> = self
            .annotations
            .iter()
            .filter(|annotation| {
                !self.external_ids.contains(&annotation.id) && annotation.pdf_ref.is_none()
            })
            .map(|annotation| {
                let body = annotation.body.clone().filter(|text| !text.is_empty());
                crate::export::ExportItem {
                    id: Some(annotation.id.get()),
                    page: annotation.page,
                    kind: annotation.kind.clone(),
                    color: annotation.color,
                    opacity: annotation.opacity,
                    body,
                }
            })
            .collect();

        // 先把自定义数据落盘（重开会重新加载 `.json`）；注释本身只写 PDF。
        if self.data_dirty {
            self.save_custom_data();
        }
        if self.data_dirty {
            self.message = "自定义数据未能保存（存档被占用？），已取消写入".to_string();
            return;
        }

        let new_count = self
            .annotations
            .iter()
            .filter(|annotation| {
                !self.external_ids.contains(&annotation.id) && annotation.pdf_ref.is_none()
            })
            .count();
        let updated = ref_updates.len();
        let deleted = ref_deletes.len();
        let temporary = match crate::export::stage_in_place_with_refs(
            &pdf,
            &items,
            &ref_updates,
            &ref_deletes,
            pages,
        ) {
            Ok(path) => path,
            Err(error) => {
                self.message = format!("写入失败（原文件未改动）：{error}");
                return;
            }
        };

        // 关闭会话：释放 PDFium 的文件句柄（Windows 上不放开就无法替换原文件）
        self.session = None;
        self.textures.clear();

        let backup = crate::export::backup_path(&pdf);
        if let Err(error) = std::fs::copy(&pdf, &backup) {
            let _ = std::fs::remove_file(&temporary);
            self.open(&pdf, None);
            self.message = format!("备份失败，已取消（原文件未改动）：{error}");
            return;
        }
        if let Err(error) = std::fs::rename(&temporary, &pdf) {
            let _ = std::fs::remove_file(&temporary);
            self.open(&pdf, None);
            self.message = format!(
                "替换原文件失败（原文件未改动，备份在 {}）：{error}",
                backup.display()
            );
            return;
        }

        // 旧 `.annot.json` 的注释已经写进 PDF：保留一次备份后退休 A。
        let overlay = persist::annot_path(&pdf);
        if overlay.exists() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let backup = overlay.with_file_name(format!(
                "{}.bak-{nanos}",
                overlay
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "overlay".to_string())
            ));
            let _ = std::fs::rename(&overlay, &backup);
        }

        // 重开并恢复视图；保留撤销栈，并把快照重新绑到新 PDF 对象号
        self.reopening_after_write = true;
        self.open(&pdf, None);
        if self.session.is_some() {
            // 两帧后再恢复：先让新会话收到真实视口尺寸（见 `pending_view_restore` 的注释）
            self.pending_view_restore = Some((page, zoom, scroll, 2));
            self.message = format!(
                "已写入 PDF：新增 {new_count}、更新 {updated}、删除 {deleted} 条注释：{}（备份：{}）",
                pdf.display(),
                backup.display()
            );
        } else {
            self.message = format!(
                "写入完成，但重新打开失败：{}（备份：{}）",
                pdf.display(),
                backup.display()
            );
        }
    }

    /// 导出带标注的 PDF（S5b-2）：overlay + 正文 → job → 子进程 → 校验 → 原子改名。
    ///
    /// 目标固定为同目录 `<stem>.annotated.pdf`，**不覆盖**已有文件；
    /// 真正的 FFI 写回在子进程里（见 `docs/current/writeback.md`），这里只做编排与冒烟校验。
    fn export_annotated_pdf(&mut self, session: &DocumentSession) {
        let Some(pdf) = self.document_path.clone() else {
            self.message = "先打开一个 PDF 再导出".to_string();
            return;
        };
        // 加密文档在子进程里也会被拒绝；这里先给一个即时、可读的原因
        if session.opened().document.info().encrypted {
            self.message = "加密文档暂不支持导出（权限位可能不允许批注）".to_string();
            return;
        }
        let (ref_updates, ref_deletes) = self.pdf_ref_changes();
        if self.annotations.is_empty()
            && self.pdf_marked_annotations == 0
            && ref_updates.is_empty()
            && ref_deletes.is_empty()
        {
            self.message = "当前文档没有标注".to_string();
            return;
        }
        // P5：pages 只放“新注释”（没有 PDF 对象号的注释）；已有注释的正文
        // 全部走 updates，不再靠 marker 差分删除/保留。
        let items: Vec<crate::export::ExportItem> = self
            .annotations
            .iter()
            .filter(|annotation| {
                !self.external_ids.contains(&annotation.id) && annotation.pdf_ref.is_none()
            })
            .map(|annotation| {
                // 正文按哨兵标题从 custom_texts 里取；空正文不写 /Contents
                let body = annotation.body.clone().filter(|text| !text.is_empty());
                crate::export::ExportItem {
                    id: Some(annotation.id.get()),
                    page: annotation.page,
                    kind: annotation.kind.clone(),
                    color: annotation.color,
                    opacity: annotation.opacity,
                    body,
                }
            })
            .collect();
        let output = crate::export::default_output_path(&pdf);
        self.message = "正在导出标注…".to_string();
        match crate::export::export_with_refs(
            &pdf,
            &output,
            &items,
            &ref_updates,
            &ref_deletes,
            session.page_count(),
        ) {
            Ok(report) => {
                self.message = format!(
                    "已导出 {} 条标注：{}（{} 字节）",
                    report.annotations,
                    report.output.display(),
                    report.bytes
                );
            }
            Err(error) => self.message = format!("导出失败：{error}"),
        }
    }

    fn save_all(&mut self) {
        if let Some((found, supported)) = self.sidecar_locked {
            self.message = format!(
                "存档由更新版本写入（v{found} > v{supported}），拒绝覆盖；请升级程序后再保存"
            );
            return;
        }
        if !self.data_dirty && !self.pdf_dirty {
            self.message = "无改动".to_string();
            return;
        }
        if self.data_dirty {
            self.save_custom_data();
        }
        if self.pdf_dirty {
            self.perform_in_place_writeback();
        }
    }

    /// 注释状态指纹（不含 `pdf_ref`：写回重开时对象号会变）。
    fn pdf_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut annotations: Vec<_> = self.annotations.iter().collect();
        annotations.sort_by_key(|annotation| annotation.id);
        for annotation in annotations {
            annotation.id.get().hash(&mut hasher);
            annotation.page.get().hash(&mut hasher);
            annotation.color.hash(&mut hasher);
            annotation.opacity.to_bits().hash(&mut hasher);
            annotation.body.hash(&mut hasher);
            format!("{:?}", annotation.kind).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// 自定义数据指纹（preview 的文本/题目/几何 + 被引用点）。
    fn data_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for preview in &self.custom_previews {
            preview.page.get().hash(&mut hasher);
            preview.title.hash(&mut hasher);
            preview.text.hash(&mut hasher);
            preview.body_size.x.to_bits().hash(&mut hasher);
            preview.body_size.y.to_bits().hash(&mut hasher);
            preview.scale.to_bits().hash(&mut hasher);
            format!("{:?}", preview.kind).hash(&mut hasher);
        }
        for page in &self.dest_marks {
            for mark in page {
                mark.point.x.to_bits().hash(&mut hasher);
                mark.point.y.to_bits().hash(&mut hasher);
                mark.rect.x.to_bits().hash(&mut hasher);
                mark.rect.y.to_bits().hash(&mut hasher);
                mark.rect.width.to_bits().hash(&mut hasher);
                mark.rect.height.to_bits().hash(&mut hasher);
                mark.scale.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// 按“当前状态 vs 上次保存状态”重算两个脏位；撤销回到保存点会自然清零。
    fn refresh_dirty(&mut self) {
        if let Some(clean) = self.pdf_clean_hash {
            self.pdf_dirty = self.pdf_fingerprint() != clean;
        } else if self.document_path.is_some() || !self.history.is_empty() {
            self.pdf_dirty = true;
        }
        if let Some(clean) = self.data_clean_hash {
            self.data_dirty = self.data_fingerprint() != clean;
        } else if self.document_path.is_some() || !self.history.is_empty() {
            self.data_dirty = true;
        }
    }

    /// 对象号级别的待写变更：外部正文更新 + 已从 PDF 删除的对象。
    fn pdf_ref_changes(
        &self,
    ) -> (
        Vec<crate::export::RefUpdate>,
        Vec<pdfcore::annotation::PdfObjectRef>,
    ) {
        let current: std::collections::BTreeSet<pdfcore::annotation::PdfObjectRef> = self
            .annotations
            .iter()
            .filter_map(|annotation| annotation.pdf_ref)
            .collect();
        let current_ids: std::collections::HashSet<AnnotationId> = self
            .annotations
            .iter()
            .map(|annotation| annotation.id)
            .collect();
        let deletes = self
            .opened_pdf_refs
            .iter()
            .filter(|reference| match self.opened_id_by_ref.get(reference) {
                // 标记注释按 id 判断：撤销/重做后的快照可能暂时没有 pdf_ref，
                // 但它仍在当前集合里，不能当“已删除”。
                Some(id) => !current_ids.contains(id),
                None => !current.contains(reference),
            })
            .cloned()
            .collect();
        let updates = self
            .annotations
            .iter()
            .filter_map(|annotation| {
                let reference = annotation.pdf_ref?;
                let before = self.opened_pdf_bodies.get(&reference)?;
                (before != &annotation.body).then(|| crate::export::RefUpdate {
                    reference,
                    body: annotation.body.clone(),
                })
            })
            .collect();
        (updates, deletes)
    }

    /// 保存自定义数据到 `<pdf>.json`（不含任何图片像素）。
    fn save_custom_data(&mut self) {
        if !self.data_dirty {
            self.message = "无自定义数据改动".to_string();
            return;
        }
        if let Some((found, supported)) = self.sidecar_locked {
            // 见 D-107：更新版本的存档不读也不写，否则就是用残缺数据覆盖完整数据
            self.message = format!(
                "同目录存档是 v{found}（本程序支持到 v{supported}）：为了不覆盖它，保存被拒绝"
            );
            return;
        }
        let Some(path) = self.document_path.clone() else {
            self.message = "没有打开的文档".to_string();
            return;
        };
        let links = self
            .navigation
            .as_ref()
            .map(|navigation| navigation.links_by_page.clone())
            .unwrap_or_default();
        let dest_marks = self
            .dest_marks
            .iter()
            .map(|marks| {
                marks
                    .iter()
                    .map(|mark| persist::SavedMark {
                        point: mark.point,
                        rect: mark.rect,
                        body: [mark.body_size.x, mark.body_size.y],
                        scale: mark.scale,
                    })
                    .collect()
            })
            .collect();
        let mut custom_images = Vec::new();
        let mut custom_texts = Vec::new();
        for preview in &self.custom_previews {
            let saved = persist::SavedCustom {
                page: preview.page,
                point: preview.point,
                rect: preview.rect,
                body: [preview.body_size.x, preview.body_size.y],
                title: preview.title.clone(),
                text: preview.text.clone(),
                scale: preview.scale,
            };
            match preview.kind {
                CustomKind::Image => custom_images.push(saved),
                CustomKind::Text => custom_texts.push(saved),
            }
        }
        let data = persist::SavedData {
            version: persist::CURRENT_VERSION,
            links,
            dest_marks,
            custom_images,
            custom_texts,
        };
        let target = persist::sidecar_path(&path);
        match persist::write_atomic(&target, &data.to_json()) {
            Ok(()) => {
                self.data_dirty = false;
                let clean = self.data_fingerprint();
                self.data_clean_hash = Some(clean);
                self.message = format!("已保存自定义数据：{}", target.display());
            }
            Err(error) => self.message = format!("保存失败：{error}"),
        }
    }

    /// 载入标注图层；文件不存在 → 空集合，坏文件 → 提示并返回空集合。
    fn load_overlay_file(
        &mut self,
        path: &std::path::Path,
    ) -> std::result::Result<pdfcore::annotation::AnnotationStore, String> {
        let target = persist::annot_path(path);
        let text = std::fs::read_to_string(&target)
            .map_err(|error| format!("读取标注图层失败：{error}"))?;
        let overlay = persist::SavedOverlay::from_json(&text)
            .map_err(|error| format!("标注图层无法加载（{error}），已按空处理"))?;
        Ok(persist::store_from_overlay(&overlay))
    }

    fn merge_readback(
        &mut self,
        readback: &backend_pdfium::readback::Readback,
        overlay_exists: bool,
        ref_pages: &[Vec<backend_pdfium::pdf_index::PageAnnot>],
    ) -> (usize, usize) {
        self.external_ids.clear();
        self.opened_pdf_refs.clear();
        self.opened_pdf_bodies.clear();
        self.opened_id_by_ref.clear();
        let mut by_page: std::collections::BTreeMap<
            u32,
            Vec<&backend_pdfium::readback::ReadAnnotation>,
        > = std::collections::BTreeMap::new();
        for item in &readback.annotations {
            by_page.entry(item.page.get()).or_default().push(item);
        }

        // 先把读回结果和对象号配对；**先导入带标记的注释，再给外部注释分配 id**，
        // 否则外部 id 可能先占用了后面 pd-fr 标记要用的 id，把注释覆盖掉。
        let mut entries: Vec<(
            &backend_pdfium::readback::ReadAnnotation,
            Option<pdfcore::annotation::PdfObjectRef>,
        )> = Vec::new();
        for (page, items) in by_page {
            let page_entries: Vec<&backend_pdfium::pdf_index::PageAnnot> = ref_pages
                .get(page as usize)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|entry| {
                            !entry.hidden && matches!(entry.subtype, Some(9 | 10 | 12 | 15))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let refs_ok = page_entries.len() == items.len();
            for (item, entry) in items.into_iter().zip(page_entries) {
                let reference = refs_ok
                    .then_some(entry.reference)
                    .flatten()
                    .map(|reference| pdfcore::annotation::PdfObjectRef {
                        num: reference.num,
                        gen: reference.gen,
                    });
                if let Some(reference) = reference {
                    self.opened_pdf_refs.insert(reference);
                    self.opened_pdf_bodies.insert(reference, item.body.clone());
                    if let Some(raw_id) = item.id {
                        self.opened_id_by_ref
                            .insert(reference, AnnotationId(raw_id));
                    }
                }
                entries.push((item, reference));
            }
        }

        let mut restored = 0;
        let mut recovered = 0;
        // 第一遍：带 pd-fr 标记的注释（id 固定，先占住 id 空间）
        for (item, reference) in &entries {
            let Some(raw_id) = item.id else {
                continue;
            };
            let id = AnnotationId(raw_id);
            if let Some(existing) = self.annotations.get_mut(id) {
                existing.pdf_ref = *reference;
                if overlay_exists && existing.body.is_none() && item.body.is_some() {
                    existing.body = item.body.clone();
                    recovered += 1;
                }
            } else if !overlay_exists {
                let Some(mut annotation) = Annotation::new(item.page, item.kind.clone()) else {
                    continue;
                };
                annotation.id = id;
                annotation.color = item.color;
                annotation.opacity = item.opacity;
                annotation.body = item.body.clone();
                annotation.pdf_ref = *reference;
                self.annotations.insert(annotation);
                restored += 1;
            }
        }
        // 第二遍：无标记的外部注释
        for (item, reference) in &entries {
            if item.id.is_some() {
                continue;
            }
            let reused = reference
                .and_then(|reference| self.pdf_ref_ids.get(&reference).copied())
                .filter(|id| self.annotations.get(*id).is_none());
            let id = if let Some(id) = reused {
                id
            } else {
                let Some(id) = self.annotations.add(item.page, item.kind.clone()) else {
                    continue;
                };
                id
            };
            if self.annotations.get(id).is_none() {
                let Some(mut annotation) = Annotation::new(item.page, item.kind.clone()) else {
                    continue;
                };
                annotation.id = id;
                self.annotations.insert(annotation);
            }
            if let Some(annotation) = self.annotations.get_mut(id) {
                annotation.color = item.color;
                annotation.opacity = item.opacity;
                annotation.body = item.body.clone();
                annotation.pdf_ref = *reference;
            }
            if let Some(reference) = reference {
                self.pdf_ref_ids.insert(*reference, id);
            }
            self.external_ids.insert(id);
        }
        (restored, recovered)
    }

    /// 写回重开后，把撤销栈里注释快照的 `pdf_ref` 重新绑到新 PDF 对象号。
    fn reconcile_history_refs(&mut self) {
        let refs: std::collections::HashMap<AnnotationId, pdfcore::annotation::PdfObjectRef> = self
            .opened_id_by_ref
            .iter()
            .map(|(reference, id)| (*id, *reference))
            .collect();
        for command in self.history.commands_mut() {
            if let AppEdit::AddAnnotation { annotation }
            | AppEdit::DeleteAnnotation { annotation } = command
            {
                if let Some(reference) = refs.get(&annotation.id) {
                    annotation.pdf_ref = Some(*reference);
                }
            }
        }
    }

    fn open(&mut self, path: &std::path::Path, password: Option<&str>) {
        let preserve_history = self.reopening_after_write;
        self.reopening_after_write = false;
        if !preserve_history {
            self.history.clear();
            self.pdf_ref_ids.clear();
        }
        let backend = PdfiumBackend::new();
        match backend.open(Source::Path(path), password) {
            Ok(opened) => {
                // S6a：读回 PDF 注释。裸 FFI 不经过 pdfium-render 的全局锁，
                // 所以**必须先放掉旧会话**（它的渲染 worker 可能还在跑）——D-134 选择的方案。
                self.session = None;
                let readback = match backend_pdfium::readback::read(path, password) {
                    Ok(readback) => readback,
                    Err(error) => {
                        eprintln!("[readback] {error}");
                        backend_pdfium::readback::Readback::default()
                    }
                };
                self.pdf_marked_annotations = readback
                    .annotations
                    .iter()
                    .filter(|annotation| annotation.id.is_some())
                    .count();
                let mut open_warning: Option<String> = None;
                let raster = opened.raster.clone();
                let concurrency = opened.capabilities.max_concurrency as usize;
                let cache = Arc::new(LruCache::with_default_capacity());
                let scheduler = Arc::new(WorkerScheduler::new(raster, concurrency));

                match DocumentSession::new(
                    opened,
                    cache,
                    scheduler,
                    Box::new(VerticalLayout),
                    Box::new(RangeSelection::new()),
                ) {
                    Ok(session) => {
                        // **先**清空上一份文档的自定义状态（含浮窗），再由存档按需恢复。
                        // 顺序很关键：清理若放在恢复之后，会把刚从存档读到的数据又抹掉。
                        self.custom_previews.clear();
                        self.custom_pin = None;
                        self.custom_dialog = None;
                        self.custom_hover = None;
                        self.editing_title = None;
                        self.editing_body = None;
                        self.click_highlight = None;
                        self.hover_tip = None;
                        self.floats.clear();
                        self.data_dirty = false;
                        self.sidecar_migrated = false;
                        self.sidecar_locked = None;
                        self.document_path = Some(path.to_path_buf());
                        // 标注图层（<pdf>.annot.json）：缺文件 = 空；坏文件只提示，不影响打开文档
                        self.annotations.clear();
                        self.pdf_dirty = false;
                        let overlay_file_exists = persist::annot_path(path).exists();
                        // 坏 overlay 当作"不存在"处理：否则 PDF 里明明有注释也恢复不出来
                        let mut overlay_ok = true;
                        // 同目录若有存档（<pdf>.json），载入并**替换**链接与被引用点/自定义数据。
                        // 旧版本就地升级并置脏（下次保存写回当前版本）；**更新版本一律拒绝**（D-107）。
                        let saved = match load_sidecar(path) {
                            Sidecar::Missing => None,
                            Sidecar::Ready(loaded) => {
                                for note in &loaded.migration.notes {
                                    eprintln!("[存档] {note}");
                                }
                                self.sidecar_migrated = loaded.migration.changed;
                                Some(loaded.data)
                            }
                            Sidecar::Locked { found, supported } => {
                                self.sidecar_locked = Some((found, supported));
                                None
                            }
                        };
                        self.navigation = session.navigation().ok();
                        // `NavigationIndex` 在会话里是共享只读的，因此这里**替换整个索引**
                        // （克隆后换成存档里的链接），而不是原地改字段。
                        if let (Some(saved), Some(navigation)) = (&saved, self.navigation.clone()) {
                            if saved.links.len() == navigation.links_by_page.len() {
                                let mut updated = (*navigation).clone();
                                updated.links_by_page = saved.links.clone();
                                self.navigation = Some(std::sync::Arc::new(updated));
                            }
                        }
                        // 摊平一次即可（虚拟化行渲染需要行列表）
                        self.outline_rows = self
                            .navigation
                            .as_ref()
                            .map(|navigation| flatten_outline(&navigation.outline))
                            .unwrap_or_default();
                        // 被引用的点（链接目的地）也只算一次：按目标页分组、按位置去重；
                        // 每个点附一个"点周围的小矩形"，截图留待用户发起动作后再构建。
                        self.dest_marks = self
                            .navigation
                            .as_ref()
                            .map(|navigation| {
                                destination_points(
                                    &navigation.links_by_page,
                                    session.page_count() as usize,
                                )
                                .into_iter()
                                .map(|points| {
                                    points
                                        .into_iter()
                                        .map(|point| {
                                            let rect = default_dest_rect(point);
                                            // 初始尺寸：该 rect 按 100% 缩放比（页面单位==像素）
                                            DestMark {
                                                point,
                                                rect,
                                                snapshot: None,
                                                body_size: egui::vec2(rect.width, rect.height),
                                                // 默认 rect 是按 pt 给的（相当于 100%）：分辨率记 1.0
                                                scale: 1.0,
                                            }
                                        })
                                        .collect()
                                })
                                .collect()
                            })
                            .unwrap_or_default();
                        // 链接 → 被引用点：建表时解析一次并**记住索引**。
                        // 之后悬停预览与"跳转到矩形中心"都用它，不再按位置临时查询。
                        self.link_marks = self
                            .navigation
                            .as_ref()
                            .map(|navigation| {
                                navigation
                                    .links_by_page
                                    .iter()
                                    .map(|links| {
                                        links
                                            .iter()
                                            .map(|link| {
                                                let Destination::Page {
                                                    page,
                                                    offset: Some(point),
                                                } = &link.destination
                                                else {
                                                    return None;
                                                };
                                                let marks =
                                                    self.dest_marks.get(page.get() as usize)?;
                                                let positions: Vec<Point> =
                                                    marks.iter().map(|mark| mark.point).collect();
                                                find_dest_index(&positions, *point)
                                                    .map(|index| (*page, index))
                                            })
                                            .collect()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        // 存档里的被引用点与自定义 preview 数据 → 覆盖/追加
                        if let Some(saved) = &saved {
                            for (page_index, page_marks) in saved.dest_marks.iter().enumerate() {
                                let Some(marks) = self.dest_marks.get_mut(page_index) else {
                                    continue;
                                };
                                for (index, saved_mark) in page_marks.iter().enumerate() {
                                    if let Some(mark) = marks.get_mut(index) {
                                        mark.rect = saved_mark.rect;
                                        mark.body_size =
                                            egui::vec2(saved_mark.body[0], saved_mark.body[1]);
                                        mark.scale = saved_mark.scale;
                                    }
                                }
                            }
                            self.custom_previews.clear();
                            for (kind, items) in [
                                (CustomKind::Image, &saved.custom_images),
                                (CustomKind::Text, &saved.custom_texts),
                            ] {
                                for item in items {
                                    self.custom_previews.push(CustomPreview {
                                        page: item.page,
                                        point: item.point,
                                        rect: item.rect,
                                        kind,
                                        title: item.title.clone(),
                                        text: item.text.clone(),
                                        snapshot: None,
                                        body_size: egui::vec2(item.body[0], item.body[1]),
                                        scale: item.scale,
                                    });
                                }
                            }
                        }
                        // 先载入主存档（custom_previews），再载入/迁移 A（v2 的正文在 `.json` 里）
                        if overlay_file_exists {
                            match self.load_overlay_file(path) {
                                Ok(store) => {
                                    if !store.is_empty() {
                                        // 旧 A 里的注释要迁移进 PDF：保存时写出并退休 A
                                        self.pdf_dirty = true;
                                    }
                                    self.annotations = store;
                                }
                                Err(message) => {
                                    overlay_ok = false;
                                    open_warning = Some(message);
                                }
                            }
                        }
                        // v3 之后注释正文不再使用 preview；清掉可能残留的 `#annot:` 条目。
                        // v2（已关闭通道）或坏 A 不清理：避免把仍可能属于旧数据的正文丢掉。
                        if overlay_file_exists && overlay_ok {
                            let before = self.custom_previews.len();
                            self.custom_previews
                                .retain(|preview| !preview.title.starts_with("#annot:"));
                            if self.custom_previews.len() != before {
                                self.data_dirty = true;
                            }
                        }
                        let overlay_exists = overlay_file_exists && overlay_ok;
                        let ref_pages = std::fs::read(path)
                            .ok()
                            .and_then(|bytes| {
                                backend_pdfium::pdf_index::page_annotations(&bytes).ok()
                            })
                            .unwrap_or_default();
                        let (restored, recovered_bodies) =
                            self.merge_readback(&readback, overlay_exists, &ref_pages);
                        if preserve_history {
                            self.reconcile_history_refs();
                        }
                        self.next_float_id = 1;
                        self.capture = None;
                        let title = session.opened().document.info().to_string();
                        self.session = Some(session);
                        self.textures.clear();
                        // D-96 的记账按页号索引：换文档后必须清空，否则旧文档的
                        // "第一块就绪时刻"会让新文档的同一页号一上来就判定超时（提前补白）。
                        self.band_since.clear();
                        self.last_plan.clear();
                        self.error = None;
                        if let Some((found, supported)) = self.sidecar_locked {
                            self.message = format!(
                                "已打开：{title}（同目录存档是 v{found}，本程序只支持到 v{supported}：不加载、也不会覆盖它）"
                            );
                        } else if self.sidecar_migrated {
                            // 旧版本存档被升级过：置脏，让"保存自定义数据"写回当前版本
                            self.data_dirty = true;
                            self.message = format!(
                                "已打开：{title}（存档已升级到 v{}，请保存）",
                                persist::CURRENT_VERSION
                            );
                        } else {
                            self.message = format!("已打开：{title}");
                        }
                        if restored > 0 {
                            self.message
                                .push_str(&format!("（已从 PDF 恢复 {restored} 条标注）"));
                        }
                        if recovered_bodies > 0 {
                            self.message
                                .push_str(&format!("（已从 PDF 补回 {recovered_bodies} 条正文）"));
                        }
                        if let Some(warning) = open_warning {
                            self.message.push_str(&format!("（{warning}）"));
                        }
                        self.pdf_clean_hash = if self.pdf_dirty {
                            None
                        } else {
                            Some(self.pdf_fingerprint())
                        };
                        self.data_clean_hash = if self.data_dirty {
                            None
                        } else {
                            Some(self.data_fingerprint())
                        };
                        self.sidecar_migrated = false;
                        self.password_for = None;
                        self.password_input.clear();
                        self.hits.clear();
                        self.active_hit = None;
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            // 加密文档：不是故障，弹密码框后重试
            Err(CoreError::PasswordRequired) => {
                self.password_for = Some(path.to_path_buf());
                self.password_input.clear();
            }
            Err(CoreError::IncorrectPassword) => {
                self.password_for = Some(path.to_path_buf());
                self.error = Some("密码错误，请重试".to_string());
            }
            Err(error) => self.error = Some(format!("{}：{error}", path.display())),
        }
    }

    /// 采集输入 → 命令。
    fn collect_commands(&mut self, ctx: &egui::Context, session: &DocumentSession) {
        // **文本输入获得焦点时，键盘归输入框**：否则在编辑框里敲空格/方向键会同时滚动页面
        // （用户反馈）。`Memory::focused()` 在有 TextEdit 聚焦时返回其 Id，
        // 覆盖搜索框、题目/正文编辑框等所有输入控件。
        if ctx.memory(|memory| memory.focused().is_some()) {
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
            if key == Key::S && command_modifier {
                self.save_all();
                continue;
            }
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
            if key == Key::Escape && self.annotation_tool.is_some() {
                self.exit_markup_tool();
                self.message = "已退出标记模式".to_string();
                self.drag = None;
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
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .first()
                .map(|file| file.path().to_path_buf())
        });
        if let Some(path) = dropped {
            self.open(&path, None);
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
                    self.error = Some(format!("第 {} 页渲染失败：{error}", page.display_number()));
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

    /// 让纹理层与"这一帧该有的纹理"（[`desired_textures`]）对账（D-112）。
    ///
    /// 三个步骤，顺序固定：
    /// 1. **算** `desired`：唯一决定"GPU 里该有哪些键"的地方（纯函数，每帧算，成本几微秒）；
    /// 2. **删**：`present - keep` 立刻删除（不留旧图过夜）；
    /// 3. **传**：`desired.upload` 里"位图已到"的那些传上去。
    ///
    /// 于是"上传集合 ⊆ 保留集合"由构造保证 —— 不会再出现"传上去、下一帧被裁、再传"的循环。
    /// `allow_upload = false`（拖动期间）只做 1、2：**只删不传**，把逐像素转换挪到松手之后。
    fn ensure_textures(
        &mut self,
        ctx: &egui::Context,
        session: &mut DocumentSession,
        allow_upload: bool,
    ) -> usize {
        // 纹理单边上限来自后端（显卡能力）。告诉会话：单边超限的页会被强制分块（D-108）。
        let max_side = ctx.input(|input| input.max_texture_side as u32);
        if session.max_texture_side() != max_side {
            session.set_max_texture_side(max_side);
        }
        self.visible.clear();
        session.visible_pages(&mut self.visible);
        let targets = texture_pages(
            &self.visible,
            session.state().current_page,
            session.page_count(),
        );

        // 开场底料（D-110）：目标页如果**一张整页图都没有**（起步就在高倍、或刚跳到没去过的页），
        // 请会话渲一张低分辨率整页图。它只当底料用，不会被当成"当前档已经清楚"。
        for page in &targets {
            if !session.has_whole_page_image(*page) && session.thumbnail_key(*page).is_none() {
                session.request_thumbnail(*page);
            }
        }

        // ---- 1. 算 desired ----
        let facts: Vec<PageFacts> = targets
            .iter()
            .filter_map(|page| {
                let placement = session.placement(*page)?;
                let render_milli =
                    RenderKey::new(*page, session.render_scale(*page), placement.rotation)
                        .scale_milli;
                let shape = match session.tile_grid(*page) {
                    None => PageShape::Whole,
                    Some(_) => match session.visible_tiles(*page) {
                        Some((col0, row0, col1, row1)) => PageShape::Tiles(col0, row0, col1, row1),
                        None => PageShape::TilesHidden,
                    },
                };
                Some(PageFacts {
                    page: *page,
                    rotation: placement.rotation,
                    render_milli,
                    shape,
                    whole_pages: session.whole_page_scales(*page),
                })
            })
            .collect();
        let page_sizes: Vec<(PageIndex, Rotation, Size)> = facts
            .iter()
            .filter_map(|fact| {
                let placement = session.placement(fact.page)?;
                Some((fact.page, fact.rotation, placement.page_size))
            })
            .collect();
        let existing = self.textures.keys_newest_first();
        let desired = desired_textures(&facts, &existing, self.textures.max_bytes(), |key| {
            let Some((_, _, page_size)) = page_sizes
                .iter()
                .find(|(page, rotation, _)| *page == key.page && *rotation == key.rotation)
            else {
                return 0;
            };
            texture_key_bytes(*page_size, key.rotation, max_side, key)
        });

        // ---- 2. 删 ----
        self.textures.keep_only(&desired.keep);

        // ---- 3. 传 ----
        let mut uploaded = 0usize;
        if allow_upload {
            for key in &desired.upload {
                if uploaded >= MAX_TEXTURE_UPLOADS_PER_FRAME {
                    break;
                }
                let Some(image) = session.image_for_key(key) else {
                    continue;
                };
                self.note_uploaded(session, key, &image);
                self.upload(ctx, *key, &image);
                uploaded += 1;
            }
        }

        // 软上限：当前视口自身就超预算时只报告一次（这台机器的显示需求超出预算）
        if desired.over_budget && !self.budget_warned {
            self.budget_warned = true;
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[贴图] 纹理超出预算：当前视口需要 {:.1}MB > 预算 {:.0}MB（不淘汰当前视口，只提示）",
                    desired.must_bytes as f64 / (1024.0 * 1024.0),
                    self.textures.max_bytes() as f64 / (1024.0 * 1024.0)
                ));
            }
        }
        self.last_retain = desired;
        uploaded
    }

    /// `[贴图]` 诊断：记录"这一帧传了哪个键、多大"。
    ///
    /// 这条日志是 D-110 那次"每帧重传"被发现的唯一线索，所以对账循环里也必须保留。
    fn note_uploaded(
        &mut self,
        session: &DocumentSession,
        key: &RenderKey,
        image: &Arc<RasterImage>,
    ) {
        if !self.diag.enabled() {
            return;
        }
        let Some(placement) = session.placement(key.page).copied() else {
            return;
        };
        let scroll = session.viewport().scroll;
        match key.tile {
            None => self.diag.line(format!(
                "[贴图] 页={} 整页图={}x{} 渲染比例={:.3} 键={}",
                key.page.display_number(),
                image.width,
                image.height,
                key.scale_milli as f32 / 1000.0,
                key.scale_milli
            )),
            Some(tile) => {
                let Some(grid) = session.tile_grid(key.page) else {
                    return;
                };
                let (page_width, page_height) = grid.render_px();
                let (x, y, width, height) = grid.tile_px(tile.col, tile.row);
                let screen = tile_view_rect(
                    &grid,
                    &placement,
                    tile.col,
                    tile.row,
                    session.render_scale(key.page).0,
                    scroll,
                );
                self.diag.line(format!(
                    "[贴图] 页={} col={} row={} 瓦片图={}x{} 页面px={}x{} 显示px={:.0}x{:.0} 瓦片px=({},{},{},{}) 屏幕=({:.1},{:.1},{:.0},{:.0}) scroll=({:.1},{:.1}) 渲染比例={:.3}",
                    key.page.get(), tile.col, tile.row, image.width, image.height,
                    page_width, page_height,
                    placement.rect.width, placement.rect.height,
                    x, y, width, height,
                    screen.x, screen.y, screen.width, screen.height,
                    scroll.x, scroll.y,
                    key.scale_milli as f32 / 1000.0
                ));
            }
        }
    }

    fn upload(&mut self, ctx: &egui::Context, key: RenderKey, image: &Arc<RasterImage>) {
        if self.textures.contains(&key) {
            return;
        }
        // 全项目唯一的像素格式转换点：**一遍**完成（见 view::color_image 的说明）
        let started = std::time::Instant::now();
        let color = color_image(image.format, image.data(), image.width, image.height);
        self.last_convert_ms += started.elapsed().as_secs_f64() * 1000.0;

        let started = std::time::Instant::now();
        let texture = ctx.load_texture(
            format!("page-{}-{}-{:?}", key.page.get(), key.scale_milli, key.tile),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.last_upload_ms += started.elapsed().as_secs_f64() * 1000.0;
        self.textures.insert(key, texture);
    }

    fn draw_toolbar(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        if self.fullscreen {
            // 全屏：顶栏**不占布局空间**，只在指针靠近顶端时悬浮出现（遮住下方内容）
            if self.reveal_toolbar {
                egui::Area::new(Id::unique("toolbar-overlay"))
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        // Area 内 `available_rect_before_wrap()` 宽度接近 0（Area 按内容收缩），
                        // 会导致右侧内容被挤掉 —— 必须用窗口视口宽度。
                        let width = ui
                            .ctx()
                            .input(|input| {
                                input
                                    .viewport()
                                    .inner_rect
                                    .map(|rect| rect.width())
                                    .unwrap_or(1200.0)
                            })
                            .max(320.0);
                        egui::Frame::side_top_panel(&ui.style().clone()).show(ui, |ui| {
                            ui.set_width(width);
                            self.toolbar_rows(ui, session);
                        });
                    });
            }
            return;
        }
        egui::Panel::top("toolbar").show(ui, |ui| {
            self.toolbar_rows(ui, session);
        });
    }

    /// 切换全屏：窗口最大化 + 去装饰，并让顶栏改为悬浮显隐。
    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        self.reveal_toolbar = self.fullscreen;
        let on = self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!on));
    }

    /// 顶栏的三行内容（正常模式与全屏悬浮共用）。
    fn toolbar_rows(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        {
            // ---- 第 0 行：文件（路径 + 保存自定义数据）----
            ui.horizontal(|ui| {
                let path_text = self
                    .document_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "（未打开文件）".to_string());
                let response = ui.add(
                    egui::Label::new(RichText::new(truncate_middle(&path_text, 70)).small())
                        .truncate(),
                );
                response.on_hover_text(&path_text);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = save_button_label(self.data_dirty);
                    match self.sidecar_locked {
                        // 存档属于更新版本：不读也不写（D-107），按钮直接禁用并说明原因
                        Some((found, supported)) => {
                            ui.add_enabled(false, egui::Button::new(label))
                                .on_disabled_hover_text(format!(
                                    "同目录存档是 v{found}，本程序只支持到 v{supported}：\
                                     为了不覆盖它，保存已禁用"
                                ));
                        }
                        None => {
                            if ui
                                .button(label)
                                .on_hover_text("保存所有有改动的文件（Ctrl+S）")
                                .clicked()
                            {
                                // 这个按钮只负责自定义数据；注释用「写入 PDF」/Ctrl+S。
                                self.save_custom_data();
                            }
                        }
                    }
                    ui.add_space(6.0);
                    // 原地写入（S5c）：会覆盖原文件，先弹确认 + 自动备份。
                    // 允许"空标注"：把 PDF 里写过的注释删干净（界面上删光之后要能写回）。
                    let has_write_work = self.pdf_dirty
                        || !self.annotations.is_empty()
                        || self.pdf_marked_annotations > 0;
                    let can_write = self.document_path.is_some() && has_write_work;
                    let write_label = if self.pdf_dirty {
                        "*写入 PDF"
                    } else {
                        "写入 PDF"
                    };
                    let write_back =
                        ui.add_enabled(can_write, egui::Button::new(write_label));
                    let write_back = if can_write {
                        write_back.on_hover_text(
                            "把标注写进原 PDF（会先备份、再原子替换；重复写入是幂等的）",
                        )
                    } else {
                        write_back.on_disabled_hover_text(
                            "需要打开一个 PDF；当前没有标注，PDF 里也没有写回的注释",
                        )
                    };
                    if write_back.clicked() {
                        self.pending_in_place = true;
                    }
                    ui.add_space(6.0);
                    // S6c：丢弃 overlay，按 PDF 里写回的注释重建（先备份 A/J）
                    let can_rebuild =
                        self.document_path.is_some() && self.pdf_marked_annotations > 0;
                    let rebuild =
                        ui.add_enabled(can_rebuild, egui::Button::new("从 PDF 重建"));
                    let rebuild = if can_rebuild {
                        rebuild.on_hover_text(
                            "用 PDF 里写回的注释替换当前标注数据（先备份 .annot.json / .json）",
                        )
                    } else {
                        rebuild.on_disabled_hover_text("PDF 里还没有带标记的注释可用来重建")
                    };
                    if rebuild.clicked() {
                        self.pending_rebuild = true;
                    }
                    ui.add_space(6.0);
                    // 导出带标注的 PDF（S5b-2）：写新文件，不覆盖；真正的写回在子进程里
                    let can_export = self.document_path.is_some() && has_write_work;
                    let export = ui.add_enabled(can_export, egui::Button::new("导出标注 PDF"));
                    let export = if can_export {
                        export.on_hover_text(
                            "把当前标注写成真正的 PDF 注释，保存为同目录的 <原名>.annotated.pdf（不覆盖）",
                        )
                    } else {
                        export.on_disabled_hover_text(
                            "需要打开一个 PDF；当前没有标注，PDF 里也没有写回的注释",
                        )
                    };
                    if export.clicked() {
                        self.export_annotated_pdf(session);
                    }
                });
            });

            // ---- 第 1 行：侧栏 / 自定义 preview / Pin 悬浮响应（全部左对齐）----
            ui.horizontal(|ui| {
                ui.toggle_value(&mut self.show_outline, "侧栏")
                    .on_hover_text("显示/隐藏侧边栏（目录 / 自定义preview）");
                ui.separator();
                ui.label("添加预览：");
                if ui
                    .button(CUSTOM_SYMBOL_TEXT)
                    .on_hover_text("添加文字预览")
                    .clicked()
                {
                    self.enter_pin(CustomKind::Text);
                }
                if ui
                    .button(CUSTOM_SYMBOL_IMAGE)
                    .on_hover_text("添加图片预览")
                    .clicked()
                {
                    self.enter_pin(CustomKind::Image);
                }
                ui.separator();
                ui.checkbox(&mut self.pin_hover_enabled, "Pin悬浮响应");
                ui.checkbox(&mut self.horizontal_scroll, "横向滚动")
                    .on_hover_text("勾选后纵向滚轮改为横向滚动（拖动不受影响）");
            });

            // ---- 第 2 行：页 / 缩放 / 旋转 / 预览模式（左） + 引用显示样式（右）----
            ui.horizontal(|ui| {
                // 全屏切换（现阶段纯文字）
                let fullscreen_label = if self.fullscreen {
                    "退出全屏"
                } else {
                    "全屏"
                };
                if ui
                    .button(fullscreen_label)
                    .on_hover_text("全屏/退出全屏（F11）")
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.toggle_fullscreen(&ctx);
                }
                egui::ComboBox::from_id_salt("layout-direction")
                    .selected_text(if self.horizontal_layout {
                        "横向"
                    } else {
                        "纵向"
                    })
                    .show_ui(ui, |ui| {
                        // 无条件提交：即使标记已经是该值，也强制重排一次，
                        // 这样"状态错位"（UI 说横向、实际纵向）点一下就能纠正。
                        if ui
                            .selectable_label(!self.horizontal_layout, "纵向")
                            .clicked()
                        {
                            self.horizontal_layout = false;
                            self.pending_layout = Some(false);
                        }
                        if ui
                            .selectable_label(self.horizontal_layout, "横向")
                            .clicked()
                        {
                            self.horizontal_layout = true;
                            self.pending_layout = Some(true);
                        }
                    });
                ui.separator();
                // 上一页 / 页码（双击原位编辑） / 下一页
                if ui.button("上一页").clicked() {
                    self.commands.push(ViewerCommand::PrevPage);
                }
                let current = session.state().current_page;
                if let Some(draft) = self.page_edit.as_mut() {
                    let response = ui.add(
                        egui::TextEdit::singleline(draft)
                            .desired_width(44.0)
                            .hint_text("页码"),
                    );
                    if self.focus_edit == Some(EditField::Page) {
                        // 双击后当帧就把焦点交给输入框 → 一步进入真编辑
                        response.request_focus();
                        self.focus_edit = None;
                    }
                    let commit = response.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter));
                    if commit {
                        match page_index_from_text(draft, session.page_count()) {
                            Some(page) => {
                                self.commands.push(ViewerCommand::GoToPage(page));
                                self.error = None;
                                self.page_edit = None;
                            }
                            None => {
                                self.error =
                                    Some(format!("页码需在 1–{} 之间", session.page_count()));
                                self.page_edit = None;
                            }
                        }
                    }
                } else {
                    let label = format!("{} / {}", current.display_number(), session.page_count());
                    if ui
                        .add(egui::Label::new(label).sense(egui::Sense::click()))
                        .on_hover_text("双击可直接输入页码")
                        .double_clicked()
                    {
                        self.page_edit = Some(current.display_number().to_string());
                        self.focus_edit = Some(EditField::Page);
                    }
                }
                if ui.button("下一页").clicked() {
                    self.commands.push(ViewerCommand::NextPage);
                }

                ui.separator();

                // 缩放比（双击原位编辑）+ 缩放模式菜单
                let zoom = session.state().zoom;
                if let Some(draft) = self.zoom_edit.as_mut() {
                    let response = ui.add(egui::TextEdit::singleline(draft).desired_width(52.0));
                    if self.focus_edit == Some(EditField::Zoom) {
                        response.request_focus();
                        self.focus_edit = None;
                    }
                    let commit = response.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter));
                    if commit {
                        match parse_zoom_percent(draft) {
                            Some(value) => {
                                self.commands.push(ViewerCommand::SetZoom(value));
                                self.zoom_mode = ZoomMode::Custom;
                                self.error = None;
                            }
                            None => self.error = Some("缩放比需为正数百分比".to_string()),
                        }
                        self.zoom_edit = None;
                    }
                } else {
                    if ui
                        .add(egui::Label::new(zoom_percent_text(zoom)).sense(egui::Sense::click()))
                        .on_hover_text("双击可直接输入缩放百分比")
                        .double_clicked()
                    {
                        self.zoom_edit = Some(format!("{:.0}", zoom * 100.0));
                        self.focus_edit = Some(EditField::Zoom);
                    }
                }
                egui::ComboBox::from_id_salt("zoom-mode")
                    .selected_text(self.zoom_mode.label())
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(false, "适应宽度").clicked() {
                            self.commands.push(ViewerCommand::FitWidth);
                            self.zoom_mode = ZoomMode::FitWidth;
                        }
                        if ui.selectable_label(false, "适应高度").clicked() {
                            if let Some(placement) = session.placement(current) {
                                if let Some(fit) = fit_height_zoom(
                                    session.viewport().size.height,
                                    placement.rect.height,
                                ) {
                                    self.commands.push(ViewerCommand::SetZoom(zoom * fit));
                                }
                            }
                            self.zoom_mode = ZoomMode::FitHeight;
                        }
                        if ui.selectable_label(false, "100%").clicked() {
                            self.commands.push(ViewerCommand::SetZoom(1.0));
                            self.zoom_mode = ZoomMode::Actual;
                        }
                    });

                ui.separator();

                // 旋转：顺时针 / 逆时针
                if ui.button("\u{21bb}").on_hover_text("顺时针旋转").clicked() {
                    self.commands.push(ViewerCommand::RotateClockwise);
                }
                if ui.button("\u{21ba}").on_hover_text("逆时针旋转").clicked() {
                    self.commands.push(ViewerCommand::RotateCounterClockwise);
                }

                ui.separator();

                // 基础工具：平移 / 选字；墨迹按钮和文本标记放在一起
                for tool in [Tool::Pan, Tool::Select] {
                    if ui
                        .selectable_label(self.tool == tool, tool.label())
                        .clicked()
                    {
                        self.switch_tool(tool);
                    }
                }

                // 验收辅助入口（D-38 截图 / D-36 测试浮窗）：默认**不显示**，
                // 设 `PDF_READER_DEV_ENTRY=1` 才出现在顶栏。代码保留（见 D-114）：
                // 它们是手工验证"截图流程 / 浮窗拖动改大小层级"最快的入口。
                if dev_entries_enabled() {
                    ui.separator();
                    if ui
                        .button("截图")
                        .on_hover_text("框选区域 → Enter 保存为浮窗，Esc 取消")
                        .clicked()
                    {
                        self.capture = Some(CaptureState {
                            rect: None,
                            anchor: None,
                            target: CaptureTarget::Plain,
                        });
                        self.message = "截图模式：拖出选区，Enter 保存，Esc 取消".to_string();
                    }
                    if ui
                        .button("测试浮窗")
                        .on_hover_text("添加一个纯文字浮窗（验收浮窗拖动/改大小/层级）")
                        .clicked()
                    {
                        self.add_test_float();
                    }
                }

                ui.separator();
                for tool in [
                    MarkupTool::Highlight,
                    MarkupTool::Underline,
                    MarkupTool::StrikeOut,
                ] {
                    let active = self.annotation_tool == Some(tool);
                    let response = ui
                        .add(egui::Button::new(tool.label()).selected(active))
                        .on_hover_text(format!(
                            "进入{}模式：拖选文字后施加；再点一次/Esc 退出",
                            tool.label()
                        ));
                    if response.clicked() {
                        self.toggle_markup_tool(tool);
                    }
                }
                let ink_active = self.tool == Tool::Ink && self.annotation_tool.is_none();
                if ui
                    .add(egui::Button::new("墨迹").selected(ink_active))
                    .on_hover_text("墨迹模式：拖拽画一笔；Esc/切换工具退出")
                    .clicked()
                {
                    self.switch_tool(Tool::Ink);
                }
                ui.menu_button("样式", |ui| {
                    ui.label("标记颜色");
                    ui.horizontal(|ui| {
                        for (name, rgb) in [
                            ("黄", [255u8, 214, 0]),
                            ("红", [235u8, 70, 70]),
                            ("绿", [70u8, 180, 90]),
                            ("蓝", [70u8, 130, 220]),
                        ] {
                            if color_swatch(ui, name, rgb, self.markup_color == rgb).clicked() {
                                self.markup_color = rgb;
                            }
                        }
                    });
                    ui.add(
                        egui::Slider::new(&mut self.markup_opacity, 0.1..=1.0)
                            .text("高亮透明度")
                            .fixed_decimals(2),
                    );
                    ui.separator();
                    ui.label("墨迹颜色");
                    ui.horizontal(|ui| {
                        for (name, rgb) in [
                            ("黑", [25u8, 25, 25]),
                            ("红", [200u8, 30, 30]),
                            ("绿", [30u8, 150, 70]),
                            ("蓝", [30u8, 90, 210]),
                        ] {
                            if color_swatch(ui, name, rgb, self.ink_color == rgb).clicked() {
                                self.ink_color = rgb;
                            }
                        }
                    });
                    ui.label("下划线/删除线固定不透明；线宽随缩放。");
                });

                // 引用显示样式（链接框 + 引用点 合并为一个菜单）
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("display-style")
                        .selected_text("引用显示样式")
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(
                                    self.show_links,
                                    checkbox_row("链接框", self.show_links),
                                )
                                .clicked()
                            {
                                self.show_links = !self.show_links;
                            }
                            if ui
                                .selectable_label(
                                    self.show_dest_points,
                                    checkbox_row("引用点", self.show_dest_points),
                                )
                                .clicked()
                            {
                                self.show_dest_points = !self.show_dest_points;
                            }
                            if ui
                                .selectable_label(
                                    self.show_annotations,
                                    checkbox_row("标注", self.show_annotations),
                                )
                                .clicked()
                            {
                                self.show_annotations = !self.show_annotations;
                            }
                        });
                });
            });
        }
    }

    /// 搜索框（Ctrl+F 呼出）：关键词、上一个/下一个、命中计数。
    ///
    /// 它在**中央区域**里作为底部面板创建，因此宽度与画布一致 —— 不会横跨到侧栏下方。
    fn draw_search_bar_in_center(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        if !self.search_open {
            return;
        }
        egui::Panel::bottom("search-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("搜索：");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_input)
                        .desired_width(180.0)
                        .hint_text("关键词"),
                );
                if self.focus_search {
                    response.request_focus();
                    self.focus_search = false;
                }
                let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (submitted || ui.button("查找").clicked()) && !self.search_input.is_empty() {
                    match session.search(&SearchQuery::new(self.search_input.clone())) {
                        Ok(hits) => {
                            self.message = if hits.is_empty() {
                                format!(
                                    "未找到“{}”（若 PDF 是扫描件则没有文本层，无法搜索）",
                                    self.search_input
                                )
                            } else {
                                format!("找到 {} 处", hits.len())
                            };
                            self.hits = hits;
                            self.error = None;
                            if self.hits.is_empty() {
                                self.active_hit = None;
                            } else {
                                self.goto_hit(0);
                            }
                        }
                        Err(error) => self.error = Some(format!("搜索失败：{error}")),
                    }
                }
                if !self.hits.is_empty() {
                    if ui.button("上一个").clicked() {
                        let index = match self.active_hit {
                            Some(0) | None => self.hits.len() - 1,
                            Some(index) => index - 1,
                        };
                        self.goto_hit(index);
                    }
                    if ui.button("下一个").clicked() {
                        let index =
                            self.active_hit.map(|index| index + 1).unwrap_or(0) % self.hits.len();
                        self.goto_hit(index);
                    }
                    ui.label(format!(
                        "{} / {}",
                        self.active_hit.map(|index| index + 1).unwrap_or(0),
                        self.hits.len()
                    ));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("关闭").clicked() {
                        self.search_open = false;
                    }
                });
            });
        });
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        if !self.show_outline {
            return;
        }
        let navigation = self.navigation.clone();
        let width = self
            .sidebar_width
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        // 尺寸由我们自己控制（`exact_size`），可拖拽的分隔条在画布左侧 —— 见 `draw_canvas`。
        //
        // 不用 egui 内建拖拽的原因：面板状态以 `parent_ui.scope_id().with_salt(id)` 为键持久化，
        // 而 eframe 每帧传入的是**新的根 `Ui`**，于是拖拽结果写进去、下一帧读不回来 ——
        // 第三、四轮验收两次反馈"空有动作没有结果"，根因就在这里。
        egui::Panel::left("outline")
            .resizable(false)
            .exact_size(width)
            .show(ui, |ui| {
                // 两个标签页：目录 / 自定义preview（Pin）。同一时刻只显示一个。
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.sidebar_tab == SidebarTab::Outline, "目录")
                        .clicked()
                    {
                        self.sidebar_tab = SidebarTab::Outline;
                    }
                    if ui
                        .selectable_label(self.sidebar_tab == SidebarTab::Pins, "自定义preview")
                        .clicked()
                    {
                        self.sidebar_tab = SidebarTab::Pins;
                    }
                    if ui
                        .selectable_label(self.sidebar_tab == SidebarTab::Annotations, "注释")
                        .clicked()
                    {
                        self.sidebar_tab = SidebarTab::Annotations;
                    }
                });
                ui.separator();
                if self.sidebar_tab == SidebarTab::Pins {
                    self.draw_pin_list(ui, session);
                    return;
                }
                if self.sidebar_tab == SidebarTab::Annotations {
                    self.draw_annotation_list(ui, session);
                    return;
                }
                let Some(navigation) = navigation else {
                    ui.label("（无目录）");
                    return;
                };
                if navigation.outline.is_empty() {
                    ui.label("（本文档没有书签）");
                    return;
                }
                // **虚拟化行**：只布局可见的那十几行。
                // 实测 1292 个节点时，每帧全量布局要花约 42ms（与缩放无关，纯 UI 开销）。
                let rows = &self.outline_rows;
                let row_height = OUTLINE_ROW_HEIGHT;
                egui::ScrollArea::vertical().show_rows(ui, row_height, rows.len(), |ui, range| {
                    for index in range {
                        let Some((depth, title, destination)) = rows.get(index) else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            ui.add_space(*depth as f32 * 12.0);
                            let clickable = destination.is_some();
                            let label = if clickable {
                                RichText::new(title).strong()
                            } else {
                                RichText::new(title)
                            };
                            if ui
                                .add_enabled(clickable, egui::Button::new(label).frame(false))
                                .clicked()
                            {
                                if let Some(Destination::Page { page, .. }) = destination {
                                    if page.get() < session.page_count() {
                                        self.commands.push(ViewerCommand::GoToPage(*page));
                                    }
                                }
                            }
                        });
                    }
                });
            });
    }

    /// 侧栏「注释」标签页：按页分组、类型/正文摘要、筛选与定位。
    fn draw_annotation_list(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        ui.horizontal(|ui| {
            ui.label("筛选");
            ui.menu_button("类型", |ui| {
                ui.checkbox(&mut self.annotation_show_highlight, "高亮");
                ui.checkbox(&mut self.annotation_show_underline, "下划线");
                ui.checkbox(&mut self.annotation_show_strike, "删除线");
                ui.checkbox(&mut self.annotation_show_ink, "墨迹");
            });
            ui.checkbox(&mut self.annotation_only_external, "仅外部");
            ui.checkbox(&mut self.annotation_only_readonly, "仅只读");
        });
        ui.separator();

        let mut rows: Vec<AnnotationRow> = Vec::new();
        for annotation in self.annotations.iter() {
            let (label, visible) = annotation_kind_label(&annotation.kind);
            if !visible {
                continue;
            }
            let enabled = match &annotation.kind {
                AnnotationKind::Highlight { .. } => self.annotation_show_highlight,
                AnnotationKind::Underline { .. } => self.annotation_show_underline,
                AnnotationKind::StrikeOut { .. } => self.annotation_show_strike,
                AnnotationKind::Ink { .. } => self.annotation_show_ink,
            };
            if !enabled {
                continue;
            }
            let external = self.external_ids.contains(&annotation.id);
            if self.annotation_only_external && !external {
                continue;
            }
            let readonly = external && annotation.pdf_ref.is_none();
            if self.annotation_only_readonly && !readonly {
                continue;
            }
            let summary = body_summary(annotation.body.as_deref());
            let preview = body_tooltip(annotation.body.as_deref());
            rows.push(AnnotationRow {
                page: annotation.page,
                id: annotation.id,
                label,
                readonly,
                external,
                summary,
                preview,
                bounds: annotation.bounds(),
            });
        }
        if rows.is_empty() {
            ui.label(if self.annotations.is_empty() {
                "（本文档还没有注释）"
            } else {
                "（筛选后没有注释）"
            });
            return;
        }
        rows.sort_by(|a, b| {
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
        });
        ui.label(format!("{} 条", rows.len()));
        let mut last_page: Option<PageIndex> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in rows {
                    let AnnotationRow {
                        page,
                        id,
                        label,
                        readonly,
                        external,
                        summary,
                        preview,
                        ..
                    } = row;
                    if last_page != Some(page) {
                        if last_page.is_some() {
                            ui.add_space(6.0);
                        }
                        ui.label(
                            RichText::new(format!("第 {} 页", page.display_number()))
                                .strong()
                                .color(ui.visuals().weak_text_color()),
                        );
                        last_page = Some(page);
                    }
                    let mut text = format!("[{label}] {summary}");
                    if readonly {
                        text.push_str("  [只读]");
                    }
                    let response = ui
                        .add(egui::Label::new(text).truncate().sense(Sense::click()))
                        .on_hover_text(format!(
                            "{preview}\n\n{label} · 第 {} 页{}{}",
                            page.display_number(),
                            if external { " · 外部" } else { " · 自有" },
                            if readonly { " · 只读" } else { "" }
                        ));
                    if response.clicked() {
                        self.focus_annotation(id, session);
                    }
                }
            });
    }

    /// 侧栏的"自定义preview（Pin）"列表：
    /// 标题左对齐、页码右对齐、过长标题截断；双击跳转；悬浮显示临时浮窗；
    /// 单击生成/唤出持久浮窗（已有则高亮并置顶，一次性高亮有时限）。
    fn draw_pin_list(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        if self.custom_previews.is_empty() {
            ui.label("（还没有自定义preview）");
            return;
        }
        let row_height = OUTLINE_ROW_HEIGHT;
        let mut hovered: Option<(usize, egui::Rect)> = None;
        let count = self.custom_previews.len();
        egui::ScrollArea::vertical().show_rows(ui, row_height, count, |ui, range| {
            for index in range {
                let Some(preview) = self.custom_previews.get(index) else {
                    continue;
                };
                // 行首：类型符号 + 标题（长标题截断）+ 右侧页码
                let symbol = match preview.kind {
                    CustomKind::Text => CUSTOM_SYMBOL_TEXT,
                    CustomKind::Image => CUSTOM_SYMBOL_IMAGE,
                };
                let title = format!(
                    "{} {}",
                    symbol,
                    if preview.title.is_empty() {
                        "（无题）"
                    } else {
                        preview.title.as_str()
                    }
                );
                let page_label = format!("第 {} 页", preview.page.display_number());

                // 1) 先占位（整行宽度），2) 再画，3) 最后在该矩形上注册交互。
                // 顺序很重要：交互最后注册才会盖在内容之上、稳定收到点击。
                let width = ui.available_width();
                let (rect, _) =
                    ui.allocate_exact_size(EVec2::new(width, row_height), Sense::hover());
                let response = ui.interact(rect, Id::unique(("pin-row", index)), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::ZERO,
                        ui.visuals().widgets.hovered.weak_bg_fill,
                    );
                    // 悬浮响应可整体关闭（用户要求）：关掉后既不出临时预览、也不点亮旧浮窗
                    if self.pin_hover_enabled {
                        // 该条目已有正式浮窗 → **只**高亮并置顶那个浮窗，不再显示临时预览；
                        // 没有正式浮窗时才显示临时预览浮窗。
                        if self.highlight_existing_custom_float(index).is_none() {
                            // 缺图（例如刚从存档恢复）→ 现在按当前缩放合成；未就绪则下帧自动重试
                            self.ensure_custom_snapshot(ui, session, index);
                            hovered = Some((index, rect));
                        }
                    }
                }
                let painter = ui.painter();
                let page_width = 56.0;
                let go_width = 24.0;
                let edit_width = 20.0;
                let delete_width = 20.0;
                let title_width =
                    (rect.width() - page_width - go_width - edit_width - delete_width - 8.0)
                        .max(24.0);
                let job = overflow_layout(
                    &title,
                    egui::FontId::proportional(13.0),
                    ui.visuals().text_color(),
                    title_width,
                    1,
                );
                let galley = painter.layout_job(job);
                painter.galley(
                    egui::pos2(rect.min.x, rect.center().y - galley.size().y * 0.5),
                    galley,
                    ui.visuals().text_color(),
                );
                painter.text(
                    egui::pos2(
                        rect.max.x - go_width - edit_width - delete_width,
                        rect.center().y,
                    ),
                    Align2::RIGHT_CENTER,
                    &page_label,
                    egui::FontId::proportional(11.0),
                    ui.visuals().weak_text_color(),
                );

                if response.clicked() {
                    // 单击立即生成/唤出浮窗（不再延迟：跳转改由右侧 → 按钮触发）
                    self.ensure_custom_snapshot(ui, session, index);
                    self.activate_custom_preview(index, rect);
                }

                // 行右侧的 go 按钮（→）：点击跳转到该条目记录的页与页内坐标。
                // 它在行交互**之后**注册，因此点击优先落到按钮上，不会同时生成浮窗。
                let go_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.max.x - 10.0, rect.center().y),
                    EVec2::new(20.0, row_height),
                );
                // ✎ 修改题目；❎ 删除条目（都在 → 之前注册，命中优先级更高）
                let edit_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        go_rect.min.x - edit_width * 0.5 - delete_width,
                        rect.center().y,
                    ),
                    EVec2::new(edit_width, row_height),
                );
                let delete_rect = egui::Rect::from_center_size(
                    egui::pos2(go_rect.min.x - delete_width * 0.5, rect.center().y),
                    EVec2::new(delete_width, row_height),
                );
                if ui
                    .put(
                        edit_rect,
                        egui::Button::new(CUSTOM_SYMBOL_EDIT).frame(false),
                    )
                    .clicked()
                {
                    if let Some(preview) = self.custom_previews.get(index) {
                        self.editing_title = Some((index, preview.title.clone()));
                    }
                }
                if ui
                    .put(
                        delete_rect,
                        egui::Button::new(CUSTOM_SYMBOL_DELETE).frame(false),
                    )
                    .clicked()
                {
                    self.delete_custom_preview(index);
                }
                if ui
                    .put(go_rect, egui::Button::new("\u{2192}").frame(false))
                    .clicked()
                {
                    if let Some(preview) = self.custom_previews.get(index) {
                        self.commands.push(ViewerCommand::GoToPage(preview.page));
                        self.commands.push(ViewerCommand::CenterOnPagePoint {
                            page: preview.page,
                            point: preview.point,
                        });
                    }
                }
            }
        });
        // 悬浮条目 → 只显示临时浮窗（离开即关）；同时记录行矩形用于定位
        self.custom_hover = hovered;
    }

    /// 由"侧栏行的屏幕矩形"决定浮窗位置（委托给已测试的 `view::float_rect_at_row`）。
    fn float_rect_at_row(&self, row: egui::Rect, size: egui::Vec2) -> egui::Rect {
        float_rect_at_row(self.canvas_rect, row, size, FLOAT_MIN_SIZE)
    }

    /// 按**记录的分辨率**取得某页某区域的预览图；缺像素就**提交渲染**并返回 `None`（D-101）。
    ///
    /// 流程：
    /// 1. 记录的分辨率 `scale`（v2 存档字段，`1.0` = 100%）→ 吸到波段上
    ///    （`render_scale_for`），这样与主视图同波段时能直接命中已有缓存；
    /// 2. `region_tiles` 已全部就绪 → 在**渲染像素**里 1:1 拼图（输出尺寸 = 区域 × 渲染比例，
    ///    与当前缩放无关）；
    /// 3. 缺块 → `request_region` 提交覆盖该区域的块，本帧返回 `None`（调用方下帧重试）。
    ///
    /// 这样"初次生成"不再依赖"用户是否在这个缩放下走过那块地方"。
    // 8 个参数都是"这次生成预览需要的事实"，打包成结构体只会把调用点变长
    #[allow(clippy::too_many_arguments)]
    fn preview_texture(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        page: PageIndex,
        page_rect: Rect,
        scale: f32,
        texture_name: String,
        tag: &str,
    ) -> Option<egui::TextureHandle> {
        let placement = session.placement(page).copied()?;
        if page_rect.width.is_nan()
            || page_rect.height.is_nan()
            || page_rect.width <= 0.0
            || page_rect.height <= 0.0
        {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成失败：{tag} 页={page:?} 区域为空 {page_rect:?}"
                ));
            }
            return None;
        }
        let display_scale = if scale > 0.0 && scale.is_finite() {
            scale
        } else {
            1.0
        };
        // 记录的比例可能让预览图超出纹理单边上限（大区域 + 高倍）：按超采样步长降档（D-108）
        let requested_scale = render_scale_for(display_scale, session.overscan());
        let render_value = crate::view::preview_render_scale(
            page_rect,
            placement.page_size,
            placement.rotation,
            requested_scale,
            session.max_texture_side(),
        );
        let render = Scale::from_zoom(render_value);
        let grid = TileGrid::new(placement.page_size.rotated(placement.rotation), render.0);
        let Some(range) =
            grid.tiles_for_page_rect(page_rect, placement.page_size, placement.rotation)
        else {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成失败：{tag} 页={} 区域 {page_rect:?} 不在页面内",
                    page.display_number()
                ));
            }
            return None;
        };
        let Some(tiles) = session.region_tiles(page, page_rect, render) else {
            let count = session.request_region(page, page_rect, render);
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成：{tag} 页={} 区域pt=({:.1},{:.1},{:.1},{:.1}) 记录分辨率={:.0}% 渲染比例={:.3} 可见块={:?} 提交渲染={} 块（等待就绪）",
                    page.display_number(),
                    page_rect.x,
                    page_rect.y,
                    page_rect.width,
                    page_rect.height,
                    display_scale * 100.0,
                    render.0,
                    range,
                    count
                ));
                // 显式说明"降档"：上面的数字里能推导出来（记录分辨率 vs 渲染比例），
                // 但这里把原因也写清楚（区域像素超过纹理单边上限）。每次请求只打一行。
                if render_value < requested_scale {
                    let (region_w, region_h) = crate::view::region_pixel_size(
                        page_rect,
                        placement.page_size,
                        placement.rotation,
                        requested_scale,
                    )
                    .unwrap_or((0, 0));
                    let steps = (requested_scale / render_value.max(f32::MIN_POSITIVE))
                        .log(crate::view::DEFAULT_PREVIEW_STEP)
                        .round();
                    self.diag.line(format!(
                        "[预览] 降档：{tag} 页={} 记录分辨率={:.0}% 原渲染比例={:.3} → 实际={:.3}（降 {:.0} 档：区域 {}x{} 超过 max_texture_side={}）",
                        page.display_number(),
                        display_scale * 100.0,
                        requested_scale,
                        render_value,
                        steps,
                        region_w,
                        region_h,
                        session.max_texture_side()
                    ));
                }
            }
            return None;
        };
        let borrowed: Vec<(TileSpec, &RasterImage)> = tiles
            .iter()
            .map(|(tile, image)| (*tile, image.as_ref()))
            .collect();
        let Some(image) = compose_region_snapshot(
            page_rect,
            placement.page_size,
            placement.rotation,
            render.0,
            &grid,
            &borrowed,
        ) else {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成失败：{tag} 页={} 区域 {page_rect:?} 的块都为空（块数 {}）",
                    page.display_number(),
                    borrowed.len()
                ));
            }
            return None;
        };
        if self.diag.enabled() {
            self.diag.line(format!(
                "[预览] 初次生成完成：{tag} 页={} 区域pt=({:.1},{:.1},{:.1},{:.1}) 记录分辨率={:.0}% 渲染比例={:.3} 尺寸={}x{}",
                page.display_number(),
                page_rect.x,
                page_rect.y,
                page_rect.width,
                page_rect.height,
                display_scale * 100.0,
                render.0,
                image.size[0],
                image.size[1]
            ));
        }
        Some(
            ui.ctx()
                .load_texture(texture_name, image, egui::TextureOptions::LINEAR),
        )
    }

    /// 为"**正在显示**、但还缺预览图"的自定义图片 preview 重试生成。
    ///
    /// 只处理已被浮窗引用的条目：不能对存档里的每一条都预渲染（那会在打开文档时
    /// 把几十个区域全提交渲染）。
    fn ensure_pending_custom_snapshots(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
    ) {
        let mut shown: Vec<usize> = self
            .floats
            .iter()
            .filter_map(|window| window.custom_index())
            .collect();
        shown.sort_unstable();
        shown.dedup();
        for index in shown {
            let missing = self.custom_previews.get(index).is_some_and(|preview| {
                preview.kind == CustomKind::Image && preview.snapshot.is_none()
            });
            if missing {
                self.ensure_custom_snapshot(ui, session, index);
            }
        }
    }

    /// 确保某个自定义**图片** preview 有可用截图（缺则按**记录的分辨率**重新生成）。
    fn ensure_custom_snapshot(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        index: usize,
    ) {
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        if preview.kind != CustomKind::Image || preview.snapshot.is_some() {
            return;
        }
        let (page, page_rect, scale) = (preview.page, preview.rect, preview.scale);
        let Some(texture) = self.preview_texture(
            ui,
            session,
            page,
            page_rect,
            scale,
            format!("custom-image-{index}"),
            "自定义图片",
        ) else {
            return;
        };
        if let Some(preview) = self.custom_previews.get_mut(index) {
            preview.snapshot = Some(texture);
        }
    }

    /// 按某个自定义 preview 的数据生成它的**正式浮窗**（正文尺寸取 `body_size`）。
    /// 把当前选区变成指定文本标记，返回新增数量（D-118 S1）。
    ///
    /// 几何来自文本层的 `rects_for_range`（已按行合并），因此三种标记共用同一套算法；
    /// 标注**只存页面空间**，缩放/旋转时由布局换算，不会漂移。
    fn annotate_selection(&mut self, session: &mut DocumentSession, tool: MarkupTool) -> usize {
        let highlights = session.highlight_selection();
        let mut added = 0;
        for highlight in highlights {
            let kind = tool.kind(highlight.rects.clone());
            let (color, opacity) = match tool {
                MarkupTool::Highlight => (self.markup_color, self.markup_opacity),
                MarkupTool::Underline | MarkupTool::StrikeOut => (self.markup_color, 1.0),
            };
            if self.add_annotation_styled(highlight.page, kind, color, opacity) {
                added += 1;
            }
        }
        if added > 0 {
            self.pdf_dirty = true;
        }
        if added == 0 && session.highlight_selection().is_empty() {
            self.message = format!("先选中文字，再{}", tool.label());
        } else if added > 0 {
            self.message = format!("已{} {added} 处", tool.label());
        }
        added
    }

    /// 页面矩形 → 该区域覆盖的文本行矩形（页面空间）。
    ///
    /// 会**按需加载**该页文本：标注必须先有文本层才能取几何（扫描件没有字符框，
    /// 这种页应当走"选框"类标注，而不是文本高亮）。
    fn quads_for_page_rect(&mut self, page: PageIndex, rect: Rect) -> Option<Vec<Rect>> {
        let text = self.session.as_mut()?.page_text(page)?;
        let range = text.range_for_page_rect(rect)?;
        Some(text.rects_for_range(range))
    }

    /// 把页面上的一块矩形区域变成高亮（按文本字符范围取几何）。
    fn annotate_page_rect(&mut self, page: PageIndex, rect: Rect) -> usize {
        let Some(quads) = self.quads_for_page_rect(page, rect) else {
            return 0;
        };
        let bounds = quads
            .iter()
            .copied()
            .reduce(|a, b| a.union(b))
            .unwrap_or(rect);
        usize::from(self.add_highlight(page, quads, bounds))
    }

    /// 进入/退出文本标记模式；进入时自动切到选字工具。
    fn toggle_markup_tool(&mut self, tool: MarkupTool) {
        if self.annotation_tool.is_some() {
            if self.annotation_tool == Some(tool) {
                self.exit_markup_tool();
            } else {
                self.annotation_tool = Some(tool);
                self.message = format!("{}模式：拖选文字即可添加，Esc 退出", tool.label());
            }
            return;
        }
        let previous = self.tool;
        self.switch_tool(Tool::Select);
        self.tool_before_annotation = Some(previous);
        self.annotation_tool = Some(tool);
        self.message = format!("{}模式：拖选文字即可添加，Esc 退出", tool.label());
    }

    /// 退出标记模式，恢复进入前的工具。
    fn exit_markup_tool(&mut self) {
        let previous = self.tool_before_annotation.take().unwrap_or(Tool::Select);
        self.annotation_tool = None;
        self.switch_tool(previous);
    }

    /// 切换工具（工具栏与回放共用）：**切走墨迹时丢弃正在画的一笔**。
    fn switch_tool(&mut self, tool: Tool) {
        if self.tool == Tool::Ink && tool != Tool::Ink {
            if self.ink.take().is_some() {
                self.message = "已切换工具，未完成的一笔已丢弃".to_string();
            }
            self.drag = None;
        }
        self.tool = tool;
        self.annotation_tool = None;
        self.tool_before_annotation = None;
        if pin_exits_on_mode_change(tool == Tool::Select, false) {
            self.custom_pin = None;
        }
    }

    /// 内容空间点 →（所在页，页面空间点）。
    ///
    /// 墨迹用它决定"这一笔画在哪一页"：可见页按顺序取第一个包含该点的。
    fn page_at_content(
        &self,
        session: &DocumentSession,
        content: Point,
    ) -> Option<(PageIndex, Point)> {
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page) else {
                continue;
            };
            if placement.rect.contains(content) {
                return Some((page, placement.content_to_page(content)));
            }
        }
        None
    }

    /// 墨迹工具的手势：按下起笔、拖动累积、**抬笔才入库**（D-118 S4b）。
    ///
    /// 拖拽中只更新 [`InkStroke`]（实时跟手），不进 `AnnotationStore`、不产生命令；
    /// 因此 Esc / 切工具丢弃时"不会留下半条"。
    fn handle_ink_pointer(
        &mut self,
        ui: &egui::Ui,
        canvas: egui::Rect,
        response: &egui::Response,
        session: &DocumentSession,
        scroll: Vec2,
    ) {
        if self.capture.is_some() {
            return;
        }
        if response.drag_started() {
            // 起点必须用 `press_origin`：egui 的 `drag_started` 要等指针移过阈值才触发，
            // 用当时的指针位置会让整笔平移几个像素。
            let Some(origin) = ui.input(|input| input.pointer.press_origin()) else {
                return;
            };
            let content = canvas_to_content(canvas, origin, scroll);
            let Some((page, page_point)) = self.page_at_content(session, content) else {
                self.message = "请在页面上开始画墨迹".to_string();
                return;
            };
            let mut stroke = InkStroke::start(page, DEFAULT_INK_WIDTH, page_point);
            // 触发 `drag_started` 需要先移过阈值，所以"按下点"与"当前位置"之间已经有一段了；
            // 把当前位置也采进来，否则一笔的开头会凭空少一段（快速起笔时尤其明显）。
            if let Some(position) = response.interact_pointer_pos() {
                let current = canvas_to_content(canvas, position, scroll);
                if let Some(placement) = session.placement(page) {
                    stroke.push(placement.content_to_page(current));
                }
            }
            self.ink = Some(stroke);
            self.drag = Some(DragMode::Ink);
            self.message = "正在画墨迹（Esc 取消）".to_string();
            return;
        }
        if response.dragged() {
            let Some(position) = response.interact_pointer_pos() else {
                return;
            };
            let Some(stroke) = self.ink.as_mut() else {
                return;
            };
            let Some(placement) = session.placement(stroke.page()) else {
                return;
            };
            let content = canvas_to_content(canvas, position, scroll);
            if stroke.push(placement.content_to_page(content)) {
                ui.ctx().request_repaint();
            }
            return;
        }
        if response.drag_stopped() {
            self.drag = None;
            let Some(stroke) = self.ink.take() else {
                return;
            };
            match stroke.finish() {
                Some((page, points, width)) => {
                    self.annotate_ink(page, &points, width);
                }
                None => {
                    self.message = "这一笔太短：至少需要两个点".to_string();
                }
            }
        }
    }

    /// **压力测试专用**：在页内铺开 `count` 笔短墨迹（回放动作 `seed_ink`）。
    ///
    /// 与真实绘制走同一个 `annotate_ink` 入口（所以每条都有配对正文、都进撤销栈）；
    /// 布局是确定性的：40 行，行距 18，列距 22，每笔是一个 4 点的小锯齿。
    /// 只给"1000 条以内忙帧 p99"的压力脚本用，不暴露给 UI。
    fn seed_ink_strokes(&mut self, page: PageIndex, count: u32, width: f32) {
        for index in 0..count {
            let row = (index % 40) as f32;
            let column = (index / 40) as f32;
            let x = 40.0 + column * 22.0;
            let y = 80.0 + row * 18.0;
            let points = [
                Point::new(x, y),
                Point::new(x + 6.0, y + 6.0),
                Point::new(x + 12.0, y),
                Point::new(x + 18.0, y + 6.0),
            ];
            self.annotate_ink(page, &points, width);
        }
        self.message = format!("已造 {count} 笔墨迹（压力测试）");
    }

    /// 把页面上的一笔墨迹变成标注（页面空间折线 + 页面空间线宽，D-118 S4）。
    ///
    /// 回放动作 `ink` 与（S4c 的）墨迹工具都调用它：**抬笔时调用一次**，
    /// 因此"一笔 = 一条标注 = 一次撤销"只有这一处实现。
    fn annotate_ink(&mut self, page: PageIndex, points: &[Point], width: f32) -> bool {
        if self.add_annotation_styled(
            page,
            AnnotationKind::Ink {
                points: points.to_vec(),
                width,
            },
            self.ink_color,
            INK_OPACITY,
        ) {
            self.message = "已添加墨迹（Ctrl+Z 可撤销）".to_string();
            true
        } else {
            false
        }
    }

    fn add_highlight(&mut self, page: PageIndex, quads: Vec<Rect>, _bounds: Rect) -> bool {
        self.add_annotation(page, AnnotationKind::Highlight { quads })
    }

    /// 建一条标注并**记入历史**（所有"新建标注"的唯一入口）。
    /// 正文直接属于注释（初始为空；首次编辑时再写）。
    fn add_annotation(&mut self, page: PageIndex, kind: AnnotationKind) -> bool {
        self.add_annotation_with_style(page, kind, None)
    }

    fn add_annotation_styled(
        &mut self,
        page: PageIndex,
        kind: AnnotationKind,
        color: [u8; 3],
        opacity: f32,
    ) -> bool {
        self.add_annotation_with_style(page, kind, Some((color, opacity)))
    }

    fn add_annotation_with_style(
        &mut self,
        page: PageIndex,
        kind: AnnotationKind,
        style: Option<([u8; 3], f32)>,
    ) -> bool {
        let Some(id) = self.annotations.add(page, kind) else {
            return false;
        };
        if let Some((color, opacity)) = style {
            if let Some(annotation) = self.annotations.get_mut(id) {
                annotation.color = color;
                annotation.opacity = opacity.clamp(0.0, 1.0);
            }
        }
        let Some(annotation) = self.annotations.get(id).cloned() else {
            return false;
        };
        // 先把"就地插入"撤掉，再走统一的 `apply_edit` 重新应用 —— 这样"新建"与"撤销后重做"
        // 走的是同一条代码路径，不会一边删了正文、另一边忘了删。
        self.annotations.remove(id);
        self.apply_edit(AppEdit::AddAnnotation { annotation }, true);
        true
    }

    /// 删除 `custom_previews[index]` 之后，把浮窗里的下标引用左移并移除指向它的浮窗。
    fn shift_float_custom_indices(&mut self, index: usize) {
        for window in &mut self.floats {
            if let FloatOwner::Custom(custom) = window.owner {
                if custom > index {
                    window.owner = FloatOwner::Custom(custom - 1);
                }
            }
        }
        self.floats
            .retain(|window| window.owner != FloatOwner::Custom(index));
    }

    /// 删除指针下的标注（`Delete` 键；Ctrl+Z 可撤销）。
    fn delete_hovered_annotation(&mut self) -> bool {
        let Some(id) = self.hovered_annotation else {
            return false;
        };
        let Some(annotation) = self.annotations.get(id).cloned() else {
            return false;
        };
        if self.external_ids.contains(&id) && annotation.pdf_ref.is_none() {
            self.message = "该外部注释没有对象号（内联/无法定位），暂不支持删除".to_string();
            return false;
        }
        self.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        self.message = "已删除标注（Ctrl+Z 可撤销）".to_string();
        true
    }

    /// 命中标注（`content` 是内容空间点，容差按**屏幕像素**换算到页面单位）。
    fn annotation_at(&self, session: &DocumentSession, content: Point) -> Option<AnnotationId> {
        let zoom = session.state().zoom.max(0.01);
        let tolerance = ANNOTATION_HIT_TOLERANCE_PX / zoom;
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page) else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let page_point = placement.content_to_page(content);
            if let Some(hit) = self
                .annotations
                .on_page(page)
                .find(|annotation| annotation.contains(page_point, tolerance))
            {
                return Some(hit.id);
            }
        }
        None
    }

    /// 从注释列表定位：跳页、给中性轮廓、打开正文浮窗。
    fn focus_annotation(&mut self, id: AnnotationId, session: &DocumentSession) {
        let Some(annotation) = self.annotations.get(id) else {
            return;
        };
        let page = annotation.page;
        let bounds = annotation.bounds();
        self.commands.push(ViewerCommand::GoToPage(page));
        let mut at = egui::pos2(360.0, 160.0);
        if self.canvas_rect.width() > 1.0 {
            if let Some(placement) = session.placement(page) {
                if let Some(bounds) = bounds {
                    let content = placement.page_to_content(bounds.min());
                    let scroll = session.viewport().scroll;
                    at = egui::pos2(
                        self.canvas_rect.min.x + content.x - scroll.x,
                        self.canvas_rect.min.y + content.y - scroll.y,
                    );
                }
            }
            let max_x = (self.canvas_rect.right() - 300.0).max(self.canvas_rect.left());
            let max_y = (self.canvas_rect.bottom() - 160.0).max(self.canvas_rect.top());
            at.x = at.x.clamp(self.canvas_rect.left(), max_x);
            at.y = at.y.clamp(self.canvas_rect.top(), max_y);
        }
        self.annotation_focus = Some((id, self.clock.now() + ANNOTATION_FOCUS));
        self.open_annotation_note(id, at);
    }

    /// 点击标注：打开/置顶它的正文浮窗（正文直接属于注释，不再经过 preview）。
    fn open_annotation_note(&mut self, id: AnnotationId, at: egui::Pos2) {
        let Some(annotation) = self.annotations.get(id) else {
            return;
        };
        let body = annotation.body.clone().unwrap_or_default();
        let title = format!("标注 #{}", id.get());
        if let Some(position) = self
            .floats
            .iter()
            .position(|window| window.owner == FloatOwner::Annotation(id))
        {
            let window = self.floats.remove(position);
            let float_id = window.id;
            self.floats.push(window);
            self.highlight_float = Some(float_id);
            self.click_highlight = Some((float_id, self.clock.now() + CLICK_HIGHLIGHT));
            return;
        }
        let rect = egui::Rect::from_min_size(at, EVec2::new(280.0, 150.0));
        let float_id = self.next_float_id;
        self.next_float_id += 1;
        self.floats.push(Float {
            id: float_id,
            title,
            content: FloatContent::Text(body),
            rect,
            owner: FloatOwner::Annotation(id),
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        self.highlight_float = Some(float_id);
    }

    fn spawn_custom_float(&mut self, index: usize, rect: egui::Rect) {
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        // 权威尺寸直接取记录值：与记录完全一致，天然不会误判"已修改"
        let body = preview.body_size;
        let id = self.next_float_id;
        self.next_float_id += 1;
        let content = match (preview.kind, preview.snapshot.clone()) {
            (CustomKind::Image, Some(texture)) => FloatContent::Image(texture),
            _ => FloatContent::Text(preview.text.clone().unwrap_or_default()),
        };
        let title = if preview.title.is_empty() {
            "自定义preview".to_string()
        } else {
            preview.title.clone()
        };
        self.floats.push(Float {
            id,
            title,
            content,
            rect,
            owner: FloatOwner::Custom(index),
            body,
            origin: None,
        });
        self.highlight_float = Some(id);
    }

    /// 删除某个自定义 preview：同时清掉它的浮窗，并把后续条目的 `custom` 下标左移。
    fn delete_custom_preview(&mut self, index: usize) {
        if index >= self.custom_previews.len() {
            return;
        }
        let preview = std::sync::Arc::new(self.custom_previews[index].clone());
        self.apply_edit(AppEdit::DeletePreview { index, preview }, true);
        self.message = "已删除该自定义preview（Ctrl+Z 可撤销）".to_string();
    }

    /// 若某个自定义 preview 已有**正式浮窗**，把它高亮并置顶（侧栏悬浮/点击共用）。
    fn highlight_existing_custom_float(&mut self, index: usize) -> Option<u64> {
        let customs: Vec<Option<usize>> = self.floats.iter().map(|f| f.custom_index()).collect();
        let position = find_float_for_custom(&customs, index)?;
        let window = self.floats.remove(position);
        let id = window.id;
        self.floats.push(window);
        self.highlight_float = Some(id);
        Some(id)
    }

    /// 侧栏条目被点击：生成正式浮窗；若已存在则高亮 + 置顶（一次性高亮有时限）。
    fn activate_custom_preview(&mut self, index: usize, row: egui::Rect) {
        let customs: Vec<Option<usize>> = self.floats.iter().map(|f| f.custom_index()).collect();
        if let Some(position) = find_float_for_custom(&customs, index) {
            let window = self.floats.remove(position);
            let id = window.id;
            self.floats.push(window);
            // 一次性高亮（点击动作需要有限时长）+ 当帧也置为高亮
            self.click_highlight = Some((id, self.clock.now() + CLICK_HIGHLIGHT));
            self.highlight_float = Some(id);
            self.message = "已唤出该自定义preview浮窗".to_string();
            return;
        }
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        let id = self.next_float_id;
        self.next_float_id += 1;
        // 正文尺寸使用该条目**储存的 body_size**（用户要求）
        let size = float_size_for_body(preview.body_size, FLOAT_TITLE_HEIGHT);
        let rect = self.float_rect_at_row(row, size);
        self.spawn_custom_float(index, rect);
        self.click_highlight = Some((id, self.clock.now() + CLICK_HIGHLIGHT));
        self.message = "已生成该自定义preview浮窗".to_string();
    }

    /// "编辑历史"可视列表：只是撤销栈的**视图**（点击不做状态跳转）。
    fn draw_history_window(&mut self, ctx: &egui::Context) {
        if !self.show_history || (!self.history.can_undo() && !self.history.can_redo()) {
            return;
        }
        let mut open = self.show_history;
        egui::Window::new("编辑历史")
            .open(&mut open)
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "可撤销 {} 步 · 可重做 {} 步",
                    self.history.len(),
                    if self.history.can_redo() { 1 } else { 0 }
                ));
                ui.separator();
                // 虚拟化：只排版可见行。1000 条编辑时整表渲染要 300ms+（实测），
                // 而这里按行高只画十几行 —— 历史窗口打开着也不影响帧时间。
                let row_height = ui.spacing().interact_size.y.max(HISTORY_ROW_HEIGHT);
                egui::ScrollArea::vertical()
                    .max_height(HISTORY_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show_rows(ui, row_height, self.history.len(), |ui, range| {
                        for index in range {
                            if let Some(description) = self.history.description(index) {
                                ui.label(format!("{}. {description}", index + 1));
                            }
                        }
                    });
                if self.history.dropped() > 0 {
                    ui.separator();
                    ui.weak(format!(
                        "更早的 {} 条记录已因内存上限被丢弃",
                        self.history.dropped()
                    ));
                }
                ui.separator();
                if ui.button("撤销一步").clicked() {
                    self.undo_edit();
                }
                if ui.button("重做一步").clicked() {
                    self.redo_edit();
                }
            });
        self.show_history = open;
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
                    ui.label(&self.message);
                }
                if let Some(error) = &self.error {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(200, 60, 60), error);
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
            if self.capture.is_none() && wheel_targets_canvas(pointer_over_canvas) {
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
            // 墨迹工具独占指针：拖拽既不滚页也不选字（S4b）
            if self.tool == Tool::Ink {
                self.handle_ink_pointer(ui, canvas, &response, session, session.viewport().scroll);
            } else if self.capture.is_none() && response.drag_started() {
                self.selection_started_on_text = false;
                self.drag_started_at = Some(self.clock.now());
                self.last_pan_at = None;
                if self.diag.enabled() {
                    self.diag.line(format!(
                        "[时序] 拖动开始 t={:.6}s",
                        self.started_at.elapsed().as_secs_f64()
                    ));
                }
                if let Some(position) = response.interact_pointer_pos() {
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

            // 2a'') 右键菜单：指针在标注上时提供"删除标注"（可撤销）——比
            // "悬停 + Delete" 更好发现，也让删除不再依赖键盘焦点。
            if self.capture.is_none() && self.hovered_annotation.is_some() {
                let hovered = self.hovered_annotation;
                response.context_menu(|ui| {
                    if ui.button("删除标注（可撤销）").clicked() {
                        self.hovered_annotation = hovered;
                        self.delete_hovered_annotation();
                        ui.close();
                    }
                });
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
                                // 墨迹在上面按折线画完了，且没有 quads，不会进入本循环。
                                AnnotationKind::Ink { .. } => {}
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
            for window in std::mem::take(&mut self.floats) {
                // 权威尺寸来自浮窗自身（创建/改大小时写入），不做任何反算
                let body = window.body;
                if let Some((page, index)) = window.origin {
                    if let Some(mark) = self
                        .dest_marks
                        .get_mut(page.get() as usize)
                        .and_then(|marks| marks.get_mut(index))
                    {
                        if body_size_changed(mark.body_size, body) {
                            mark.body_size = body;
                            self.data_dirty = true;
                        }
                    }
                }
                if let Some(index) = window.custom_index() {
                    if let Some(preview) = self.custom_previews.get_mut(index) {
                        if body_size_changed(preview.body_size, body) {
                            preview.body_size = body;
                            self.data_dirty = true;
                        }
                    }
                }
                self.floats.push(window);
            }

            // 固定浮窗：绘制与注册都在此；后加入的在更上层（见 draw_floats）
            if self.capture.is_none() && self.custom_pin.is_none() {
                // 正在显示、但还没拿到预览图的自定义图片 preview：每帧重试一次。
                // 旧实现只在"悬浮/点击侧栏条目"时重试，于是"点了但渲染还没好"的浮窗
                // 会一直空着（D-101 把初次生成改成了"提交渲染 + 等待"，必须有重试入口）。
                self.ensure_pending_custom_snapshots(ui, session);
                self.draw_floats(ui, canvas);
                // 正文原位编辑：先检测双击，再绘制编辑框（两者都在浮窗之后，层级正确）
                self.detect_body_double_click(ui);
                self.draw_float_body_editing(ui);
                // 临时预览**最后**绘制 → 真正位于最上层（用户要求）
                self.draw_hover_preview(ui, canvas);
                // 侧栏条目悬浮 → 主视图右侧的临时浮窗
                self.draw_sidebar_hover_float(ui, canvas);
            } else if self.capture.is_some() {
                self.draw_capture(ui, canvas, session, scroll);
            }
        });
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

    /// 悬停预览浮窗：检测鼠标下的页内链接 → 找到对应被引用点 → 必要时**渲染一次**截图 →
    /// 画一个临时浮窗。指针离开链接与浮窗后消失；点击它则转为永久浮窗。
    fn update_hover_preview(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        canvas: egui::Rect,
        scroll: Vec2,
    ) {
        let pointer = ui.ctx().pointer_hover_pos();

        // 闸门（用户要求）：
        // 1. 滚动/缩放的静默窗口内不显示；
        // 2. 指针落在**任何浮窗**（永久或临时）之上时不显示 —— 浮窗屏蔽主视图动作，
        //    被浮窗盖住的链接不应再有悬停反馈。
        let scrolling = self
            .scroll_quiet_until
            .is_some_and(|deadline| self.clock.now() < deadline);
        let float_rects: Vec<egui::Rect> = self.floats.iter().map(|window| window.rect).collect();
        let over_float = pointer.is_some_and(|position| pointer_over_any(position, &float_rects));
        // 3. **画墨迹期间**不弹出（用户实测：笔画压住链接时不该出现目的地预览）。
        //    `ink` 要等指针移过拖动阈值才建，所以"按住但还没成笔"这一小段也算绘制中。
        let drawing = self.tool == Tool::Ink
            && (self.ink.is_some() || ui.input(|input| input.pointer.primary_down()));
        if !hover_preview_allowed(scrolling, over_float, drawing) {
            self.hover_tip = None;
            return;
        }

        let mut found: Option<((PageIndex, usize), egui::Pos2)> = None;

        if let (Some(pointer), Some(navigation)) = (pointer, self.navigation.clone()) {
            'outer: for page in self.visible.clone() {
                let Some(placement) = session.placement(page).copied() else {
                    continue;
                };
                let Some(registered) = self.link_marks.get(page.get() as usize) else {
                    continue;
                };
                for (index, link) in navigation.links_on(page).iter().enumerate() {
                    // 链接 → 被引用点：**直接取登记表**（建表时已解析）
                    let Some(Some(target)) = registered.get(index).copied() else {
                        continue;
                    };
                    let content = placement.rect_to_content(link.rect);
                    let screen = egui::Rect::from_min_size(
                        canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                        EVec2::new(content.width, content.height),
                    );
                    if !screen.contains(pointer) {
                        continue;
                    }
                    // 锚点 = 链接框中心（浮窗左上角放这里，保证与链接框重叠）
                    found = Some((target, screen.center()));
                    // 记录链接框尺寸，供 tip_rect_for 计算（只需宽高）
                    self.hover_link_size = screen.size();
                    break 'outer;
                }
            }
        }

        // 指针不在链接上时：只有还在浮窗上才保留
        let keep_existing = match (&self.hover_tip, pointer) {
            (Some(tip), Some(pointer)) => tip.rect.is_some_and(|rect| rect.contains(pointer)),
            _ => false,
        };
        // 该目的地的浮窗**已经固定且仍在显示** → 不再弹临时预览，
        // 改为高亮它并把提到最上层（"重新激发"，用户要求）。
        if let Some(target) = found.map(|(target, _)| target) {
            let origins: Vec<Option<(PageIndex, usize)>> =
                self.floats.iter().map(|window| window.origin).collect();
            if let Some(position) = find_float_for_target(&origins, target) {
                let window = self.floats.remove(position);
                self.highlight_float = Some(window.id);
                self.floats.push(window); // 移到末尾 → 绘制与交互都在最上层
                self.hover_tip = None;
                return;
            }
        }

        match found {
            Some((target, anchor)) => {
                self.hover_tip = Some(HoverTip {
                    target,
                    anchor,
                    rect: None,
                });
            }
            None if keep_existing => {}
            None => {
                self.hover_tip = None;
                return;
            }
        }

        // 需要时**按记录的分辨率**生成一次（之后复用；缺像素则提交渲染、下帧重试）
        let Some(tip) = self.hover_tip.as_ref() else {
            return;
        };
        let (page, index) = tip.target;
        let pending = self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
            .filter(|mark| mark.snapshot.is_none())
            .map(|mark| (mark.rect, mark.scale));
        if let Some((saved_rect, scale)) = pending {
            let texture = self.preview_texture(
                ui,
                session,
                page,
                saved_rect,
                scale,
                format!("dest-hover-{}-{}", page.get(), index),
                "被引用点",
            );
            if let Some(texture) = texture {
                if let Some(mark) = self
                    .dest_marks
                    .get_mut(page.get() as usize)
                    .and_then(|marks| marks.get_mut(index))
                {
                    mark.snapshot = Some(texture);
                }
            }
        }

        // 快照已就绪；实际绘制在 `draw_hover_preview`（要画在固定浮窗之上 → 最上层）
        if self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
            .and_then(|mark| mark.snapshot.clone())
            .is_none()
        {}
    }

    /// 绘制临时预览浮窗并处理"点击固定"（本函数在所有固定浮窗之后调用 → 位于最上层）。
    fn draw_hover_preview(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        let Some(tip) = self.hover_tip.as_ref() else {
            return;
        };
        let (page, index) = tip.target;
        let anchor = tip.anchor;
        let Some(texture) = self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
            .and_then(|mark| mark.snapshot.clone())
        else {
            self.message = "该被引用点还没有可用的截图（稍后重试）".to_string();
            return;
        };
        // 正文尺寸取该被引用点储存的 body_size（用户要求）
        let body = self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
            .map(|mark| mark.body_size)
            .unwrap_or_else(|| texture.size_vec2());
        let wanted = float_size_for_body(body, FLOAT_TITLE_HEIGHT);
        // 锚点 anchor = 链接框中心；把它当作链接框中心传进去（尺寸用上一帧记录的链接框尺寸）
        let link_rect = egui::Rect::from_center_size(anchor, self.hover_link_size);
        let rect = tip_rect_for(link_rect, wanted, canvas);
        // 临时预览：**点击或拖动都固定**；拖动时把位移带到新浮窗上（用户要求）
        let response = ui.interact(rect, Id::unique("hover-tip"), Sense::click_and_drag());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(30, 32, 36));
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_gray(150)),
            StrokeKind::Inside,
        );
        let title_rect =
            egui::Rect::from_min_size(rect.min, EVec2::new(rect.width(), FLOAT_TITLE_HEIGHT));
        painter.rect_filled(
            title_rect,
            CornerRadius::same(4),
            Color32::from_rgb(54, 58, 66),
        );
        let tip_title = format!("引用点预览 · 第 {} 页（点击固定）", page.display_number());
        let tip_job = overflow_layout(
            &tip_title,
            egui::FontId::proportional(12.0),
            Color32::WHITE,
            title_rect.width() - 12.0,
            1,
        );
        let tip_galley = painter.layout_job(tip_job);
        painter.galley(
            egui::pos2(
                title_rect.min.x + 8.0,
                title_rect.center().y - tip_galley.size().y * 0.5,
            ),
            tip_galley,
            Color32::WHITE,
        );
        let body = egui::Rect::from_min_max(
            rect.min + EVec2::new(4.0, FLOAT_TITLE_HEIGHT + 4.0),
            rect.max - EVec2::new(4.0, 4.0),
        );
        painter.image(
            texture.id(),
            fit_inside(texture.size_vec2(), body),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        if let Some(tip) = self.hover_tip.as_mut() {
            tip.rect = Some(rect);
        }

        // 点击或拖动 → 转为永久浮窗（保留同一张图；拖动时采用偏移后的位置）
        if response.clicked() || response.drag_started() {
            let id = self.next_float_id;
            self.next_float_id += 1;
            self.floats.push(Float {
                id,
                title: format!("引用点截图 · 第 {} 页", page.display_number()),
                content: FloatContent::Image(texture),
                origin: Some((page, index)),
                owner: FloatOwner::None,
                body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
                rect: clamp_float_rect(
                    rect.translate(response.drag_delta()),
                    canvas,
                    FLOAT_MIN_SIZE,
                ),
            });
            self.hover_tip = None;
            self.message = format!("已固定为浮窗 #{id}");
        }
    }

    /// 侧栏 Pin 条目被悬浮时，在主视图**右侧**显示该条目的临时浮窗（离开条目即关闭）。
    fn draw_sidebar_hover_float(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        let Some((index, row)) = self.custom_hover else {
            return;
        };
        let Some(preview) = self.custom_previews.get(index) else {
            self.custom_hover = None;
            return;
        };
        let title = if preview.title.is_empty() {
            "自定义preview".to_string()
        } else {
            preview.title.clone()
        };
        let text = preview.text.clone();
        let texture = preview.snapshot.clone();
        // 与"点击生成的固定浮窗"用同一套定位：紧贴侧边栏、与条目行垂直对齐；
        // 正文尺寸同样取该条目的 body_size（用户要求）
        let size = float_size_for_body(preview.body_size, FLOAT_TITLE_HEIGHT);
        let rect = clamp_float_rect(
            egui::Rect::from_min_size(
                egui::pos2(canvas.min.x + 16.0, row.center().y - size.y * 0.5),
                size,
            ),
            canvas,
            FLOAT_MIN_SIZE,
        );
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(30, 32, 36));
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_gray(150)),
            StrokeKind::Inside,
        );
        let title_rect =
            egui::Rect::from_min_size(rect.min, EVec2::new(rect.width(), FLOAT_TITLE_HEIGHT));
        painter.rect_filled(
            title_rect,
            CornerRadius::same(4),
            Color32::from_rgb(54, 58, 66),
        );
        let job = overflow_layout(
            &title,
            egui::FontId::proportional(13.0),
            Color32::WHITE,
            title_rect.width() - 12.0,
            1,
        );
        let galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(
                title_rect.min.x + 8.0,
                title_rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            Color32::WHITE,
        );
        let body = egui::Rect::from_min_max(
            rect.min + EVec2::new(8.0, FLOAT_TITLE_HEIGHT + 6.0),
            rect.max - EVec2::new(8.0, 6.0),
        );
        match (texture, text) {
            (Some(texture), _) => {
                painter.image(
                    texture.id(),
                    fit_inside(texture.size_vec2(), body),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            (None, Some(text)) => {
                let row_height = 13.0 * 1.3;
                let rows = ((body.height() / row_height).floor() as usize).max(1);
                let job = overflow_layout(
                    &text,
                    egui::FontId::proportional(13.0),
                    Color32::from_gray(225),
                    body.width(),
                    rows,
                );
                let galley = painter.layout_job(job);
                painter
                    .with_clip_rect(body)
                    .galley(body.min, galley, Color32::from_gray(225));
            }
            (None, None) => {
                painter.text(
                    body.min,
                    Align2::LEFT_TOP,
                    "（无内容）",
                    egui::FontId::proportional(13.0),
                    Color32::from_gray(160),
                );
            }
        }
    }

    /// 浮窗正文的原位编辑：双击进入，失焦提交。
    ///
    /// * 自定义 preview：沿用旧行为（直接写 preview.text，不进撤销栈）；
    /// * 注释：写入 `Annotation.body`，走 `AppEdit::SetBody`（可撤销）。
    fn draw_float_body_editing(&mut self, ui: &mut egui::Ui) {
        let Some((editing_owner, _)) = self.editing_body.clone() else {
            return;
        };
        let Some(window) = self
            .floats
            .iter()
            .find(|window| window.owner == editing_owner)
        else {
            self.editing_body = None;
            return;
        };
        let body_rect = egui::Rect::from_min_max(
            window.rect.min + EVec2::new(8.0, FLOAT_TITLE_HEIGHT + 4.0),
            window.rect.max - EVec2::new(8.0, 8.0),
        );
        let Some((_, draft)) = self.editing_body.take() else {
            return;
        };
        let mut text = draft;
        let response = ui.put(
            body_rect,
            egui::TextEdit::multiline(&mut text)
                .desired_width(body_rect.width())
                .desired_rows(6),
        );
        if self.focus_body_edit {
            response.request_focus();
            self.focus_body_edit = false;
        }
        if response.lost_focus() {
            match editing_owner {
                FloatOwner::Custom(index) => {
                    if let Some(preview) = self.custom_previews.get_mut(index) {
                        preview.text = Some(text);
                    }
                    self.data_dirty = true;
                }
                FloatOwner::Annotation(id) => {
                    if self.external_ids.contains(&id)
                        && self
                            .annotations
                            .get(id)
                            .is_some_and(|annotation| annotation.pdf_ref.is_none())
                    {
                        self.message =
                            "该外部注释没有对象号（内联/无法定位），暂不支持编辑".to_string();
                        self.editing_body = None;
                        return;
                    }
                    let before = self.annotations.get(id).and_then(|a| a.body.clone());
                    let after = if text.is_empty() { None } else { Some(text) };
                    if before != after {
                        let page = self
                            .annotations
                            .get(id)
                            .map(|a| a.page)
                            .unwrap_or(PageIndex::FIRST);
                        self.apply_edit(
                            AppEdit::SetBody {
                                id,
                                page,
                                before,
                                after,
                            },
                            true,
                        );
                    }
                }
                FloatOwner::None => {}
            }
            self.editing_body = None;
            self.message = "已更新内容".to_string();
        } else {
            self.editing_body = Some((editing_owner, text));
        }
    }

    /// 双击浮窗正文 → 进入原位编辑（自定义文字 preview 或注释正文）。
    fn detect_body_double_click(&mut self, ui: &mut egui::Ui) {
        if self.editing_body.is_some() {
            return;
        }
        let targets: Vec<(u64, FloatOwner, egui::Rect)> = self
            .floats
            .iter()
            .filter_map(|window| match window.owner {
                FloatOwner::Custom(index) => {
                    let preview = self.custom_previews.get(index)?;
                    (preview.kind == CustomKind::Text).then_some((
                        window.id,
                        window.owner,
                        window.rect,
                    ))
                }
                FloatOwner::Annotation(_) => Some((window.id, window.owner, window.rect)),
                FloatOwner::None => None,
            })
            .collect();
        for (id, owner, rect) in targets {
            let body_rect = egui::Rect::from_min_max(
                rect.min + EVec2::new(8.0, FLOAT_TITLE_HEIGHT + 4.0),
                rect.max - EVec2::new(8.0, 8.0),
            );
            let hit = ui.interact(
                body_rect,
                Id::unique(("float-body-edit", id)),
                Sense::click(),
            );
            if hit.double_clicked() {
                let text = match owner {
                    FloatOwner::Custom(index) => self
                        .custom_previews
                        .get(index)
                        .and_then(|preview| preview.text.clone())
                        .unwrap_or_default(),
                    FloatOwner::Annotation(id) => self
                        .annotations
                        .get(id)
                        .and_then(|annotation| annotation.body.clone())
                        .unwrap_or_default(),
                    FloatOwner::None => String::new(),
                };
                self.editing_body = Some((owner, text));
                self.focus_body_edit = true;
                ui.ctx().request_repaint();
                return;
            }
        }
    }

    /// 编辑题目 / 正文的对话框（✎ 按钮与"固定浮窗内双击文字"共用）。
    fn draw_edit_dialogs(&mut self, ctx: &egui::Context) {
        // ---- 题目 ----
        if let Some((index, draft)) = self.editing_title.clone() {
            let mut text = draft;
            let mut close = false;
            egui::Modal::new(Id::unique("edit-title")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.heading("修改题目");
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("save").clicked() || ui.button("关闭").clicked() {
                        close = true;
                    }
                    if ui.button("cancel").clicked() {
                        text = self
                            .custom_previews
                            .get(index)
                            .map(|preview| preview.title.clone())
                            .unwrap_or_default();
                        close = true;
                    }
                });
            });
            self.editing_title = Some((index, text.clone()));
            if close {
                if let Some(preview) = self.custom_previews.get_mut(index) {
                    preview.title = text;
                }
                self.editing_title = None;
                self.data_dirty = true;
                self.message = "已更新题目".to_string();
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
        let Some(preview) = self.custom_previews.get_mut(index) else {
            return;
        };
        preview.snapshot = Some(texture);
        preview.rect = page_rect;
        preview.body_size = egui::vec2(overlap.width, overlap.height);
        // ▲v2：记录这张图的分辨率（截图时的显示比例）—— 之后重建预览时按它请求渲染
        preview.scale = session.state().zoom;
        self.data_dirty = true;
        self.message = format!(
            "已保存图片预览（取第 {} 页，交叠 {:.0}×{:.0}）",
            page.display_number(),
            overlap.width,
            overlap.height
        );
    }

    /// 进入 Pin 状态：**预览模式回到平移**、退出截图模式（用户要求）。
    fn enter_pin(&mut self, kind: CustomKind) {
        self.tool = Tool::Pan;
        self.capture = None;
        self.custom_pin = Some(kind);
        self.message = "当前自定义preview：点击页面确定位置（Esc 取消）".to_string();
    }

    /// Pin 状态：在页面某处点击 → 结束选点并弹出对应的输入对话框。
    fn finish_pin(
        &mut self,
        session: &DocumentSession,
        canvas: egui::Rect,
        scroll: Vec2,
        pointer: egui::Pos2,
    ) {
        let Some(kind) = self.custom_pin else { return };
        let content = canvas_to_content(canvas, pointer, scroll);
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let point = placement.content_to_page(content);
            self.custom_dialog = Some(CustomDialog {
                kind,
                page,
                point,
                title: String::new(),
                content: String::new(),
            });
            self.custom_pin = None;
            return;
        }
    }

    /// 自定义 preview 的输入对话框（文字：题目+内容；图片：题目）。
    fn draw_custom_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.custom_dialog.as_ref() else {
            return;
        };
        let kind = dialog.kind;
        let page = dialog.page;
        let point = dialog.point;
        let mut title = dialog.title.clone();
        let mut content = dialog.content.clone();
        let mut save = false;
        let mut cancel = false;
        let mut shoot = false;
        egui::Modal::new(Id::unique("custom-preview-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(match kind {
                CustomKind::Text => "文字预览",
                CustomKind::Image => "图片预览",
            });
            ui.label(format!(
                "位置：第 {} 页 ({:.1}, {:.1})",
                page.display_number(),
                point.x,
                point.y
            ));
            ui.separator();
            ui.label("题目");
            ui.add(egui::TextEdit::singleline(&mut title).desired_width(f32::INFINITY));
            if kind == CustomKind::Text {
                ui.label("内容");
                ui.add(
                    egui::TextEdit::multiline(&mut content)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6),
                );
            }
            ui.separator();
            ui.horizontal(|ui| {
                match kind {
                    // 文字预览：关闭 与 save 都是保存（用户要求）
                    CustomKind::Text => {
                        if ui.button("关闭").clicked() || ui.button("save").clicked() {
                            save = true;
                        }
                    }
                    // 图片预览：关闭 与 截图 都进入截图阶段
                    CustomKind::Image => {
                        if ui.button("关闭").clicked() || ui.button("截图").clicked() {
                            shoot = true;
                        }
                    }
                }
                if ui.button("cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if let Some(state) = self.custom_dialog.as_mut() {
            state.title = title;
            state.content = content;
        }

        if save {
            if let Some(dialog) = self.custom_dialog.take() {
                let body_size = DEFAULT_CUSTOM_TEXT_BODY;
                self.custom_previews.push(CustomPreview {
                    page: dialog.page,
                    point: dialog.point,
                    rect: zero_rect_at(dialog.point),
                    kind: CustomKind::Text,
                    title: dialog.title.clone(),
                    text: Some(dialog.content.clone()),
                    snapshot: None,
                    body_size,
                    scale: 1.0,
                });
                // **不**自动生成浮窗（用户要求）：只在从侧栏悬浮/点击该条目时出现
                self.data_dirty = true;
                self.message = "已保存文字预览（从侧栏「自定义preview」唤出）".to_string();
            }
        } else if shoot {
            if let Some(dialog) = self.custom_dialog.take() {
                let index = self.custom_previews.len();
                self.custom_previews.push(CustomPreview {
                    page: dialog.page,
                    point: dialog.point,
                    rect: zero_rect_at(dialog.point),
                    kind: CustomKind::Image,
                    title: dialog.title,
                    text: None,
                    snapshot: None,
                    // 占位：截图完成时按"选区在当时缩放下的显示尺寸"覆盖
                    body_size: DEFAULT_CUSTOM_TEXT_BODY,
                    // 占位：截图完成时写入当时的显示比例
                    scale: 1.0,
                });
                // 进入截图模式：结果回写到这条自定义记录
                self.capture = Some(CaptureState {
                    rect: None,
                    anchor: None,
                    target: CaptureTarget::Custom(index),
                });
                self.message = "截图：拖动框选区域，Enter 保存，Esc 取消".to_string();
            }
        } else if cancel {
            self.custom_dialog = None;
            self.message = "已取消自定义preview".to_string();
        }
    }

    /// 截图模式：框选区域 + 半透明蒙层（矩形内透明）+ Enter/Esc 退出。
    ///
    /// * 蒙层用"绕开选区的四块"实现（egui 没有擦除混合，见 `view::veil_bands`）；
    /// * 拖动即框选；松开后选区保留，可再次拖动重画；
    /// * `Enter` → 终端输出区域信息（视图坐标 + 覆盖页的页面空间矩形）并退出；
    /// * `Esc` → 直接退出，无输出。
    fn draw_capture(
        &mut self,
        ui: &mut egui::Ui,
        canvas: egui::Rect,
        session: &DocumentSession,
        scroll: Vec2,
    ) {
        // ---- 框选 ----
        let response = ui.interact(canvas, Id::unique("capture-area"), Sense::drag());
        if let Some(position) = response.interact_pointer_pos() {
            if response.drag_started() {
                self.capture.as_mut().unwrap().anchor = Some(position);
                self.capture.as_mut().unwrap().rect = None;
            }
            if response.dragged() {
                if let Some(anchor) = self.capture.as_ref().and_then(|state| state.anchor) {
                    let rect = egui::Rect::from_two_pos(anchor, position).intersect(canvas);
                    self.capture.as_mut().unwrap().rect = Some(rect);
                }
                ui.ctx().request_repaint();
            }
        }
        if response.drag_stopped() {
            self.capture.as_mut().unwrap().anchor = None;
        }

        // ---- 蒙层（选区透明）+ 选区边框 ----
        let selection = self.capture.as_ref().and_then(|state| state.rect);
        let veil_color = Color32::from_rgba_unmultiplied(225, 225, 225, 110);
        for band in veil_bands(canvas, selection) {
            ui.painter()
                .rect_filled(band, CornerRadius::ZERO, veil_color);
        }
        if let Some(selection) = selection {
            ui.painter().rect_stroke(
                selection,
                CornerRadius::ZERO,
                Stroke::new(1.5, Color32::from_rgb(60, 120, 220)),
                StrokeKind::Inside,
            );
        }

        // ---- Enter / Esc ----
        let (enter, escape) = ui.ctx().input(|input| {
            (
                input.key_pressed(egui::Key::Enter),
                input.key_pressed(egui::Key::Escape),
            )
        });
        if enter {
            match selection {
                Some(selection) => {
                    // 屏幕矩形 → 内容空间矩形（供快照与页面空间换算使用）
                    let selection_rect = screen_rect_to_content(canvas.min, selection, scroll);
                    match self.capture.as_ref().map(|state| state.target) {
                        // 1a) 由被引用点标识进入：回写该点的矩形与截图，并用浮窗展示新图
                        Some(CaptureTarget::DestMark(page, index)) => {
                            self.commit_dest_mark(ui, session, page, index, selection_rect);
                        }
                        // 1c) 自定义图片预览：把截图存进该记录，并以固定浮窗显示
                        Some(CaptureTarget::Custom(index)) => {
                            self.commit_custom_image(ui, session, index, selection_rect);
                        }
                        // 1b) 普通截图：只放进一个新浮窗
                        _ => self.capture_into_float(ui, session, selection_rect),
                    }
                    let mut parts: Vec<String> = Vec::new();
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else {
                            continue;
                        };
                        // 注意：`selection_rect` 已是**内容空间**，故这里传零滚动
                        if let Some(page_rect) =
                            view_rect_to_page_rect(&placement, selection_rect, Vec2::ZERO)
                        {
                            parts.push(format!(
                                "页 {} 页空间 ({:.1},{:.1} {:.1}×{:.1})",
                                page.display_number(),
                                page_rect.x,
                                page_rect.y,
                                page_rect.width,
                                page_rect.height
                            ));
                        }
                    }
                    self.diag.line(format!(
                        "[截图] 视图区域 ({:.1},{:.1} {:.1}×{:.1}){}",
                        selection.min.x,
                        selection.min.y,
                        selection.width(),
                        selection.height(),
                        if parts.is_empty() {
                            String::new()
                        } else {
                            format!(" | {}", parts.join(" | "))
                        }
                    ));
                    self.message = format!(
                        "截图区域已输出：{:.0}×{:.0}",
                        selection.width(),
                        selection.height()
                    );
                }
                None => {
                    self.diag.line("[截图] 未框选任何区域".to_string());
                    self.message = "截图模式：未框选区域".to_string();
                }
            }
            self.capture = None;
        } else if escape {
            self.capture = None;
            self.message = "已退出截图模式".to_string();
        }
    }

    /// 把截图模式选中的区域**回写**到某个被引用点：保存与该页重叠的矩形 + 构建截图 + 浮窗展示。
    ///
    /// 用户要求：矩形可能超出单页范围，因此**只保存与当前页重叠的部分**；
    /// 若完全不重叠则不改动原信息并给出提示。
    fn commit_dest_mark(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        page: PageIndex,
        index: usize,
        content_rect: Rect,
    ) {
        let Some(placement) = session.placement(page).copied() else {
            return;
        };
        // 只保留与目标页重叠的部分（区域可能跨页/跨留白）
        let Some(overlap) = content_rect.intersection(placement.rect) else {
            self.message = "截图区域与该被引用点所在页没有重叠，未保存".to_string();
            return;
        };
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            self.message = "截图区域与该被引用点所在页没有重叠，未保存".to_string();
            return;
        }
        // 内容空间 → 页面空间（保存到被引用点）
        let Some(page_rect) = view_rect_to_page_rect(&placement, overlap, Vec2::ZERO) else {
            self.message = "截图区域换算失败，未保存".to_string();
            return;
        };
        let page_rect = Rect::new(
            page_rect.x.round(),
            page_rect.y.round(),
            page_rect.width.round(),
            page_rect.height.round(),
        );

        // 构建快照（只用与该页重叠的部分）
        let Some(snapshot) = self.snapshot_for(ui, session, overlap) else {
            self.message = "该区域还没有已渲染的像素（请稍候再试）".to_string();
            return;
        };
        let texture = ui.ctx().load_texture(
            format!("dest-{}-{}", page.get(), index),
            snapshot,
            egui::TextureOptions::LINEAR,
        );

        // 回写：矩形 + 截图 + **正文尺寸**（= 该选区在**当时缩放比例**下的显示尺寸，用户要求）
        let old = self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
            .cloned();
        let mut rect_saved = false;
        if let Some(old) = old {
            let mut after = old.clone();
            after.rect = page_rect;
            after.snapshot = Some(texture.clone());
            after.body_size = egui::vec2(overlap.width, overlap.height);
            // ▲v2：记录这张图的分辨率（截图时的显示比例）
            after.scale = session.state().zoom;
            self.apply_edit(
                AppEdit::EditDestMark {
                    page,
                    index,
                    before: std::sync::Arc::new(old),
                    after: std::sync::Arc::new(after),
                },
                true,
            );
            rect_saved = true;
        }
        if !rect_saved {
            self.message = "被引用点已不存在，未保存".to_string();
            return;
        }

        // **不**自动生成浮窗（用户要求）：截图只写回数据，浮窗由悬停链接或点击标识按需出现
        self.message = format!(
            "已保存第 {} 页被引用点 #{} 的截图（矩形 {:.0}×{:.0} pt）",
            page.display_number(),
            index + 1,
            page_rect.width,
            page_rect.height
        );
    }

    /// 从**已缓存**位图合成一块内容空间区域的快照。
    fn snapshot_for(
        &self,
        _ui: &mut egui::Ui,
        session: &DocumentSession,
        content_rect: Rect,
    ) -> Option<egui::ColorImage> {
        let mut owned: Vec<SnapshotLayer> = Vec::new();
        // **跨页**：凡与选区相交的页都参与拼合（不依赖 visible 集合，也不只取最大重叠那一页）
        let all_pages: Vec<(usize, Rect)> = (0..session.page_count() as usize)
            .filter_map(|index| {
                let page = PageIndex::new(index as u32);
                session
                    .placement(page)
                    .map(|placement| (index, placement.rect))
            })
            .collect();
        for (index, page_rect) in pages_intersecting(all_pages, content_rect) {
            let page = PageIndex::new(index as u32);
            // D-94：块的**显示矩形**由网格给出（位图像素 ≠ 内容单位），
            // 因此这里带上每块在内容空间的位置，合成时才能按 u 正确重采样。
            let tiles = match (session.placement(page).copied(), session.tile_grid(page)) {
                (Some(placement), Some(grid)) => {
                    let render = session.render_scale(page);
                    session
                        .tiles_for(page)
                        .into_iter()
                        .map(|(tile, image)| {
                            (
                                grid.display_rect(
                                    placement.rect,
                                    tile.col,
                                    tile.row,
                                    placement.scale.0,
                                    render.0,
                                ),
                                image,
                            )
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            owned.push((page_rect, session.image_for(page), tiles));
        }
        let sources: Vec<SnapshotPage<'_>> = owned
            .iter()
            .map(|(rect, whole, tiles)| SnapshotPage {
                rect: *rect,
                whole: whole.as_deref(),
                tiles: tiles
                    .iter()
                    .map(|(rect, image)| (*rect, image.as_ref()))
                    .collect(),
            })
            .collect();
        compose_snapshot(&sources, content_rect)
    }

    /// 把选中的区域做成截图快照，放入一个新的浮窗。
    ///
    /// 快照来源是**已经渲染好的位图**（整页或瓦片，直接从缓存取），因此无需重新渲染；
    /// 拷贝成独立纹理后与主视图状态完全解耦：之后缩放/滚动/换页都不会改变浮窗内容。
    fn capture_into_float(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        // `content_rect` 已经是**内容空间**矩形（调用方用 `screen_rect_to_content` 换算过）
        content_rect: Rect,
    ) {
        // `content_rect` 由调用方从屏幕坐标换算而来（**必须减去画布原点**，见 view::screen_rect_to_content）

        let Some(snapshot) = self.snapshot_for(ui, session, content_rect) else {
            self.message = "截图：该区域还没有已渲染的像素（请稍候再试）".to_string();
            return;
        };

        let id = self.next_float_id;
        self.next_float_id += 1;
        let texture = ui.ctx().load_texture(
            format!("snapshot-{id}"),
            snapshot,
            egui::TextureOptions::LINEAR,
        );
        // 默认窗口大小按区域尺寸给（夹取在合理范围内），之后可自由改大小
        let size = EVec2::new(
            content_rect.width.clamp(FLOAT_MIN_SIZE.x, 640.0),
            content_rect.height.clamp(FLOAT_MIN_SIZE.y, 640.0) + FLOAT_TITLE_HEIGHT,
        );
        let cascade = ((id as f32 - 1.0) % 6.0) * 24.0;
        self.floats.push(Float {
            id,
            title: format!("截图 #{id}"),
            content: FloatContent::Image(texture),
            body: body_size_of(
                egui::Rect::from_min_size(egui::pos2(32.0 + cascade, 32.0 + cascade), size),
                FLOAT_TITLE_HEIGHT,
            ),
            rect: egui::Rect::from_min_size(egui::pos2(32.0 + cascade, 32.0 + cascade), size),
            origin: None,
            owner: FloatOwner::None,
        });
        self.message = format!("已把截图放入浮窗 #{id}");
    }

    /// 绘制并处理浮窗：移动（拖标题栏）、改大小（拖右下角）、关闭（右上角 ✕）。
    ///
    /// 注册顺序即层级：按 `floats` 顺序注册 → **后加入的浮窗既画在上面、也优先拿到鼠标**
    /// （用户要求"后出现的浮窗更高"）。子区域（标题栏/把手/关闭按钮）在主提之后注册，
    /// 因此优先于主体，不会被主体吞掉。
    fn draw_floats(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        if self.floats.is_empty() {
            return;
        }
        let mut close: Option<u64> = None;
        let mut raise: Option<u64> = None;
        for index in 0..self.floats.len() {
            // 每帧夹取回视图区域内（用户要求：浮窗始终在视图区域内部）
            let rect = clamp_float_rect(self.floats[index].rect, canvas, FLOAT_MIN_SIZE);
            self.floats[index].rect = rect;
            let id = self.floats[index].id;

            // 主体：占住点击（避免"点在浮窗上却给文档落了光标"）；不参与拖动。
            // 点击主体任意处（除关闭按钮）→ 该浮窗置顶，其余浮窗相对顺序不变。
            // 主体（文字区/截图区）同样**整体拖动浮窗** —— 不能让拖动穿透去拖 PDF 页面
            let body = ui.interact(rect, Id::unique(("float", id)), Sense::click_and_drag());
            if body.clicked() || body.drag_started() {
                raise = Some(id);
            }
            if body.dragged() {
                self.floats[index].rect =
                    drag_float_rect(rect, body.drag_delta(), canvas, FLOAT_MIN_SIZE);
            }

            // 标题栏：拖动移动
            let title_rect =
                egui::Rect::from_min_size(rect.min, EVec2::new(rect.width(), FLOAT_TITLE_HEIGHT));
            // 标题栏：**点击或开始拖动都要置顶**（用户要求：点击区域要包含标题栏）；
            // 拖动则移动浮窗。
            let title = ui.interact(
                title_rect,
                Id::unique(("float-title", id)),
                Sense::click_and_drag(),
            );
            if title.clicked() || title.drag_started() {
                raise = Some(id);
            }
            if title.dragged() {
                self.floats[index].rect =
                    drag_float_rect(rect, title.drag_delta(), canvas, FLOAT_MIN_SIZE);
            }

            // 右下角把手：拖动改大小（不小于最小尺寸）
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(
                    rect.right() - FLOAT_HANDLE_SIZE,
                    rect.bottom() - FLOAT_HANDLE_SIZE,
                ),
                EVec2::splat(FLOAT_HANDLE_SIZE),
            );
            let handle = ui.interact(handle_rect, Id::unique(("float-resize", id)), Sense::drag());
            if handle.dragged() {
                let delta = handle.drag_delta();
                let resized = egui::Rect::from_min_size(
                    rect.min,
                    EVec2::new(
                        (rect.width() + delta.x).max(FLOAT_MIN_SIZE.x),
                        (rect.height() + delta.y).max(FLOAT_MIN_SIZE.y),
                    ),
                );
                self.floats[index].rect = resized;
                // 尺寸是**权威值**：只在真正改大小时写入（不从 rect 每帧反算）
                self.floats[index].body = body_size_of(resized, FLOAT_TITLE_HEIGHT);
            }

            // 右上角关闭按钮：**画成带底色的圆**，悬停/按下变红 —— 否则只有一个"✕"太不显眼
            let close_center = egui::pos2(title_rect.right() - 13.0, title_rect.center().y);
            let close_rect = egui::Rect::from_center_size(close_center, EVec2::splat(18.0));
            let close_response =
                ui.interact(close_rect, Id::unique(("float-close", id)), Sense::click());
            let close_color = if close_response.is_pointer_button_down_on() {
                Color32::from_rgb(170, 45, 45)
            } else if close_response.hovered() {
                Color32::from_rgb(205, 70, 70)
            } else {
                Color32::from_gray(95)
            };
            ui.painter().circle_filled(close_center, 8.5, close_color);
            ui.painter().text(
                close_center,
                Align2::CENTER_CENTER,
                "✕",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            if close_response.on_hover_text("关闭浮窗").clicked() {
                close = Some(id);
            }

            // ---- 绘制 ----
            // 被"再次悬停同一目的地"激发时高亮：边框加粗变橙、标题栏变暖色
            // 高亮有两个来源：悬浮（每帧判定）与点击（一次性，带截止时刻）
            let clicked_highlight = self
                .click_highlight
                .is_some_and(|(target, until)| target == id && self.clock.now() < until);
            let highlighted = self.highlight_float == Some(id) || clicked_highlight;
            let (border, border_width, title_color) = if highlighted {
                (
                    Color32::from_rgb(255, 170, 60),
                    2.0,
                    Color32::from_rgb(140, 96, 36),
                )
            } else {
                (Color32::from_gray(130), 1.0, Color32::from_rgb(54, 58, 66))
            };
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(30, 32, 36));
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(border_width, border),
                StrokeKind::Inside,
            );
            painter.rect_filled(title_rect, CornerRadius::same(4), title_color);
            // 标题：**单行截断**（长标题不再压到关闭按钮上）
            let title_font = egui::FontId::proportional(13.0);
            let title_job = overflow_layout(
                &self.floats[index].title,
                title_font,
                Color32::WHITE,
                title_rect.width() - 30.0,
                1,
            );
            let title_galley = painter.layout_job(title_job);
            painter.galley(
                egui::pos2(
                    title_rect.min.x + 8.0,
                    title_rect.center().y - title_galley.size().y * 0.5,
                ),
                title_galley,
                Color32::WHITE,
            );
            // 缩放手柄示意
            painter.rect_filled(
                handle_rect.shrink(5.0),
                CornerRadius::ZERO,
                Color32::from_gray(120),
            );
            // 自定义 preview 的浮窗：内容从记录里**每帧刷新**，这样"快照迟到"也能自动补上
            if let Some(custom_index) = self.floats[index].custom_index() {
                if let Some(preview) = self.custom_previews.get(custom_index) {
                    let fresh = match (preview.kind, preview.snapshot.clone()) {
                        (CustomKind::Image, Some(texture)) => Some(FloatContent::Image(texture)),
                        (CustomKind::Text, _) => {
                            Some(FloatContent::Text(preview.text.clone().unwrap_or_default()))
                        }
                        _ => None,
                    };
                    if let Some(fresh) = fresh {
                        self.floats[index].content = fresh;
                    }
                    // 题目也要同步：否则从 ✎ 改完题目，已有浮窗仍显示旧标题
                    let title = if preview.title.is_empty() {
                        "自定义preview".to_string()
                    } else {
                        preview.title.clone()
                    };
                    self.floats[index].title = title;
                }
            }
            // 注释正文：每帧从 Annotation.body 刷新（编辑/撤销后立即同步）
            if let FloatOwner::Annotation(id) = self.floats[index].owner {
                if let Some(annotation) = self.annotations.get(id) {
                    self.floats[index].content =
                        FloatContent::Text(annotation.body.clone().unwrap_or_default());
                    self.floats[index].title = format!("标注 #{}", id.get());
                }
            }
            // 正在原位编辑的浮窗：跳过正文绘制（编辑框由
            // `draw_float_body_editing` 单独绘制，见下）
            let editing_inline = self
                .editing_body
                .as_ref()
                .is_some_and(|(owner, _)| *owner == self.floats[index].owner);
            if editing_inline {
                continue;
            }
            match &self.floats[index].content {
                FloatContent::Text(text) => {
                    // 正文：**自动换行 + 行数上限（放不下就截断并显示 …）**
                    let body_origin = rect.min + EVec2::new(10.0, FLOAT_TITLE_HEIGHT + 8.0);
                    let body_area =
                        egui::Rect::from_min_max(body_origin, rect.max - EVec2::new(10.0, 8.0));
                    if body_area.width() > 0.0 && body_area.height() > 0.0 {
                        // 行高按字体 1.3 倍估算；至少保留 1 行
                        let row_height = 13.0 * 1.3;
                        let rows = ((body_area.height() / row_height).floor() as usize).max(1);
                        let job = overflow_layout(
                            text,
                            egui::FontId::proportional(13.0),
                            Color32::from_gray(225),
                            body_area.width(),
                            rows,
                        );
                        let galley = painter.layout_job(job);
                        painter.with_clip_rect(body_area).galley(
                            body_area.min,
                            galley,
                            Color32::from_gray(225),
                        );
                    }
                }
                FloatContent::Image(texture) => {
                    // 正文区域：按**保持长宽比**放进窗口（居中、不拉伸变形）；
                    // 改变窗口大小只影响显示尺寸，图片内容本身不变。
                    let body = egui::Rect::from_min_max(
                        rect.min + EVec2::new(4.0, FLOAT_TITLE_HEIGHT + 4.0),
                        rect.max - EVec2::new(4.0, 4.0),
                    );
                    let size = texture.size_vec2();
                    let target = fit_inside(size, body);
                    painter.image(
                        texture.id(),
                        target,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
        }
        if let Some(id) = close {
            self.floats.retain(|window| window.id != id);
            self.message = format!("已关闭浮窗 #{id}");
        } else if let Some(id) = raise {
            // 关闭按钮优先；否则把被点击的浮窗移到末尾（最上层），其余顺序不变
            if let Some(index) = self.floats.iter().position(|window| window.id == id) {
                move_to_last(&mut self.floats, index);
            }
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

    /// 加密文档的密码输入框（用模态窗口阻断背后交互）。
    fn draw_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(path) = self.password_for.clone() else {
            return;
        };
        egui::Modal::new(Id::unique("pdf-password")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("文档已加密");
            ui.label(format!("{}", path.display()));
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.password_input)
                    .password(true)
                    .hint_text("密码"),
            );
            let submitted =
                response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                if ui.button("打开").clicked() || submitted {
                    let password = self.password_input.clone();
                    self.open(&path, Some(&password));
                }
                if ui.button("取消").clicked() {
                    self.password_for = None;
                    self.message = "已取消打开加密文档".to_string();
                }
            });
        });
    }
}

impl eframe::App for PdfReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 拖放打开：**空状态下也必须处理**（v1 只在已有会话时读取，导致启动后拖入无反应）
        self.handle_dropped_files(&ctx);
        // 原地写入的确认框必须在借用 session 之前处理：确认后要**关掉会话**才能替换文件
        self.draw_in_place_dialog(&ctx);
        self.draw_rebuild_dialog(&ctx);
        self.apply_pending_view_restore(&ctx);
        self.draw_history_window(&ctx);

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
        self.draw_custom_dialog(&ctx);
        self.draw_edit_dialogs(&ctx);
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
                ui.colored_label(Color32::from_rgb(200, 60, 60), error);
            });
        }
    }
}

/// 画一张纹理（UV 取满）。
fn paint_image(painter: &egui::Painter, texture: &egui::TextureHandle, rect: egui::Rect) {
    painter.image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// 画"白底页框"占位（D-06：位图未就绪时用它；D-98 起缺块处优先铺整页底料，不再补白）。
fn paint_placeholder(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, Color32::from_gray(180)),
        StrokeKind::Inside,
    );
}

/// 画"旧内容"（整页拉伸 / 旧波段的块拼），返回实际画了几张。用两个地方：
/// 整屏预览（`PageDraw::Stretch`/`TilePreview`）与缺块处的**底料**（D-98）。
///
/// * [`Preview::Whole`]：把该波段的整页图拉伸到整页矩形；
/// * [`Preview::Tiles`]：把该波段的块按占比拼回整页；
/// * [`Preview::None`]：什么都不画（露出纸张）。
///
/// `preview` 里的比例来自同一份 `KeyedStore`，正常不会落空；万一同帧被裁剪，
/// 只是少画一层，不 panic。
// 8 个参数：画笔 + 画布原点 + 纹理层 + 页面放置 + 两个比例 + 是否标记 + 诊断开关
#[allow(clippy::too_many_arguments)]
fn paint_preview(
    painter: &egui::Painter,
    canvas_min: egui::Pos2,
    textures: &KeyedStore<egui::TextureHandle>,
    page: PageIndex,
    placement: &PagePlacement,
    page_rect: Rect,
    screen: egui::Rect,
    preview: Preview,
) -> u32 {
    match preview {
        Preview::None => 0,
        Preview::Whole(scale_milli) => {
            let Some(texture) = textures.page_texture_at(page, placement.rotation, scale_milli)
            else {
                return 0;
            };
            paint_image(painter, texture, screen);
            1
        }
        Preview::Tiles(scale_milli) => {
            // 先按它自己的渲染比例重建网格，再"按占比"映射到当前页面矩形
            // （与显示比例无关，因此不需要旧的显示比例）。
            let grid = TileGrid::new(
                placement.page_size.rotated(placement.rotation),
                scale_milli as f32 / 1000.0,
            );
            let (old_width, old_height) = grid.render_px();
            let old_page = Size::new(old_width as f32, old_height as f32);
            let new_page = page_rect.size();
            let mut count = 0u32;
            for (tile, texture) in textures.tiles_at(page, placement.rotation, scale_milli) {
                let (x, y, width, height) = grid.tile_px(tile.col, tile.row);
                // 兜底：`scale_milli` 是四舍五入值，理论上可能与真实比例差一点
                // （若正好落在层级边界上，块数会不同）。越界的块直接跳过。
                if width == 0 || height == 0 {
                    continue;
                }
                let pixels = Rect::new(x as f32, y as f32, width as f32, height as f32);
                let rect = tile_view_rect_scaled(page_rect, pixels, old_page, new_page);
                let tile_screen = egui::Rect::from_min_size(
                    canvas_min + EVec2::new(rect.x, rect.y),
                    EVec2::new(rect.width, rect.height),
                );
                paint_image(painter, texture, tile_screen);
                count += 1;
            }
            count
        }
    }
}

/// 画一块高亮：PDF 页面空间 → 内容空间 → 视图空间。
fn paint_highlight(
    painter: &egui::Painter,
    session: &DocumentSession,
    highlight: &Highlight,
    scroll: Vec2,
    canvas_min: egui::Pos2,
) {
    let Some(placement) = session.placement(highlight.page).copied() else {
        return;
    };
    let color = match highlight.kind {
        HighlightKind::Selection => Color32::from_rgba_unmultiplied(0, 120, 255, 90),
        HighlightKind::SearchMatch => Color32::from_rgba_unmultiplied(255, 220, 0, 80),
        HighlightKind::ActiveSearchMatch => Color32::from_rgba_unmultiplied(255, 140, 0, 110),
    };
    for rect in &highlight.rects {
        // 坐标系换算链：PDF 页面空间 → 内容空间 → 视图空间
        let content = placement.rect_to_content(*rect);
        let screen = egui::Rect::from_min_size(
            canvas_min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
            EVec2::new(content.width, content.height),
        );
        painter.rect_filled(screen, CornerRadius::ZERO, color);
    }
}

/// 画布坐标 → 内容坐标。
fn canvas_to_content(canvas: egui::Rect, screen_position: egui::Pos2, scroll: Vec2) -> Point {
    view_to_content(
        Point::new(
            screen_position.x - canvas.min.x,
            screen_position.y - canvas.min.y,
        ),
        scroll,
    )
}

/// 画一杆墨迹：**按页面矩形裁剪**，并在折角补圆点。
///
/// * 裁剪：墨迹属于起笔那一页；指针跨到下一页时，采样点在起始页坐标系里会落到页面之外。
///   不裁剪的话它会“画到相邻页的空白上”（用户实测），而保存成 PDF 后阅读器会按页面裁剪 ——
///   于是编辑时与导出后不一致。这里统一按页面矩形裁剪（与阅读器一致）。
/// * 折角补点：egui 的折线是逐段四边形，直角/锐角拐角会漏出一道缝（用户实测）；
///   在每个内部顶点补一个半径 = 半个线宽的圆即可补上，视觉上也更接近 PDF 的 miter 连接。
fn paint_ink(
    painter: &egui::Painter,
    placement: &PagePlacement,
    canvas: egui::Rect,
    scroll: Vec2,
    points: &[Point],
    width: f32,
    color: Color32,
) {
    if points.is_empty() {
        return;
    }
    let page_screen = egui::Rect::from_min_max(
        egui::pos2(
            canvas.min.x + placement.rect.x - scroll.x,
            canvas.min.y + placement.rect.y - scroll.y,
        ),
        egui::pos2(
            canvas.min.x + placement.rect.x + placement.rect.width - scroll.x,
            canvas.min.y + placement.rect.y + placement.rect.height - scroll.y,
        ),
    );
    let painter = painter.with_clip_rect(page_screen);
    let screen_points: Vec<egui::Pos2> = points
        .iter()
        .map(|point| {
            let content = placement.page_to_content(*point);
            egui::pos2(
                canvas.min.x + content.x - scroll.x,
                canvas.min.y + content.y - scroll.y,
            )
        })
        .collect();
    let radius = (width * 0.5).max(0.5);
    if let [only] = screen_points.as_slice() {
        // 刚起笔还没拖动：画一个点，让"按下有反应"可见
        painter.circle_filled(*only, radius, color);
        return;
    }
    painter.add(egui::Shape::line(
        screen_points.clone(),
        Stroke::new(width, color),
    ));
    for joint in &screen_points[1..screen_points.len() - 1] {
        painter.circle_filled(*joint, radius, color);
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
    fn markup_mode_remembers_ink_as_previous_tool() {
        let mut app = app();
        app.switch_tool(Tool::Ink);
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool, Tool::Select);
        app.exit_markup_tool();
        assert_eq!(app.annotation_tool, None);
        assert_eq!(app.tool, Tool::Ink, "Esc/退出应回到墨迹工具");
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
        app.pdf_clean_hash = Some(app.pdf_fingerprint());
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
        app.pdf_clean_hash = Some(app.pdf_fingerprint());
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
        let (_, _) = app.merge_readback(&readback, false, &refs);
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
