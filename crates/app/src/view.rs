//! 纯逻辑层：视图坐标换算、输入映射、纹理存储。
//!
//! 这些都不需要窗口/GPU，因此**可以被 `cargo test` 覆盖**；
//! 需要真实窗口与人工观察的部分（滚动跟手性、字形、配色）全部留给 `app.rs` 的 egui 胶水，
//! 按 agent.md 规则 2 交由用户验收。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use egui::{Key, Modifiers, Vec2 as EguiVec2};
use pdfcore::prelude::*;

/// 键盘滚动步长（内容空间像素）。
pub const KEY_SCROLL_STEP: f32 = 64.0;

/// 滚轮缩放系数（每格）。
pub const WHEEL_ZOOM_FACTOR: f32 = 1.1;

/// 把内容空间的页矩形换算到**视图空间**（减去滚动偏移）。
///
/// 这是 UI 每帧都要做的事：`PagePlacement` 给出的是内容空间矩形，
/// `Viewport::scroll` 是视口左上角在内容空间的位置，两者相减即得屏幕上的落点。
pub fn view_rect_for(placement: &PagePlacement, scroll: Vec2) -> Rect {
    placement.rect.translate(Vec2::new(-scroll.x, -scroll.y))
}

/// 视图坐标 → 内容坐标（鼠标事件需要这个方向的换算）。
pub fn view_to_content(view_point: Point, scroll: Vec2) -> Point {
    Point::new(view_point.x + scroll.x, view_point.y + scroll.y)
}

/// 键盘 → 命令。`page_count` 用于 `End` 键（跳到最后一页）。
pub fn command_for_key(key: Key, modifiers: Modifiers, page_count: u32) -> Option<ViewerCommand> {
    let command_modifier = modifiers.ctrl || modifiers.command;
    match key {
        Key::ArrowDown => Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP))),
        Key::ArrowUp => Some(ViewerCommand::ScrollBy(Vec2::new(0.0, -KEY_SCROLL_STEP))),
        Key::Space => Some(ViewerCommand::ScrollBy(Vec2::new(
            0.0,
            KEY_SCROLL_STEP * 4.0,
        ))),
        Key::PageDown => Some(ViewerCommand::NextPage),
        Key::PageUp => Some(ViewerCommand::PrevPage),
        Key::Home => Some(ViewerCommand::GoToPage(PageIndex::FIRST)),
        Key::End if page_count > 0 => Some(ViewerCommand::GoToPage(PageIndex::new(page_count - 1))),
        Key::Plus | Key::Equals => Some(ViewerCommand::ZoomIn),
        Key::Minus => Some(ViewerCommand::ZoomOut),
        Key::Num0 => Some(ViewerCommand::SetZoom(1.0)),
        Key::F if command_modifier => Some(ViewerCommand::SetSidebar(SidebarTab::Search)),
        Key::C if command_modifier => Some(ViewerCommand::CopySelection),
        Key::A if command_modifier => Some(ViewerCommand::SelectAll),
        Key::Escape => Some(ViewerCommand::SelectClear),
        _ => None,
    }
}

/// 滚轮 → 命令。
///
/// 注意方向：egui 的 `raw_scroll_delta.y` **向上为正**，而 `Viewport::scroll` 是
/// "视口左上角在内容空间的位置"（向下为正），因此这里取负号。
/// 按住 Ctrl/Cmd 时改为缩放（以当前缩放为基准按比例变化）。
pub fn command_for_scroll(
    delta: EguiVec2,
    zoom_modifier: bool,
    current_zoom: f32,
) -> Option<ViewerCommand> {
    if zoom_modifier {
        if delta.y.abs() < 0.5 {
            return None;
        }
        let factor = if delta.y > 0.0 {
            WHEEL_ZOOM_FACTOR
        } else {
            1.0 / WHEEL_ZOOM_FACTOR
        };
        Some(ViewerCommand::SetZoom(current_zoom * factor))
    } else if delta.x.abs() > 0.0 || delta.y.abs() > 0.0 {
        Some(ViewerCommand::ScrollBy(Vec2::new(-delta.x, -delta.y)))
    } else {
        None
    }
}

/// 触控板/Ctrl+滚轮产生的**缩放增量** → 命令。
///
/// egui 会把 `Ctrl`+滚轮识别成缩放而不是滚动（见 `InputOptions::zoom_modifier`）：
/// 此时 `smooth_scroll_delta` 为 0，而 `InputState::zoom_delta()` 给出倍率。
/// 人工验收反馈的"Ctrl+滚轮不生效"根因就在这里 —— v1 只看滚动增量。
pub fn command_for_zoom_delta(zoom_delta: f32, current_zoom: f32) -> Option<ViewerCommand> {
    if !zoom_delta.is_finite() || (zoom_delta - 1.0).abs() < 1e-3 {
        return None;
    }
    Some(ViewerCommand::SetZoom(current_zoom * zoom_delta))
}

/// 一块瓦片在**视图空间**中的矩形。
///
/// 位置与尺寸由 [`TileGrid::display_rect`] 给出：`x = 页.x + 块像素_x · u`、
/// `w = 块像素_w · u`，其中 `u = 显示比例 / 渲染比例`；最后减去滚动量。
///
/// ## D-17 的旧结论是 `u = 1` 的特例（D-94）
/// 旧实现直接相加像素偏移（"内容单位 == 位图像素"），因为它假设渲染比例就是显示比例。
/// 波段超采样后 `u = 显示/渲染 ≤ 1` 且**随波段内缩放连续变化**，因此必须乘 `u`；
/// 但**绝不能**乘成"除以显示比例"—— 那正是第三轮验收"748% 时整块画布全黑"的根因
/// （6 块瓦片被压进页面左上角 1/7 的小方格）。
pub fn tile_view_rect(
    grid: &TileGrid,
    placement: &PagePlacement,
    col: u32,
    row: u32,
    render_scale: f32,
    scroll: Vec2,
) -> Rect {
    let content = grid.display_rect(placement.rect, col, row, placement.scale.0, render_scale);
    content.translate(Vec2::new(-scroll.x, -scroll.y))
}

/// 把**上一波段的块**映射到当前页面矩形上（缩放过场的预览用）。
///
/// 高倍缩放下缓存里只有块、没有整页图，因此"上一档的旧图"要用旧块拼出来：
/// 每块按 `新页尺寸 / 旧页尺寸` 的比例缩放后贴到页面矩形的对应位置。
///
/// ▲D-94：`old_page` 传**旧波段的整页渲染像素尺寸**（`TileGrid::render_px`），
/// `new_page` 传**当前页面在内容空间的显示尺寸**。这样得到的是"按占比"的映射，
/// 与 `u = 显示/渲染` 具体是多少无关（旧块的位图本身就是超采样过的）。
pub fn tile_view_rect_scaled(
    page_rect: Rect,
    pixels: Rect,
    old_page: Size,
    new_page: Size,
) -> Rect {
    let sx = if old_page.width > 0.0 {
        new_page.width / old_page.width
    } else {
        1.0
    };
    let sy = if old_page.height > 0.0 {
        new_page.height / old_page.height
    } else {
        1.0
    };
    Rect::new(
        page_rect.x + pixels.x * sx,
        page_rect.y + pixels.y * sy,
        pixels.width * sx,
        pixels.height * sy,
    )
}

/// 画布矩形 = 中央面板区域去掉左侧拖拽条，**高度必须保持满高**。
///
/// ## 为什么单拎出来
/// 第五轮验收出现"分割线能拖，但没有任何图像"：当时用
/// `ui.allocate_painter(5px × 整高)` 画拖拽条，纵向布局把**整个高度**都分配给了它，
/// 画布拿到的高度就是 0 —— 什么都画不出来。
/// 现在拖拽条与画布都用"显式矩形 + `interact`"，不参与布局流；这个函数负责算矩形，
/// 并由 `canvas_rect_keeps_full_height` 钉住"高度不得被吃掉"。
pub fn canvas_rect_for(full: egui::Rect, handle_width: f32) -> egui::Rect {
    // 夹取拖拽条宽度，避免区域比它更窄时出现负尺寸矩形
    let handle = handle_width.max(0.0).min(full.width().max(0.0));
    egui::Rect::from_min_max(egui::pos2(full.min.x + handle, full.min.y), full.max)
}

/// 链接框在这些条件下可见：开关打开、且缩放不低于阈值。
///
/// 缩放过小时热区框会糊成一片、反而干扰阅读，因此低于阈值直接不画（用户要求）。
pub fn link_overlay_visible(enabled: bool, zoom: f32, min_zoom: f32) -> bool {
    enabled && zoom.is_finite() && zoom >= min_zoom
}

/// 链接跳转要发出的命令序列。
///
/// 用户要求：**链接目的地若带具体坐标，就把该坐标放到视口中心**（`CenterOnPagePoint`）；
/// 只有页、没有坐标时退化为顶部对齐（D-11 的跳页语义）。
/// 外部 URI 目前只提示、不打开浏览器（未实现的平台能力）。
pub fn commands_for_destination(destination: &Destination) -> Vec<ViewerCommand> {
    match destination {
        Destination::Page { page, offset } => {
            let mut commands = vec![ViewerCommand::GoToPage(*page)];
            if let Some(point) = offset {
                commands.push(ViewerCommand::CenterOnPagePoint {
                    page: *page,
                    point: *point,
                });
            }
            commands
        }
        Destination::Uri(_) => Vec::new(),
    }
}

/// 链接热区是否落在页面范围内（诊断用）。
///
/// 若某个文件的链接框"与超链接位置不符"，先看这条：
/// **热区坐标超出了页尺寸** 就意味着坐标空间不一致（例如 PDF 的 `MediaBox` 原点不为 0，
/// 或 `CropBox` 与 `MediaBox` 不同），而不是"框画错了"。
/// 允许 1pt 的浮点余量。
pub fn link_rect_in_page_bounds(rect: Rect, page_size: Size) -> bool {
    let slack = 1.0;
    rect.x >= -slack
        && rect.y >= -slack
        && rect.right() <= page_size.width + slack
        && rect.bottom() <= page_size.height + slack
}

/// 合成快照时"一页可用的已渲染位图"。
///
/// ## 位图像素 ≠ 内容单位（D-94）
/// 波段渲染比例可以大于显示比例（超采样），因此"位图像素偏移直接相加"这条旧不变量
/// （D-17）只在 `u = 显示/渲染 == 1` 时成立。这里改成**与 `u` 无关**的表达：
/// 每张位图都配一个"它铺满哪个内容矩形"，铺放时统一按比例重采样。
/// 于是整页图（渲染比例位图 → 页面显示矩形）与块（块位图 → 块的显示矩形）
/// 走同一条路径，`u` 是多少都不影响结果。
pub struct SnapshotPage<'a> {
    /// 该页在内容空间的矩形（尺寸 = 显示尺寸）。
    pub rect: Rect,
    /// 整页位图（未分块且已缓存时可用）。
    pub whole: Option<&'a RasterImage>,
    /// 已缓存的块：`(该块在内容空间的矩形, 位图)`，矩形由 `TileGrid::display_rect` 给出。
    pub tiles: Vec<(Rect, &'a RasterImage)>,
}

/// 合成快照时的一层内容：区域 + 整页图 + 该区域内的块。
pub type SnapshotLayer = (
    Rect,
    Option<Arc<RasterImage>>,
    Vec<(Rect, Arc<RasterImage>)>,
);

/// 把 `content_rect` 覆盖到的**已渲染像素**拷成一张自包含图像（截图快照）。
///
/// ## 为什么存成内存快照，而不是"引用已渲染纹理的对应区域"
/// 1. 主视图一旦缩放或换页，原纹理会被淘汰或换成另一比例 —— 浮窗内容会失效或显示错区域；
/// 2. 区域可能跨页、跨留白，单张纹理根本无法表达；
/// 3. 快照自包含，与主视图状态完全解耦，单页/多页走**同一条**代码路径。
///
/// 代价只有一次区域像素拷贝（区域 ≤ 一屏），采样为最近邻（预览用，不追求重采样质量）。
/// 未被页面覆盖或**尚未就绪**的地方保持白色。
/// 完全没有可用像素时返回 `None`（调用方据此提示）。
pub fn compose_snapshot(
    pages: &[SnapshotPage<'_>],
    content_rect: Rect,
) -> Option<egui::ColorImage> {
    let width = content_rect.width.ceil().max(1.0) as usize;
    let height = content_rect.height.ceil().max(1.0) as usize;
    // 白底：页面未覆盖处（页间距、页边距）与未就绪处都留白
    let mut rgba = vec![255u8; width * height * 4];

    /// 把 `image` 铺满 `dest`（内容空间）后，拷进目标缓冲里与 `content_rect` 相交的部分。
    ///
    /// 采样点取目标像素中心再按比例映射回源像素：`dest` 比 `image` 大就是放大，
    /// 小就是缩小（波段超采样时正是缩小），两种情况用同一段代码。
    fn blit_fitted(
        image: &RasterImage,
        dest: Rect,
        content_rect: Rect,
        out: &mut [u8],
        out_size: (usize, usize),
    ) -> bool {
        if dest.width <= 0.0 || dest.height <= 0.0 || image.width == 0 || image.height == 0 {
            return false;
        }
        let Some(overlap) = dest.intersection(content_rect) else {
            return false;
        };
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            return false;
        }
        let stride = image.format.bytes_per_pixel();
        let origin = content_rect.min();
        let dest_left = ((overlap.x - origin.x).round().max(0.0)) as usize;
        let dest_top = ((overlap.y - origin.y).round().max(0.0)) as usize;
        let columns = overlap.width.round().max(0.0) as usize;
        let rows = overlap.height.round().max(0.0) as usize;
        let scale_x = image.width as f32 / dest.width;
        let scale_y = image.height as f32 / dest.height;
        let data = image.data();
        let mut painted = false;
        for row in 0..rows {
            let content_y = overlap.y + row as f32 + 0.5;
            let source_y = ((content_y - dest.y) * scale_y).floor();
            if source_y < 0.0 || source_y >= image.height as f32 {
                continue;
            }
            let dest_y = dest_top + row;
            if dest_y >= out_size.1 {
                continue;
            }
            for column in 0..columns {
                let content_x = overlap.x + column as f32 + 0.5;
                let source_x = ((content_x - dest.x) * scale_x).floor();
                if source_x < 0.0 || source_x >= image.width as f32 {
                    continue;
                }
                let dest_x = dest_left + column;
                if dest_x >= out_size.0 {
                    continue;
                }
                let from = (source_y as usize * image.width as usize + source_x as usize) * stride;
                let to = (dest_y * out_size.0 + dest_x) * 4;
                let (r, g, b) = match image.format {
                    PixelFormat::Bgra8 => (data[from + 2], data[from + 1], data[from]),
                    PixelFormat::Rgba8 => (data[from], data[from + 1], data[from + 2]),
                    PixelFormat::Gray8 => (data[from], data[from], data[from]),
                };
                out[to] = r;
                out[to + 1] = g;
                out[to + 2] = b;
                out[to + 3] = 255;
                painted = true;
            }
        }
        painted
    }

    let mut painted = false;
    for page in pages {
        let Some(overlap) = page.rect.intersection(content_rect) else {
            continue;
        };
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            continue;
        }

        if let Some(image) = page.whole {
            painted |= blit_fitted(image, page.rect, content_rect, &mut rgba, (width, height));
            continue;
        }

        for (tile_rect, image) in &page.tiles {
            painted |= blit_fitted(image, *tile_rect, content_rect, &mut rgba, (width, height));
        }
    }

    if !painted {
        return None;
    }
    Some(color_image(
        PixelFormat::Rgba8,
        &rgba,
        width as u32,
        height as u32,
    ))
}

// ---------------------------------------------------------------------------
// 画法决策（波段切换时"旧内容 vs 新块"）
// ---------------------------------------------------------------------------

/// 当前波段成为"当前"之后，最多等多久再放弃预览、改为"已就绪 + 补白"。
///
/// 取 400ms：比"160ms 缩放预览窗口 + 十几块 × 约 9ms 串行渲染"略宽，
/// 又短于用户感知的"画面卡住"。它同时是"某块渲染失败，不让页面永远停在糊图"的兜底。
pub const READY_TIMEOUT_MS: u64 = 400;

/// 一页可用的"旧内容"（**整页级**，绝不逐块混用 —— D-88）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    /// 没有任何旧内容（首次打开某页、或刚切换文档）。
    None,
    /// 有整页图（参数是它所在波段的 `scale_milli`）。
    Whole(u32),
    /// 只有旧波段的块（参数是它所在波段的 `scale_milli`）。
    Tiles(u32),
}

/// 一页在这一帧应该怎么画。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDraw {
    /// 当前波段的整页图（不分块路径）。
    Whole,
    /// 旧整页图拉伸到整页矩形（`scale_milli` 供诊断）。
    Stretch(u32),
    /// 旧波段的块按占比拼出整页（`scale_milli` 供诊断）。
    TilePreview(u32),
    /// 当前波段的块，未就绪处补白。
    Tiles,
    /// 什么都没有。
    Blank,
}

impl PageDraw {
    /// 诊断用的短名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Whole => "整页纹理",
            Self::Stretch(_) => "旧图拉伸",
            Self::TilePreview(_) => "旧瓦片预览",
            Self::Tiles => "瓦片",
            Self::Blank => "白底页框",
        }
    }
}

/// 某一帧某页的"画法快照"。
///
/// 两个用途：`[画法]` 的**边沿触发**输出（只在变化时打一行），以及回放器读取
/// "一次波段切换里画法翻了哪几次"（断言"只翻一次"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanStamp {
    /// 这一帧采用的画法。
    pub plan: PageDraw,
    /// 该页当前的渲染波段（`scale_milli`）。
    pub band: u32,
    /// 是否"部分块"（画 `Tiles` 但可见范围还没齐：粘性模式的常规情况，或超时兜底）。
    pub partial: bool,
    /// 可见范围内已就绪的块数。
    pub ready: usize,
    /// 可见范围内的块数。
    pub total: usize,
    /// 该波段第一块就绪以来经过的毫秒数（0 = 还没有任何块就绪）。
    pub waited_ms: u64,
    /// 这一帧可用的预览源（`None` = 没有任何旧内容）。
    pub preview: Preview,
}

impl PlanStamp {
    /// 用于"是否变化"的比较签名（**不含** `waited_ms`：它每帧都在变）。
    pub fn stable_key(self) -> (String, u32, bool, usize, usize) {
        (
            self.describe(),
            self.band,
            self.preview != Preview::None,
            self.ready,
            self.total,
        )
    }

    /// 诊断用的短名（兜底时带后缀）。
    pub fn describe(self) -> String {
        if self.partial {
            format!("{}（兜底）", self.plan.label())
        } else {
            self.plan.label().to_string()
        }
    }
}

/// [`plan_page`] 的输入：全部是"已经知道的事实"，构造后即可单测。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawInput {
    /// 该页当前波段是否分块（`DocumentSession::visible_tiles` 是否 `Some`）。
    pub tiled: bool,
    /// 当前波段的整页纹理是否已在（只有不分块路径才可能为真）。
    pub has_whole_current: bool,
    /// 可见范围内"已在纹理层"的块数。
    pub ready: usize,
    /// 可见范围内有效块数（已按 [`TileGrid::contains`] 过滤）。
    pub total: usize,
    /// 可用的旧内容。
    pub preview: Preview,
    /// 当前波段**第一块就绪**以来的毫秒数（0 = 还没有任何块就绪）。
    ///
    /// 起点选"第一块就绪"而不是"波段变化"：慢手势期间波段不断变化，
    /// 从波段变化计时会让第一块一到就超时，白块就又回来了。
    pub waited_ms: u64,
    /// **该页在当前波段上一帧已经是 `Tiles`**（D-104）。
    ///
    /// "已经切到瓦片就不再退回预览"：波段内滚动会不断露出新块，
    /// 若因为"新的可见范围没齐"就退回整页预览，用户会看到**整页**变糊一下（实测 40~100ms）。
    /// 一旦进入瓦片模式，新块没就绪就由底料补上（D-98），已清晰的部分保持清晰。
    pub showing_tiles: bool,
}

