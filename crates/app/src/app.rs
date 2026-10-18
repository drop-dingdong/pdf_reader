//! egui/eframe 外壳：把 [`DocumentSession`] 的"命令进、事件出"模型接到界面上。
//!
//! ## 本模块的定位
//! **只做胶水**：坐标换算、输入映射、纹理存储都在 [`crate::view`] 里（可单测）。
//! 剩下的部分需要真实窗口才能验证，按 agent.md 规则 2 **交由用户验收**。
//!
//! ## 每帧流程（与 architecture.md §4.2 的契约一致）
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
use pdfcore::prelude::*;

use crate::persist;
use crate::view::{
    canvas_rect_for, command_for_key, command_for_scroll, command_for_zoom_delta,
    clamp_float_rect, color_image, commands_for_destination, default_dest_rect,
    body_size_of, destination_points, drag_float_rect, find_dest_index, find_float_for_custom,
    float_rect_at_row, float_size_for_body, overflow_layout,
    pages_intersecting,
    body_size_changed, checkbox_row, fit_height_zoom, page_index_from_text,
    parse_zoom_percent, save_button_label, zoom_percent_text, CUSTOM_SYMBOL_DELETE, CUSTOM_SYMBOL_EDIT,
    CUSTOM_SYMBOL_IMAGE,
    CUSTOM_SYMBOL_TEXT,
    pin_exits_on_mode_change,
    zero_rect_at,
    find_float_for_target, fit_inside,
    compose_snapshot, hover_preview_allowed, move_to_last,
    page_rect_to_screen, pointer_over_any,
    screen_rect_to_content, tip_rect_for, veil_bands, view_rect_to_page_rect, SnapshotPage,
    flatten_outline, link_overlay_visible, link_rect_in_page_bounds, outline_node_count,
    texture_pages, wheel_targets_canvas, FLOAT_HANDLE_SIZE, FLOAT_MIN_SIZE, FLOAT_TITLE_HEIGHT,
    tile_view_rect, tile_view_rect_scaled,
    view_rect_for, view_to_content, compose_region_snapshot, needs_underlay, plan_page, DrawInput,
    KeyedStore, PageDraw, Preview,
};

/// 纹理缓存容量（条目数）。真正的淘汰依据是"是否可见 + 波段"（`retain_preview_scales`），
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
/// 滚动/缩放之后多久内不显示悬停预览（避免滚动时预览不断闪现）。
const HOVER_SCROLL_QUIET: std::time::Duration = std::time::Duration::from_millis(250);
/// 大纲每行的固定高度（虚拟化行渲染要求等行高）。
const OUTLINE_ROW_HEIGHT: f32 = 18.0;
/// 链接热区框的颜色（空心红线，用户指定）。
const LINK_OVERLAY_COLOR: Color32 = Color32::from_rgb(220, 40, 40);

/// 每帧最多上传几张纹理。
///
/// 一张整页位图的 RGBA 转换 + GPU 上传在毫秒量级；一帧内连传多张就会掉帧。
/// 设上限后开销被摊到多帧，视觉上是"相邻页稍后补齐"，比"翻页顿一下"好得多。
const MAX_UPLOADS_PER_FRAME: usize = 2;

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
}

/// 鼠标拖拽的语义（第二轮验收：去掉不可靠的"自动"模式，只留两种明确行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    /// 总是平移画面（默认：拖拽永远不会意外改动选区）。
    #[default]
    Pan,
    /// 总是选字（拖到空白处不产生选区）。
    Select,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "平移",
            Self::Select => "选字",
        }
    }
}

