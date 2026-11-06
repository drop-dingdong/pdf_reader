//! `app::model`：`app` 子模块共用的类型、常量与小工具。
//!
//! 从 `app.rs` 拆出（纯移动）。字段用 `pub(super)`，便于 `app` 子模块直接访问；
//! 这里不放 egui 绘制流程，只放数据与纯函数。

use std::path::PathBuf;

use eframe::egui;
use egui::{Color32, Id, RichText, Vec2 as EVec2};
use pdfcore::annotation::{Annotation, AnnotationId, AnnotationKind};
use pdfcore::prelude::*;

use crate::persist;

/// 纹理层的**字节**预算（软上限，D-111）。
///
/// 256 MiB 的来历：本机 12 个回放脚本实测峰值 9–52 MB（A4 家庭）；
/// 极端情形（带鱼屏 5120×1440 + A0 在 800%）估算"当前视口"一类就要 68–206 MB。
/// 软上限的语义：**当前视口所需的那一类永不淘汰**，超出只打一行提示；
/// 只有"底料 / 旧内容"这些可再生的纹理才受它约束。
/// 可用 `PDF_READER_TEXTURE_BUDGET_MB` 覆盖（0 = 不限制）。
pub(super) const TEXTURE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// 注释列表的视图：按页分组 / 最近修改（平铺，按修改时间倒序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AnnotationListView {
    /// 按页分组、页内自上而下（阅读顺序）。
    #[default]
    ByPage,
    /// 平铺，按修改时间倒序；没有时间的排最后。
    Recent,
}

impl AnnotationListView {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ByPage => "按页",
            Self::Recent => "最近修改",
        }
    }
}

/// 注释列表的一行（渲染前先收集、排序，避免边遍历边改 UI）。
pub(super) struct AnnotationRow {
    pub(super) page: PageIndex,
    pub(super) id: AnnotationId,
    pub(super) label: &'static str,
    pub(super) readonly: bool,
    pub(super) external: bool,
    pub(super) summary: String,
    pub(super) preview: String,
    pub(super) author: Option<String>,
    pub(super) created: Option<i64>,
    pub(super) modified: Option<i64>,
    pub(super) bounds: Option<Rect>,
}