/// 决定一页这一帧怎么画 —— "旧内容 vs 新块"的**唯一**判据。
///
/// ## 为什么需要"整页级就绪门槛"（D-96）
/// 旧实现是"当前波段**任意一块**到了就翻页"（`has_tiles`）。波段切换后块是逐块到达的
/// （PDFium 串行，约 9ms/块），于是第一块一到，整页立刻从"上一波段的预览"切成
/// "几块清晰 + 大片纯白补白" —— 这正是真机看到的白块。
///
/// 现在只有**可见范围内全部块就绪**才切；否则继续用整页级的旧内容顶住。
/// 落笔仍然"整页一个来源"，不是 D-87 那种"缺哪块补哪块"的逐块混用（D-88 的结论）。
pub fn plan_page(input: &DrawInput) -> PageDraw {
    if input.has_whole_current {
        return PageDraw::Whole;
    }
    if !input.tiled {
        return match input.preview {
            Preview::Whole(scale) => PageDraw::Stretch(scale),
            Preview::Tiles(scale) => PageDraw::TilePreview(scale),
            Preview::None => PageDraw::Blank,
        };
    }
    // 分块：**已经切到瓦片**就留在瓦片模式（D-104）。
    // 波段内滚出新块时，缺的那块由底料补（D-98），不把整页退回预览 —— 那会让已经清晰的
    // 区域也变糊。波段变化时 `showing_tiles` 自然为假（上一帧属于旧波段），仍走下面的门槛。
    if input.showing_tiles {
        return PageDraw::Tiles;
    }
    // 分块：可见范围一块不缺才切
    if input.total > 0 && input.ready >= input.total {
        return PageDraw::Tiles;
    }
    // 兜底：已经等过超时（某块渲染失败、或块特别多）→ 先画已就绪的，别永远糊着
    if input.ready > 0 && input.waited_ms >= READY_TIMEOUT_MS {
        return PageDraw::Tiles;
    }
    match input.preview {
        Preview::Whole(scale) => PageDraw::Stretch(scale),
        Preview::Tiles(scale) => PageDraw::TilePreview(scale),
        // 没有任何旧内容（首次打开某页）：只能边渲染边补白
        Preview::None => PageDraw::Tiles,
    }
}

/// 这一帧是否需要在"主图层"下面再铺一层**底料**（D-99）。
///
/// 预览层（整页拉伸 / 旧块拼）本身也可能**不完整**：连续放大时上一波段常常只画完
/// 一两块就被下一次缩放取消（`set_zoom` 会 `cancel_all`），此时 `TilePreview` 只覆盖
/// 页面的一小块 —— 光靠它，其余位置没有任何图层，就是实测到的白块。
///
/// 因此规则是：只要这一帧画的不是"当前波段整页图"，就先把覆盖优先的底料铺上；
/// `Tiles` 模式只在**还有块没就绪**时需要（全齐时块已经把底料盖满了）。
pub fn needs_underlay(plan: PageDraw, ready: usize, total: usize) -> bool {
    match plan {
        PageDraw::Whole | PageDraw::Blank => false,
        PageDraw::Stretch(_) | PageDraw::TilePreview(_) => true,
        PageDraw::Tiles => ready < total,
    }
}

/// 预览实际可用的渲染比例：从"记录分辨率对应的波段比例"开始，按超采样步长**降档**，
/// 直到区域像素尺寸的**单边**不超过纹理上限（`max_side == 0` 表示不限制）。
///
/// 为什么需要：预览区域可能很大（比如整页截图）而记录的分辨率很高，直接按记录值
/// 合成会得到一张超过显卡上限的纹理 —— 那是创建不出来的。降档后图会糊一点，但能显示（D-108）。
pub fn preview_render_scale(
    page_rect: Rect,
    page_size: Size,
    rotation: Rotation,
    recorded_scale: f32,
    max_side: u32,
) -> f32 {
    let mut scale = if recorded_scale > 0.0 && recorded_scale.is_finite() {
        recorded_scale
    } else {
        1.0
    };
    if max_side == 0 {
        return scale;
    }
    // 最多降到 1/1.6^4（约 1/6.5）：再小就没有预览的意义了
    for _ in 0..4 {
        match region_pixel_size(page_rect, page_size, rotation, scale) {
            Some((width, height)) if width.max(height) > max_side => {
                scale /= DEFAULT_PREVIEW_STEP;
            }
            _ => break,
        }
    }
    scale
}

/// 预览降档的步长（与超采样倍数一致：每降一档就是"上一个波段"）。
pub const DEFAULT_PREVIEW_STEP: f32 = 1.6;

/// 页面空间矩形在**设备像素**里的整数矩形 `(x0, y0, 宽, 高)`。
///
/// 换算链：四角 `Rotation::map_point` → 显示坐标 → × 渲染比例 → 外接矩形 → 两端取整
/// （与块边界同一套规则：两端各 `round` 一次）。
///
/// 这是"预览图应该多大、从哪个像素开始"的**唯一**来源：`compose_region_snapshot` 用它分配缓冲，
/// 回放器的断言也用它比对实际纹理尺寸（D-103 的断言 ③）。
pub fn region_device_rect(
    page_rect: Rect,
    page_size: Size,
    rotation: Rotation,
    render_scale: f32,
) -> Option<(i64, i64, u32, u32)> {
    if !(page_rect.width > 0.0) || !(page_rect.height > 0.0) {
        return None;
    }
    let scale = if render_scale > 0.0 {
        render_scale
    } else {
        1.0
    };
    let corners = [
        Point::new(page_rect.x, page_rect.y),
        Point::new(page_rect.right(), page_rect.y),
        Point::new(page_rect.x, page_rect.bottom()),
        Point::new(page_rect.right(), page_rect.bottom()),
    ];
    let mut min = Point::new(f32::MAX, f32::MAX);
    let mut max = Point::new(f32::MIN, f32::MIN);
    for corner in corners {
        let display = rotation.map_point(corner, page_size);
        let device = Point::new(display.x * scale, display.y * scale);
        min = Point::new(min.x.min(device.x), min.y.min(device.y));
        max = Point::new(max.x.max(device.x), max.y.max(device.y));
    }
    let x0 = min.x.round() as i64;
    let y0 = min.y.round() as i64;
    let width = (max.x.round() as i64 - x0).max(0);
    let height = (max.y.round() as i64 - y0).max(0);
    if width == 0 || height == 0 {
        return None;
    }
    Some((x0, y0, width as u32, height as u32))
}

/// 预览区域的**输出像素尺寸** = [`region_device_rect`] 的宽高。
pub fn region_pixel_size(
    page_rect: Rect,
    page_size: Size,
    rotation: Rotation,
    render_scale: f32,
) -> Option<(u32, u32)> {
    region_device_rect(page_rect, page_size, rotation, render_scale)
        .map(|(_, _, width, height)| (width, height))
}

/// 由"某页某区域、指定渲染比例的块"拼出一张预览图（D-101）。
///
/// ## 与 [`compose_snapshot`] 的分工
/// * `compose_snapshot` 处理**内容空间**的任意矩形（可跨页），用于手动截图；
///   它必须按 `u = 显示/渲染` 重采样。
/// * 本函数处理**页面空间**的单页矩形，在**渲染像素**里 1:1 拷贝（块位图与目标缓冲
///   处在同一坐标系、同一比例），因此没有重采样误差；输出尺寸 = `矩形 × 渲染比例`，
///   与"当前缩放"完全无关 —— 这正是预览图要按**记录的分辨率**重建的原因。
///
/// 未覆盖到的像素保持白色；一个像素都没覆盖到返回 `None`。
pub fn compose_region_snapshot(
    page_rect: Rect,
    page_size: Size,
    rotation: Rotation,
    render_scale: f32,
    grid: &TileGrid,
    tiles: &[(TileSpec, &RasterImage)],
) -> Option<egui::ColorImage> {
    if !(page_rect.width > 0.0) || !(page_rect.height > 0.0) {
        return None;
    }
    let (x0, y0, width, height) = region_device_rect(page_rect, page_size, rotation, render_scale)?;
    let (width, height) = (width as usize, height as usize);
    let mut rgba = vec![255u8; width * height * 4];
    let mut painted = false;
    for (tile, image) in tiles {
        let (tile_x, tile_y, tile_width, tile_height) = grid.tile_px(tile.col, tile.row);
        if tile_width == 0 || tile_height == 0 {
            continue;
        }
        let stride = image.format.bytes_per_pixel();
        let data = image.data();
        // 块与目标区域的交叠（两者都是渲染像素，直接按整数拷贝）
        let left = (tile_x as i64).max(x0);
        let top = (tile_y as i64).max(y0);
        let right = (tile_x as i64 + image.width as i64).min(x0 + width as i64);
        let bottom = (tile_y as i64 + image.height as i64).min(y0 + height as i64);
        if left >= right || top >= bottom {
            continue;
        }
        for row in top..bottom {
            let source_y = (row - tile_y as i64) as usize;
            let dest_y = (row - y0) as usize;
            for column in left..right {
                let source_x = (column - tile_x as i64) as usize;
                let dest_x = (column - x0) as usize;
                let from = (source_y * image.width as usize + source_x) * stride;
                let to = (dest_y * width + dest_x) * 4;
                let (r, g, b) = match image.format {
                    PixelFormat::Bgra8 => (data[from + 2], data[from + 1], data[from]),
                    PixelFormat::Rgba8 => (data[from], data[from + 1], data[from + 2]),
                    PixelFormat::Gray8 => (data[from], data[from], data[from]),
                };
                rgba[to] = r;
                rgba[to + 1] = g;
                rgba[to + 2] = b;
                rgba[to + 3] = 255;
                painted = true;
            }
        }
    }
    if !painted {
        return None;
    }
    Some(color_image(
        PixelFormat::Rgba8,
        &rgba,
        width as u32,
        height as u32,
    ))
}

/// 把图像按**保持长宽比**的方式放进 `area`（居中，不拉伸变形）。
pub fn fit_inside(image: egui::Vec2, area: egui::Rect) -> egui::Rect {
    if image.x <= 0.0 || image.y <= 0.0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return area;
    }
    let scale = (area.width() / image.x).min(area.height() / image.y);
    let size = egui::vec2(image.x * scale, image.y * scale);
    egui::Rect::from_center_size(area.center(), size)
}

/// 在若干页里选出与 `content_rect` **重叠面积最大**的那一页，并返回其交叠矩形。
///
/// 用途：自定义**图片** preview 的截图只保留一页 —— 若框选跨越 a/b 两页，
/// 则取重叠更多的那一页，只截该页内的部分（并记录这块矩形的尺寸）。
/// 面积并列时取**先出现**的页（页面顺序），行为确定。
pub fn largest_overlap_page(
    pages: impl IntoIterator<Item = (usize, Rect)>,
    content_rect: Rect,
) -> Option<(usize, Rect)> {
    let mut best: Option<(usize, Rect, f32)> = None;
    for (index, page_rect) in pages {
        let Some(overlap) = page_rect.intersection(content_rect) else {
            continue;
        };
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            continue;
        }
        let area = overlap.width * overlap.height;
        if best.map_or(true, |(_, _, best_area)| area > best_area) {
            best = Some((index, overlap, area));
        }
    }
    best.map(|(index, overlap, _)| (index, overlap))
}

/// 找出所有与 `content_rect` 相交的页（下标 + 该页内容空间矩形）。
///
/// 截图要**允许跨页**：不能只取"重叠面积最大的那一页"，也不能依赖当前可见页集合
/// （选区可能越过可见集合的边界）。页间距等空白处保持白色。
pub fn pages_intersecting(
    pages: impl IntoIterator<Item = (usize, Rect)>,
    content_rect: Rect,
) -> Vec<(usize, Rect)> {
    pages
        .into_iter()
        .filter(|(_, rect)| {
            rect.intersection(content_rect)
                .is_some_and(|overlap| overlap.width > 0.0 && overlap.height > 0.0)
        })
        .collect()
}

/// 浮窗正文区在"浮窗尺寸"里的边距（左右、上下各 8）。
pub const FLOAT_BODY_PADDING: f32 = 8.0;

/// 由**正文尺寸**换算浮窗整体尺寸（加标题栏与内边距）。
pub fn float_size_for_body(body: egui::Vec2, title_height: f32) -> egui::Vec2 {
    egui::vec2(
        body.x + FLOAT_BODY_PADDING * 2.0,
        body.y + title_height + FLOAT_BODY_PADDING * 2.0,
    )
}

/// 由浮窗整体尺寸反推**正文尺寸**（与 [`float_size_for_body`] 必须互为逆运算，
/// 否则"生成浮窗 → 写回尺寸"的循环会让尺寸逐帧漂移）。
pub fn body_size_of(rect: egui::Rect, title_height: f32) -> egui::Vec2 {
    egui::vec2(
        (rect.width() - FLOAT_BODY_PADDING * 2.0).max(1.0),
        (rect.height() - title_height - FLOAT_BODY_PADDING * 2.0).max(1.0),
    )
}

/// 侧栏条目触发的浮窗位置：**紧贴主视图左缘（侧边栏）**、与该行垂直居中，再夹取回视图内。
///
/// 临时预览与"首次点击生成的固定浮窗"都用它，因此两者位置一致（用户要求）；
/// 这样也替代了原先"从头左上角按级联顺序摆放"的做法。
pub fn float_rect_at_row(
    canvas: egui::Rect,
    row: egui::Rect,
    size: egui::Vec2,
    min_size: egui::Vec2,
) -> egui::Rect {
    if canvas.width() <= 0.0 || canvas.height() <= 0.0 {
        return egui::Rect::from_min_size(egui::pos2(48.0, 48.0), size);
    }
    let wanted = egui::Rect::from_min_size(
        egui::pos2(canvas.min.x + 16.0, row.center().y - size.y * 0.5),
        size,
    );
    clamp_float_rect(wanted, canvas, min_size)
}

/// 侧栏 Pin 列表行首的**类型符号**：文字 / 截图（用户指定的 unicode）。
///
/// 用 Rust 的 `\u{...}` 转义写出，确保码点与用户要求一致：
/// 文字 `U+1F5B9`、截图 `U+1F5BB`。若渲染成方块，说明当前字体缺这两个字形
/// （与码点无关，换个带符号的字体即可）。
pub const CUSTOM_SYMBOL_TEXT: &str = "\u{1F5B9}";
/// 缩放比 → 显示文本（四舍五入到整数百分比），如 `1.532` → `"153%"`。
pub fn zoom_percent_text(zoom: f32) -> String {
    format!("{:.0}%", zoom * 100.0)
}

/// 解析用户输入的百分比 → 缩放比（取整百分比），如 `"153.2"` → `Some(1.53)`。
///
/// 只用整数百分比：输入 `153.2` 与 `153` 等价（用户要求"识别后舍入整数"）。
/// 非正数或无法解析返回 `None`。
pub fn parse_zoom_percent(text: &str) -> Option<f32> {
    let trimmed = text.trim().trim_end_matches('%').trim();
    let value: f32 = trimmed.parse().ok()?;
    let percent = value.round();
    (percent > 0.0).then_some(percent / 100.0)
}

/// 适应高度的缩放比：视口高 ÷ 页高（页面旋转后取实际显示高度）。
pub fn fit_height_zoom(viewport_height: f32, page_height: f32) -> Option<f32> {
    (page_height > 0.0 && viewport_height > 0.0).then_some(viewport_height / page_height)
}

/// 页码输入 → 页下标：合法范围 `1..=page_count`，否则 `None`。
pub fn page_index_from_text(text: &str, page_count: u32) -> Option<PageIndex> {
    let number: u32 = text.trim().parse().ok()?;
    (number >= 1 && number <= page_count).then(|| PageIndex::new(number - 1))
}

/// 菜单里的一行 checkbox 文本：行首 `✓` 或空格（用户要求）。
pub fn checkbox_row(label: &str, checked: bool) -> String {
    if checked {
        format!("✓ {label}")
    } else {
        format!("  {label}")
    }
}

/// 浮窗正文尺寸是否**真的**变了（带容差）。
///
/// 尺寸在"正文 ↔ 浮窗"之间来回换算、还要经过夹取，必然带有亚像素误差；
/// 用 `!=` 比较几乎永远为真 → 会把"仅移动浮窗"也误判成修改（用户实测）。
/// 容差取 0.5px：小于半个像素的变化肉眼不可见，也不值得写盘。
pub fn body_size_changed(stored: egui::Vec2, current: egui::Vec2) -> bool {
    (stored.x - current.x).abs() > 0.5 || (stored.y - current.y).abs() > 0.5
}

/// "保存自定义数据"按钮的文案：存在未保存修改时加 `*` 前缀。
pub fn save_button_label(dirty: bool) -> &'static str {
    if dirty {
        "*保存自定义数据"
    } else {
        "保存自定义数据"
    }
}

/// 编辑按钮符号（U+270E）。
pub const CUSTOM_SYMBOL_EDIT: &str = "\u{270E}";
/// 删除按钮符号（U+274E）。
pub const CUSTOM_SYMBOL_DELETE: &str = "\u{274E}";
/// 截图类型符号，见 [`CUSTOM_SYMBOL_TEXT`]。
pub const CUSTOM_SYMBOL_IMAGE: &str = "\u{1F5BB}";

/// 在"浮窗来源"列表里找**某个自定义 preview 的正式浮窗**（侧栏响应要用）。
///
/// 与 `find_float_for_target` 同构：只有"由侧栏/保存逻辑正式生成"的浮窗才带 `custom` 下标，
/// 测试用的 "(delete)" 副本等为 `None`。
pub fn find_float_for_custom(customs: &[Option<usize>], index: usize) -> Option<usize> {
    customs.iter().position(|custom| *custom == Some(index))
}

/// 生成"**自动换行 + 行数上限 + 溢出省略号**"的文本排版。
///
/// 用于浮窗里的文字：
/// * 单行长文本 → 在浮窗宽度内自动换行（不再横向溢出）；
/// * 行数超过上限（浮窗高度放不下）→ 截断并显示 `…`；
/// * `max_rows = 1` 用于标题栏 → 长标题被截断，不会压到关闭按钮上。
///
/// 注：自动换行产生的行距与显式换行符的行距目前一致（都用同一个字体度量），
/// 用户已说明"现阶段相对即可"，更精细的行距留待以后。
pub fn overflow_layout(
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
    max_rows: usize,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(0.0),
        max_rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    job
}

/// 以某点为**中心**、大小为零的矩形。
///
/// 自定义 preview 需要"与被引用点对应"的位置信息：位置就是 Pin 结束时点击的页内坐标，
/// 同时另存一份以该点为中心、**零尺寸**的矩形（用户要求；以后若要给它截图/延伸，
/// 直接在这块矩形上扩即可，而不必再回头找位置）。
pub fn zero_rect_at(point: Point) -> Rect {
    Rect::new(point.x, point.y, 0.0, 0.0)
}

/// Pin 状态是否应当退出。
///
/// 用户最终要求：**只有当切换到"选字"或"截图"模式时才退出** Pin
/// （平移/选字/截图是三种"预览模式"，Pin 只能存在于平移模式）。
/// 缩放类动作（适应宽度、100%）、翻页、侧栏开关、checkbox 等都**不**影响 Pin。
pub fn pin_exits_on_mode_change(to_select: bool, to_capture: bool) -> bool {
    to_select || to_capture
}

/// 浮窗被拖动后的新矩形：先按位移平移，再夹取回视图区域内（且不小于最小尺寸）。
///
/// 抽成函数是因为"拖动浮窗"有三个入口（标题栏、正文/截图区、临时预览被拖动后固定），
/// 它们必须**用同一套夹取规则**，否则会出现"拖出去回不来""大小被改动"这类分歧。
pub fn drag_float_rect(
    rect: egui::Rect,
    delta: egui::Vec2,
    canvas: egui::Rect,
    min_size: egui::Vec2,
) -> egui::Rect {
    clamp_float_rect(rect.translate(delta), canvas, min_size)
}

/// 把第 `index` 项移到末尾，**其余项相对顺序保持不变**。
///
/// 用途：点击浮窗任意处 → 该浮窗置顶；因为绘制与交互都按顺序进行，
/// "移到末尾"就等于最上层，同时其它浮窗的层次关系不变（用户要求）。
pub fn move_to_last<T>(items: &mut Vec<T>, index: usize) {
    if index < items.len() {
        let item = items.remove(index);
        items.push(item);
    }
}

/// 在"浮窗来源"列表里找**由某个被引用点固定出来的浮窗**。
///
/// 用户要求：如果同一目的地的浮窗已经被固定且仍在显示，再次悬停该链接时
/// **不再弹临时预览**，而是高亮并置顶那个已有浮窗（"重新激发"）。
/// 只有从悬停预览固定出来的浮窗带有来源（`Some(target)`），其余为 `None`。
pub fn find_float_for_target(
    origins: &[Option<(PageIndex, usize)>],
    target: (PageIndex, usize),
) -> Option<usize> {
    origins.iter().position(|origin| *origin == Some(target))
}