/// 读取同目录存档（不存在或解析失败都返回 `None`，不打扰用户）。
fn load_sidecar(pdf: &std::path::Path) -> Option<persist::SavedData> {
    let target = persist::sidecar_path(pdf);
    let text = std::fs::read_to_string(&target).ok()?;
    match persist::SavedData::from_json(&text) {
        Ok(data) => Some(data),
        Err(error) => {
            eprintln!("[存档] 解析失败（忽略）：{}：{error}", target.display());
            None
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
}

/// 自定义 preview 的两种类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomKind {
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
#[allow(dead_code)]
struct CustomPreview {
    page: PageIndex,
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

/// 浮窗内容：纯文字，或一张**自包含的截图**。
enum FloatContent {
    /// 纯文字。
    Text(String),
    /// 截图（独立纹理，与主视图状态解耦；改窗口大小只改变显示尺寸，不改变内容）。
    Image(egui::TextureHandle),
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
    /// 若是某个自定义 preview 的**正式浮窗**，记录其在 `custom_previews` 中的下标。
    custom: Option<usize>,
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

/// 应用状态。
pub struct PdfReaderApp {
    session: Option<DocumentSession>,
    navigation: Option<Arc<NavigationIndex>>,
    textures: KeyedStore<egui::TextureHandle>,
    /// D-96：每页"当前波段从何时开始"（`(scale_milli, 起始时刻)`），用于就绪门槛的超时兜底。
    band_since: std::collections::HashMap<PageIndex, (u32, std::time::Instant)>,
    /// D-96：每页上一帧的画法（`(标签, 波段)`），只在变化时打一行 `[画法]` 诊断。
    last_plan: std::collections::HashMap<PageIndex, (String, u32)>,
    /// 本次打开是否发生过**存档版本迁移**（旧版本 → 当前版本）：是则置脏，提示保存。
    sidecar_migrated: bool,
    /// 每帧临时持有的命令与事件缓冲（避免反复分配）。
    commands: Vec<ViewerCommand>,
    events: Vec<SessionEvent>,
    visible: Vec<PageIndex>,
    drag: Option<DragMode>,
    /// 拖拽语义（工具栏可切换）。
    tool: Tool,
    /// 是否画出链接热区框（工具栏 checkbox，默认关闭）。
    show_links: bool,
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
    editing_body: Option<(usize, String)>,
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
    diagnose: bool,
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
        cc.egui_ctx.options_mut(|options| options.zoom_with_keyboard = false);

        let mut app = Self {
            session: None,
            navigation: None,
            textures: KeyedStore::new(TEXTURE_CAPACITY),
            band_since: std::collections::HashMap::new(),
            last_plan: std::collections::HashMap::new(),
            sidecar_migrated: false,
            commands: Vec::new(),
            events: Vec::new(),
            visible: Vec::new(),
            drag: None,
            tool: Tool::default(),
            show_links: false,
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
            pin_hover_enabled: true,
            custom_hover: None,
            canvas_rect: egui::Rect::NOTHING,
            click_highlight: None,
            message: String::new(),
            error: None,
            diagnose: std::env::var_os("PDF_READER_DIAGNOSE").is_some(),
            last_report: std::time::Instant::now(),
            preview_until: None,
            last_zoom: 1.0,
            last_rotation: Rotation::None,
            started_at: std::time::Instant::now(),
            drag_started_at: None,
            last_pan_at: None,
            last_visible: Vec::new(),
        };
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
        let Some(hit) = self.hits.get(index) else { return };
        self.active_hit = Some(index);
        let page = hit.page;
        self.commands.push(ViewerCommand::GoToPage(page));
        if let Some(rect) = hit.rects.first() {
            self.commands.push(ViewerCommand::CenterOnPagePoint { page, point: rect.center() });
        }
    }

    /// 打开文档（装配会话：LRU 缓存 + 后台调度器 + 纵向布局 + 选区模型）。
    /// 保存自定义数据到 `<pdf>.json`（不含任何图片像素）。
    fn save_custom_data(&mut self) {
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
        match std::fs::write(&target, data.to_json()) {
            Ok(()) => {
                self.data_dirty = false;
                self.message = format!("已保存自定义数据：{}", target.display());
            }
            Err(error) => self.message = format!("保存失败：{error}"),
        }
    }

    fn open(&mut self, path: &std::path::Path, password: Option<&str>) {
        let backend = PdfiumBackend::new();
        match backend.open(Source::Path(path), password) {
            Ok(opened) => {
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
                        self.document_path = Some(path.to_path_buf());
                        // 同目录若有存档（<pdf>.json），载入并**替换**链接与被引用点/自定义数据。
                        // 读到旧版本就**就地升级**并置脏（下次保存写回当前版本）。
                        let saved = load_sidecar(path).map(|mut data| {
                            let migration = persist::migrate(&mut data);
                            for note in &migration.notes {
                                eprintln!("[存档] {note}");
                            }
                            self.sidecar_migrated = migration.changed;
                            data
                        });
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
                                                let marks = self
                                                    .dest_marks
                                                    .get(page.get() as usize)?;
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
                                let Some(marks) = self.dest_marks.get_mut(page_index) else { continue };
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
                        if self.sidecar_migrated {
                            // 旧版本存档被升级过：置脏，让"保存自定义数据"写回当前版本
                            self.data_dirty = true;
                            self.message = format!("已打开：{title}（存档已升级到 v{}，请保存）", persist::CURRENT_VERSION);
                        } else {
                            self.message = format!("已打开：{title}");
                        }
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
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
                .collect()
        });
        // 复制：egui 把 Ctrl/Cmd+C 翻译成 `Event::Copy`（**不是** Key 事件），
        // 因此必须单独识别它 —— 这就是第二轮验收里"复制按钮可用但 Ctrl+C 无反应"的原因。
        let copy_event = ctx.input(|input| {
            input.events.iter().any(|event| matches!(event, egui::Event::Copy))
        });
        if copy_event {
            self.commands.push(ViewerCommand::CopySelection);
        }
        for (key, modifiers) in pressed {
            // 已经由 Event::Copy 处理过时不重复处理（避免复制两次）
            if copy_event && key == Key::C {
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

    /// 为可见页（以及相邻页）准备纹理，并裁剪不再需要的纹理。
    ///
    /// 两个细节都来自第二轮人工验收的"翻页边界处卡顿"：
    /// 1. **提前上传相邻页**：会话已按 `PREFETCH_PAGE_RADIUS` 预渲染，这里把它们也传成纹理，
    ///    于是"新页进入视口"那一刻不再有 4 MiB 量级的转换 + GPU 上传开销；
    /// 2. **每帧限量上传**：一帧内同时上传多页会造成明显掉帧，因此设上限、分帧摊开。
    ///
    /// `allow_upload = false` 时只更新可见页与裁剪，**跳过转换与上传** ——
    /// 拖动期间用它把"新页进入"那一帧约 150ms 的逐像素转换挪到松手之后（用户要求）。
    fn ensure_textures(
        &mut self,
        ctx: &egui::Context,
        session: &DocumentSession,
        allow_upload: bool,
    ) -> usize {
        self.visible.clear();
        session.visible_pages(&mut self.visible);

        // 上传目标 = 保留目标（**同一个集合**，见 `view::texture_pages`）：
        // 曾经两者不一致（上传"可见页 ±1"、裁剪只留"可见页"），导致相邻页纹理
        // 每帧被反复上传又丢弃 —— 200% 缩放下实测每帧 300ms 全耗在这里。
        let targets = texture_pages(
            &self.visible,
            session.state().current_page,
            session.page_count(),
        );

        let mut uploaded = 0usize;
        if allow_upload {
            for page in &targets {
                if uploaded >= MAX_UPLOADS_PER_FRAME {
                    break;
                }
                uploaded += self.upload_page(ctx, session, *page);
            }
        }

        // 裁剪：只保留可见页（±1）与当前页，并且每页只保留"当前比例 + 最近一个旧比例"
        // —— 旧比例是缩放过场要用的（第四轮验收：丢掉其它页的旧图会让它们闪白）。
        //
        // 旋转按页取：同一份文档里各页的 /Rotate 可能不同。
        let pages: Vec<(PageIndex, Rotation)> = targets
            .iter()
            .filter_map(|page| session.placement(*page).map(|placement| (*page, placement.rotation)))
            .collect();
        let current = session.state().current_page;
        // D-94：纹理按**波段渲染比例**分档（同一波段内所有缩放共用一份纹理）
        let current_scale_milli = session
            .placement(current)
            .map(|placement| {
                RenderKey::new(current, session.render_scale(current), placement.rotation)
                    .scale_milli
            })
            .unwrap_or(1000);
        self.textures
            .retain_preview_scales(&pages, current_scale_milli);
        uploaded
    }

    /// 上传某一页当前可用的位图（整页或瓦片），返回新上传的纹理数量。
    fn upload_page(
        &mut self,
        ctx: &egui::Context,
        session: &DocumentSession,
        page: PageIndex,
    ) -> usize {
        let Some(placement) = session.placement(page).copied() else {
            return 0;
        };
        // D-94：键与"块在页面里的位置"都用**波段渲染比例**，不能用显示比例
        let render = session.render_scale(page);
        let scroll = session.viewport().scroll;
        let mut count = 0;
        // 整页路径
        if let Some(image) = session.image_for(page) {
            let key = RenderKey::new(page, render, placement.rotation);
            if !self.textures.contains(&key) {
                self.upload(ctx, key, &image);
                count += 1;
            }
        }
        // 分块路径
        for (tile, image) in session.tiles_for(page) {
            let key = RenderKey::tiled(page, render, placement.rotation, tile);
            if !self.textures.contains(&key) {
                if self.diagnose {
                    // 贴图诊断：渲染尺寸 vs 页面像素尺寸 vs 屏幕摆放位置
                    // 用于区分"内容错位"（后端渲染）与"摆放错位"（前端映射）。
                    // 判据 4/5：同一波段内 `页面px` 只应出现一个值、且不应有新行。
                    if let Some(grid) = session.tile_grid(page) {
                        let (page_width, page_height) = grid.render_px();
                        let (x, y, width, height) = grid.tile_px(tile.col, tile.row);
                        let screen = tile_view_rect(
                            &grid,
                            &placement,
                            tile.col,
                            tile.row,
                            render.0,
                            scroll,
                        );
                        eprintln!(
                            "[贴图] 页={} col={} row={} 瓦片图={}x{} 页面px={}x{} 显示px={:.0}x{:.0} 瓦片px=({},{},{},{}) 屏幕=({:.1},{:.1},{:.0},{:.0}) scroll=({:.1},{:.1}) 渲染比例={:.3}",
                            page.get(), tile.col, tile.row, image.width, image.height,
                            page_width, page_height,
                            placement.rect.width, placement.rect.height,
                            x, y, width, height,
                            screen.x, screen.y, screen.width, screen.height,
                            scroll.x, scroll.y, render.0
                        );
                    }
                }
                self.upload(ctx, key, &image);
                count += 1;
            }
        }
        count
    }

    /// 上传一张位图为纹理（已存在则跳过）。
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
                    if ui.button(save_button_label(self.data_dirty)).clicked() {
                        self.save_custom_data();
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
                let fullscreen_label = if self.fullscreen { "退出全屏" } else { "全屏" };
                if ui.button(fullscreen_label).on_hover_text("全屏/退出全屏（F11）").clicked() {
                    let ctx = ui.ctx().clone();
                    self.toggle_fullscreen(&ctx);
                }
                egui::ComboBox::from_id_salt("layout-direction")
                    .selected_text(if self.horizontal_layout { "横向" } else { "纵向" })
                    .show_ui(ui, |ui| {
                        // 无条件提交：即使标记已经是该值，也强制重排一次，
                        // 这样"状态错位"（UI 说横向、实际纵向）点一下就能纠正。
                        if ui.selectable_label(!self.horizontal_layout, "纵向").clicked() {
                            self.horizontal_layout = false;
                            self.pending_layout = Some(false);
                        }
                        if ui.selectable_label(self.horizontal_layout, "横向").clicked() {
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
                                self.error = Some(format!(
                                    "页码需在 1–{} 之间",
                                    session.page_count()
                                ));
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
                        .add(
                            egui::Label::new(zoom_percent_text(zoom))
                                .sense(egui::Sense::click()),
                        )
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

                // 预览模式：平移 / 选字
                for tool in [Tool::Pan, Tool::Select] {
                    if ui.selectable_label(self.tool == tool, tool.label()).clicked() {
                        self.tool = tool;
                        if pin_exits_on_mode_change(tool == Tool::Select, false) {
                            self.custom_pin = None;
                        }
                    }
                }

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
        let width = self.sidebar_width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
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
                });
                ui.separator();
                if self.sidebar_tab == SidebarTab::Pins {
                    self.draw_pin_list(ui, session);
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
                        let Some((depth, title, destination)) = rows.get(index) else { continue };
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
                let Some(preview) = self.custom_previews.get(index) else { continue };
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
                let (rect, _) = ui.allocate_exact_size(
                    EVec2::new(width, row_height),
                    Sense::hover(),
                );
                let response = ui.interact(
                    rect,
                    Id::unique(("pin-row", index)),
                    Sense::click(),
                );
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
                    egui::pos2(rect.max.x - go_width - edit_width - delete_width, rect.center().y),
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
                    egui::pos2(go_rect.min.x - edit_width * 0.5 - delete_width, rect.center().y),
                    EVec2::new(edit_width, row_height),
                );
                let delete_rect = egui::Rect::from_center_size(
                    egui::pos2(go_rect.min.x - delete_width * 0.5, rect.center().y),
                    EVec2::new(delete_width, row_height),
                );
                if ui
                    .put(edit_rect, egui::Button::new(CUSTOM_SYMBOL_EDIT).frame(false))
                    .clicked()
                {
                    if let Some(preview) = self.custom_previews.get(index) {
                        self.editing_title = Some((index, preview.title.clone()));
                    }
                }
                if ui
                    .put(delete_rect, egui::Button::new(CUSTOM_SYMBOL_DELETE).frame(false))
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
        let Some(placement) = session.placement(page).copied() else { return None };
        if !(page_rect.width > 0.0) || !(page_rect.height > 0.0) {
            if self.diagnose {
                eprintln!("[预览] 初次生成失败：{tag} 页={page:?} 区域为空 {page_rect:?}");
            }
            return None;
        }
        let display_scale = if scale > 0.0 && scale.is_finite() { scale } else { 1.0 };
        let render = Scale::from_zoom(render_scale_for(display_scale, session.overscan()));
        let grid = TileGrid::new(
            placement.page_size.rotated(placement.rotation),
            render.0,
        );
        let Some(range) =
            grid.tiles_for_page_rect(page_rect, placement.page_size, placement.rotation)
        else {
            if self.diagnose {
                eprintln!(
                    "[预览] 初次生成失败：{tag} 页={} 区域 {page_rect:?} 不在页面内",
                    page.display_number()
                );
            }
            return None;
        };
        let Some(tiles) = session.region_tiles(page, page_rect, render) else {
            let count = session.request_region(page, page_rect, render);
            if self.diagnose {
                eprintln!(
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
                );
            }
            return None;
        };
        let borrowed: Vec<(TileSpec, &RasterImage)> =
            tiles.iter().map(|(tile, image)| (*tile, image.as_ref())).collect();
        let Some(image) = compose_region_snapshot(
            page_rect,
            placement.page_size,
            placement.rotation,
            render.0,
            &grid,
            &borrowed,
        ) else {
            if self.diagnose {
                eprintln!(
                    "[预览] 初次生成失败：{tag} 页={} 区域 {page_rect:?} 的块都为空（块数 {}）",
                    page.display_number(),
                    borrowed.len()
                );
            }
            return None;
        };
        if self.diagnose {
            eprintln!(
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
            );
        }
        Some(ui.ctx().load_texture(texture_name, image, egui::TextureOptions::LINEAR))
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
            .filter_map(|window| window.custom)
            .collect();
        shown.sort_unstable();
        shown.dedup();
        for index in shown {
            let missing = self
                .custom_previews
                .get(index)
                .map_or(false, |preview| {
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
        let Some(preview) = self.custom_previews.get(index) else { return };
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
    fn spawn_custom_float(&mut self, index: usize, rect: egui::Rect) {
        let Some(preview) = self.custom_previews.get(index) else { return };
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
            custom: Some(index),
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
        self.custom_previews.remove(index);
        self.floats.retain(|window| window.custom != Some(index));
        for window in &mut self.floats {
            if let Some(custom) = window.custom {
                if custom > index {
                    window.custom = Some(custom - 1);
                }
            }
        }
        if self.custom_hover.map_or(false, |(hovered, _)| hovered == index) {
            self.custom_hover = None;
        }
        if self.editing_title.as_ref().map_or(false, |(editing, _)| *editing == index) {
            self.editing_title = None;
        }
        if self.editing_body.as_ref().map_or(false, |(editing, _)| *editing == index) {
            self.editing_body = None;
        }
        self.data_dirty = true;
        self.message = "已删除该自定义preview".to_string();
    }

    /// 若某个自定义 preview 已有**正式浮窗**，把它高亮并置顶（侧栏悬浮/点击共用）。
    fn highlight_existing_custom_float(&mut self, index: usize) -> Option<u64> {
        let customs: Vec<Option<usize>> = self.floats.iter().map(|f| f.custom).collect();
        let position = find_float_for_custom(&customs, index)?;
        let window = self.floats.remove(position);
        let id = window.id;
        self.floats.push(window);
        self.highlight_float = Some(id);
        Some(id)
    }

    /// 侧栏条目被点击：生成正式浮窗；若已存在则高亮 + 置顶（一次性高亮有时限）。
    fn activate_custom_preview(&mut self, index: usize, row: egui::Rect) {
        let customs: Vec<Option<usize>> = self.floats.iter().map(|f| f.custom).collect();
        if let Some(position) = find_float_for_custom(&customs, index) {
            let window = self.floats.remove(position);
            let id = window.id;
            self.floats.push(window);
            // 一次性高亮（点击动作需要有限时长）+ 当帧也置为高亮
            self.click_highlight = Some((id, std::time::Instant::now() + CLICK_HIGHLIGHT));
            self.highlight_float = Some(id);
            self.message = "已唤出该自定义preview浮窗".to_string();
            return;
        }
        let Some(preview) = self.custom_previews.get(index) else { return };
        let id = self.next_float_id;
        self.next_float_id += 1;
        // 正文尺寸使用该条目**储存的 body_size**（用户要求）
        let size = float_size_for_body(preview.body_size, FLOAT_TITLE_HEIGHT);
        let rect = self.float_rect_at_row(row, size);
        self.spawn_custom_float(index, rect);
        self.click_highlight = Some((id, std::time::Instant::now() + CLICK_HIGHLIGHT));
        self.message = "已生成该自定义preview浮窗".to_string();
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
        // 搜索栏放在**中央区域**的底部：只占画布宽度（不会跑到侧栏下方），
        // 且位于状态栏之上（用户建议）。
        self.draw_search_bar_in_center(ui, &session);
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
                .map_or(false, |position| canvas.contains(position));
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
                    if self.diagnose {
                        let after = session.viewport().scroll;
                        let viewport = session.viewport().size;
                        eprintln!(
                            "[滚动] 滚轮 delta=({:.1},{:.1}) scroll ({:.1},{:.1})→({:.1},{:.1}) 视口={}x{} 横向布局={}",
                            scroll_delta.x, scroll_delta.y, scroll_before.x, scroll_before.y,
                            after.x, after.y, viewport.width, viewport.height,
                            self.horizontal_layout
                        );
                    }
                }
                if let Some(command) = command_for_zoom_delta(zoom_delta, session.state().zoom) {
                    session.handle(command, &mut self.events);
                    view_changed = true;
                }
                if view_changed {
                    // 视图刚被滚动/缩放 → 静默一段时间，期间不显示悬停预览（用户要求）
                    self.scroll_quiet_until = Some(std::time::Instant::now() + HOVER_SCROLL_QUIET);
                    ui.ctx().request_repaint();
                }
            }

            // ---- 拖动**当帧生效** ----
            //
            // 拖动命令原先要等下一帧 `apply_and_pump` 才执行，于是画面永远落后指针一帧；
            // 快速拖动时就是上百像素的偏差（用户反馈"落后鼠标移动过多"）。
            // 这里在读取 scroll / 绘制**之前**先处理拖动，内容与指针同帧同步。
            let inverted_now = ui.input(|input| input.modifiers.shift);
            let select_now = match self.tool {
                Tool::Select => !inverted_now,
                Tool::Pan => inverted_now,
            };
            if self.capture.is_none() && response.drag_started() {
                self.drag_started_at = Some(std::time::Instant::now());
                self.last_pan_at = None;
                if self.diagnose {
                    eprintln!(
                        "[时序] 拖动开始 t={:.6}s",
                        self.started_at.elapsed().as_secs_f64()
                    );
                }
                if let Some(position) = response.interact_pointer_pos() {
                    let content =
                        canvas_to_content(canvas, position, session.viewport().scroll);
                    let hit = session.text_position_at(content);
                    match (select_now, hit) {
                        (true, Some(text_position)) => {
                            self.drag = Some(DragMode::Select);
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
                            Some(std::time::Instant::now() + HOVER_SCROLL_QUIET);
                        if self.diagnose && after != before {
                            let now = std::time::Instant::now();
                            let since_start = self
                                .drag_started_at
                                .map(|start| now.duration_since(start).as_secs_f64() * 1000.0)
                                .unwrap_or(0.0);
                            let since_last = self
                                .last_pan_at
                                .map(|last| now.duration_since(last).as_secs_f64() * 1000.0)
                                .unwrap_or(f64::NAN);
                            eprintln!(
                                "[时序] 拖动位移生效 t={:.6}s Δ起={:.1}ms Δ上次={:.1}ms scroll=({:.1},{:.1})",
                                self.started_at.elapsed().as_secs_f64(),
                                since_start,
                                since_last,
                                after.x,
                                after.y
                            );
                            self.last_pan_at = Some(now);
                            // "因为拖动而重新决定显示内容"= 可见页集合发生变化（要换/补位图）
                            let mut visible = Vec::new();
                            session.visible_pages(&mut visible);
                            if visible != self.last_visible {
                                eprintln!(
                                    "[时序] 可见页集变化 t={:.6}s Δ起={:.1}ms 可见页={:?}",
                                    self.started_at.elapsed().as_secs_f64(),
                                    since_start,
                                    visible.iter().map(|page| page.display_number()).collect::<Vec<_>>()
                                );
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
                let plan = plan_page(&DrawInput {
                    tiled: tile_area.is_some(),
                    has_whole_current: self.textures.contains(&whole_key),
                    ready,
                    total,
                    preview,
                    waited_ms,
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

                if self.diagnose {
                    // 边沿触发：画法一变就立刻打一行（不受 1 秒节流），
                    // 白块到底是"没渲染好"还是"被旧内容挤掉"一眼可见（D-96）。
                    let fallback = plan == PageDraw::Tiles && total > 0 && ready < total;
                    let label = if fallback {
                        format!("{}（兜底）", plan.label())
                    } else {
                        plan.label().to_string()
                    };
                    let previous = self.last_plan.get(&page).cloned();
                    if previous.as_ref() != Some(&(label.clone(), whole_key.scale_milli)) {
                        eprintln!(
                            "[画法] p{} 波段={} 从={} 到={} 就绪={}/{} 等待={}ms 可见={:?} 预览={:?} 层={} 底料={:?}",
                            page.display_number(),
                            whole_key.scale_milli,
                            previous
                                .map(|(label, scale)| format!("{label}@{scale}"))
                                .unwrap_or_else(|| "—".to_string()),
                            label,
                            ready,
                            total,
                            waited_ms,
                            tile_area.as_ref().map(|(_, range)| *range),
                            preview,
                            layers,
                            underlay
                        );
                        self.last_plan.insert(page, (label, whole_key.scale_milli));
                    }
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
            if self.diagnose
                && self.last_report.elapsed() >= std::time::Duration::from_secs(1)
            {
                self.last_report = std::time::Instant::now();
                let max_side = ui.ctx().input(|input| input.max_texture_side);
                eprintln!(
                    "[诊断] 缩放={:.0}% 视口={:.0}×{:.0} scroll=({:.1},{:.1}) 当前页={} 可见页={:?} \
                     纹理={} 缓存在飞={} 预览中={} max_texture_side={} | {}",
                    session.state().zoom * 100.0,
                    viewport.size.width,
                    viewport.size.height,
                    scroll.x,
                    scroll.y,
                    session.state().current_page.display_number(),
                    self.visible.iter().map(|page| page.display_number()).collect::<Vec<_>>(),
                    self.textures.len(),
                    session.pending_renders(),
                    previewing,
                    max_side,
                    report.join(" | ")
                );
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
            if self.diagnose {
                if let Some(navigation) = &self.navigation {
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else { continue };
                        let links = navigation.links_on(page);
                        if links.is_empty() {
                            continue;
                        }
                        let size = placement.page_size;
                        let first = links[0].rect;
                        eprintln!(
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
                        );
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
                                Stroke::new(1.5, LINK_OVERLAY_COLOR),
                                StrokeKind::Outside,
                            );
                        }
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
                    Some(DragMode::Pan) => {
                        // 已在绘制之前当帧处理（见"拖动当帧生效"），这里不再重复提交
                    }
                    None => {}
                }
            }

            // 拖动过程诊断：把 egui 报的状态与下一帧要执行的滚动量打出来（PDF_READER_DIAGNOSE=1）
            if self.diagnose
                && (response.drag_started()
                    || response.dragged()
                    || response.drag_stopped()
                    || response.clicked())
            {
                eprintln!(
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
                );
            }
            if response.drag_stopped() {
                if self.diagnose {
                    let total = self
                        .drag_started_at
                        .map(|start| start.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or(0.0);
                    eprintln!(
                        "[时序] 拖动结束 t={:.6}s 全程={:.1}ms",
                        self.started_at.elapsed().as_secs_f64(),
                        total
                    );
                }
                self.drag = None;
                self.drag_started_at = None;
                self.last_pan_at = None;
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
                    // 链接优先：命中热区就跳转（不论当前工具）—— 目的地有坐标时居中（用户要求）
                    if !self.follow_link(session, content) && select_tool {
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
                if let Some(index) = window.custom {
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
    /// 入口按钮已按用户要求从界面移除（代码保留，便于以后挂到菜单里恢复）。
    #[allow(dead_code)]
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
            custom: None,
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
            .map_or(false, |deadline| std::time::Instant::now() < deadline);
        let float_rects: Vec<egui::Rect> = self.floats.iter().map(|window| window.rect).collect();
        let over_float = pointer.map_or(false, |position| pointer_over_any(position, &float_rects));
        if !hover_preview_allowed(scrolling, over_float) {
            self.hover_tip = None;
            return;
        }

        let mut found: Option<((PageIndex, usize), egui::Pos2)> = None;

        if let (Some(pointer), Some(navigation)) = (pointer, self.navigation.clone()) {
            'outer: for page in self.visible.clone() {
                let Some(placement) = session.placement(page).copied() else { continue };
                let Some(registered) = self.link_marks.get(page.get() as usize) else { continue };
                for (index, link) in navigation.links_on(page).iter().enumerate() {
                    // 链接 → 被引用点：**直接取登记表**（建表时已解析）
                    let Some(Some(target)) = registered.get(index).copied() else { continue };
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
            (Some(tip), Some(pointer)) => tip.rect.map_or(false, |rect| rect.contains(pointer)),
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
                self.hover_tip = Some(HoverTip { target, anchor, rect: None });
            }
            None if keep_existing => {}
            None => {
                self.hover_tip = None;
                return;
            }
        }

        // 需要时**按记录的分辨率**生成一次（之后复用；缺像素则提交渲染、下帧重试）
        let Some(tip) = self.hover_tip.as_ref() else { return };
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
        {
            return;
        }
    }

    /// 绘制临时预览浮窗并处理"点击固定"（本函数在所有固定浮窗之后调用 → 位于最上层）。
    fn draw_hover_preview(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        let Some(tip) = self.hover_tip.as_ref() else { return };
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
        let response = ui.interact(
            rect,
            Id::unique("hover-tip"),
            Sense::click_and_drag(),
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
        painter.rect_filled(title_rect, CornerRadius::same(4), Color32::from_rgb(54, 58, 66));
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
                custom: None,
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
        let Some((index, row)) = self.custom_hover else { return };
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
        painter.rect_filled(title_rect, CornerRadius::same(4), Color32::from_rgb(54, 58, 66));
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
                painter.with_clip_rect(body).galley(
                    body.min,
                    galley,
                    Color32::from_gray(225),
                );
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

    /// 文字 preview 浮窗的**正文原位编辑**：双击进入，输入框失焦即写回并回到阅览模式。
    ///
    /// 单独成一个 pass（在 `draw_floats` 之后调用），是为了避免与 `draw_floats` 里的
    /// `painter` 借用冲突 —— 那里只负责"编辑中跳过正文绘制"。
    fn draw_float_body_editing(&mut self, ui: &mut egui::Ui) {
        // 找到正在编辑的那条记录 + 它对应的浮窗
        let Some((editing_index, _)) = self.editing_body.clone() else {
            return;
        };
        let Some(window) = self
            .floats
            .iter()
            .find(|window| window.custom == Some(editing_index))
        else {
            self.editing_body = None;
            return;
        };
        let body_rect = egui::Rect::from_min_max(
            window.rect.min + EVec2::new(8.0, FLOAT_TITLE_HEIGHT + 4.0),
            window.rect.max - EVec2::new(8.0, 8.0),
        );
        let Some((_, draft)) = self.editing_body.take() else { return };
        let mut text = draft;
        let response = ui.put(
            body_rect,
            egui::TextEdit::multiline(&mut text)
                .desired_width(body_rect.width())
                .desired_rows(6),
        );
        if self.focus_body_edit {
            // 进入编辑的那一帧就把焦点交给输入框，否则键入无效（"改了没变化"）
            response.request_focus();
            self.focus_body_edit = false;
        }
        if response.lost_focus() {
            if let Some(preview) = self.custom_previews.get_mut(editing_index) {
                preview.text = Some(text);
            }
            self.editing_body = None;
            self.data_dirty = true;
            self.message = "已更新内容".to_string();
        } else {
            self.editing_body = Some((editing_index, text));
        }
    }

    /// 双击文字 preview 浮窗的正文 → 进入原位编辑。
    fn detect_body_double_click(&mut self, ui: &mut egui::Ui) {
        if self.editing_body.is_some() {
            return;
        }
        let targets: Vec<(u64, usize, egui::Rect)> = self
            .floats
            .iter()
            .filter_map(|window| {
                let custom = window.custom?;
                let preview = self.custom_previews.get(custom)?;
                (preview.kind == CustomKind::Text).then_some((window.id, custom, window.rect))
            })
            .collect();
        for (id, custom, rect) in targets {
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
                if let Some(preview) = self.custom_previews.get(custom) {
                    self.editing_body = Some((custom, preview.text.clone().unwrap_or_default()));
                    self.focus_body_edit = true;
                    ui.ctx().request_repaint();
                }
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
                session.placement(page).map(|placement| (page_index, placement.rect))
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
            if self.diagnose {
                eprintln!(
                    "[图片预览] 合成失败：页={:?} 选区={:?} 缩放={:.3} 可见页={:?}",
                    page,
                    overlap,
                    session.state().zoom,
                    self.visible
                );
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
        let Some(preview) = self.custom_previews.get_mut(index) else { return };
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
    fn finish_pin(&mut self, session: &DocumentSession, canvas: egui::Rect, scroll: Vec2, pointer: egui::Pos2) {
        let Some(kind) = self.custom_pin else { return };
        let content = canvas_to_content(canvas, pointer, scroll);
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else { continue };
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
        let Some(dialog) = self.custom_dialog.as_ref() else { return };
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
            ui.painter().rect_filled(band, CornerRadius::ZERO, veil_color);
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
            (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape))
        });
        if enter {
            match selection {
                Some(selection) => {
                    // 屏幕矩形 → 内容空间矩形（供快照与页面空间换算使用）
                    let selection_rect =
                        screen_rect_to_content(canvas.min, selection, scroll);
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
                        let Some(placement) = session.placement(page).copied() else { continue };
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
                    eprintln!(
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
                    );
                    self.message = format!(
                        "截图区域已输出：{:.0}×{:.0}",
                        selection.width(),
                        selection.height()
                    );
                }
                None => {
                    eprintln!("[截图] 未框选任何区域");
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
        let Some(placement) = session.placement(page).copied() else { return };
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
        let mut rect_saved = false;
        if let Some(mark) = self
            .dest_marks
            .get_mut(page.get() as usize)
            .and_then(|marks| marks.get_mut(index))
        {
            mark.rect = page_rect;
            mark.snapshot = Some(texture.clone());
            mark.body_size = egui::vec2(overlap.width, overlap.height);
            // ▲v2：记录这张图的分辨率（截图时的显示比例）
            mark.scale = session.state().zoom;
            self.data_dirty = true;
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
        let mut owned: Vec<(Rect, Option<Arc<RasterImage>>, Vec<(Rect, Arc<RasterImage>)>)> =
            Vec::new();
        // **跨页**：凡与选区相交的页都参与拼合（不依赖 visible 集合，也不只取最大重叠那一页）
        let all_pages: Vec<(usize, Rect)> = (0..session.page_count() as usize)
            .filter_map(|index| {
                let page = PageIndex::new(index as u32);
                session.placement(page).map(|placement| (index, placement.rect))
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
                tiles: tiles.iter().map(|(rect, image)| (*rect, image.as_ref())).collect(),
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
            custom: None,
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
                egui::pos2(rect.right() - FLOAT_HANDLE_SIZE, rect.bottom() - FLOAT_HANDLE_SIZE),
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
            let close_response = ui.interact(close_rect, Id::unique(("float-close", id)), Sense::click());
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
                .map_or(false, |(target, until)| target == id && std::time::Instant::now() < until);
            let highlighted = self.highlight_float == Some(id) || clicked_highlight;
            let (border, border_width, title_color) = if highlighted {
                (Color32::from_rgb(255, 170, 60), 2.0, Color32::from_rgb(140, 96, 36))
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
            painter.rect_filled(handle_rect.shrink(5.0), CornerRadius::ZERO, Color32::from_gray(120));
            // 自定义 preview 的浮窗：内容从记录里**每帧刷新**，这样"快照迟到"也能自动补上
            if let Some(custom_index) = self.floats[index].custom {
                if let Some(preview) = self.custom_previews.get(custom_index) {
                    let fresh = match (preview.kind, preview.snapshot.clone()) {
                        (CustomKind::Image, Some(texture)) => Some(FloatContent::Image(texture)),
                        (CustomKind::Text, _) => Some(FloatContent::Text(
                            preview.text.clone().unwrap_or_default(),
                        )),
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
            // 文字预览的浮窗：正在原位编辑时跳过正文绘制（编辑框由
            // `draw_float_body_editing` 单独绘制，见下）
            let editing_inline = self.floats[index].custom.map_or(false, |custom_index| {
                self.custom_previews
                    .get(custom_index)
                    .map_or(false, |preview| preview.kind == CustomKind::Text)
                    && self.editing_body.as_ref().map(|(editing, _)| *editing) == Some(custom_index)
            });
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
            let Some(placement) = session.placement(page).copied() else { continue };
            if !placement.rect.contains(content) {
                continue;
            }
            let page_point = placement.content_to_page(content);
            // 找出命中的链接下标（登记表与 links_on 同构，因此能直接查）
            let Some(navigation) = self.navigation.clone() else { return false };
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
        let Some(path) = self.password_for.clone() else { return };
        egui::Modal::new(Id::unique("pdf-password")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("文档已加密");
            ui.label(format!("{}", path.display()));
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.password_input)
                    .password(true)
                    .hint_text("密码"),
            );
            let submitted = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
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

        // 把会话临时取出，避免与 `self` 的其它字段发生借用冲突
        let Some(mut session) = self.session.take() else {
            self.draw_empty(ui);
            self.draw_password_prompt(&ctx);
            return;
        };

        let frame_start = std::time::Instant::now();
        // 高亮只维持一帧：由本帧的悬停判定重新设置
        self.highlight_float = None;
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
            self.preview_until = Some(std::time::Instant::now() + ZOOM_PREVIEW_WINDOW);
        }
        let previewing = self.previewing();

        self.last_convert_ms = 0.0;
        self.last_upload_ms = 0.0;
        // 拖动期间**暂缓**转换与上传：把"新页进入"那一帧的逐像素开销挪到松手之后，
        // 拖动本身只需保证画面跟着走（未就绪的区域显示旧图或白纸）。
        let dragging = self.drag.is_some();
        let uploaded = self.ensure_textures(&ctx, &session, !dragging);
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
        if self.diagnose {
            let total_ms = after_draw.as_secs_f64() * 1000.0;
            if self.drag.is_some() || total_ms > 25.0 {
                eprintln!(
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
                );
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
                ctx.request_repaint_after(deadline.saturating_duration_since(std::time::Instant::now()));
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
        let now = std::time::Instant::now();
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
            .map_or(false, |deadline| std::time::Instant::now() < deadline)
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

/// 画"白底页框"占位（decision-log D-06：位图未就绪时用它，而不是保留上一档拉伸图）。
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
    let Some(placement) = session.placement(highlight.page).copied() else { return };
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
        Point::new(screen_position.x - canvas.min.x, screen_position.y - canvas.min.y),
        scroll,
    )
}


/// 让 `Align2` 与 `FontId` 在需要时可用（保持导入稳定）。
#[allow(dead_code)]
fn _unused_imports() -> (Align2, egui::FontId) {
    (Align2::CENTER_CENTER, egui::FontId::proportional(12.0))
}