/// 注释列表搜索：正文或作者包含查询词（忽略大小写）。
pub(super) fn annotation_matches_query(annotation: &Annotation, query: &str) -> bool {
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
pub(super) fn by_page_order(a: &AnnotationRow, b: &AnnotationRow) -> std::cmp::Ordering {
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
pub(super) fn recent_order(a: &AnnotationRow, b: &AnnotationRow) -> std::cmp::Ordering {
    b.modified
        .unwrap_or(i64::MIN)
        .cmp(&a.modified.unwrap_or(i64::MIN))
        .then_with(|| a.page.get().cmp(&b.page.get()))
        .then_with(|| a.id.get().cmp(&b.id.get()))
}

/// 注释列表里的类型名；`visible` 恒为 true，预留给将来不可列类型。
pub(super) fn annotation_kind_label(kind: &AnnotationKind) -> (&'static str, bool) {
    match kind {
        AnnotationKind::Highlight { .. } => ("高亮", true),
        AnnotationKind::Underline { .. } => ("下划线", true),
        AnnotationKind::StrikeOut { .. } => ("删除线", true),
        AnnotationKind::Ink { .. } => ("墨迹", true),
        AnnotationKind::Note { .. } => ("便签", true),
    }
}

/// 悬停预览：压平全部空白，最多 280 字。
pub(super) fn body_tooltip(body: Option<&str>) -> String {
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
pub(super) fn body_summary(body: Option<&str>) -> String {
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
pub(super) fn text_float_is_editable(owner: FloatOwner, content: &FloatContent) -> bool {
    matches!(content, FloatContent::Text(_)) && !matches!(owner, FloatOwner::None)
}

/// 当前 Unix 秒（UTC）。
pub(super) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// 注释摘要的默认路径：`<stem>.annotations.md`，已存在则递增 `-2`、`-3`…（不覆盖）。
pub(super) fn annotation_summary_path(pdf: &std::path::Path) -> PathBuf {
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
pub(super) fn settings_key(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 「另存为」的提示位；条件集中在这里，便于按真值表测试。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SaveAsWarnings {
    /// 新文件不会写入数据文件。
    pub(super) new_missing_data: bool,
    /// 未保存数据未归档到新文件（但原文件已写）。
    pub(super) unsaved_new: bool,
    /// 两个数据文件都不写。
    pub(super) no_data_file: bool,
    /// 新文件写了，但原数据文件不更新。
    pub(super) original_stale: bool,
}

pub(super) fn save_as_warnings(
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
pub(super) fn color_swatch(
    ui: &mut egui::Ui,
    name: &str,
    rgb: [u8; 3],
    selected: bool,
) -> egui::Response {
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
pub(super) fn texture_key_bytes(
    page_size: Size,
    rotation: Rotation,
    max_side: u32,
    key: &RenderKey,
) -> u64 {
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
pub(super) fn dev_entries_enabled() -> bool {
    std::env::var_os("PDF_READER_DEV_ENTRY").is_some()
}

/// 从环境变量 `PDF_READER_TEXTURE_BUDGET_MB` 读预算（缺省用 [`TEXTURE_BUDGET_BYTES`]）。
pub(super) fn texture_budget_bytes() -> u64 {
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
pub(super) const TEXTURE_CAPACITY: usize = 128;

/// 侧栏宽度范围（内容空间像素）。宽度由 app 自己保存 —— 见 `sidebar_width` 的说明。
pub(super) const SIDEBAR_MIN_WIDTH: f32 = 160.0;
pub(super) const SIDEBAR_MAX_WIDTH: f32 = 640.0;
/// 侧栏拖拽条的宽度（由 app 自己处理拖拽，见 `draw_sidebar` 的说明）。
pub(super) const SIDEBAR_HANDLE_WIDTH: f32 = 5.0;

/// 显示链接热区框所需的最低缩放：更小的时候热区框会糊成一片，反而干扰阅读（用户要求）。
pub(super) const LINK_OVERLAY_MIN_ZOOM: f32 = 0.75;
/// 自定义**文字** preview 的默认正文尺寸（用户要求：初始化为一个默认值）。
pub(super) const DEFAULT_CUSTOM_TEXT_BODY: egui::Vec2 = egui::Vec2::new(300.0, 170.0);

/// 侧栏条目被点击后，浮窗高亮的持续时间（一次性动作需要一个有限时长）。
pub(super) const CLICK_HIGHLIGHT: std::time::Duration = std::time::Duration::from_millis(700);

/// 注释列表点击后，中性定位轮廓的持续时间。
pub(super) const ANNOTATION_FOCUS: std::time::Duration = std::time::Duration::from_millis(1200);

/// 写回前发现 PDF 被外部修改时的统一提示。
pub(super) const PDF_CONFLICT_MESSAGE: &str = "PDF 已被其他程序修改，已取消写入（原文件未改动）；当前修改仍在编辑器中。请丢弃修改后重新打开，或使用“另存为”保存到新文件。";
/// 滚动/缩放之后多久内不显示悬停预览（避免滚动时预览不断闪现）。
pub(super) const HOVER_SCROLL_QUIET: std::time::Duration = std::time::Duration::from_millis(250);
/// 大纲每行的固定高度（虚拟化行渲染要求等行高）。
pub(super) const OUTLINE_ROW_HEIGHT: f32 = 18.0;
/// 链接热区框的颜色（空心红线，用户指定）。
pub(super) const LINK_OVERLAY_COLOR: Color32 = Color32::from_rgb(220, 40, 40);

/// 标注命中的容差（**屏幕像素**；命中时按缩放换算成页面单位，否则高倍下会变成"无边框的靶子"）。
pub(super) const ANNOTATION_HIT_TOLERANCE_PX: f32 = 4.0;

/// 墨迹的默认线宽（**页面空间**单位；回放动作与 S4 的墨迹工具共用）。
pub(super) const DEFAULT_INK_WIDTH: f32 = 2.0;
/// 新建便签的图标边长（页面空间 pt）。
pub(super) const NOTE_ICON_SIZE: f32 = 18.0;

/// 历史窗口每行的高度（虚拟化列表要求等行高）。
pub(super) const HISTORY_ROW_HEIGHT: f32 = 18.0;

/// 历史窗口列表的最大高度（超出滚动；虚拟化只排版可见行）。
pub(super) const HISTORY_MAX_HEIGHT: f32 = 360.0;

// 链接框颜色（红/绿）由 `view::link_box_color` 决定，见 D-116。

/// 每帧最多上传几张纹理。
///
/// 一张整页位图的 RGBA 转换 + GPU 上传在毫秒量级；一帧内连传多张就会掉帧。
/// 设上限后开销被摊到多帧，视觉上是"相邻页稍后补齐"，比"翻页顿一下"好得多。
/// 每帧最多上传多少**张**纹理。
///
/// 原来是"每帧最多 2 **页**"（一个含糊的近似）；对账循环按"缺哪些键"逐张传，
/// 所以这里换成张数：40 张 ≈ 高倍下两页的可见块数量（12~18 块/页 × 2）。
pub(super) const MAX_TEXTURE_UPLOADS_PER_FRAME: usize = 40;

/// 缩放/旋转后进入"预览窗口"的时长：这段时间内不提交新渲染、只显示旧图拉伸。
///
/// 取 160ms：比人手连续滚轮的间隔（约 50–120ms）略长，因此一次连续缩放只会
/// 在最后渲染一次；又不至于让单次缩放的清晰图来得太迟。
pub(super) const ZOOM_PREVIEW_WINDOW: std::time::Duration = std::time::Duration::from_millis(160);

/// 拖拽中：区分"平移画面"与"选择文字"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragMode {
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
pub(super) struct InkMoveState {
    /// 被移动的注释。
    pub(super) id: AnnotationId,
    /// 拖动前的快照（undo 用）。
    pub(super) before: Annotation,
    /// 按下时指针在页面空间的位置。
    pub(super) start: Point,
    /// 原始折线（每次拖动都从它出发，避免累积误差）。
    pub(super) original: Vec<Point>,
}

/// 关闭/切换文件前挂起的动作。
pub(super) enum PendingActionKind {
    Quit,
    Open(PathBuf),
}

/// 有未保存改动时，关闭窗口或切换文件前先问一句。
pub(super) struct PendingAction {
    pub(super) kind: PendingActionKind,
}

/// 正文编辑草稿未保存时，用户又要求关闭浮窗/切换文件，先挂起该动作。
#[derive(Clone, Debug)]
pub(super) enum EditDiscardAction {
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
pub(super) struct WriteRequest {
    /// 目标地址（等于当前文档就是原地保存）。
    pub(super) target: PathBuf,
    /// 目标已存在时是否允许覆盖（UI 已确认）。
    pub(super) allow_overwrite: bool,
    /// 覆盖原文件时忽略外部修改（UI 已二次确认）。
    pub(super) allow_conflict: bool,
    /// 是否写原文件的数据文件。
    pub(super) write_source_data: bool,
    /// 是否写目标文件的数据文件。
    pub(super) write_target_data: bool,
}

/// 鼠标拖拽的语义（第二轮验收：去掉不可靠的"自动"模式，只留两种明确行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Tool {
    /// 总是平移画面（默认：拖拽永远不会意外改动选区）。
    #[default]
    Pan,
    /// 总是选字（拖到空白处不产生选区）。
    Select,
    /// 画墨迹（S4b）：拖拽画一笔，**抬起才入库**；不滚页也不选字。
    Ink,
}

impl Tool {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Pan => "平移",
            Self::Select => "选字",
            Self::Ink => "墨迹",
        }
    }

    /// 脚本里的名字（回放动作 `set_tool`）。
    pub(super) fn from_key(key: &str) -> Option<Self> {
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
pub(super) struct PdfFileStamp {
    pub(super) modified: Option<std::time::SystemTime>,
    pub(super) len: u64,
}

pub(super) fn pdf_file_stamp(path: &std::path::Path) -> std::io::Result<PdfFileStamp> {
    let metadata = std::fs::metadata(path)?;
    Ok(PdfFileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// 同目录存档的读取结果。
pub(super) enum Sidecar {
    /// 没有存档 / 解析失败（都当作"没有"）。
    Missing,
    /// 可用（可能刚从旧版本升级）。
    Ready(persist::Loaded),
    /// 由**更新版本**的程序写入：拒绝读取，并且**禁止写回**（D-107）。
    Locked { found: u32, supported: u32 },
}

/// 读取同目录存档。解析失败只打一行日志、当作没有；更新版本则返回 [`Sidecar::Locked`]。
pub(super) fn load_sidecar(pdf: &std::path::Path) -> Sidecar {
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
pub(super) enum EditField {
    /// 页码
    Page,
    /// 缩放比
    Zoom,
}

/// 缩放模式（决定缩放菜单的标题）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZoomMode {
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
    pub(super) fn label(self) -> &'static str {
        match self {
            ZoomMode::FitWidth => "适应宽度",
            ZoomMode::FitHeight => "适应高度",
            ZoomMode::Actual => "100%",
            ZoomMode::Custom => "自定义",
        }
    }
}

/// 路径过长时中间截断（保留开头与文件名），如 `aa/…/bb.pdf`。
pub(super) fn truncate_middle(text: &str, limit: usize) -> String {
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
pub(super) enum SidebarPane {
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
pub(super) struct CustomDialog {
    pub(super) kind: CustomKind,
    /// Pin 结束时点击的页号与页内坐标（PDF 页面空间）
    pub(super) page: PageIndex,
    pub(super) point: Point,
    pub(super) title: String,
    pub(super) content: String,
}

/// 新建便签的输入对话框。
pub(super) struct NoteDraft {
    /// 所在页。
    pub(super) page: PageIndex,
    /// 图标矩形（页面空间）。
    pub(super) rect: Rect,
    /// 正文草稿。
    pub(super) text: String,
}

/// 生成一个跨会话唯一的图片记录 id（无需额外依赖）。
pub(super) fn new_image_id(prefix: &str) -> String {
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
pub(super) struct PreparedMarkdown {
    /// 源文本哈希；变化时重建。
    pub(super) source_hash: u64,
    /// 换行/公式块规范化后的文本。
    pub(super) normalized: String,
    /// 图片引用（用于 owner 懒渲染，不必每帧重新解析）。
    pub(super) image_refs: Vec<crate::image_store::ImageRef>,
    /// 不可重排内容的最小宽度。
    pub(super) min_width: f32,
}

/// Markdown 源文本哈希（预处理缓存 key）。
pub(super) fn markdown_source_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// 在 TextEdit 当前光标/选区处插入文本，并把光标放到插入内容之后。
pub(super) fn insert_at_text_cursor(ctx: &egui::Context, id: Id, text: &mut String, snippet: &str) {
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
pub(super) struct CustomPreview {
    /// 会话内稳定 id：按页从 0 开始，删除不回收；不写进 sidecar。
    pub(super) id: u64,
    pub(super) page: PageIndex,
    /// 记录时的页内坐标。**只写不读**：当前只用于写回存档（D-49 的"位置类字段"），
    /// 后续"自定义 preview 与链接/被引用点挂接"会读它。
    #[allow(dead_code)]
    pub(super) point: Point,
    /// 以 `point` 为中心、**零尺寸**的矩形（与链接对应用）
    pub(super) rect: Rect,
    pub(super) kind: CustomKind,
    pub(super) title: String,
    /// 文字预览的内容
    pub(super) text: Option<String>,
    /// 截图预览的图像
    pub(super) snapshot: Option<egui::TextureHandle>,
    /// **浮窗正文尺寸**（egui 单位），语义同 `DestMark::body_size`。
    pub(super) body_size: egui::Vec2,
    /// ▲v2 截图时的**显示比例**（`1.0` = 100%）：预览图按它请求渲染，
    /// 因此预览分辨率与"当前缩放"解耦（D-100）。文字预览用不到，填 1.0。
    pub(super) scale: f32,
    /// ▲v4 稳定图片记录 id；文字 preview 为空。
    pub(super) image_id: String,
    /// ▲v4 图片内容版本。
    pub(super) image_revision: u64,
}

/// 截图模式产出的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum CaptureTarget {
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
pub(super) struct HoverTip {
    /// 对应的被引用点 `(页号, 该页内下标)`
    pub(super) target: (PageIndex, usize),
    /// 屏幕空间锚点（链接矩形的左下角附近）
    pub(super) anchor: egui::Pos2,
    /// 上一帧实际绘制出来的矩形（用于判断"指针是否还在浮窗上"）
    pub(super) rect: Option<egui::Rect>,
}

/// 一个**被引用点**（链接跳转目的地）及其可编辑的截图信息。
///
/// * `rect`：页面空间的一块矩形，初始化时是点周围的一小块；用户可在截图模式里改；
/// * `snapshot`：**初始为 `None`** —— 没有额外动作前不渲染任何截图（用户要求）；
///   只有"点击标识 → 截图模式 → Enter"之后才会变成 `Some`。
#[derive(Clone)]
pub(super) struct DestMark {
    /// 页面空间的位置。
    pub(super) point: Point,
    /// 页面空间的矩形（与当前页重叠的部分）。
    pub(super) rect: Rect,
    /// 已构建的截图（自包含纹理）。
    pub(super) snapshot: Option<egui::TextureHandle>,
    /// **浮窗正文尺寸**（egui 单位）：生成浮窗时正文按它显示；
    /// 用户改浮窗大小后回写。
    pub(super) body_size: egui::Vec2,
    /// ▲v2 截图时的**显示比例**（`1.0` = 100%）：预览图按它请求渲染（D-100）。
    pub(super) scale: f32,
    /// ▲v4 稳定图片记录 id。
    pub(super) image_id: String,
    /// ▲v4 图片内容版本。
    pub(super) image_revision: u64,
}

/// 截图模式的状态。
///
/// 进入后：主视图的拖动/滚动/缩放/链接跳转等**一律不响应**（瓦片加载仍继续），
/// 浮窗与所有叠层隐藏，主视图蒙上半透明亮灰；鼠标拖出矩形，矩形内透明（"挖洞"）。
/// `Enter` 退出并在终端输出区域信息，`Esc` 退出且无输出。
#[derive(Default)]
pub(super) struct CaptureState {
    /// 已画出的选区（**屏幕**空间）；`None` = 尚未开始
    pub(super) rect: Option<egui::Rect>,
    /// 正在拖拽的起点（屏幕空间）
    pub(super) anchor: Option<egui::Pos2>,
    /// 这次截图的结果要回写到哪儿。
    pub(super) target: CaptureTarget,
}

/// 当前“下一步把选区变成哪种文本标记”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkupTool {
    Highlight,
    Underline,
    StrikeOut,
}

impl MarkupTool {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Highlight => "高亮",
            Self::Underline => "下划线",
            Self::StrikeOut => "删除线",
        }
    }

    pub(super) fn kind(self, quads: Vec<Rect>) -> AnnotationKind {
        match self {
            Self::Highlight => AnnotationKind::Highlight { quads },
            Self::Underline => AnnotationKind::Underline { quads },
            Self::StrikeOut => AnnotationKind::StrikeOut { quads },
        }
    }
}

pub(super) type ReadAnnotationEntry<'a> = (
    &'a backend_pdfium::readback::ReadAnnotation,
    Option<pdfcore::annotation::PdfObjectRef>,
    Option<(PageIndex, usize)>,
);

/// 自定义 preview 的会话内稳定键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PreviewKey {
    pub(super) page: PageIndex,
    pub(super) id: u64,
}

/// 一条可撤销的编辑（S3）。
///
/// 命令里**持有旧值**（`Arc` 共享，不做深拷贝）：撤销时把它移回应用即可。
#[derive(Clone)]
pub(super) enum AppEdit {
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
    /// 调整引用点浮窗正文尺寸（与 preview 的 resize 同语义）。
    ResizeDestMark {
        /// 页号。
        page: PageIndex,
        /// 该页第几个被引用点。
        index: usize,
        /// 旧正文尺寸。
        before: egui::Vec2,
        /// 新正文尺寸。
        after: egui::Vec2,
    },
}

impl AppEdit {
    /// 这条编辑是否改变 PDF 里的内容（注释）。preview/引用点只写 `.json`。
    pub(super) fn affects_pdf(&self) -> bool {
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
    pub(super) fn describe(&self) -> String {
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
            Self::ResizeDestMark { page, .. } => {
                format!("调整引用点浮窗大小（第 {} 页）", page.display_number())
            }
        }
    }

    /// 估算权重：截图类命令持有位图（在 `Arc` 后面共享），按 1 MiB 估。
    pub(super) fn weight(&self) -> usize {
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
pub(super) enum FloatContent {
    /// 纯文字。
    Text(String),
    /// 截图（独立纹理，与主视图状态解耦；改窗口大小只改变显示尺寸，不改变内容）。
    Image(egui::TextureHandle),
}

/// 浮窗的来源：注释正文是一类独立对象，不与自定义 preview 混用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FloatOwner {
    /// 注释的正文浮窗。
    Annotation(AnnotationId),
    /// 自定义文字/图片 preview 的正式浮窗。
    Custom(usize),
    /// 测试浮窗、链接预览、截图等。
    None,
}

/// 视图区域内的浮窗（始终在画布内部、可移动/改大小/关闭）。
pub(super) struct Float {
    /// 唯一 id（用于 egui 的交互标识）。
    pub(super) id: u64,
    /// 标题栏文字。
    pub(super) title: String,
    /// 内容（文字或截图）。
    pub(super) content: FloatContent,
    /// 在**视图区域**（画布）内的矩形；每帧会被夹取回区域内。
    pub(super) rect: egui::Rect,
    /// 来源（注释 / 自定义 preview / 其它）。
    pub(super) owner: FloatOwner,
    /// 正文尺寸的**权威值**：只在"创建"与"拖动右下角改大小"时写入。
    ///
    /// 不从 `rect` 每帧反算 —— 那样会带入换算与夹取的亚像素误差，
    /// 使"尺寸是否变化"的判断失效（D-73 的根因）。浮窗自己持有它，
    /// 回写时直接赋值，比较就是数值对数值（用户建议的模型）。
    pub(super) body: egui::Vec2,
    /// 来源：由某个被引用点的悬停预览**固定**而来时记录 `(页号, 下标)`。
    /// 用于"同一目的地再次悬停时高亮并置顶这个浮窗，而不是再弹预览"。
    pub(super) origin: Option<(PageIndex, usize)>,
}

impl Float {
    pub(super) fn custom_index(&self) -> Option<usize> {
        match self.owner {
            FloatOwner::Custom(index) => Some(index),
            _ => None,
        }
    }
}
