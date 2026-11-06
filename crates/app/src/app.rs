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
use egui::{Color32, Id, RichText, Vec2 as EVec2};
use pdfcore::annotation::{Annotation, AnnotationId, AnnotationKind, AnnotationStore};
use pdfcore::prelude::*;

use crate::ink::InkStroke;
use crate::persist;

mod annotate;
mod body_editor;
mod canvas;
mod capture;
mod dialogs;
mod document;
mod edit;
mod floats;
mod frame;
mod hover;
mod modes;
mod paint;
/// 无窗口回放器（脚本驱动真实 `App::ui`，用于回归断言与性能基线，见 D-103）。
pub mod replay;
mod save;
mod sidebar;
mod textures;
mod toolbar;

use crate::view::{commands_for_destination, outline_node_count, Desired, KeyedStore, PlanStamp};
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
}

impl eframe::App for PdfReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

#[cfg(test)]
mod tests;