/// 悬停预览是否应当出现。
///
/// 两个约束（用户要求）：
/// 1. **滚动/缩放进行中或刚进行过**（静默窗口内）不出现 —— 否则滚动时预览会不断闪现；
/// 2. 指针在**任何浮窗**（永久或临时）之上时不出现 —— 浮窗会屏蔽主视图的动作，
///    被浮窗覆盖的链接不应再有悬停反馈（点击本来就已经被浮窗拦掉了）。
pub fn hover_preview_allowed(scrolling: bool, pointer_over_float: bool) -> bool {
    !scrolling && !pointer_over_float
}

/// 指针是否落在任一矩形内（用于"浮窗是否遮住了指针"）。
pub fn pointer_over_any(pointer: egui::Pos2, rects: &[egui::Rect]) -> bool {
    rects.iter().any(|rect| rect.contains(pointer))
}

/// 临时预览浮窗的位置：**左上角放在链接框中心**（用户要求）。
///
/// 这样浮窗与链接框**必然重叠**，鼠标从链接移向浮窗的路径是连续的，
/// 不会出现"刚离开链接框，预览就消失了"。
/// 仍会夹取到视图区域内；若因此与链接框不再重叠，那是区域边界的极端情形（用户已说明可接受）。
pub fn tip_rect_for(link_rect: egui::Rect, size: egui::Vec2, canvas: egui::Rect) -> egui::Rect {
    clamp_float_rect(
        egui::Rect::from_min_size(link_rect.center(), size),
        canvas,
        egui::Vec2::new(120.0, 80.0),
    )
}

/// 被引用点**初始的矩形**：以该点为中心的一小块区域（PDF 页面空间）。
///
/// 初始化时只为"以后可能被截取"预留一个可编辑的框；**不会**因此触发任何渲染 ——
/// 只有用户对该标识发起动作（点击 → 截图模式 → Enter）后才会产生截图。
pub fn default_dest_rect(point: Point) -> Rect {
    const HALF_WIDTH: f32 = 60.0;
    const HALF_HEIGHT: f32 = 30.0;
    Rect::new(
        point.x - HALF_WIDTH,
        point.y - HALF_HEIGHT,
        HALF_WIDTH * 2.0,
        HALF_HEIGHT * 2.0,
    )
}

/// PDF 页面空间矩形 → **屏幕**矩形（把被引用点已存的矩形回填成截图模式的选区初值）。
pub fn page_rect_to_screen(
    placement: &PagePlacement,
    page_rect: Rect,
    scroll: Vec2,
    canvas_min: egui::Pos2,
) -> egui::Rect {
    let content = placement.rect_to_content(page_rect);
    egui::Rect::from_min_size(
        egui::pos2(
            content.x - scroll.x + canvas_min.x,
            content.y - scroll.y + canvas_min.y,
        ),
        egui::vec2(content.width, content.height),
    )
}

/// 画布上的**屏幕矩形** → **内容空间**矩形。
///
/// ## 为什么必须单独有这个函数
/// `Response::interact_pointer_pos()`（以及 `Response::rect`）给的是**窗口绝对（屏幕）坐标**，
/// 而内容空间的原点是**画布左上角**。截图时若直接拿屏幕坐标加 `scroll`，
/// 结果会整体偏移 `canvas.min`（有侧栏时约 265×40 px）——
/// 高倍缩放下这点偏移足以落到**视野之外**，表现就是"截到的图像与框选位置不符、
/// 而且显示的是屏幕外区域"（用户实测反馈）。
pub fn screen_rect_to_content(canvas_min: egui::Pos2, rect: egui::Rect, scroll: Vec2) -> Rect {
    Rect::new(
        rect.min.x - canvas_min.x + scroll.x,
        rect.min.y - canvas_min.y + scroll.y,
        rect.width(),
        rect.height(),
    )
}

/// 截图模式的遮罩分块：返回 4 块矩形，把 `hole` 挖空（区域内保持透明）。
///
/// egui 没有"擦除"混合，因此"半透明蒙层 + 中间透明"的常规做法就是
/// **绕开选区画四块**（上/下/左/右）。`hole` 为空时返回整块区域。
pub fn veil_bands(area: egui::Rect, hole: Option<egui::Rect>) -> Vec<egui::Rect> {
    let Some(hole) = hole else { return vec![area] };
    let hole = hole.intersect(area);
    if hole.width() <= 0.0 || hole.height() <= 0.0 {
        return vec![area];
    }
    let mut bands = Vec::with_capacity(4);
    // 上
    if hole.min.y > area.min.y {
        bands.push(egui::Rect::from_min_max(
            area.min,
            egui::pos2(area.max.x, hole.min.y),
        ));
    }
    // 下
    if hole.max.y < area.max.y {
        bands.push(egui::Rect::from_min_max(
            egui::pos2(area.min.x, hole.max.y),
            area.max,
        ));
    }
    // 左
    if hole.min.x > area.min.x {
        bands.push(egui::Rect::from_min_max(
            egui::pos2(area.min.x, hole.min.y),
            egui::pos2(hole.min.x, hole.max.y),
        ));
    }
    // 右
    if hole.max.x < area.max.x {
        bands.push(egui::Rect::from_min_max(
            egui::pos2(hole.max.x, hole.min.y),
            egui::pos2(area.max.x, hole.max.y),
        ));
    }
    bands
}

/// 视图空间矩形 → 某页的 **PDF 页面空间**矩形（截图功能输出区域信息用）。
///
/// 与该页无交集时返回 `None`；交集会先按页面矩形裁剪，再换算到页面空间并归一化。
pub fn view_rect_to_page_rect(
    placement: &PagePlacement,
    view_rect: Rect,
    scroll: Vec2,
) -> Option<Rect> {
    let content = view_rect.translate(scroll);
    let overlap = content.intersection(placement.rect)?;
    if overlap.width <= 0.0 || overlap.height <= 0.0 {
        return None;
    }
    let a = placement.content_to_page(Point::new(overlap.x, overlap.y));
    let b = placement.content_to_page(Point::new(overlap.right(), overlap.bottom()));
    Some(Rect::new(
        a.x.min(b.x),
        a.y.min(b.y),
        (b.x - a.x).abs(),
        (b.y - a.y).abs(),
    ))
}

/// 浮窗的最小尺寸（egui 单位，因为浮窗是纯 UI 元素）。
pub const FLOAT_MIN_SIZE: egui::Vec2 = egui::Vec2::new(160.0, 90.0);
/// 浮窗标题栏高度。
pub const FLOAT_TITLE_HEIGHT: f32 = 22.0;
/// 浮窗右下角缩放把手边长。
pub const FLOAT_HANDLE_SIZE: f32 = 16.0;

/// 把浮窗矩形夹取到**视图区域内部**（用户要求：浮窗始终在视图区域内），并保证不小于最小尺寸。
///
/// 区域本身比最小尺寸还小时以区域为准（不会算出负尺寸）。
pub fn clamp_float_rect(rect: egui::Rect, area: egui::Rect, min_size: egui::Vec2) -> egui::Rect {
    let max_width = area.width().max(0.0);
    let max_height = area.height().max(0.0);
    let width = rect.width().max(min_size.x.min(max_width)).min(max_width);
    let height = rect
        .height()
        .max(min_size.y.min(max_height))
        .min(max_height);
    let x = rect
        .min
        .x
        .clamp(area.min.x, (area.max.x - width).max(area.min.x));
    let y = rect
        .min
        .y
        .clamp(area.min.y, (area.max.y - height).max(area.min.y));
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, height))
}

/// 被引用点的去重网格：0.5pt（远小于一个字，肉眼视为同一点）。
pub const DEST_QUANTUM: f32 = 0.5;

/// 把位置量化到去重网格。**建表与查询必须用同一个函数**，
/// 否则"链接 → 被引用点"会查不到（两处各写一份量化就迟早会走偏）。
pub fn quantize_dest_point(point: Point) -> Point {
    Point::new(
        (point.x / DEST_QUANTUM).round() * DEST_QUANTUM,
        (point.y / DEST_QUANTUM).round() * DEST_QUANTUM,
    )
}

/// 在一页的已量化位置里查找某个链接目标对应的下标（悬停预览要用它找到被引用点）。
pub fn find_dest_index(points: &[Point], target: Point) -> Option<usize> {
    let target = quantize_dest_point(target);
    points.iter().position(|point| *point == target)
}

/// 收集**被引用的点**（链接跳转目的地），按目标页分组，并按位置去重。
///
/// * 一条链接只在自己带**具体坐标**（`Destination::Page.offset`）时算一个引用点；
///   只有页、没有坐标的目标不产生标记（没有具体位置可标）；
/// * 大量链接可能指向同一个点，因此按 0.5pt 网格量化后去重（用户要求）；
/// * 返回 `Vec` 的下标就是目标页号，越界的目标丢弃。
pub fn destination_points(links_by_page: &[Vec<Link>], page_count: usize) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = vec![Vec::new(); page_count];
    for links in links_by_page {
        for link in links {
            let Destination::Page {
                page,
                offset: Some(point),
            } = &link.destination
            else {
                continue;
            };
            let Some(bucket) = out.get_mut(page.get() as usize) else {
                continue;
            };
            let quantized = quantize_dest_point(*point);
            if !bucket.contains(&quantized) {
                bucket.push(quantized);
            }
        }
    }
    out
}

/// 滚轮是否应作用于**文档视图**（而不是侧栏、工具栏等区域）。
///
/// 实测缺陷：在侧栏里滚动时文档也跟着滚 —— egui 的 `ScrollArea` 会消费滚动增量，
/// 但 app 原先在**面板绘制之前**读取 `smooth_scroll_delta`，读到的仍是未消费的值。
/// 修法有两层：读取时机移到面板之后 + 用本函数做指针位置的显式判据。
pub fn wheel_targets_canvas(pointer_over_canvas: bool) -> bool {
    pointer_over_canvas
}

/// 把位图**一遍**转成 egui 的 `ColorImage`。
///
/// 为什么不用 `RasterImage::to_rgba8()` + `ColorImage::from_rgba_unmultiplied()`：
/// 那是**两遍**逐像素处理 + 两次整块分配（实测"新页进入"那一帧 ~150ms）。
/// 这里一遍完成，且避免每像素 `extend_from_slice` 的调用开销。
/// PDFium 的 BGRA 是**预乘 alpha**，故用 `from_rgba_premultiplied`
/// （页面区域 alpha 恒为 255，两者等价）。
pub fn color_image(format: PixelFormat, data: &[u8], width: u32, height: u32) -> egui::ColorImage {
    let (w, h) = (width as usize, height as usize);
    let mut pixels = Vec::with_capacity(w * h);
    match format {
        PixelFormat::Bgra8 => {
            for px in data.chunks_exact(4).take(w * h) {
                pixels.push(egui::Color32::from_rgba_premultiplied(
                    px[2], px[1], px[0], px[3],
                ));
            }
        }
        PixelFormat::Rgba8 => {
            for px in data.chunks_exact(4).take(w * h) {
                pixels.push(egui::Color32::from_rgba_premultiplied(
                    px[0], px[1], px[2], px[3],
                ));
            }
        }
        PixelFormat::Gray8 => {
            for &gray in data.iter().take(w * h) {
                pixels.push(egui::Color32::from_gray(gray));
            }
        }
    }
    egui::ColorImage {
        size: [w, h],
        source_size: egui::vec2(w as f32, h as f32),
        pixels,
    }
}

/// 扁平行：`(缩进层级, 标题, 跳转目标)`。
///
/// 侧栏用虚拟化行渲染（只布局可见的那十几行），因此需要先把树**一次性**摊平 ——
/// 实测 1292 个节点时，每帧全量布局要稳定花掉 ~42ms（与缩放无关）。
pub type OutlineRow = (usize, String, Option<Destination>);

/// 把大纲树摊平成行列表（深度优先，父在前、子紧随，层级从 0 起）。
pub fn flatten_outline(nodes: &[OutlineNode]) -> Vec<OutlineRow> {
    fn walk(nodes: &[OutlineNode], depth: usize, out: &mut Vec<OutlineRow>) {
        for node in nodes {
            out.push((depth, node.title.clone(), node.destination.clone()));
            walk(&node.children, depth + 1, out);
        }
    }
    let mut rows = Vec::new();
    walk(nodes, 0, &mut rows);
    rows
}

/// 大纲节点总数（含子节点）。诊断用：侧栏若一次布局上百行，会稳定吃掉几十毫秒/帧。
pub fn outline_node_count(nodes: &[OutlineNode]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + outline_node_count(&node.children))
        .sum()
}

/// 需要**上传并保留**纹理的页集合：可见页 + 前后各一页（当前页必含，越界自动裁剪）。
///
/// ## 为什么必须是"单一来源"
/// 上传（预热相邻页）与裁剪必须用**同一个集合**。曾经上传用"可见页 ±1"、
/// 裁剪只用"可见页"，于是相邻页的纹理**上传完立刻被裁掉**，下一帧又上传 ——
/// 实测 200% 缩放下每帧有 **302ms** 全花在这个抖动上（800% 走分块、没有整页图可传，
/// 所以只有 0.2ms，这也是"中间缩放比例最卡"的原因）。
pub fn texture_pages(visible: &[PageIndex], current: PageIndex, page_count: u32) -> Vec<PageIndex> {
    let mut pages: Vec<PageIndex> = Vec::with_capacity(visible.len() + 3);
    for page in visible {
        if page.get() > 0 && !pages.contains(&PageIndex::new(page.get() - 1)) {
            pages.push(PageIndex::new(page.get() - 1));
        }
        if !pages.contains(page) {
            pages.push(*page);
        }
        let next = page.get() + 1;
        if next < page_count && !pages.contains(&PageIndex::new(next)) {
            pages.push(PageIndex::new(next));
        }
    }
    if !pages.contains(&current) && current.get() < page_count {
        pages.push(current);
    }
    pages.retain(|page| page.get() < page_count);
    pages.sort();
    pages.dedup();
    pages
}

/// 一个带容量上限的键值存储（用于 `RenderKey` → 纹理）。
///
/// 与 LRU 位图缓存的区别：这里存的是 GPU 纹理句柄（`egui::TextureHandle`），
/// 丢失后可以按需从位图缓存重新上传，因此策略是"容量 + 按可见性裁剪"，
/// 而不是精确的字节计量。
/// 一页在当前视图下的"形状"（决定 A 类要哪些键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageShape {
    /// 不分块：整页一张（在 `render_milli` 上）。
    Whole,
    /// 分块且可见：这些块。
    Tiles(u32, u32, u32, u32),
    /// 分块但不可见（预取页）：A 类为空。
    TilesHidden,
}

/// 保留策略需要的"事实"（app 从会话里取，view 不依赖会话）。
#[derive(Debug, Clone, PartialEq)]
pub struct PageFacts {
    /// 页号。
    pub page: PageIndex,
    /// 显示用的旋转（按页不同）。
    pub rotation: Rotation,
    /// 当前**波段渲染比例**（`scale_milli`）。
    pub render_milli: u32,
    /// 这一页当前的形状。
    pub shape: PageShape,
    /// 这一页"本来就可以是整页图"的比例（升序，含开场底料，全部 ≤ `render_milli`）。
    pub whole_pages: Vec<u32>,
}

/// 一个键在纹理层里的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureClass {
    /// A 必须：当前波段的可见内容。
    Must,
    /// B 底料：整页图（含开场底料）。
    Bed,
    /// C 旧内容：最近一个旧波段（缩放过场用）。
    Old,
}

/// 一次 `desired` 计算的结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Desired {
    /// 这一帧 GPU 里**应当存在**的键。
    pub keep: HashSet<RenderKey>,
    /// 其中"现在没有、且值得上传"的键（只有 A/B 会出现在这里；C 从不上传）。
    pub upload: Vec<RenderKey>,
    /// A 类字节数。
    pub must_bytes: u64,
    /// B 类字节数。
    pub bed_bytes: u64,
    /// C 类字节数。
    pub old_bytes: u64,
    /// 被预算裁掉的字节数。
    pub dropped_bytes: u64,
    /// A 类自身就超过预算（软上限：不裁 A，只报告）。
    pub over_budget: bool,
}

impl Desired {
    /// 总保留字节数。
    pub fn keep_bytes(&self) -> u64 {
        self.must_bytes + self.bed_bytes + self.old_bytes
    }
}

/// **唯一**决定"这一帧 GPU 里应当有哪些纹理"的函数（D-112）。
///
/// 它是**纯函数**：只依赖视图事实、纹理层现有的键、预算和"每个键多大"。
/// 三个类：
/// * A 必须 —— 当前波段的可见内容（不分块时整页图；分块时可见块的键）；
/// * B 底料 —— `whole_pages` 列出的整页图（含开场底料）；
/// * C 旧内容 —— 对每页取"现有键里与当前比例最近的那一档"（只留不上传）。
///
/// 预算语义：A 永不淘汰（软上限，`over_budget` 报告）；其余按 **B 优先、C 其后**，
/// 类内按 `existing` 的顺序（**最新在前**）保留，放不下的整批丢弃并计入 `dropped_bytes`。
pub fn desired_textures(
    pages: &[PageFacts],
    existing: &[RenderKey],
    budget: u64,
    size_of: impl Fn(&RenderKey) -> u64,
) -> Desired {
    let mut desired = Desired::default();
    let present: HashSet<RenderKey> = existing.iter().copied().collect();

    // ---- A：当前波段的可见内容 ----
    let mut must: Vec<RenderKey> = Vec::new();
    for fact in pages {
        match fact.shape {
            PageShape::Whole => must.push(RenderKey {
                page: fact.page,
                scale_milli: fact.render_milli,
                rotation: fact.rotation,
                tile: None,
            }),
            PageShape::Tiles(col0, row0, col1, row1) => {
                for row in row0..=row1 {
                    for col in col0..=col1 {
                        must.push(RenderKey {
                            page: fact.page,
                            scale_milli: fact.render_milli,
                            rotation: fact.rotation,
                            tile: Some(TileSpec::new(col, row)),
                        });
                    }
                }
            }
            PageShape::TilesHidden => {}
        }
    }

    // ---- B：整页底料（含开场底料）----
    let mut bed: Vec<RenderKey> = Vec::new();
    for fact in pages {
        for milli in &fact.whole_pages {
            if *milli > fact.render_milli {
                continue;
            }
            bed.push(RenderKey {
                page: fact.page,
                scale_milli: *milli,
                rotation: fact.rotation,
                tile: None,
            });
        }
    }
    bed.sort_by_key(|key| (key.page.get(), key.scale_milli));
    bed.dedup();

    // ---- C：每页"最近一个旧波段"里现有的键（只留，不上传）----
    let mut old: Vec<RenderKey> = Vec::new();
    for fact in pages {
        let mut nearest: Option<u32> = None;
        for key in existing {
            if key.page != fact.page
                || key.rotation != fact.rotation
                || key.scale_milli == fact.render_milli
            {
                continue;
            }
            let distance = key.scale_milli.abs_diff(fact.render_milli);
            if nearest.map_or(true, |current| {
                distance < current.abs_diff(fact.render_milli)
            }) {
                nearest = Some(key.scale_milli);
            }
        }
        if let Some(scale) = nearest {
            for key in existing {
                if key.page == fact.page
                    && key.rotation == fact.rotation
                    && key.scale_milli == scale
                {
                    old.push(*key);
                }
            }
        }
    }

    // ---- 组装 keep 与"该上传的" ----
    let add = |key: RenderKey, class: TextureClass, desired: &mut Desired| {
        if desired.keep.insert(key) {
            let bytes = size_of(&key);
            match class {
                // A 是"让画面变清楚"的那一类：缺了必须传，否则永远停在糊的预览上
                TextureClass::Must => {
                    desired.must_bytes += bytes;
                    if !present.contains(&key) {
                        desired.upload.push(key);
                    }
                }
                TextureClass::Bed => {
                    desired.bed_bytes += bytes;
                    if !present.contains(&key) {
                        desired.upload.push(key);
                    }
                }
                // C 只保留、永不上传（它只是缩放过场的顶替，缺了不影响正确性）
                TextureClass::Old => desired.old_bytes += bytes,
            }
        }
    };
    for key in &must {
        add(*key, TextureClass::Must, &mut desired);
    }
    for key in &bed {
        add(*key, TextureClass::Bed, &mut desired);
    }
    let old_keys = old;
    for key in &old_keys {
        add(*key, TextureClass::Old, &mut desired);
    }

    // ---- 预算 ----
    if budget == 0 {
        return desired;
    }
    if desired.must_bytes >= budget {
        // 软上限：A 本身放不下 —— 只留 A，B/C 全让位，只报告
        desired.over_budget = true;
        let before = desired.keep_bytes();
        desired.keep.retain(|key| must.contains(key));
        desired.bed_bytes = 0;
        desired.old_bytes = 0;
        // 只允许上传 A：否则连"当前视口"都传不上去，画面永远是白的
        desired.upload.retain(|key| must.contains(key));
        desired.dropped_bytes = before.saturating_sub(desired.must_bytes);
        return desired;
    }
    let mut remaining = budget - desired.must_bytes;
    // B 优先：现有纹理（"最新在前"）先保，其次是还没上传的底料（也要给它留额度）
    let mut kept_bed: HashSet<RenderKey> = HashSet::new();
    for key in existing.iter().chain(bed.iter()) {
        if !bed.contains(key) {
            continue;
        }
        let bytes = size_of(key);
        if bytes <= remaining {
            remaining -= bytes;
            kept_bed.insert(*key);
        }
    }
    let mut kept_old: HashSet<RenderKey> = HashSet::new();
    for key in existing {
        if !old_keys.contains(key) {
            continue;
        }
        let bytes = size_of(key);
        if bytes <= remaining {
            remaining -= bytes;
            kept_old.insert(*key);
        }
    }
    let before = desired.keep_bytes();
    desired
        .keep
        .retain(|key| must.contains(key) || kept_bed.contains(key) || kept_old.contains(key));
    desired.bed_bytes = bed
        .iter()
        .filter(|key| desired.keep.contains(*key))
        .map(&size_of)
        .sum();
    desired.old_bytes = old_keys
        .iter()
        .filter(|key| desired.keep.contains(*key))
        .map(size_of)
        .sum();
    desired.upload.retain(|key| desired.keep.contains(key));
    desired.dropped_bytes = before.saturating_sub(desired.keep_bytes());
    desired
}

/// 按 [`RenderKey`] 索引的纹理存储：LRU 顺序 + 字节账 + 按类保留（D-111）。
pub struct KeyedStore<V> {
    entries: HashMap<RenderKey, V>,
    order: VecDeque<RenderKey>,
    capacity: usize,
    /// 当前占用的字节数（由构造时给的 `bytes_of` 算出）。诊断与预算用（D-109）。
    bytes: u64,
    /// 单个值占多少字节（测试替身恒为 0）。
    bytes_of: fn(&V) -> u64,
    /// 字节预算（0 = 不限制）。见 [`KeyedStore::retain_for_view`]。
    max_bytes: u64,
}

/// 一张 `egui` 纹理占多少字节（`w×h×4`）。
///
/// 有了字节账才能把"128 条目"这种 A4 时代的经验值换成真正有意义的上限：
/// 条目数相同的两张纹理，面积可能差几十倍（D-109）。
pub fn texture_bytes(texture: &egui::TextureHandle) -> u64 {
    let [width, height] = texture.size();
    (width as u64) * (height as u64) * 4
}

impl<V> KeyedStore<V> {
    /// 构造（容量至少为 1）。
    pub fn new(capacity: usize) -> Self {
        Self::with_bytes(capacity, |_| 0)
    }

    /// 同 [`KeyedStore::new`]，但知道每个值占多少字节（纹理层用 [`texture_bytes`]）。
    pub fn with_bytes(capacity: usize, bytes_of: fn(&V) -> u64) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
            bytes: 0,
            bytes_of,
            max_bytes: 0,
        }
    }

    /// 设置字节预算（0 = 不限制）。超过时按"必须 → 底料 → 旧内容"依次让位。
    pub fn set_max_bytes(&mut self, bytes: u64) {
        self.max_bytes = bytes;
    }

    /// 当前预算（0 = 不限制）。
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// 再放一张 `bytes` 会不会超出预算（0 = 不限制，永远可以）。
    ///
    /// 只给"可再生的纹理"（底料、旧内容）用：它们必须在**上传前**就问一句，
    /// 否则会出现"传上去 → 被预算裁掉 → 下一帧再传"的循环（D-111 实测过一次 350MB）。
    /// A 类（当前视口）不受此限，因为保留策略不会裁它。
    pub fn can_admit(&self, bytes: u64) -> bool {
        self.max_bytes == 0 || self.bytes.saturating_add(bytes) <= self.max_bytes
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 只读遍历（回放/断言用：把 `TextureId` 反查成 `RenderKey`）。
    pub fn iter(&self) -> impl Iterator<Item = (&RenderKey, &V)> {
        self.entries.iter()
    }

    /// 是否存在。
    pub fn contains(&self, key: &RenderKey) -> bool {
        self.entries.contains_key(key)
    }

    /// 查询。
    pub fn get(&self, key: &RenderKey) -> Option<&V> {
        self.entries.get(key)
    }

    /// 当前占用的字节数。
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// 平均每条的字节数（诊断用）。
    pub fn average_bytes(&self) -> u64 {
        if self.entries.is_empty() {
            0
        } else {
            self.bytes / self.entries.len() as u64
        }
    }

    /// 丢弃所有不满足 `keep` 的条目（用于"只保留可见页的纹理"），并重算字节账。
    pub fn retain(&mut self, mut keep: impl FnMut(&RenderKey) -> bool) {
        self.entries.retain(|key, _| keep(key));
        self.order.retain(|key| self.entries.contains_key(key));
        self.refresh_bytes();
    }

    /// 按当前实际持有的值**重算**字节账。
    ///
    /// 为什么是"重算"而不是"逐个扣减"：摘除路径有好几条（`retain` / LRU 淘汰 / 替换），
    /// 任何一条忘了扣减都会让账目静默漂移；而条目数最多几十个，重算是 O(n) 的常数活。
    pub fn refresh_bytes(&mut self) {
        let bytes_of = self.bytes_of;
        self.bytes = self.entries.values().map(bytes_of).sum();
    }

    /// 写入并维护 LRU 顺序；超出容量时淘汰最旧的。
    pub fn insert(&mut self, key: RenderKey, value: V) {
        let bytes_of = self.bytes_of;
        let cost = bytes_of(&value);
        if let Some(previous) = self.entries.insert(key, value) {
            self.bytes = self.bytes.saturating_sub(bytes_of(&previous));
        }
        self.bytes += cost;
        self.touch(key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(value) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(bytes_of(&value));
            }
        }
    }

    /// 同 [`Self::best_page_texture`]，但返回**键**（调用方要取它的 `scale_milli`）。
    pub fn best_page_texture_key(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Option<RenderKey> {
        self.entries
            .iter()
            .filter(|(key, _)| key.page == page && key.rotation == rotation && key.tile.is_none())
            .min_by_key(|(key, _)| key.scale_milli.abs_diff(target_scale_milli))
            .map(|(key, _)| *key)
    }

    /// 指定波段的**整页**纹理（精确匹配；`scale_milli` 来自 [`Preview::Whole`]）。
    pub fn page_texture_at(
        &self,
        page: PageIndex,
        rotation: Rotation,
        scale_milli: u32,
    ) -> Option<&V> {
        self.entries.get(&RenderKey {
            page,
            scale_milli,
            rotation,
            tile: None,
        })
    }

    /// 找同一页、同一旋转下**缩放最接近**的整页纹理。
    ///
    /// 用途：缩放过程中新比例的位图还没渲染好时，用旧比例的图**拉伸显示**，
    /// 避免出现大片白色填充（第二轮人工验收反馈）。真正的"最近邻"由 `scale_milli` 决定；
    /// 瓦片（`tile.is_some()`）不参与匹配 —— 它们的几何含义与整页位图不同。
    pub fn best_page_texture(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Option<&V> {
        self.entries
            .iter()
            .filter(|(key, _)| key.page == page && key.rotation == rotation && key.tile.is_none())
            .min_by_key(|(key, _)| key.scale_milli.abs_diff(target_scale_milli))
            .map(|(_, value)| value)
    }

    /// 该键对应的页/缩放/旋转下是否已有**任意一块瓦片**就绪。
    ///
    /// 用途：分块渲染时判断"能不能立刻画真内容"。只要能画，就不该用上一档缩放的旧图去顶替，
    /// 否则瓦片虽已就绪却永远被旧图盖住。
    pub fn has_tiles(&self, like: &RenderKey) -> bool {
        self.entries.keys().any(|key| {
            key.page == like.page
                && key.scale_milli == like.scale_milli
                && key.rotation == like.rotation
                && key.tile.is_some()
        })
    }

    /// 该页/旋转下**最近的一个有瓦片的缩放档**（`scale_milli`），排除 `exclude` 自身。
    ///
    /// 用途：缩放过场时用上一档的瓦片拼出预览（高倍缩放下缓存里没有整页图）。
    pub fn best_tile_scale(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Option<u32> {
        let mut best: Option<u32> = None;
        for key in self.entries.keys() {
            if key.page != page || key.rotation != rotation || key.tile.is_none() {
                continue;
            }
            if key.scale_milli == target_scale_milli {
                continue;
            }
            let distance = key.scale_milli.abs_diff(target_scale_milli);
            if best.map_or(true, |current| {
                distance < current.abs_diff(target_scale_milli)
            }) {
                best = Some(key.scale_milli);
            }
        }
        best
    }

    /// 可见范围内"已在纹理层"的块数 / 有效块数。
    ///
    /// 判定与绘制循环**完全一致**：只有 [`TileGrid::contains`] 为真的块才计入总数，
    /// 键也由同一个 `render` 比例生成。两侧一旦不一致，就会出现
    /// "永远 `ready < total`" → 页面永远停在预览（糊）。
    pub fn ready_tiles_in_view(
        &self,
        page: PageIndex,
        rotation: Rotation,
        render: Scale,
        grid: &TileGrid,
        range: (u32, u32, u32, u32),
    ) -> (usize, usize) {
        let (col0, row0, col1, row1) = range;
        let mut ready = 0usize;
        let mut total = 0usize;
        for row in row0..=row1 {
            for col in col0..=col1 {
                if !grid.contains(col, row) {
                    continue;
                }
                total += 1;
                let key = RenderKey::tiled(page, render, rotation, TileSpec::new(col, row));
                if self.contains(&key) {
                    ready += 1;
                }
            }
        }
        (ready, total)
    }

    /// 某个缩放档下该页已就绪的全部瓦片。
    pub fn tiles_at(
        &self,
        page: PageIndex,
        rotation: Rotation,
        scale_milli: u32,
    ) -> Vec<(TileSpec, &V)> {
        self.entries
            .iter()
            .filter(|(key, _)| {
                key.page == page
                    && key.rotation == rotation
                    && key.scale_milli == scale_milli
                    && key.tile.is_some()
            })
            .filter_map(|(key, value)| key.tile.map(|tile| (tile, value)))
            .collect()
    }

    /// 波段切换时可用的"旧内容"（整页级）。
    ///
    /// 取舍规则（见 `docs/current/rendering.md` §8 与 D-98/D-99）：
    /// * 只有一种可用 → 用它；
    /// * 两种都有：
    ///   - **缩小方向**（块来自更高波段、整页图来自更低波段）→ 用**整页图**。
    ///     缩小时视口覆盖的页面面积变大，旧块只覆盖"缩之前那一片"
    ///     （800% → 500% 时约只剩 39% 面积），整页图虽糊但不会露白；
    ///   - 其它（放大方向，或两者同向）→ 取**更接近目标**的那个（更清晰）。
    pub fn preview_source(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Preview {
        let whole = self
            .best_page_texture_key(page, rotation, target_scale_milli)
            .map(|key| key.scale_milli);
        let tiles = self.best_tile_scale(page, rotation, target_scale_milli);
        match (whole, tiles) {
            (None, None) => Preview::None,
            (Some(scale), None) => Preview::Whole(scale),
            (None, Some(scale)) => Preview::Tiles(scale),
            (Some(whole_scale), Some(tile_scale)) => {
                let zooming_out =
                    tile_scale > target_scale_milli && whole_scale < target_scale_milli;
                let whole_is_nearer = whole_scale.abs_diff(target_scale_milli)
                    <= tile_scale.abs_diff(target_scale_milli);
                if zooming_out || whole_is_nearer {
                    Preview::Whole(whole_scale)
                } else {
                    Preview::Tiles(tile_scale)
                }
            }
        }
    }

    /// 给"缺块区域"打底的旧内容：**覆盖优先**。
    ///
    /// 与 [`Self::preview_source`] 的区别在取舍方向：
    /// * 整屏预览要"好看"，所以放大方向优先更接近目标的（更清晰）；
    /// * 打底要"盖住"，所以只要有整页图就用整页图 —— 它是**一张连续图像**铺满整页，
    ///   缺块的位置都能从它上面取到对应区域的像素，不会露白。
    ///
    /// 这不等同于 D-87 的"逐块拉伸旧瓦片"：底料是一张整页图（单一比例、单一映射），
    /// 叠在上面的当前波段块也由同一套共享边界摆放，二者都对齐到同一个页面矩形，
    /// 因此接缝处只会有"清晰度差"，不会出现 D-88 那类**几何错位**。
    pub fn underlay_source(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Preview {
        if let Some(key) = self.best_page_texture_key(page, rotation, target_scale_milli) {
            return Preview::Whole(key.scale_milli);
        }
        match self.best_tile_scale(page, rotation, target_scale_milli) {
            Some(scale) => Preview::Tiles(scale),
            None => Preview::None,
        }
    }

    /// 裁剪纹理：保留每页的
    /// ① 当前波段；② **最接近它的另一个波段**（放大方向的预览底料）；
    /// ③ 当前比例**以下、最近的"有整页纹理"的波段**（缩小方向的整页底料）。
    ///
    /// 两个方向都要顾：
    /// * **不能全丢**：缩放过场要用上一档的旧内容顶上，否则会闪白 ——
    ///   v1 在 `LayoutChanged` 时只留"当前页"的纹理，于是中心页正常、相邻页闪白
    ///   （第四轮验收反馈的现象）；
    /// * **不能全留**：一次连续缩放会产生十几档比例，显存会无限增长。
    ///
    /// 旋转也必须匹配：旧旋转的图是横躺的，留着只会画错。
    /// 现有键，**最新在前**（预算裁剪时先保新的）。图层的"删除步"用它做输入之一。
    pub fn keys_newest_first(&self) -> Vec<RenderKey> {
        self.order.iter().rev().copied().collect()
    }

    /// 只保留 `keep` 里的键（**对账的删除步**，D-112）。
    pub fn keep_only(&mut self, keep: &HashSet<RenderKey>) {
        self.retain(|key| keep.contains(key));
    }

    /// 按 [`desired_textures`] 的结果裁剪纹理层（**唯一的裁剪入口**，D-112）。
    ///
    /// `size_of` 由调用方按几何给出（整页 = 渲染像素 × 4，块 = 块的像素 × 4），
    /// 因此预算决策不依赖"纹理现在有没有"，不会因为上传/删除而反复。
    pub fn retain_for_view(
        &mut self,
        pages: &[PageFacts],
        size_of: impl Fn(&RenderKey) -> u64,
    ) -> Desired {
        // LRU 顺序里"最新在前"：预算裁剪时先保新的
        let existing: Vec<RenderKey> = self.order.iter().rev().copied().collect();
        let desired = desired_textures(pages, &existing, self.max_bytes, &size_of);
        self.retain(|key| desired.keep.contains(key));
        self.refresh_bytes();
        desired
    }

    /// 清空（字节账归零）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    fn touch(&mut self, key: RenderKey) {
        if let Some(position) = self.order.iter().position(|existing| *existing == key) {
            self.order.remove(position);
        }
        self.order.push_back(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement() -> PagePlacement {
        PagePlacement::new(
            PageIndex::new(0),
            Rect::new(12.0, 12.0, 612.0, 792.0),
            Scale::IDENTITY,
            Rotation::None,
            Size::new(612.0, 792.0),
        )
    }

    fn key(page: u32) -> RenderKey {
        RenderKey::new(PageIndex::new(page), Scale::IDENTITY, Rotation::None)
    }

    #[test]
    fn view_rect_subtracts_scroll() {
        let rect = view_rect_for(&placement(), Vec2::new(100.0, 250.0));
        assert_eq!(rect, Rect::new(-88.0, -238.0, 612.0, 792.0));
    }

    /// 回归测试：块的位置是 `页.x + 块像素·u`，**不是**"除以显示比例"（D-17/D-94）。
    ///
    /// 数字取自第三轮验收的真实诊断行（748% 下的第 2 页）：
    /// `视图矩形=(-1679,-2042 3870×4785)`，网格里的第 (1,1) 块。
    /// * `u == 1`（渲染比例 = 显示比例）时退化成"像素偏移直接相加"；
    /// * `u < 1`（波段超采样）时块按 `u` 缩小 —— 但绝不会被压到 1/显示比例 那么小。
    #[test]
    fn tile_view_rect_uses_the_display_to_render_ratio() {
        let page_size = Size::new(612.0, 792.0);
        let display = 7.48f32;
        let page_rect = Rect::new(-1679.0, -2042.0, 3870.0, 4785.0);
        let placement = PagePlacement::new(
            PageIndex::new(1),
            page_rect,
            Scale::from_zoom(display),
            Rotation::None,
            page_size,
        );

        // u == 1：与"像素偏移直接相加"完全一致（D-17 的旧行为）
        let grid = TileGrid::new(page_size, display);
        let (x, y, w, _) = grid.tile_px(1, 1);
        let rect = tile_view_rect(&grid, &placement, 1, 1, display, Vec2::ZERO);
        assert_eq!(
            (rect.x, rect.y, rect.width),
            (page_rect.x + x as f32, page_rect.y + y as f32, w as f32)
        );
        assert_eq!(rect.x, -1679.0 + x as f32, "块位置 = 页原点 + 块像素偏移");

        // u < 1（渲染比例更高）：块按 u 缩小，且仍在页面范围内
        let render = display * 1.6;
        let grid = TileGrid::new(page_size, render);
        let rect = tile_view_rect(&grid, &placement, 1, 1, render, Vec2::ZERO);
        let u = display / render;
        let (x, _, w, _) = grid.tile_px(1, 1);
        assert!((rect.x - (page_rect.x + x as f32 * u)).abs() < 1e-2);
        assert!((rect.width - w as f32 * u).abs() < 1e-2);
        assert!(rect.right() <= page_rect.right() + 1.0, "块不得超出页面");

        // 反面：除以**显示比例**的实现会把块压到页面左上角（第三轮全黑）
        let wrong = Rect::new(
            page_rect.x + x as f32 / display,
            page_rect.y + y as f32 / display,
            0.0,
            0.0,
        );
        assert!(
            rect.x > wrong.x + 100.0,
            "块位置一旦除以显示比例就会被压到左上角"
        );
    }

    #[test]
    fn view_and_content_coordinates_round_trip() {
        let scroll = Vec2::new(33.0, 77.0);
        let content = Point::new(120.0, 340.0);
        let view = Point::new(content.x - scroll.x, content.y - scroll.y);
        assert_eq!(view_to_content(view, scroll), content);
    }

    #[test]
    fn key_mapping_covers_navigation_and_zoom() {
        let none = Modifiers::NONE;
        assert_eq!(
            command_for_key(Key::PageDown, none, 10),
            Some(ViewerCommand::NextPage)
        );
        assert_eq!(
            command_for_key(Key::End, none, 10),
            Some(ViewerCommand::GoToPage(PageIndex::new(9)))
        );
        assert_eq!(command_for_key(Key::End, none, 0), None, "空文档不应越界");
        assert_eq!(
            command_for_key(Key::Num0, none, 3),
            Some(ViewerCommand::SetZoom(1.0))
        );
        assert_eq!(
            command_for_key(Key::ArrowDown, none, 3),
            Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP)))
        );
        assert_eq!(command_for_key(Key::F, none, 3), None, "单独按 F 不映射");
        assert_eq!(
            command_for_key(Key::F, Modifiers::COMMAND, 3),
            Some(ViewerCommand::SetSidebar(SidebarTab::Search))
        );
        assert_eq!(
            command_for_key(Key::C, Modifiers::COMMAND, 3),
            Some(ViewerCommand::CopySelection)
        );
        assert_eq!(command_for_key(Key::Q, none, 3), None);
    }

    #[test]
    fn scroll_mapping_flips_sign_and_supports_zoom_modifier() {
        // 滚轮向上（egui 中 y 为正）→ 滚动偏移减小
        assert_eq!(
            command_for_scroll(EguiVec2::new(0.0, 40.0), false, 1.0),
            Some(ViewerCommand::ScrollBy(Vec2::new(0.0, -40.0)))
        );
        // 横向滚动同理
        assert_eq!(
            command_for_scroll(EguiVec2::new(15.0, 0.0), false, 1.0),
            Some(ViewerCommand::ScrollBy(Vec2::new(-15.0, 0.0)))
        );
        // Ctrl + 滚轮 → 以当前缩放为基准放大
        match command_for_scroll(EguiVec2::new(0.0, 10.0), true, 2.0) {
            Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 2.2).abs() < 1e-4),
            other => panic!("期望 SetZoom，实际 {other:?}"),
        }
        // 无输入 → 无命令
        assert_eq!(command_for_scroll(EguiVec2::ZERO, false, 1.0), None);
        assert_eq!(command_for_scroll(EguiVec2::new(0.0, 0.1), true, 1.0), None);
    }

    /// Ctrl+滚轮 / 触控板捏合的缩放增量必须被翻译成文档缩放（v1 遗漏了这条路径）。
    #[test]
    fn zoom_delta_maps_ctrl_wheel_and_pinch() {
        assert_eq!(command_for_zoom_delta(1.0, 2.0), None, "无缩放时不产生命令");
        assert_eq!(
            command_for_zoom_delta(f32::NAN, 2.0),
            None,
            "异常值必须被忽略"
        );

        match command_for_zoom_delta(1.1, 2.0) {
            Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 2.2).abs() < 1e-4),
            other => panic!("期望 SetZoom，实际 {other:?}"),
        }
        match command_for_zoom_delta(0.5, 2.0) {
            Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 1.0).abs() < 1e-4),
            other => panic!("期望 SetZoom，实际 {other:?}"),
        }
    }

    #[test]
    fn keyed_store_evicts_oldest_beyond_capacity() {
        let mut store = KeyedStore::new(2);
        store.insert(key(0), "a");
        store.insert(key(1), "b");
        assert_eq!(store.len(), 2);
        store.insert(key(2), "c");
        assert_eq!(store.len(), 2, "容量上限必须生效");
        assert!(!store.contains(&key(0)), "最旧的应被淘汰");
        assert_eq!(store.get(&key(2)), Some(&"c"));
    }

    /// D-109：字节账跟着插入/替换/LRU 淘汰/retain/clear 一起动，且**重算**与实际值一致。
    /// 构造一条"页事实"（`whole_pages` 由调用方补）。
    fn facts(
        page: PageIndex,
        rotation: Rotation,
        render_milli: u32,
        shape: PageShape,
    ) -> PageFacts {
        PageFacts {
            page,
            rotation,
            render_milli,
            shape,
            whole_pages: Vec::new(),
        }
    }

    #[test]
    fn keyed_store_tracks_bytes_of_its_values() {
        // 用"值本身即字节数"的替身，便于手算
        let mut store = KeyedStore::with_bytes(3, |value: &u64| *value);
        store.insert(key(0), 100);
        store.insert(key(1), 200);
        assert_eq!(store.bytes(), 300);
        assert_eq!(store.average_bytes(), 150);

        // 同键替换：只加差值
        store.insert(key(0), 50);
        assert_eq!(store.bytes(), 250);

        // 超过容量（3）→ 淘汰最旧的（key(0) 刚被 touch，被淘汰的是 key(1)）
        store.insert(key(2), 400);
        store.insert(key(3), 1000);
        assert_eq!(store.len(), 3);
        assert_eq!(store.bytes(), 50 + 400 + 1000, "淘汰后账目要对上");

        // retain 摘除后重算
        store.retain(|key| key.page.get() % 2 == 0);
        assert_eq!(
            store.bytes(),
            store.iter().map(|(_, value)| *value).sum::<u64>()
        );

        // 清空归零
        store.clear();
        assert_eq!(store.bytes(), 0);
        assert!(store.is_empty());
    }

    /// D-111：预算裁剪 —— A 永不淘汰（软上限），C 先让位，B 在额度内保；类内 LRU（最旧先丢）。
    #[test]
    fn retain_respects_the_byte_budget_by_class() {
        let page = PageIndex::new(0);
        let tile = |scale: f32, col: u32| {
            RenderKey::tiled(
                page,
                Scale::from_zoom(scale),
                Rotation::None,
                TileSpec::new(col, 0),
            )
        };
        let whole = |scale: f32| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);

        // 每张 1 MB（值即字节数）
        const MB: u64 = 1024 * 1024;
        let mut store = KeyedStore::with_bytes(64, |value: &u64| *value);
        // A：当前波段 2 块；B：一张整页底料；C：旧波段的 2 块
        store.insert(tile(4.096, 0), MB);
        store.insert(tile(4.096, 1), MB);
        store.insert(whole(1.6), MB);
        store.insert(tile(6.5536, 0), MB);
        store.insert(tile(6.5536, 1), MB);

        let view = [PageFacts {
            whole_pages: vec![1_600],
            ..facts(page, Rotation::None, 4096, PageShape::Tiles(0, 0, 1, 0))
        }];

        // 预算充足：一张不丢
        store.set_max_bytes(64 * MB);
        let report = store.retain_for_view(&view, |_| MB);
        assert_eq!(store.len(), 5);
        assert_eq!(report.must_bytes, 2 * MB);
        assert_eq!(report.bed_bytes, MB);
        assert_eq!(report.old_bytes, 2 * MB);
        assert!(!report.over_budget);

        // 预算 3 MB：A(2) 之后只剩 1 MB → 只能保一张；B 优先于 C
        store.set_max_bytes(3 * MB);
        let report = store.retain_for_view(&view, |_| MB);
        assert_eq!(store.len(), 3, "2 块 A + 1 张底料");
        assert_eq!(report.bed_bytes, MB, "底料优先保住");
        assert_eq!(report.old_bytes, 0, "旧内容先让位");
        assert!(!report.over_budget);

        // 预算 1 MB：连 A 都放不下 → 软上限：A 全留，B/C 全让位，只报告
        store.set_max_bytes(MB);
        let report = store.retain_for_view(&view, |_| MB);
        assert_eq!(store.len(), 2, "A 必须完整保留");
        assert!(report.over_budget);
        assert_eq!(report.bed_bytes + report.old_bytes, 0);
    }

    /// D-112：`desired_textures` 是纯函数 —— 同样的输入永远同一个结果，
    /// 且 A/B/C 三类的键可以逐个断言（不依赖纹理层现在有什么）。
    #[test]
    fn desired_is_pure_and_classified() {
        let page = PageIndex::new(0);
        let view = [PageFacts {
            page,
            rotation: Rotation::None,
            render_milli: 4096,
            shape: PageShape::Tiles(1, 0, 2, 1),
            whole_pages: vec![1600, 2560],
        }];
        // 纹理层现在只有一张 1600 整页 + 一张 6554 的旧块
        let existing = vec![
            RenderKey::new(page, Scale::from_zoom(1.6), Rotation::None),
            RenderKey::tiled(
                page,
                Scale::from_zoom(6.5536),
                Rotation::None,
                TileSpec::new(0, 0),
            ),
        ];
        let size = |key: &RenderKey| if key.tile.is_none() { 1_000 } else { 100 };

        let first = desired_textures(&view, &existing, 0, size);
        let second = desired_textures(&view, &existing, 0, size);
        assert_eq!(first, second, "纯函数：同样输入必须同样输出");

        // A：可见范围 (1,0)-(2,1) 共 2×2 = 4 块
        let must = |col, row| {
            RenderKey::tiled(
                page,
                Scale::from_zoom(4.096),
                Rotation::None,
                TileSpec::new(col, row),
            )
        };
        for col in 1..=2 {
            for row in 0..=1 {
                assert!(first.keep.contains(&must(col, row)), "A 缺 ({col},{row})");
            }
        }
        assert_eq!(first.must_bytes, 4 * 100);
        // B：两张整页底料（其中 1600 已存在 → 不上传；2560 不存在 → 要上传）
        let whole = |scale| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);
        assert!(first.keep.contains(&whole(1.6)));
        assert!(first.keep.contains(&whole(2.56)));
        assert_eq!(first.bed_bytes, 2_000);
        // upload = "缺的 A" + "缺的 B"：4 块瓦片（A 不在 GPU 里）+ 2560 整页（B 缺）
        assert_eq!(first.upload.len(), 5, "{:?}", first.upload);
        assert!(
            first.upload.iter().any(|key| *key == whole(2.56)),
            "缺的底料要上传"
        );
        assert!(
            first.upload.iter().any(|key| *key == must(1, 0)),
            "缺的可见块更要上传（否则画面永远糊着）"
        );
        // C：最近一个旧波段（6554）里的现有键
        let old = RenderKey::tiled(
            page,
            Scale::from_zoom(6.5536),
            Rotation::None,
            TileSpec::new(0, 0),
        );
        assert!(first.keep.contains(&old));
        assert_eq!(first.old_bytes, 100);

        // 同一个函数在"没有任何现有纹理"时也只改变 C 与 upload，不改 A/B
        let empty = desired_textures(&view, &[], 0, size);
        assert_eq!(empty.must_bytes, first.must_bytes);
        assert_eq!(empty.bed_bytes, first.bed_bytes);
        assert_eq!(empty.old_bytes, 0);
        // 4 块 A + 2 张底料都还不在 GPU 里
        assert_eq!(empty.upload.len(), 6, "{:?}", empty.upload);
    }

    /// 预算在 `desired` 内部一次算完：A 豁免、B 优先于 C。
    #[test]
    fn desired_trim_by_budget_prefers_must_then_bed() {
        let page = PageIndex::new(0);
        let view = [PageFacts {
            page,
            rotation: Rotation::None,
            render_milli: 4096,
            shape: PageShape::Tiles(0, 0, 1, 0),
            whole_pages: vec![1600],
        }];
        let existing = vec![
            RenderKey::new(page, Scale::from_zoom(1.6), Rotation::None),
            RenderKey::tiled(
                page,
                Scale::from_zoom(6.5536),
                Rotation::None,
                TileSpec::new(0, 0),
            ),
        ];
        let size = |key: &RenderKey| if key.tile.is_none() { 1_000 } else { 100 };

        // 1250 预算：A(200) + B(1000) 之后只剩 50 → C(100) 让位
        let desired = desired_textures(&view, &existing, 1_250, size);
        assert_eq!(desired.must_bytes, 200);
        assert_eq!(desired.bed_bytes, 1_000);
        assert_eq!(desired.old_bytes, 0, "C 先让位");
        assert!(!desired.over_budget);

        // 500 预算：连 B 都放不下 → 只留 A
        let desired = desired_textures(&view, &existing, 500, size);
        assert_eq!(desired.must_bytes, 200);
        assert_eq!(desired.bed_bytes, 0);
        assert!(!desired.over_budget, "A 放得下就不算超预算");

        // 100 预算：A 都放不下 → 软上限：A 全留 + 报告
        let desired = desired_textures(&view, &existing, 100, size);
        assert_eq!(desired.must_bytes, 200);
        assert!(desired.over_budget);
        assert_eq!(desired.bed_bytes + desired.old_bytes, 0);
    }

    #[test]
    fn keyed_store_reinsert_moves_to_newest() {
        let mut store = KeyedStore::new(2);
        store.insert(key(0), "a");
        store.insert(key(1), "b");
        // 重新插入 0 → 它变成"最新"，被淘汰的应该是 1
        store.insert(key(0), "a2");
        store.insert(key(2), "c");
        assert!(store.contains(&key(0)));
        assert!(!store.contains(&key(1)));
        assert_eq!(store.get(&key(0)), Some(&"a2"));
    }

    #[test]
    fn keyed_store_retain_drops_stale_entries() {
        let mut store = KeyedStore::new(16);
        for page in 0..5 {
            store.insert(key(page), page);
        }
        store.retain(|key| key.page.get() < 2);
        assert_eq!(store.len(), 2);
        assert!(store.contains(&key(0)) && store.contains(&key(1)));

        // retain 之后仍能正常插入（内部顺序表未损坏）
        store.insert(key(9), 9);
        assert_eq!(store.get(&key(9)), Some(&9));
        assert_eq!(store.len(), 3);

        store.clear();
        assert!(store.is_empty());
    }

    /// D-101：按页面空间矩形 + 指定渲染比例拼图 —— 与当前缩放无关、1:1 拷贝。
    #[test]
    fn compose_region_snapshot_copies_tiles_one_to_one() {
        let make = |width: u32, height: u32, fill: u8| {
            let mut data = Vec::with_capacity((width * height * 4) as usize);
            for _ in 0..(width * height) {
                data.extend_from_slice(&[fill, fill, fill, 255]);
            }
            RasterImage::new(
                width,
                height,
                PixelFormat::Bgra8,
                std::sync::Arc::from(data),
            )
            .expect("位图")
        };
        // 612×792 @2 倍 → 1224×1584，2×2 网格，每块 612×792
        let page = Size::new(612.0, 792.0);
        let grid = TileGrid::new(page, 2.0);
        assert_eq!(grid.side, 2);
        let a = make(612, 792, 10);
        let b = make(612, 792, 20);
        let tiles = vec![(TileSpec::new(0, 1), &a), (TileSpec::new(1, 1), &b)];
        // 页面下半部分 → 1224×792 的图，左半 10、右半 20
        let rect = Rect::new(0.0, 0.0, 612.0, 396.0);
        let image = compose_region_snapshot(rect, page, Rotation::None, 2.0, &grid, &tiles)
            .expect("应有内容");
        assert_eq!(image.size, [1224, 792]);
        assert_eq!(image.pixels[0].r(), 10);
        assert_eq!(image.pixels[611].r(), 10);
        assert_eq!(image.pixels[612].r(), 20);
        assert_eq!(image.pixels[1224 * 791 + 1223].r(), 20);

        // 区域跨越上下两块 → 上半没有块 → 白（而不是 None）
        let rect = Rect::new(0.0, 100.0, 612.0, 700.0);
        let image = compose_region_snapshot(rect, page, Rotation::None, 2.0, &grid, &tiles)
            .expect("部分覆盖也应有图");
        assert_eq!(image.size, [1224, 1400]);
        assert_eq!(image.pixels[0].r(), 255, "未覆盖的页面上部为白");

        // 一个像素都覆盖不到 → None（调用方据此"继续等待渲染"）
        let rect = Rect::new(0.0, 700.0, 612.0, 92.0); // 页面上部
        assert!(compose_region_snapshot(rect, page, Rotation::None, 2.0, &grid, &tiles).is_none());

        // 90° 旋转：块在页面空间是"横"的，映射到设备像素后宽高互换。
        // 旋转后页 792×612pt @2 → 1584×1224 px，网格 2×2，每块 792×612 px。
        let grid = TileGrid::new(page.rotated(Rotation::Clockwise90), 2.0);
        assert_eq!(grid.side, 2);
        let rotated = make(792, 612, 30);
        let rect = grid.tile_page_rect(0, 0, page, Rotation::Clockwise90);
        let image = compose_region_snapshot(
            rect,
            page,
            Rotation::Clockwise90,
            2.0,
            &grid,
            &[(TileSpec::new(0, 0), &rotated)],
        )
        .expect("旋转后应有内容");
        assert_eq!(image.size, [792, 612], "一块在设备像素里的尺寸");
        assert_eq!(image.pixels[0].r(), 30);
    }

    /// D-108：记录分辨率让预览超过纹理单边上限时，按 1.6 步长降档。
    #[test]
    fn preview_scale_drops_when_the_texture_side_limit_would_be_exceeded() {
        let page = Size::new(595.28, 841.89);
        let rect = Rect::new(0.0, 0.0, 595.28, 841.89);
        // 整页 + 800% 波段（10.486）→ 6242×8828，边长远小于 16384：不动
        assert_eq!(
            preview_render_scale(rect, page, Rotation::None, 10.486, 16_384),
            10.486
        );
        // 上限 4096：需要降档，降完必须放得下
        let clamped = preview_render_scale(rect, page, Rotation::None, 10.486, 4096);
        assert!(clamped < 10.486, "应降档：{clamped}");
        let (width, height) =
            region_pixel_size(rect, page, Rotation::None, clamped).expect("区域有效");
        assert!(width.max(height) <= 4096, "降档后 {width}×{height} 仍超限");
        // 0 = 不限制
        assert_eq!(
            preview_render_scale(rect, page, Rotation::None, 10.486, 0),
            10.486
        );
    }

    // ------------------------------------------------ D-96：整页级就绪门槛

    fn draw_input() -> DrawInput {
        DrawInput {
            tiled: true,
            has_whole_current: false,
            ready: 0,
            total: 18,
            preview: Preview::Tiles(65_540),
            waited_ms: 0,
            showing_tiles: false,
        }
    }

    /// **关键回归**：只要还缺一块，就不许翻页 —— 否则就是"几块清晰 + 大片白"。
    #[test]
    fn plan_page_waits_for_every_visible_tile() {
        let input = DrawInput {
            ready: 17,
            waited_ms: 100,
            ..draw_input()
        };
        assert_eq!(
            plan_page(&input),
            PageDraw::TilePreview(65_540),
            "缺一块仍用旧内容顶住"
        );
        let input = DrawInput {
            ready: 2,
            waited_ms: 0,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));

        let input = DrawInput {
            ready: 18,
            waited_ms: 0,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Tiles, "全齐才切到当前波段");
    }

    /// D-104：同一波段内**已经切到瓦片**后，滚出新块也不退回预览。
    ///
    /// 退回预览 = 整页变糊（实测真机 40~100ms）；正确做法是保留已清晰的块，
    /// 缺的那块由底料补（D-98）。
    #[test]
    fn tiles_are_sticky_inside_a_band() {
        let input = DrawInput {
            ready: 4,
            total: 6,
            showing_tiles: true,
            ..draw_input()
        };
        assert_eq!(
            plan_page(&input),
            PageDraw::Tiles,
            "已进入瓦片模式：留在瓦片 + 底料"
        );

        // 一块就绪都没有也一样粘住（底料在，页面不会露白）
        let input = DrawInput {
            ready: 0,
            total: 6,
            showing_tiles: true,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Tiles);
    }

    /// 粘性只在**同一波段**内成立：新波段第一帧仍要等可见范围全齐（D-97 的门槛不许被绕过）。
    #[test]
    fn a_new_band_still_waits_for_the_whole_range() {
        let input = DrawInput {
            ready: 4,
            total: 6,
            showing_tiles: false,
            ..draw_input()
        };
        assert_eq!(
            plan_page(&input),
            PageDraw::TilePreview(65_540),
            "新波段（上一帧属于旧波段）仍先显示旧内容"
        );
    }

    /// 粘性不能盖过"不分块"和"当前波段整页在"这两条更早的判据。
    #[test]
    fn stickiness_does_not_override_earlier_rules() {
        let tiled_input = DrawInput {
            tiled: false,
            showing_tiles: true,
            ..draw_input()
        };
        assert_eq!(
            plan_page(&tiled_input),
            PageDraw::TilePreview(65_540),
            "不分块时按旧内容拉伸，与粘性无关"
        );
        let whole_input = DrawInput {
            has_whole_current: true,
            showing_tiles: true,
            ..draw_input()
        };
        assert_eq!(plan_page(&whole_input), PageDraw::Whole);
    }

    /// 超时兜底：某块渲染失败/块特别多时，不许永远停在糊图。
    #[test]
    fn plan_page_falls_back_to_partial_tiles_after_the_timeout() {
        let input = DrawInput {
            ready: 5,
            waited_ms: READY_TIMEOUT_MS - 1,
            ..draw_input()
        };
        assert_eq!(
            plan_page(&input),
            PageDraw::TilePreview(65_540),
            "超时前继续顶住"
        );
        let input = DrawInput {
            ready: 5,
            waited_ms: READY_TIMEOUT_MS,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Tiles, "超时后画已就绪部分");
        // 一块都没有时超时也没有意义（没有内容可画）
        let input = DrawInput {
            ready: 0,
            waited_ms: 9_999,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));
    }

    /// 旧内容优先级：整页图 > 旧块；都没有时才允许补白。
    #[test]
    fn plan_page_prefers_whole_page_preview_then_tiles_then_blank() {
        let input = DrawInput {
            ready: 3,
            preview: Preview::Whole(25_600),
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Stretch(25_600));
        let input = DrawInput {
            ready: 3,
            preview: Preview::Tiles(65_540),
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));
        let input = DrawInput {
            ready: 3,
            preview: Preview::None,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Tiles, "没有旧内容时只能补白");
    }

    /// 不分块（整页路径）不受就绪门槛影响：有整页图看整页，只有旧内容就顶住。
    #[test]
    fn plan_page_keeps_the_whole_page_path_unchanged() {
        let input = DrawInput {
            tiled: false,
            has_whole_current: true,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Whole);
        let input = DrawInput {
            tiled: false,
            preview: Preview::Whole(16_000),
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Stretch(16_000));
        let input = DrawInput {
            tiled: false,
            preview: Preview::Tiles(16_000),
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::TilePreview(16_000));
        let input = DrawInput {
            tiled: false,
            preview: Preview::None,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Blank);
    }

    /// 退化输入（可见范围为空的极端情形）不得 panic，也不得永远停在预览。
    #[test]
    fn plan_page_handles_an_empty_range() {
        let input = DrawInput {
            ready: 0,
            total: 0,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));
        let input = DrawInput {
            ready: 0,
            total: 0,
            preview: Preview::None,
            ..draw_input()
        };
        assert_eq!(plan_page(&input), PageDraw::Tiles);
    }

    /// `ready/total` 必须与绘制用同一段枚举（`grid.contains` 过滤），否则会"永远差一块"。
    #[test]
    fn ready_tiles_in_view_matches_the_draw_range() {
        let page = PageIndex::new(0);
        let grid = TileGrid::new(Size::new(612.0, 792.0), 8.0);
        assert_eq!(grid.side, 8);
        let render = Scale::from_zoom(8.0);
        let range = (0, 0, 1, 1);
        let mut store = KeyedStore::new(64);

        assert_eq!(
            store.ready_tiles_in_view(page, Rotation::None, render, &grid, range),
            (0, 4),
            "空存储：四块就绪"
        );
        for (col, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            store.insert(
                RenderKey::tiled(page, render, Rotation::None, TileSpec::new(col, row)),
                (col, row),
            );
        }
        assert_eq!(
            store.ready_tiles_in_view(page, Rotation::None, render, &grid, range),
            (4, 4)
        );

        // 越界块不计入总数（否则永远 ready < total）
        let out_of_grid = (0, 0, grid.side, grid.side);
        let (ready, total) =
            store.ready_tiles_in_view(page, Rotation::None, render, &grid, out_of_grid);
        assert!(total > 4 && ready >= 4);
        assert_eq!(
            total,
            grid.all_tiles().len(),
            "总数必须与网格的有效块数一致（越界的行列被过滤）"
        );

        // 别的波段 / 别的旋转不算
        let other = Scale::from_zoom(4.0);
        assert_eq!(
            store.ready_tiles_in_view(page, Rotation::None, other, &grid, range),
            (0, 4)
        );
        assert_eq!(
            store.ready_tiles_in_view(page, Rotation::Clockwise90, render, &grid, range),
            (0, 4)
        );
    }

    /// 预览源取舍：缩小方向优先整页图（覆盖），放大方向优先更接近的（清晰）。
    #[test]
    fn preview_source_prefers_coverage_when_zooming_out() {
        let page = PageIndex::new(0);
        let whole = |scale: f32| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);
        let tile = |scale: f32, col: u32| {
            RenderKey::tiled(
                page,
                Scale::from_zoom(scale),
                Rotation::None,
                TileSpec::new(col, 0),
            )
        };
        // 只有整页图 / 只有块 / 都没有
        let mut store = KeyedStore::new(16);
        store.insert(whole(1.6), "whole");
        assert_eq!(
            store.preview_source(page, Rotation::None, 10_486),
            Preview::Whole(1_600)
        );
        let mut store = KeyedStore::new(16);
        store.insert(tile(6.5536, 0), "tile");
        assert_eq!(
            store.preview_source(page, Rotation::None, 10_486),
            Preview::Tiles(6_554)
        );
        let store: KeyedStore<&str> = KeyedStore::new(16);
        assert_eq!(
            store.preview_source(page, Rotation::None, 10_486),
            Preview::None
        );

        // 缩小方向：整页图来自更低波段、块来自更高波段 → 用整页图（覆盖整个视口）
        let mut store = KeyedStore::new(16);
        store.insert(whole(2.56), "whole");
        store.insert(tile(10.48576, 0), "tile");
        assert_eq!(
            store.preview_source(page, Rotation::None, 6_554),
            Preview::Whole(2_560),
            "缩小时旧块只覆盖缩之前那一片，必须用整页图顶住"
        );

        // 放大方向：块来自更低波段 → 取更接近的（更清晰），且它覆盖是新视口的超集
        let mut store = KeyedStore::new(16);
        store.insert(whole(1.6), "whole");
        store.insert(tile(6.5536, 0), "tile");
        assert_eq!(
            store.preview_source(page, Rotation::None, 10_486),
            Preview::Tiles(6_554),
            "放大时优先更接近目标的预览"
        );

        // 距离相等 → 整页图（覆盖更全）
        let mut store = KeyedStore::new(16);
        store.insert(whole(2.0), "whole");
        store.insert(tile(4.0, 0), "tile");
        assert_eq!(
            store.preview_source(page, Rotation::None, 3_000),
            Preview::Whole(2_000)
        );
    }

    /// **回归（D-99）**：预览层也必须叠在底料之上 —— 连续放大时旧波段可能只有一两块，
    /// 光靠"旧瓦片预览"会露白。
    #[test]
    fn preview_layers_always_need_an_underlay() {
        assert!(
            needs_underlay(PageDraw::TilePreview(6_554), 0, 0),
            "旧块预览可能只覆盖一小块，必须有底料"
        );
        assert!(needs_underlay(PageDraw::TilePreview(6_554), 18, 18));
        assert!(needs_underlay(PageDraw::Stretch(2_560), 0, 0));
        assert!(needs_underlay(PageDraw::Tiles, 7, 8), "缺块时需要底料");
        assert!(
            !needs_underlay(PageDraw::Tiles, 8, 8),
            "块全齐时块已把底料盖满，不必再画"
        );
        assert!(
            !needs_underlay(PageDraw::Whole, 0, 0),
            "当前波段整页图无需底料"
        );
        assert!(!needs_underlay(PageDraw::Blank, 0, 0));
    }

    /// 打底要"覆盖优先"：即使旧块更接近目标，也优先用整页图（能盖满整页）。
    #[test]
    fn underlay_source_prefers_coverage_over_sharpness() {
        let page = PageIndex::new(0);
        let whole = RenderKey::new(page, Scale::from_zoom(2.56), Rotation::None);
        let tile = RenderKey::tiled(
            page,
            Scale::from_zoom(6.5536),
            Rotation::None,
            TileSpec::new(5, 7),
        );

        let mut store = KeyedStore::new(16);
        store.insert(whole, "whole");
        store.insert(tile, "tile");
        // 目标 800%：旧块 6554 比整页图 2560 更接近，但打底必须用整页图
        assert_eq!(
            store.underlay_source(page, Rotation::None, 10_486),
            Preview::Whole(2_560)
        );
        // 同时也说明"整屏预览"仍可选更清晰的那个（两条规则各司其职）
        assert_eq!(
            store.preview_source(page, Rotation::None, 10_486),
            Preview::Tiles(6_554)
        );

        // 没有整页图时退回旧块；都没有则 None
        let mut store = KeyedStore::new(16);
        store.insert(tile, "tile");
        assert_eq!(
            store.underlay_source(page, Rotation::None, 10_486),
            Preview::Tiles(6_554)
        );
        let store: KeyedStore<&str> = KeyedStore::new(16);
        assert_eq!(
            store.underlay_source(page, Rotation::None, 10_486),
            Preview::None
        );
    }

    /// 保留规则：当前 + 最近的其它波段 + 当前以下最近的"整页图"所在波段。
    #[test]
    fn retain_keeps_whole_pages_declared_by_the_facts() {
        let page = PageIndex::new(0);
        let whole = |scale: f32| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);
        let tile = |scale: f32, col: u32| {
            RenderKey::tiled(
                page,
                Scale::from_zoom(scale),
                Rotation::None,
                TileSpec::new(col, 0),
            )
        };

        let mut store = KeyedStore::new(64);
        // 100% / 200% 是整页图，300% 起分块
        store.insert(whole(1.6), "w1600");
        store.insert(whole(2.56), "w2560");
        store.insert(tile(4.096, 0), "t4096");
        store.insert(tile(6.5536, 0), "t6554");
        store.insert(tile(10.48576, 0), "t10486");

        // B 类现在是**声明式**的：由 facts.whole_pages 指定（app 用波段代数算出），
        // 不再"扫描现有键猜哪些是底料"（D-112）
        let view = [PageFacts {
            whole_pages: vec![1_600, 2_560],
            ..facts(page, Rotation::None, 10_486, PageShape::Tiles(0, 0, 1, 1))
        }];
        store.retain_for_view(&view, |_| 0);

        assert!(store.contains(&tile(10.48576, 0)), "A 类：当前波段的可见块");
        assert!(store.contains(&tile(6.5536, 0)), "C 类：最近一个旧波段");
        assert!(store.contains(&whole(2.56)), "B 类：声明的整页底料");
        assert!(store.contains(&whole(1.6)), "B 类：更小的整页底料也留");
        assert!(!store.contains(&tile(4.096, 0)), "更旧的块波段应丢弃");
    }

    /// 没有整页图时退回"当前 + 最近"（与旧行为一致）。
    #[test]
    fn retain_without_whole_pages_only_keeps_current_and_nearest() {
        let page = PageIndex::new(0);
        let tile = |scale: f32, col: u32| {
            RenderKey::tiled(
                page,
                Scale::from_zoom(scale),
                Rotation::None,
                TileSpec::new(col, 0),
            )
        };
        let mut store = KeyedStore::new(64);
        store.insert(tile(4.096, 0), "a");
        store.insert(tile(6.5536, 0), "b");
        store.insert(tile(10.48576, 0), "c");
        store.retain_for_view(
            &[facts(
                page,
                Rotation::None,
                10_486,
                PageShape::Tiles(0, 0, 1, 1),
            )],
            |_| 0,
        );
        assert!(store.contains(&tile(10.48576, 0)));
        assert!(store.contains(&tile(6.5536, 0)));
        assert!(!store.contains(&tile(4.096, 0)));
    }

    /// 缩放期间用"上一档缩放"的位图拉伸占位（第二轮验收：避免大片白色填充）。
    #[test]
    fn best_page_texture_picks_the_nearest_scale() {
        let scaled = |page: u32, scale: f32, rotation: Rotation| {
            RenderKey::new(PageIndex::new(page), Scale::from_zoom(scale), rotation)
        };

        let mut store = KeyedStore::new(16);
        store.insert(scaled(0, 1.0, Rotation::None), "one");
        store.insert(scaled(0, 2.0, Rotation::None), "two");
        store.insert(scaled(1, 2.0, Rotation::None), "other-page");

        // 目标 2.1 → 最接近的整页图是 2.0
        assert_eq!(
            store.best_page_texture(PageIndex::new(0), Rotation::None, 2100),
            Some(&"two")
        );
        // 目标 1.1 → 1.0
        assert_eq!(
            store.best_page_texture(PageIndex::new(0), Rotation::None, 1100),
            Some(&"one")
        );
        // 其它页不参与
        assert_eq!(
            store.best_page_texture(PageIndex::new(2), Rotation::None, 2000),
            None
        );
        // 旋转不同不可顶替（否则画面会横躺）
        assert_eq!(
            store.best_page_texture(PageIndex::new(0), Rotation::Clockwise90, 2000),
            None
        );
    }

    /// 分块渲染时，判断"能不能立刻画真内容"：同页同缩放的瓦片只要有一块就绪就算能。
    #[test]
    fn has_tiles_detects_ready_tiles_for_the_same_key() {
        let mut store = KeyedStore::new(8);
        let whole = RenderKey::new(PageIndex::new(0), Scale::from_zoom(2.0), Rotation::None);
        assert!(!store.has_tiles(&whole), "空存储不应报告有瓦片");

        store.insert(
            RenderKey::tiled(
                PageIndex::new(0),
                Scale::from_zoom(2.0),
                Rotation::None,
                TileSpec::new(0, 0),
            ),
            "tile",
        );
        assert!(store.has_tiles(&whole), "同页同缩放的瓦片应被识别");
        assert!(
            !store.has_tiles(&RenderKey::new(
                PageIndex::new(1),
                Scale::from_zoom(2.0),
                Rotation::None
            )),
            "其它页的瓦片不算"
        );
        assert!(
            !store.has_tiles(&RenderKey::new(
                PageIndex::new(0),
                Scale::from_zoom(1.5),
                Rotation::None
            )),
            "其它缩放的瓦片不算"
        );
    }

    /// 高倍缩放的过场：旧瓦片要按"新页像素 / 旧页像素"等比放大回整页。
    #[test]
    fn tile_view_rect_scaled_maps_old_tiles_onto_the_new_page() {
        let page_rect = Rect::new(-100.0, -200.0, 2000.0, 4000.0);
        // 旧比例下页位图 1000×2000，瓦片 (0,0) 占 500×500 像素
        let old_page = Size::new(1000.0, 2000.0);
        let new_page = Size::new(2000.0, 4000.0);
        let rect = tile_view_rect_scaled(
            page_rect,
            Rect::new(500.0, 500.0, 500.0, 500.0),
            old_page,
            new_page,
        );
        // 放大 2 倍：偏移与原尺寸都翻倍
        assert_eq!(rect, Rect::new(900.0, 800.0, 1000.0, 1000.0));

        // 尺寸为 0 时不得产生 NaN/Inf
        let degenerate = tile_view_rect_scaled(
            page_rect,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Size::new(0.0, 0.0),
            new_page,
        );
        assert!(degenerate.x.is_finite() && degenerate.width.is_finite());
    }

    /// 预览用的旧瓦片查询：只认同页同旋转、且不等于当前缩放档的那些。
    #[test]
    fn best_tile_scale_and_tiles_at_pick_the_previous_scale() {
        let mut store = KeyedStore::new(16);
        let tile = |scale: f32, col: u32, row: u32| {
            RenderKey::tiled(
                PageIndex::new(0),
                Scale::from_zoom(scale),
                Rotation::None,
                TileSpec::new(col, row),
            )
        };
        store.insert(tile(1.0, 0, 0), "old-a");
        store.insert(tile(1.0, 1, 0), "old-b");
        store.insert(tile(2.0, 0, 0), "current");

        // 当前档 2.0 → 最近的"有瓦片的其它档"是 1.0
        assert_eq!(
            store.best_tile_scale(PageIndex::new(0), Rotation::None, 2000),
            Some(1000)
        );
        // 当前档 1.0 → 2.0
        assert_eq!(
            store.best_tile_scale(PageIndex::new(0), Rotation::None, 1000),
            Some(2000)
        );
        // 其它页 / 其它旋转都没有
        assert_eq!(
            store.best_tile_scale(PageIndex::new(1), Rotation::None, 2000),
            None
        );
        assert_eq!(
            store.best_tile_scale(PageIndex::new(0), Rotation::Clockwise90, 2000),
            None
        );

        let old = store.tiles_at(PageIndex::new(0), Rotation::None, 1000);
        assert_eq!(old.len(), 2, "旧档应有 2 块");
        assert!(old
            .iter()
            .any(|(tile, value)| *tile == TileSpec::new(0, 0) && **value == "old-a"));
        assert_eq!(
            store
                .tiles_at(PageIndex::new(0), Rotation::None, 2000)
                .len(),
            1
        );
    }

    /// 诊断助手：判断热区是否越界（越界即坐标空间不一致的证据）。
    #[test]
    fn link_rect_in_page_bounds_detects_coordinate_space_mismatch() {
        let page = Size::new(612.0, 792.0);
        assert!(link_rect_in_page_bounds(
            Rect::new(72.0, 700.0, 200.0, 20.0),
            page
        ));
        // 浮点余量：贴边不算越界
        assert!(link_rect_in_page_bounds(
            Rect::new(0.0, 0.0, 612.0, 792.0),
            page
        ));
        // 典型的"整体偏移"：MediaBox 原点不为 0 时，热区会跑到页外
        assert!(!link_rect_in_page_bounds(
            Rect::new(72.0, 800.0, 200.0, 20.0),
            page
        ));
        assert!(!link_rect_in_page_bounds(
            Rect::new(-40.0, 100.0, 20.0, 20.0),
            page
        ));
        assert!(!link_rect_in_page_bounds(
            Rect::new(600.0, 100.0, 100.0, 20.0),
            page
        ));
    }

    /// 链接跳转语义：有落点 → 先跳页再居中；无落点 → 只跳页（顶部对齐）。
    #[test]
    fn commands_for_destination_centers_when_an_offset_exists() {
        let with_offset = Destination::Page {
            page: PageIndex::new(3),
            offset: Some(Point::new(120.0, 700.0)),
        };
        assert_eq!(
            commands_for_destination(&with_offset),
            vec![
                ViewerCommand::GoToPage(PageIndex::new(3)),
                ViewerCommand::CenterOnPagePoint {
                    page: PageIndex::new(3),
                    point: Point::new(120.0, 700.0),
                },
            ]
        );

        let page_only = Destination::Page {
            page: PageIndex::new(1),
            offset: None,
        };
        assert_eq!(
            commands_for_destination(&page_only),
            vec![ViewerCommand::GoToPage(PageIndex::new(1))],
            "没有落点就只跳页，不做居中"
        );

        assert!(commands_for_destination(&Destination::Uri("https://x".into())).is_empty());
    }

    /// 链接框的显示条件：开关 + 缩放阈值。
    #[test]
    fn link_overlay_visibility_respects_the_switch_and_zoom_threshold() {
        assert!(!link_overlay_visible(false, 2.0, 0.75), "开关关闭时不画");
        assert!(!link_overlay_visible(true, 0.5, 0.75), "缩放低于阈值时不画");
        assert!(link_overlay_visible(true, 0.75, 0.75), "正好达到阈值就画");
        assert!(link_overlay_visible(true, 3.0, 0.75));
        assert!(
            !link_overlay_visible(true, f32::NAN, 0.75),
            "异常缩放值不画"
        );
    }

    /// 自定义图片 preview：跨页框选时只保留**重叠面积最大**的那一页及其交叠矩形。
    #[test]
    fn largest_overlap_page_picks_the_dominant_page() {
        let page_a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let page_b = Rect::new(0.0, 110.0, 100.0, 100.0);
        let pages = [(0usize, page_a), (1usize, page_b)];

        // 选区内 A 占 60 高、B 占 20 高 → 取 A
        let selection = Rect::new(0.0, 40.0, 100.0, 90.0);
        let (index, overlap) =
            largest_overlap_page(pages.iter().copied(), selection).expect("应命中");
        assert_eq!(index, 0, "取重叠更大的 A 页");
        assert_eq!(overlap, Rect::new(0.0, 40.0, 100.0, 60.0), "只保留交叠部分");

        // 反过来：B 占得多 → 取 B
        let selection = Rect::new(0.0, 90.0, 100.0, 60.0);
        let (index, overlap) =
            largest_overlap_page(pages.iter().copied(), selection).expect("应命中");
        assert_eq!(index, 1);
        assert_eq!(overlap, Rect::new(0.0, 110.0, 100.0, 40.0));

        // 完全落在页间距 → 没有页
        assert!(
            largest_overlap_page(pages.iter().copied(), Rect::new(0.0, 104.0, 100.0, 4.0))
                .is_none()
        );

        // 面积并列时取先出现者（页面顺序），结果确定
        let tie = vec![
            (0usize, Rect::new(0.0, 0.0, 100.0, 50.0)),
            (1usize, Rect::new(0.0, 50.0, 100.0, 50.0)),
        ];
        let (index, _) =
            largest_overlap_page(tie, Rect::new(0.0, 0.0, 100.0, 100.0)).expect("应命中");
        assert_eq!(index, 0);
    }

    /// 跨页截图：所有与选区相交的页都要参与（含页间距留白，不能只留最大重叠那一页）。
    #[test]
    fn pages_intersecting_keeps_every_overlapping_page() {
        let page_a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let page_b = Rect::new(0.0, 110.0, 100.0, 100.0); // 与 A 之间有 10pt 页间距
        let page_c = Rect::new(0.0, 400.0, 100.0, 100.0);
        let pages = [(0usize, page_a), (1usize, page_b), (2usize, page_c)];

        // 选区跨越 A 与 B（含间距）
        let selection = Rect::new(0.0, 80.0, 100.0, 60.0);
        let hit = pages_intersecting(pages.iter().copied(), selection);
        assert_eq!(hit.len(), 2, "A 与 B 都要在");
        assert_eq!(hit[0].0, 0);
        assert_eq!(hit[1].0, 1);

        // 选区只碰 C
        let hit = pages_intersecting(pages.iter().copied(), Rect::new(0.0, 420.0, 50.0, 20.0));
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].0, 2);

        // 只落在页间距里 → 没有任何页（结果是一张全白图）
        let hit = pages_intersecting(pages.iter().copied(), Rect::new(0.0, 104.0, 100.0, 4.0));
        assert!(hit.is_empty());

        // 空输入不 panic
        assert!(pages_intersecting(std::iter::empty(), selection).is_empty());
    }

    /// 正文尺寸 ↔ 浮窗尺寸必须互逆（否则每帧写回会让尺寸漂移）。
    #[test]
    fn body_size_and_float_size_are_inverse() {
        let title = 22.0;
        let body = egui::vec2(300.0, 170.0);
        let size = float_size_for_body(body, title);
        assert_eq!(size, egui::vec2(316.0, 208.0), "两侧 8px + 标题 22px");

        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), size);
        assert_eq!(body_size_of(rect, title), body, "往返必须回到原值");

        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2.0, 2.0));
        let back = body_size_of(tiny, title);
        assert!(back.x >= 1.0 && back.y >= 1.0, "退化尺寸不给 0/负值");
    }

    /// 侧栏条目触发的浮窗：贴左缘、与该行居中、夹取在视图内。
    #[test]
    fn float_rect_at_row_hugs_the_sidebar_and_aligns_with_the_row() {
        let canvas = egui::Rect::from_min_size(egui::pos2(270.0, 40.0), egui::vec2(800.0, 600.0));
        let size = egui::vec2(320.0, 200.0);
        let min = egui::vec2(160.0, 90.0);

        // 中间某行 → 贴左缘（画布左 + 16），垂直与行居中
        let row = egui::Rect::from_min_size(egui::pos2(0.0, 300.0), egui::vec2(260.0, 18.0));
        let rect = float_rect_at_row(canvas, row, size, min);
        assert!(
            (rect.min.x - (canvas.min.x + 16.0)).abs() < 1e-3,
            "紧贴侧边栏"
        );
        assert!(
            (rect.center().y - row.center().y).abs() < 1e-3,
            "与条目行垂直居中"
        );

        // 顶部行 → 会被夹回视图内（不会超出上边）
        let top = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(260.0, 18.0));
        let rect = float_rect_at_row(canvas, top, size, min);
        assert!(rect.min.y >= canvas.min.y - 1e-3);

        // 画布未知 → 退回固定位置，不 panic
        let rect = float_rect_at_row(egui::Rect::NOTHING, row, size, min);
        assert_eq!(rect.min, egui::pos2(48.0, 48.0));
    }

    /// 顶栏要用的几个纯换算：缩放百分比、适应高度、页码校验、菜单行文本。
    #[test]
    fn toolbar_conversions() {
        // 缩放：显示为整数百分比，解析时取整（153.2 与 153 等价）
        assert_eq!(zoom_percent_text(1.532), "153%");
        assert_eq!(zoom_percent_text(1.0), "100%");
        assert_eq!(parse_zoom_percent("153.2"), Some(1.53));
        assert_eq!(parse_zoom_percent("153"), Some(1.53));
        assert_eq!(parse_zoom_percent(" 153% "), Some(1.53));
        assert_eq!(parse_zoom_percent("abc"), None);
        assert_eq!(parse_zoom_percent("0"), None, "0% 无意义");
        assert_eq!(parse_zoom_percent("-5"), None);

        // 适应高度：视口高 ÷ 页高；退化输入返回 None
        assert_eq!(fit_height_zoom(600.0, 300.0), Some(2.0));
        assert_eq!(fit_height_zoom(0.0, 300.0), None);
        assert_eq!(fit_height_zoom(600.0, 0.0), None);

        // 页码：1..=page_count 合法
        assert_eq!(page_index_from_text("3", 10), Some(PageIndex::new(2)));
        assert_eq!(page_index_from_text("1", 10), Some(PageIndex::FIRST));
        assert_eq!(page_index_from_text("0", 10), None);
        assert_eq!(page_index_from_text("11", 10), None);
        assert_eq!(page_index_from_text("x", 10), None);

        // 菜单行：勾选时行首为 ✓，否则为空格
        assert_eq!(checkbox_row("链接框", true), "✓ 链接框");
        assert_eq!(checkbox_row("链接框", false), "  链接框");
    }

    /// 尺寸比较要带容差：亚像素误差不能算"修改"。
    #[test]
    fn body_size_changed_ignores_subpixel_noise() {
        let stored = egui::vec2(320.0, 200.0);
        assert!(!body_size_changed(stored, egui::vec2(320.0, 200.0)));
        assert!(
            !body_size_changed(stored, egui::vec2(320.3, 199.8)),
            "亚像素误差不算修改"
        );
        assert!(
            body_size_changed(stored, egui::vec2(330.0, 200.0)),
            "明显变宽算修改"
        );
        assert!(
            body_size_changed(stored, egui::vec2(320.0, 190.0)),
            "明显变矮算修改"
        );
    }

    /// 保存按钮文案随"是否有未保存修改"变化。
    #[test]
    fn save_button_label_marks_unsaved_changes() {
        assert_eq!(save_button_label(false), "保存自定义数据");
        assert_eq!(save_button_label(true), "*保存自定义数据");
        assert!(save_button_label(true).starts_with('*'));
    }

    /// 侧栏行首类型符号：码点必须与用户指定的完全一致。
    #[test]
    fn custom_preview_symbols_are_the_requested_codepoints() {
        assert_eq!(
            CUSTOM_SYMBOL_TEXT.chars().next().map(u32::from),
            Some(0x1F5B9)
        );
        assert_eq!(
            CUSTOM_SYMBOL_IMAGE.chars().next().map(u32::from),
            Some(0x1F5BB)
        );
        assert_eq!(CUSTOM_SYMBOL_TEXT.chars().count(), 1);
        assert_eq!(CUSTOM_SYMBOL_IMAGE.chars().count(), 1);
        assert_eq!(
            CUSTOM_SYMBOL_EDIT.chars().next().map(u32::from),
            Some(0x270E)
        );
        assert_eq!(
            CUSTOM_SYMBOL_DELETE.chars().next().map(u32::from),
            Some(0x274E)
        );
    }

    /// 侧栏 Pin 列表 → 正式浮窗的查找。
    #[test]
    fn find_float_for_custom_matches_only_the_official_float() {
        let customs = vec![None, Some(0), None, Some(1)];
        assert_eq!(find_float_for_custom(&customs, 0), Some(1));
        assert_eq!(find_float_for_custom(&customs, 1), Some(3));
        assert_eq!(find_float_for_custom(&customs, 2), None, "没有正式浮窗");
        assert_eq!(find_float_for_custom(&[], 0), None);
    }

    /// 浮窗文本排版：自动换行、行数上限、溢出省略号。
    #[test]
    fn overflow_layout_wraps_limits_rows_and_ellipsizes() {
        let font = egui::FontId::proportional(13.0);
        let color = egui::Color32::WHITE;

        // 正文：宽度受限（自动换行）+ 行数上限
        let job = overflow_layout("很长的单行文本", font.clone(), color, 200.0, 6);
        assert_eq!(job.wrap.max_width, 200.0, "宽度受限 → 自动换行");
        assert_eq!(job.wrap.max_rows, 6);
        assert_eq!(job.wrap.overflow_character, Some('…'), "超出要显示省略号");
        assert_eq!(job.text, "很长的单行文本");
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.sections[0].format.font_id, font);

        // 标题：只允许一行
        let job = overflow_layout("一个非常非常长的标题", font.clone(), color, 120.0, 1);
        assert_eq!(job.wrap.max_rows, 1, "标题只保留一行（超出截断）");

        // 负数宽度不应 panic，按 0 处理
        let job = overflow_layout("x", font, color, -10.0, 1);
        assert_eq!(job.wrap.max_width, 0.0);
    }

    /// 自定义 preview 的配套小工具：零尺寸矩形 + 退出 Pin 的唯一条件。
    #[test]
    fn zero_rect_and_pinning_command_whitelist() {
        let point = Point::new(120.0, 340.0);
        let rect = zero_rect_at(point);
        assert_eq!(rect, Rect::new(120.0, 340.0, 0.0, 0.0));
        assert_eq!(rect.width, 0.0, "大小为零（用户要求）");
        assert_eq!(rect.center(), point, "以该点为中心");

        // 只有切到"选字"或"截图"模式才退出 Pin
        assert!(pin_exits_on_mode_change(true, false), "切到选字 → 退出");
        assert!(pin_exits_on_mode_change(false, true), "切到截图 → 退出");
        assert!(pin_exits_on_mode_change(true, true));
        assert!(
            !pin_exits_on_mode_change(false, false),
            "缩放/翻页/侧栏/checkbox 等不改预览模式 → 保持 Pin"
        );
    }

    /// 拖动浮窗：平移后仍被夹在视图区域内（三个拖动入口共用同一规则）。
    #[test]
    fn drag_float_rect_translates_and_clamps() {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let min = egui::vec2(160.0, 90.0);
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0));

        // 正常平移
        let moved = drag_float_rect(rect, egui::vec2(20.0, -30.0), canvas, min);
        assert_eq!(moved.min, egui::pos2(120.0, 70.0));
        assert_eq!(moved.size(), rect.size(), "拖动只改位置，不改尺寸");

        // 拖出右下 → 拉回区域内
        let moved = drag_float_rect(rect, egui::vec2(2000.0, 2000.0), canvas, min);
        assert!(moved.max.x <= canvas.max.x + 1e-3 && moved.max.y <= canvas.max.y + 1e-3);

        // 拖出左上 → 贴住左上角
        let moved = drag_float_rect(rect, egui::vec2(-2000.0, -2000.0), canvas, min);
        assert_eq!(moved.min, canvas.min);
    }

    /// 置顶：移到末尾，且不移乱其它项的相对顺序。
    #[test]
    fn move_to_last_keeps_the_relative_order_of_the_rest() {
        let mut items = vec!["a", "b", "c", "d"];
        move_to_last(&mut items, 1); // 把 b 置顶
        assert_eq!(items, vec!["a", "c", "d", "b"], "其余顺序不变（a, c, d）");

        move_to_last(&mut items, 0); // 把 a 置顶
        assert_eq!(items, vec!["c", "d", "b", "a"]);

        // 越界不 panic、不改变内容
        let before = items.clone();
        move_to_last(&mut items, 99);
        assert_eq!(items, before);
    }

    /// 同一目的地的固定浮窗查找：只认同来源的浮窗，普通浮窗（None）不参与。
    #[test]
    fn find_float_for_target_matches_only_pinned_previews() {
        let page = PageIndex::new(2);
        let origins = vec![None, Some((page, 0)), Some((page, 1))];

        assert_eq!(find_float_for_target(&origins, (page, 1)), Some(2));
        assert_eq!(find_float_for_target(&origins, (page, 0)), Some(1));
        assert_eq!(
            find_float_for_target(&origins, (page, 9)),
            None,
            "没固定过就没有"
        );
        assert_eq!(
            find_float_for_target(&origins, (PageIndex::new(3), 0)),
            None,
            "页不同不算"
        );
        assert_eq!(find_float_for_target(&[], (page, 0)), None);
    }

    /// 悬停预览的两个约束：滚动期间不出现；浮窗之下不出现。
    #[test]
    fn hover_preview_is_blocked_while_scrolling_and_under_floats() {
        assert!(hover_preview_allowed(false, false), "正常情况应出现");
        assert!(
            !hover_preview_allowed(true, false),
            "滚动/缩放静默窗口内不出现"
        );
        assert!(
            !hover_preview_allowed(false, true),
            "指针在浮窗上不出现（浮窗屏蔽主视图）"
        );
        assert!(!hover_preview_allowed(true, true));

        let rects = [
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 50.0)),
            egui::Rect::from_min_size(egui::pos2(200.0, 200.0), egui::vec2(100.0, 50.0)),
        ];
        assert!(pointer_over_any(egui::pos2(50.0, 30.0), &rects));
        assert!(pointer_over_any(egui::pos2(250.0, 220.0), &rects));
        assert!(
            !pointer_over_any(egui::pos2(150.0, 150.0), &rects),
            "浮窗之间不算遮挡"
        );
        assert!(
            !pointer_over_any(egui::pos2(50.0, 30.0), &[]),
            "没有浮窗时不遮挡"
        );
    }

    /// 临时预览浮窗：左上角放在链接框中心（保证与链接框重叠，鼠标移动路径连续）。
    #[test]
    fn tip_rect_starts_at_the_link_center_so_it_overlaps() {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let link = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(180.0, 24.0));
        let size = egui::vec2(360.0, 260.0);

        let tip = tip_rect_for(link, size, canvas);
        assert_eq!(tip.min, link.center(), "左上角 = 链接框中心");
        assert!(
            tip.intersects(link),
            "必须与链接框重叠（否则鼠标一移开预览就消失）"
        );
        assert!(link.contains(tip.min), "锚点落在链接框内");

        // 靠近右下边界时会夹取回区域内
        let edge_link = egui::Rect::from_min_size(egui::pos2(900.0, 760.0), egui::vec2(80.0, 24.0));
        let tip = tip_rect_for(edge_link, size, canvas);
        assert!(tip.max.x <= canvas.max.x + 1e-3 && tip.max.y <= canvas.max.y + 1e-3);
    }

    /// 建表与查询必须走同一个量化：链接 → 被引用点 的查找才不会落空。
    #[test]
    fn dest_lookup_uses_the_same_quantization_as_the_table() {
        let link = |page: u32, point: Point| Link {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            destination: Destination::Page {
                page: PageIndex::new(page),
                offset: Some(point),
            },
        };
        let links = vec![
            vec![
                link(1, Point::new(10.0, 20.0)),
                link(1, Point::new(300.0, 400.0)),
            ],
            Vec::new(),
        ];
        let points = destination_points(&links, 2);
        let first_page = &points[1];
        assert_eq!(first_page.len(), 2);

        // 用"略偏一点"的同一点去查，仍应命中（量化到同一格）
        assert_eq!(find_dest_index(first_page, Point::new(10.2, 20.2)), Some(0));
        assert_eq!(find_dest_index(first_page, Point::new(10.0, 20.0)), Some(0));
        assert_eq!(
            find_dest_index(first_page, Point::new(300.0, 400.0)),
            Some(1)
        );
        // 另一个点 → 未命中
        assert_eq!(find_dest_index(first_page, Point::new(50.0, 60.0)), None);
        // 量化函数本身：0.4 → 0.5，0.2 → 0.0
        assert_eq!(
            quantize_dest_point(Point::new(10.4, 20.2)),
            Point::new(10.5, 20.0)
        );
    }

    /// 被引用点的默认矩形：以该点为中心、尺寸固定；页面矩形→屏幕矩形要能来回对上。
    #[test]
    fn default_dest_rect_and_page_rect_to_screen() {
        let rect = default_dest_rect(Point::new(100.0, 200.0));
        assert_eq!(rect, Rect::new(40.0, 170.0, 120.0, 60.0));
        assert_eq!(rect.center(), Point::new(100.0, 200.0), "以该点为中心");

        // 页 200×100 放在内容空间 (10,20)，缩放 1、无旋转；滚动 (5,7)，画布原点 (300,40)
        let placement = PagePlacement::new(
            PageIndex::FIRST,
            Rect::new(10.0, 20.0, 200.0, 100.0),
            Scale::IDENTITY,
            Rotation::None,
            Size::new(200.0, 100.0),
        );
        let screen = page_rect_to_screen(
            &placement,
            Rect::new(30.0, 40.0, 50.0, 20.0),
            Vec2::new(5.0, 7.0),
            egui::pos2(300.0, 40.0),
        );
        // 内容空间 = 页内 + 页原点 = (40,60,50,20)；屏幕 = 内容 − 滚动 + 画布原点
        assert_eq!(
            screen,
            egui::Rect::from_min_size(egui::pos2(335.0, 93.0), egui::vec2(50.0, 20.0))
        );
    }

    /// 回归：屏幕坐标 → 内容空间必须减去画布原点；且框选区域截到的内容必然**落在当前可见范围内**。
    #[test]
    fn screen_rect_to_content_subtracts_the_canvas_origin() {
        // 画布左上角在屏幕 (265, 40)（左侧有 260px 侧栏 + 5px 拖拽条）
        let canvas_min = egui::pos2(265.0, 40.0);
        let selection =
            egui::Rect::from_min_size(egui::pos2(365.0, 140.0), egui::vec2(100.0, 50.0));
        let scroll = Vec2::new(1000.0, 2000.0);

        let content = screen_rect_to_content(canvas_min, selection, scroll);
        assert_eq!(content, Rect::new(1100.0, 2100.0, 100.0, 50.0));

        // 反例：漏掉 canvas.min 会得到这个（正是缺陷时的取值）
        let buggy = Rect::new(
            selection.min.x + scroll.x,
            selection.min.y + scroll.y,
            selection.width(),
            selection.height(),
        );
        assert_ne!(content, buggy, "漏减画布原点会整体偏移 canvas.min");

        // 关键性质：**框选区域截到的内容必然落在当前可见范围内**（用户实测的判据）
        let viewport = Size::new(800.0, 600.0);
        let visible = Rect::new(scroll.x, scroll.y, viewport.width, viewport.height);
        let inside = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(100.0, 50.0));
        let content = screen_rect_to_content(canvas_min, inside, scroll);
        assert!(
            visible.intersection(content).is_some(),
            "截图内容 {content:?} 必须落在可见范围 {visible:?} 内"
        );
    }

    /// 快照合成：白底、按页裁剪、支持整页与分块两种来源。
    #[test]
    fn compose_snapshot_blits_pages_tiles_and_leaves_white_outside() {
        let make = |width: u32, height: u32, fill: u8| {
            let mut data = Vec::with_capacity((width * height * 4) as usize);
            for _ in 0..(width * height) {
                data.extend_from_slice(&[fill, fill, fill, 255]); // BGRA：灰
            }
            RasterImage::new(
                width,
                height,
                PixelFormat::Bgra8,
                std::sync::Arc::from(data),
            )
            .expect("位图")
        };

        // 整页来源：页 4×4 放在内容空间 (10,10)，整页填 100
        let whole = make(4, 4, 100);
        let pages = vec![SnapshotPage {
            rect: Rect::new(10.0, 10.0, 4.0, 4.0),
            whole: Some(&whole),
            tiles: Vec::new(),
        }];

        // 区域完全落在页内 → 全部为 100
        let snapshot = compose_snapshot(&pages, Rect::new(11.0, 11.0, 2.0, 2.0)).expect("应有内容");
        assert_eq!(snapshot.size, [2, 2]);
        assert!(snapshot.pixels.iter().all(|pixel| pixel.r() == 100));

        // 区域跨出页面 → 页外部分留白（255）
        let snapshot = compose_snapshot(&pages, Rect::new(12.0, 12.0, 4.0, 4.0)).expect("应有内容");
        assert_eq!(snapshot.size, [4, 4]);
        assert_eq!(snapshot.pixels[0].r(), 100, "页内像素应拷贝过来");
        assert_eq!(snapshot.pixels[3 * 4 + 3].r(), 255, "页外像素应为白");

        // 完全在页外 → 无可用像素
        assert!(compose_snapshot(&pages, Rect::new(100.0, 100.0, 4.0, 4.0)).is_none());

        // 分块来源：两块各占页宽的一半（块的位置由 TileGrid 给出，这里直接给内容空间矩形）
        let left = make(512, 512, 50);
        let right = make(512, 512, 200);
        let tiled = vec![SnapshotPage {
            rect: Rect::new(0.0, 0.0, 1024.0, 512.0),
            whole: None,
            tiles: vec![
                (Rect::new(0.0, 0.0, 512.0, 512.0), &left),
                (Rect::new(512.0, 0.0, 512.0, 512.0), &right),
            ],
        }];
        let snapshot =
            compose_snapshot(&tiled, Rect::new(0.0, 0.0, 1024.0, 512.0)).expect("应有内容");
        assert_eq!(snapshot.size, [1024, 512]);
        assert_eq!(snapshot.pixels[0].r(), 50, "左块覆盖 x<512");
        assert_eq!(snapshot.pixels[511].r(), 50);
        assert_eq!(snapshot.pixels[512].r(), 200, "右块覆盖 x≥512");
        assert_eq!(snapshot.pixels[1024 * 10].r(), 50, "第二行左侧仍是左块");
        assert_eq!(
            snapshot.pixels[1024 * 10 + 512].r(),
            200,
            "第二行右侧仍是右块"
        );

        // 多页 + 页间距：中间留白
        let top = make(2, 2, 10);
        let bottom = make(2, 2, 20);
        let stacked = vec![
            SnapshotPage {
                rect: Rect::new(0.0, 0.0, 2.0, 2.0),
                whole: Some(&top),
                tiles: Vec::new(),
            },
            SnapshotPage {
                rect: Rect::new(0.0, 3.0, 2.0, 2.0),
                whole: Some(&bottom),
                tiles: Vec::new(),
            },
        ];
        let snapshot = compose_snapshot(&stacked, Rect::new(0.0, 0.0, 2.0, 5.0)).expect("应有内容");
        assert_eq!(snapshot.size, [2, 5]);
        assert_eq!(snapshot.pixels[0].r(), 10, "第一页");
        assert_eq!(snapshot.pixels[2 * 2].r(), 255, "页间距应留白");
        assert_eq!(snapshot.pixels[3 * 2].r(), 20, "第二页");
    }

    /// 波段超采样（`u = 显示/渲染 < 1`）：位图比它铺满的内容矩形**大**。
    ///
    /// 这是 D-94 之后必须成立的一条：快照按"铺满内容矩形"重采样，
    /// 因此整页图与块都不会因为渲染比例更高而只截到左上角一小块。
    #[test]
    fn compose_snapshot_scales_supersampled_bitmaps_to_fit() {
        let make = |width: u32, height: u32, fill: u8| {
            let mut data = Vec::with_capacity((width * height * 4) as usize);
            for _ in 0..(width * height) {
                data.extend_from_slice(&[fill, fill, fill, 255]);
            }
            RasterImage::new(
                width,
                height,
                PixelFormat::Bgra8,
                std::sync::Arc::from(data),
            )
            .expect("位图")
        };

        // 页内容 100×100，整页位图按 1.6 倍渲染成 160×160
        let whole = make(160, 160, 90);
        let pages = vec![SnapshotPage {
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            whole: Some(&whole),
            tiles: Vec::new(),
        }];
        let snapshot =
            compose_snapshot(&pages, Rect::new(0.0, 0.0, 100.0, 100.0)).expect("应有内容");
        assert_eq!(snapshot.size, [100, 100]);
        assert!(
            snapshot.pixels.iter().all(|pixel| pixel.r() == 90),
            "整页必须铺满（右下角也不能留白）"
        );

        // 块：内容空间各占一半，位图同样按 1.6 倍更大 → 仍然各占一半
        let left = make(160, 160, 10);
        let right = make(160, 160, 20);
        let tiled = vec![SnapshotPage {
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            whole: None,
            tiles: vec![
                (Rect::new(0.0, 0.0, 50.0, 100.0), &left),
                (Rect::new(50.0, 0.0, 50.0, 100.0), &right),
            ],
        }];
        let snapshot =
            compose_snapshot(&tiled, Rect::new(0.0, 0.0, 100.0, 100.0)).expect("应有内容");
        assert_eq!(snapshot.pixels[0].r(), 10, "左块覆盖 x<50");
        assert_eq!(snapshot.pixels[49].r(), 10);
        assert_eq!(snapshot.pixels[50].r(), 20, "右块覆盖 x≥50");
        assert_eq!(snapshot.pixels[99 * 100 + 99].r(), 20, "右下角属于右块");
    }

    /// 图像按保持长宽比放进窗口区域（居中、不拉伸）。
    #[test]
    fn fit_inside_preserves_aspect_ratio() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));

        // 宽图 → 受宽度限制，高度按比例
        let fitted = fit_inside(egui::vec2(400.0, 100.0), area);
        assert_eq!(fitted.width(), 200.0);
        assert_eq!(fitted.height(), 50.0);
        assert_eq!(fitted.center(), area.center());

        // 高图 → 受高度限制
        let fitted = fit_inside(egui::vec2(100.0, 400.0), area);
        assert_eq!(fitted.height(), 100.0);
        assert_eq!(fitted.width(), 25.0);

        // 退化输入不 panic
        assert_eq!(fit_inside(egui::vec2(0.0, 0.0), area), area);
    }

    /// 截图遮罩：选区内部必须**不被**蒙层覆盖（即只有四块绕开选区的矩形）。
    #[test]
    fn veil_bands_leave_the_selection_transparent() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 80.0));

        // 无选区 → 整块蒙层
        assert_eq!(veil_bands(area, None), vec![area]);

        // 中间选区 → 四块，且没有任何一块覆盖选区中心
        let hole = egui::Rect::from_min_size(egui::pos2(30.0, 20.0), egui::vec2(40.0, 30.0));
        let bands = veil_bands(area, Some(hole));
        assert_eq!(bands.len(), 4);
        for band in &bands {
            assert!(!band.contains(hole.center()), "蒙层不得覆盖选区中心");
        }
        // 四块面积之和 = 总面积 − 选区面积
        let covered: f32 = bands.iter().map(|band| band.width() * band.height()).sum();
        let expected = area.width() * area.height() - hole.width() * hole.height();
        assert!(
            (covered - expected).abs() < 1e-3,
            "覆盖面积 {covered} vs {expected}"
        );

        // 选区贴住左边 → 只剩 3 块（左块宽度为 0，不产生）
        let edge = egui::Rect::from_min_size(egui::pos2(0.0, 20.0), egui::vec2(40.0, 30.0));
        assert_eq!(veil_bands(area, Some(edge)).len(), 3);

        // 选区完全在区域外 → 退回整块蒙层
        let outside = egui::Rect::from_min_size(egui::pos2(500.0, 500.0), egui::vec2(10.0, 10.0));
        assert_eq!(veil_bands(area, Some(outside)), vec![area]);
    }

    /// 截图区域 → PDF 页面空间：只取与该页的交集，且坐标归一化。
    #[test]
    fn view_rect_to_page_rect_maps_and_clips() {
        // 页 200×100 放在内容空间 (10, 20)，缩放 1、无旋转
        let placement = PagePlacement::new(
            PageIndex::FIRST,
            Rect::new(10.0, 20.0, 200.0, 100.0),
            Scale::IDENTITY,
            Rotation::None,
            Size::new(200.0, 100.0),
        );

        // 滚动为 0 时，视图矩形 (30,40,50,20) → 内容 (30,40,50,20) → 页内 (20,20)
        let page_rect =
            view_rect_to_page_rect(&placement, Rect::new(30.0, 40.0, 50.0, 20.0), Vec2::ZERO)
                .expect("应命中页面");
        assert!((page_rect.x - 20.0).abs() < 1e-3);
        assert!((page_rect.width - 50.0).abs() < 1e-3);

        // 滚动会平移内容：scroll=(10,10) 时同一视图矩形对应的页内区域右移下移
        let scrolled = view_rect_to_page_rect(
            &placement,
            Rect::new(30.0, 40.0, 50.0, 20.0),
            Vec2::new(10.0, 10.0),
        )
        .expect("应命中页面");
        assert!((scrolled.x - 30.0).abs() < 1e-3, "实际 {}", scrolled.x);

        // 完全在页外 → None
        assert!(view_rect_to_page_rect(
            &placement,
            Rect::new(500.0, 500.0, 10.0, 10.0),
            Vec2::ZERO
        )
        .is_none());
    }

    /// 引用点：按目标页分组、按位置去重、无形坐标的目标不产生标记。
    #[test]
    fn destination_points_group_by_page_and_dedupe_by_position() {
        let link = |destination: Destination| Link {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            destination,
        };
        let links_by_page = vec![
            vec![
                // 两个链接指向**同一点** → 只记一次
                link(Destination::Page {
                    page: PageIndex::new(1),
                    offset: Some(Point::new(10.0, 20.0)),
                }),
                link(Destination::Page {
                    page: PageIndex::new(1),
                    offset: Some(Point::new(10.2, 20.2)),
                }),
                // 另一个点 → 另记一次
                link(Destination::Page {
                    page: PageIndex::new(1),
                    offset: Some(Point::new(300.0, 400.0)),
                }),
                // 只有页、没有坐标 → 不产生标记
                link(Destination::Page {
                    page: PageIndex::new(2),
                    offset: None,
                }),
                // 外部 URI → 忽略
                link(Destination::Uri("https://example.com".into())),
            ],
            // 第 2 页上的链接也可以指向第 1 页（同样落在同一个 0.5pt 网格上 → 仍视为同一点）
            vec![link(Destination::Page {
                page: PageIndex::new(1),
                offset: Some(Point::new(10.2, 20.2)),
            })],
        ];

        let points = destination_points(&links_by_page, 3);
        assert_eq!(points.len(), 3, "按目标页分桶");
        assert_eq!(
            points[1],
            vec![Point::new(10.0, 20.0), Point::new(300.0, 400.0)],
            "同一点去重（10.2/20.2 量化到 0.5pt 网格后仍是 10.0/20.0），不同点保留"
        );
        assert!(points[2].is_empty(), "无坐标的目标不产生标记");
        assert!(points[0].is_empty());

        // 越界目标丢弃，不 panic
        let points = destination_points(&links_by_page, 1);
        assert_eq!(points.len(), 1);
    }

    /// 浮窗：夹取到视图区域内、不小于最小尺寸、区域过小时不产生负尺寸。
    #[test]
    fn clamp_float_rect_keeps_the_window_inside_the_view() {
        let rect = |x: f32, y: f32, w: f32, h: f32| {
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
        };
        let area = rect(100.0, 50.0, 800.0, 600.0);

        // 拖到右下角外 → 拉回区域内部
        let clamped = clamp_float_rect(rect(2000.0, 2000.0, 300.0, 200.0), area, FLOAT_MIN_SIZE);
        assert!(clamped.max.x <= area.max.x + 1e-3 && clamped.max.y <= area.max.y + 1e-3);
        assert!(clamped.min.x >= area.min.x && clamped.min.y >= area.min.y);

        // 拖到左上角外 → 拉回区域内
        let clamped = clamp_float_rect(rect(-500.0, -500.0, 300.0, 200.0), area, FLOAT_MIN_SIZE);
        assert_eq!((clamped.min.x, clamped.min.y), (area.min.x, area.min.y));

        // 太小 → 抬到最小尺寸
        let clamped = clamp_float_rect(rect(200.0, 100.0, 10.0, 10.0), area, FLOAT_MIN_SIZE);
        assert_eq!(
            (clamped.width(), clamped.height()),
            (FLOAT_MIN_SIZE.x, FLOAT_MIN_SIZE.y)
        );

        // 比区域还大 → 收缩到区域尺寸
        let clamped = clamp_float_rect(rect(100.0, 50.0, 5000.0, 5000.0), area, FLOAT_MIN_SIZE);
        assert_eq!(
            (clamped.width(), clamped.height()),
            (area.width(), area.height())
        );

        // 区域本身比最小尺寸还小 → 不得出现负尺寸
        let tiny = rect(0.0, 0.0, 40.0, 30.0);
        let clamped = clamp_float_rect(rect(0.0, 0.0, 200.0, 200.0), tiny, FLOAT_MIN_SIZE);
        assert!(clamped.width() >= 0.0 && clamped.height() >= 0.0);
        assert_eq!(clamped.width(), tiny.width());
    }

    /// 回归：指针不在画布上（例如在侧栏里滚动）时，滚轮**不得**作用于文档视图。
    #[test]
    fn wheel_targets_canvas_only_when_the_pointer_is_over_it() {
        assert!(wheel_targets_canvas(true), "指针在画布上 → 滚动文档");
        assert!(
            !wheel_targets_canvas(false),
            "指针在侧栏/工具栏上 → 不滚动文档"
        );
    }

    /// 位图 → ColorImage：一遍完成、通道顺序正确、尺寸匹配。
    #[test]
    fn color_image_converts_bgra_in_one_pass() {
        // 2×2 BGRA：红 / 绿 / 蓝 / 半透明白
        let data: Vec<u8> = vec![
            0, 0, 255, 255, // BGRA 红
            0, 255, 0, 255, // BGRA 绿
            255, 0, 0, 255, // BGRA 蓝
            255, 255, 255, 128,
        ];
        let image = color_image(PixelFormat::Bgra8, &data, 2, 2);
        assert_eq!(image.size, [2, 2]);
        assert_eq!(image.pixels.len(), 4);
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(image.pixels[1], egui::Color32::from_rgb(0, 255, 0));
        assert_eq!(image.pixels[2], egui::Color32::from_rgb(0, 0, 255));
        assert_eq!(image.pixels[3].a(), 128, "alpha 必须保留");

        // RGBA 走另一条分支
        let rgba: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0];
        let image = color_image(PixelFormat::Rgba8, &rgba, 2, 2);
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(255, 0, 0));

        // 灰度：R=G=B
        let gray: Vec<u8> = vec![10, 200, 0, 255];
        let image = color_image(PixelFormat::Gray8, &gray, 2, 2);
        assert_eq!(image.pixels[1], egui::Color32::from_rgb(200, 200, 200));

        // 数据不足时不得越界（按可用像素截断）
        let short = color_image(PixelFormat::Bgra8, &data[..4], 2, 2);
        assert_eq!(short.pixels.len(), 1, "不足的像素不应被凭空补齐");
    }

    /// 大纲摊平：顺序、层级、目标都要原样保留（虚拟化行渲染依赖它）。
    #[test]
    fn flatten_outline_keeps_order_depth_and_destination() {
        let leaf = |title: &str, page: u32| OutlineNode {
            title: title.to_string(),
            destination: Some(Destination::Page {
                page: PageIndex::new(page),
                offset: None,
            }),
            children: Vec::new(),
        };
        let mut root = leaf("章 1", 0);
        root.children = vec![leaf("节 1.1", 1), leaf("节 1.2", 2)];
        let nodes = vec![root, leaf("章 2", 3)];

        let rows = flatten_outline(&nodes);
        assert_eq!(rows.len(), outline_node_count(&nodes), "行数应等于节点总数");
        assert_eq!(
            rows.iter()
                .map(|(depth, title, _)| (*depth, title.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "章 1"), (1, "节 1.1"), (1, "节 1.2"), (0, "章 2"),],
            "深度优先、父在前、层级正确"
        );
        assert_eq!(
            rows[1].2,
            Some(Destination::Page {
                page: PageIndex::new(1),
                offset: None
            }),
            "目标必须保留（点击跳转要用）"
        );
        assert!(flatten_outline(&[]).is_empty());
    }

    /// 大纲节点计数（诊断用：侧栏一次布局上百行会稳定吃掉几十毫秒/帧）。
    #[test]
    fn outline_node_count_counts_nested_nodes() {
        let leaf = |title: &str| OutlineNode {
            title: title.to_string(),
            destination: None,
            children: Vec::new(),
        };
        let mut root = leaf("章 1");
        root.children = vec![leaf("节 1.1"), leaf("节 1.2")];
        let nodes = vec![root, leaf("章 2")];
        assert_eq!(
            outline_node_count(&nodes),
            4,
            "根 1 + 它的 2 个子节点 + 根 2 = 4"
        );
        assert_eq!(outline_node_count(&[]), 0);
    }

    /// 上传与裁剪必须共用同一集合，否则相邻页纹理会被"上传→裁掉→再上传"地抖动。
    #[test]
    fn texture_pages_covers_visible_plus_neighbours() {
        let visible = [PageIndex::new(4)];
        let pages = texture_pages(&visible, PageIndex::new(4), 10);
        assert_eq!(
            pages,
            vec![PageIndex::new(3), PageIndex::new(4), PageIndex::new(5)],
            "可见页 ±1 都要（上传与裁剪共用）"
        );

        // 首尾越界自动裁剪
        assert_eq!(
            texture_pages(&[PageIndex::FIRST], PageIndex::FIRST, 10),
            vec![PageIndex::FIRST, PageIndex::new(1)]
        );
        let last = PageIndex::new(9);
        assert_eq!(
            texture_pages(&[last], last, 10),
            vec![PageIndex::new(8), last]
        );

        // 当前页不在可见集合里时也要保留
        let pages = texture_pages(&[PageIndex::new(4)], PageIndex::new(7), 10);
        assert!(pages.contains(&PageIndex::new(7)), "当前页必须保留");

        // 多页可见时不重复
        let pages = texture_pages(
            &[PageIndex::new(4), PageIndex::new(5)],
            PageIndex::new(4),
            10,
        );
        assert_eq!(
            pages,
            vec![
                PageIndex::new(3),
                PageIndex::new(4),
                PageIndex::new(5),
                PageIndex::new(6)
            ]
        );
        assert_eq!(pages.len(), 4, "不得有重复项");
    }

    /// 回归：画布必须保留**全部剩余高度**。
    ///
    /// 第五轮缺陷：拖拽条用 `allocate_painter(5px × 整高)` 画，纵向布局把整高吃光，
    /// 画布高度变成 0 → "分割线能拖，但没有任何图像"。
    #[test]
    fn canvas_rect_keeps_full_height() {
        let full = egui::Rect::from_min_size(egui::pos2(260.0, 40.0), egui::vec2(840.0, 800.0));

        let canvas = canvas_rect_for(full, 5.0);
        assert_eq!(canvas.min.x, 265.0, "画布应从拖拽条右侧开始");
        assert_eq!(canvas.min.y, 40.0);
        assert_eq!(canvas.width(), 835.0, "只让出拖拽条的宽度");
        assert_eq!(
            canvas.height(),
            800.0,
            "高度必须保持满高（这正是当时的缺陷）"
        );

        // 侧栏隐藏时不让位
        assert_eq!(canvas_rect_for(full, 0.0), full);

        // 区域比拖拽条还窄时不得出现负尺寸
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(3.0, 10.0));
        let canvas = canvas_rect_for(tiny, 5.0);
        assert!(
            canvas.width() >= 0.0 && canvas.height() >= 0.0,
            "实际 {canvas:?}"
        );
        assert_eq!(canvas.height(), 10.0, "仍要保持满高");
    }

    /// 缩放过场要用的"上一档"必须保留，但比例不能无限堆积（第五轮验收：相邻页闪白）。
    #[test]
    fn retain_keeps_current_and_one_old_scale() {
        let page = PageIndex::new(0);
        let other = PageIndex::new(1);
        // 用**瓦片**表达"旧档"：整页图另有"全留"的规则（D-110），不能混在一起测
        let key = |page: PageIndex, scale: f32, rotation: Rotation| {
            RenderKey::tiled(page, Scale::from_zoom(scale), rotation, TileSpec::new(0, 0))
        };

        let mut store = KeyedStore::new(64);
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            store.insert(key(page, scale, Rotation::None), scale);
        }
        store.insert(key(other, 1.5, Rotation::None), 1.5);
        store.insert(key(other, 2.0, Rotation::None), 2.0);
        store.insert(key(PageIndex::new(2), 2.0, Rotation::None), 2.0);
        // 旧旋转的图留着只会画成横躺
        store.insert(key(page, 2.0, Rotation::Clockwise90), 2.0);

        store.retain_for_view(
            &[
                facts(page, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
                facts(other, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
            ],
            |_| 0,
        );

        assert!(
            store.contains(&key(page, 2.0, Rotation::None)),
            "当前比例必须保留"
        );
        assert!(
            store.contains(&key(page, 1.5, Rotation::None)),
            "最近的旧比例必须保留（缩放过场要用它顶替，否则闪白）"
        );
        assert!(
            !store.contains(&key(page, 1.25, Rotation::None)),
            "更旧的档应丢弃"
        );
        assert!(!store.contains(&key(page, 1.0, Rotation::None)));
        assert!(!store.contains(&key(page, 3.0, Rotation::None)));
        // 高于当前比例的**整页图**没有用（缩小用不到），丢掉
        store.insert(
            RenderKey::new(page, Scale::from_zoom(3.0), Rotation::None),
            3.0,
        );
        store.retain_for_view(
            &[
                facts(page, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
                facts(other, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
            ],
            |_| 0,
        );
        assert!(
            !store.contains(&RenderKey::new(page, Scale::from_zoom(3.0), Rotation::None)),
            "高于当前比例的整页图应丢弃"
        );
        assert!(
            !store.contains(&key(page, 2.0, Rotation::Clockwise90)),
            "旧旋转必须丢弃"
        );
        assert!(
            !store.contains(&key(PageIndex::new(2), 2.0, Rotation::None)),
            "不在保留列表里的页应丢弃"
        );
        // 其它可见页同样"当前 + 最近旧档"
        assert!(store.contains(&key(other, 2.0, Rotation::None)));
        assert!(store.contains(&key(other, 1.5, Rotation::None)));
    }
}
