//! 纯逻辑层：视图坐标换算、输入映射、纹理存储。
//!
//! 这些都不需要窗口/GPU，因此**可以被 `cargo test` 覆盖**；
//! 需要真实窗口与人工观察的部分（滚动跟手性、字形、配色）全部留给 `app.rs` 的 egui 胶水，
//! 按 agent.md 规则 2 交由用户验收。

use std::collections::{HashMap, VecDeque};

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
        Key::Space => Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP * 4.0))),
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
        let factor = if delta.y > 0.0 { WHEEL_ZOOM_FACTOR } else { 1.0 / WHEEL_ZOOM_FACTOR };
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

/// 瓦片在**视图空间**中的矩形。
///
/// ## 关键不变量（黑屏缺陷的根源，D-17）
///
/// 内容空间单位在当前缩放下**就等于该页位图的像素**：`PagePlacement::rect` 已经是
/// `页点数 × 缩放`，而 `PagePlacement::pixel_size()` 就是取整后的 `rect.size()`。
/// 因此把"位图像素偏移"换算成视图偏移时 **直接相加即可，绝不能再乘/除缩放**。
///
/// v1 在这里写了 `pixels.x / scale`：748% 时 6 块瓦片被压缩到页面左上角 1/7 的小方格里，
/// 而视口落在页面中部 —— 于是"画法=瓦片 6/6"却整块画布全黑（第三轮验收）。
pub fn tile_view_rect(page_rect: Rect, pixels: Rect) -> Rect {
    Rect::new(
        page_rect.x + pixels.x,
        page_rect.y + pixels.y,
        pixels.width,
        pixels.height,
    )
}

/// 把**旧缩放的瓦片**映射到当前页面矩形上（缩放过场的预览用）。
///
/// 高倍缩放下缓存里只有瓦片、没有整页图，因此"上一档的旧图"要用旧瓦片拼出来：
/// 每块旧瓦片按 `新页像素 / 旧页像素` 的比例缩放后贴到页面矩形的对应位置。
pub fn tile_view_rect_scaled(
    page_rect: Rect,
    pixels: Rect,
    old_page: Size,
    new_page: Size,
) -> Rect {
    let sx = if old_page.width > 0.0 { new_page.width / old_page.width } else { 1.0 };
    let sy = if old_page.height > 0.0 { new_page.height / old_page.height } else { 1.0 };
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
                commands.push(ViewerCommand::CenterOnPagePoint { page: *page, point: *point });
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
        bands.push(egui::Rect::from_min_max(area.min, egui::pos2(area.max.x, hole.min.y)));
    }
    // 下
    if hole.max.y < area.max.y {
        bands.push(egui::Rect::from_min_max(egui::pos2(area.min.x, hole.max.y), area.max));
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
    let height = rect.height().max(min_size.y.min(max_height)).min(max_height);
    let x = rect.min.x.clamp(area.min.x, (area.max.x - width).max(area.min.x));
    let y = rect.min.y.clamp(area.min.y, (area.max.y - height).max(area.min.y));
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, height))
}

/// 收集**被引用的点**（链接跳转目的地），按目标页分组，并按位置去重。
///
/// * 一条链接只在自己带**具体坐标**（`Destination::Page.offset`）时算一个引用点；
///   只有页、没有坐标的目标不产生标记（没有具体位置可标）；
/// * 大量链接可能指向同一个点，因此按 0.5pt 网格量化后去重（用户要求）；
/// * 返回 `Vec` 的下标就是目标页号，越界的目标丢弃。
pub fn destination_points(links_by_page: &[Vec<Link>], page_count: usize) -> Vec<Vec<Point>> {
    /// 去重网格：0.5pt（远小于一个字，肉眼视为同一点）。
    const QUANTUM: f32 = 0.5;

    let mut out: Vec<Vec<Point>> = vec![Vec::new(); page_count];
    for links in links_by_page {
        for link in links {
            let Destination::Page { page, offset: Some(point) } = &link.destination else {
                continue;
            };
            let Some(bucket) = out.get_mut(page.get() as usize) else { continue };
            let quantized = Point::new(
                (point.x / QUANTUM).round() * QUANTUM,
                (point.y / QUANTUM).round() * QUANTUM,
            );
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
pub fn color_image(
    format: PixelFormat,
    data: &[u8],
    width: u32,
    height: u32,
) -> egui::ColorImage {
    let (w, h) = (width as usize, height as usize);
    let mut pixels = Vec::with_capacity(w * h);
    match format {
        PixelFormat::Bgra8 => {
            for px in data.chunks_exact(4).take(w * h) {
                pixels.push(egui::Color32::from_rgba_premultiplied(px[2], px[1], px[0], px[3]));
            }
        }
        PixelFormat::Rgba8 => {
            for px in data.chunks_exact(4).take(w * h) {
                pixels.push(egui::Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]));
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
#[derive(Debug)]
pub struct KeyedStore<V> {
    entries: HashMap<RenderKey, V>,
    order: VecDeque<RenderKey>,
    capacity: usize,
}

impl<V> KeyedStore<V> {
    /// 构造（容量至少为 1）。
    pub fn new(capacity: usize) -> Self {
        Self { entries: HashMap::new(), order: VecDeque::new(), capacity: capacity.max(1) }
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 是否存在。
    pub fn contains(&self, key: &RenderKey) -> bool {
        self.entries.contains_key(key)
    }

    /// 查询。
    pub fn get(&self, key: &RenderKey) -> Option<&V> {
        self.entries.get(key)
    }

    /// 写入并维护 LRU 顺序；超出容量时淘汰最旧的。
    pub fn insert(&mut self, key: RenderKey, value: V) {
        self.entries.insert(key, value);
        self.touch(key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else { break };
            self.entries.remove(&oldest);
        }
    }

    /// 丢弃所有不满足 `keep` 的条目（用于"只保留可见页的纹理"）。
    pub fn retain(&mut self, mut keep: impl FnMut(&RenderKey) -> bool) {
        self.entries.retain(|key, _| keep(key));
        self.order.retain(|key| self.entries.contains_key(key));
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
            if best.map_or(true, |current| distance < current.abs_diff(target_scale_milli)) {
                best = Some(key.scale_milli);
            }
        }
        best
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

    /// 裁剪纹理：只保留这些页在 `current_scale_milli` 与**每页最接近它的另一个比例**上的条目。
    ///
    /// 两个方向都要顾：
    /// * **不能全丢**：缩放过场要用上一档的旧内容顶上，否则会闪白 ——
    ///   v1 在 `LayoutChanged` 时只留"当前页"的纹理，于是中心页正常、相邻页闪白
    ///   （第四轮验收反馈的现象）；
    /// * **不能全留**：一次连续缩放会产生十几档比例，显存会无限增长。
    ///
    /// 旋转也必须匹配：旧旋转的图是横躺的，留着只会画错。
    pub fn retain_preview_scales(
        &mut self,
        pages: &[(PageIndex, Rotation)],
        current_scale_milli: u32,
    ) {
        // 注意：旋转必须**按页**取（同一份文档里各页的 /Rotate 可能不同），
        // 否则含混合旋转的文档里，与"当前页"旋转不同的那些页纹理会被误删（页面空白）。
        let rotation_of = |page: PageIndex| {
            pages
                .iter()
                .find(|(candidate, _)| *candidate == page)
                .map(|(_, rotation)| *rotation)
        };

        // 每页选出"最接近当前比例的另一个比例"
        let mut alternate: HashMap<PageIndex, u32> = HashMap::new();
        for key in self.entries.keys() {
            if rotation_of(key.page) != Some(key.rotation)
                || key.scale_milli == current_scale_milli
            {
                continue;
            }
            let distance = key.scale_milli.abs_diff(current_scale_milli);
            match alternate.get(&key.page) {
                Some(existing) if existing.abs_diff(current_scale_milli) <= distance => {}
                _ => {
                    alternate.insert(key.page, key.scale_milli);
                }
            }
        }

        self.retain(|key| {
            rotation_of(key.page) == Some(key.rotation)
                && (key.scale_milli == current_scale_milli
                    || alternate.get(&key.page) == Some(&key.scale_milli))
        });
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
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

    /// 回归测试：瓦片位置**不得**再除以缩放。
    ///
    /// 数字取自第三轮验收的真实诊断行（748% 下的第 2 页）：
    /// `视图矩形=(-1679,-2042 3870×4785)`，网格里的第 (1,1) 块瓦片。
    /// 正确结果是"页面左上角 + (512,512)"；错误实现会把它压到 +69.4 附近 ——
    /// 视口落在页面中部时就什么都看不到（全黑）。
    #[test]
    fn tile_view_rect_is_in_content_units_not_scaled() {
        let page_rect = Rect::new(-1679.0, -2042.0, 3870.0, 4785.0);
        let pixels = Rect::new(512.0, 512.0, 512.0, 512.0);
        let rect = tile_view_rect(page_rect, pixels);
        assert_eq!(rect, Rect::new(-1167.0, -1530.0, 512.0, 512.0));

        // 反面：任何"除以缩放"的实现都会得到这个值（748% ≈ 7.38 倍）
        let wrong = Rect::new(
            page_rect.x + pixels.x / 7.38,
            page_rect.y + pixels.y / 7.38,
            pixels.width / 7.38,
            pixels.height / 7.38,
        );
        assert_ne!(rect, wrong, "瓦片位置一旦除缩放，瓦片会被压到页面左上角");
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
        assert_eq!(command_for_key(Key::Num0, none, 3), Some(ViewerCommand::SetZoom(1.0)));
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
        assert_eq!(command_for_zoom_delta(f32::NAN, 2.0), None, "异常值必须被忽略");

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
        assert_eq!(store.best_page_texture(PageIndex::new(0), Rotation::None, 2100), Some(&"two"));
        // 目标 1.1 → 1.0
        assert_eq!(store.best_page_texture(PageIndex::new(0), Rotation::None, 1100), Some(&"one"));
        // 其它页不参与
        assert_eq!(store.best_page_texture(PageIndex::new(2), Rotation::None, 2000), None);
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
            !store.has_tiles(&RenderKey::new(PageIndex::new(1), Scale::from_zoom(2.0), Rotation::None)),
            "其它页的瓦片不算"
        );
        assert!(
            !store.has_tiles(&RenderKey::new(PageIndex::new(0), Scale::from_zoom(1.5), Rotation::None)),
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
        let tile = |scale: f32, col: u16, row: u16| {
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
        assert_eq!(store.best_tile_scale(PageIndex::new(0), Rotation::None, 2000), Some(1000));
        // 当前档 1.0 → 2.0
        assert_eq!(store.best_tile_scale(PageIndex::new(0), Rotation::None, 1000), Some(2000));
        // 其它页 / 其它旋转都没有
        assert_eq!(store.best_tile_scale(PageIndex::new(1), Rotation::None, 2000), None);
        assert_eq!(store.best_tile_scale(PageIndex::new(0), Rotation::Clockwise90, 2000), None);

        let old = store.tiles_at(PageIndex::new(0), Rotation::None, 1000);
        assert_eq!(old.len(), 2, "旧档应有 2 块");
        assert!(old
            .iter()
            .any(|(tile, value)| *tile == TileSpec::new(0, 0) && **value == "old-a"));
        assert_eq!(store.tiles_at(PageIndex::new(0), Rotation::None, 2000).len(), 1);
    }

    /// 诊断助手：判断热区是否越界（越界即坐标空间不一致的证据）。
    #[test]
    fn link_rect_in_page_bounds_detects_coordinate_space_mismatch() {
        let page = Size::new(612.0, 792.0);
        assert!(link_rect_in_page_bounds(Rect::new(72.0, 700.0, 200.0, 20.0), page));
        // 浮点余量：贴边不算越界
        assert!(link_rect_in_page_bounds(Rect::new(0.0, 0.0, 612.0, 792.0), page));
        // 典型的"整体偏移"：MediaBox 原点不为 0 时，热区会跑到页外
        assert!(!link_rect_in_page_bounds(Rect::new(72.0, 800.0, 200.0, 20.0), page));
        assert!(!link_rect_in_page_bounds(Rect::new(-40.0, 100.0, 20.0, 20.0), page));
        assert!(!link_rect_in_page_bounds(Rect::new(600.0, 100.0, 100.0, 20.0), page));
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

        let page_only = Destination::Page { page: PageIndex::new(1), offset: None };
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
        assert!(!link_overlay_visible(true, f32::NAN, 0.75), "异常缩放值不画");
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
        assert!((covered - expected).abs() < 1e-3, "覆盖面积 {covered} vs {expected}");

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
        let page_rect = view_rect_to_page_rect(&placement, Rect::new(30.0, 40.0, 50.0, 20.0), Vec2::ZERO)
            .expect("应命中页面");
        assert!((page_rect.x - 20.0).abs() < 1e-3);
        assert!((page_rect.width - 50.0).abs() < 1e-3);

        // 滚动会平移内容：scroll=(10,10) 时同一视图矩形对应的页内区域右移下移
        let scrolled = view_rect_to_page_rect(&placement, Rect::new(30.0, 40.0, 50.0, 20.0), Vec2::new(10.0, 10.0))
            .expect("应命中页面");
        assert!((scrolled.x - 30.0).abs() < 1e-3, "实际 {}", scrolled.x);

        // 完全在页外 → None
        assert!(view_rect_to_page_rect(&placement, Rect::new(500.0, 500.0, 10.0, 10.0), Vec2::ZERO).is_none());
    }

    /// 引用点：按目标页分组、按位置去重、无形坐标的目标不产生标记。
    #[test]
    fn destination_points_group_by_page_and_dedupe_by_position() {
        let link = |destination: Destination| Link { rect: Rect::new(0.0, 0.0, 10.0, 10.0), destination };
        let links_by_page = vec![
            vec![
                // 两个链接指向**同一点** → 只记一次
                link(Destination::Page { page: PageIndex::new(1), offset: Some(Point::new(10.0, 20.0)) }),
                link(Destination::Page { page: PageIndex::new(1), offset: Some(Point::new(10.2, 20.2)) }),
                // 另一个点 → 另记一次
                link(Destination::Page { page: PageIndex::new(1), offset: Some(Point::new(300.0, 400.0)) }),
                // 只有页、没有坐标 → 不产生标记
                link(Destination::Page { page: PageIndex::new(2), offset: None }),
                // 外部 URI → 忽略
                link(Destination::Uri("https://example.com".into())),
            ],
            // 第 2 页上的链接也可以指向第 1 页（同样落在同一个 0.5pt 网格上 → 仍视为同一点）
            vec![link(Destination::Page { page: PageIndex::new(1), offset: Some(Point::new(10.2, 20.2)) })],
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
        assert_eq!((clamped.width(), clamped.height()), (FLOAT_MIN_SIZE.x, FLOAT_MIN_SIZE.y));

        // 比区域还大 → 收缩到区域尺寸
        let clamped = clamp_float_rect(rect(100.0, 50.0, 5000.0, 5000.0), area, FLOAT_MIN_SIZE);
        assert_eq!((clamped.width(), clamped.height()), (area.width(), area.height()));

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
        assert!(!wheel_targets_canvas(false), "指针在侧栏/工具栏上 → 不滚动文档");
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
            destination: Some(Destination::Page { page: PageIndex::new(page), offset: None }),
            children: Vec::new(),
        };
        let mut root = leaf("章 1", 0);
        root.children = vec![leaf("节 1.1", 1), leaf("节 1.2", 2)];
        let nodes = vec![root, leaf("章 2", 3)];

        let rows = flatten_outline(&nodes);
        assert_eq!(rows.len(), outline_node_count(&nodes), "行数应等于节点总数");
        assert_eq!(
            rows.iter().map(|(depth, title, _)| (*depth, title.as_str())).collect::<Vec<_>>(),
            vec![
                (0, "章 1"),
                (1, "节 1.1"),
                (1, "节 1.2"),
                (0, "章 2"),
            ],
            "深度优先、父在前、层级正确"
        );
        assert_eq!(
            rows[1].2,
            Some(Destination::Page { page: PageIndex::new(1), offset: None }),
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
        assert_eq!(outline_node_count(&nodes), 4, "根 1 + 它的 2 个子节点 + 根 2 = 4");
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
        assert_eq!(texture_pages(&[last], last, 10), vec![PageIndex::new(8), last]);

        // 当前页不在可见集合里时也要保留
        let pages = texture_pages(&[PageIndex::new(4)], PageIndex::new(7), 10);
        assert!(pages.contains(&PageIndex::new(7)), "当前页必须保留");

        // 多页可见时不重复
        let pages = texture_pages(&[PageIndex::new(4), PageIndex::new(5)], PageIndex::new(4), 10);
        assert_eq!(pages, vec![PageIndex::new(3), PageIndex::new(4), PageIndex::new(5), PageIndex::new(6)]);
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
        assert_eq!(canvas.height(), 800.0, "高度必须保持满高（这正是当时的缺陷）");

        // 侧栏隐藏时不让位
        assert_eq!(canvas_rect_for(full, 0.0), full);

        // 区域比拖拽条还窄时不得出现负尺寸
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(3.0, 10.0));
        let canvas = canvas_rect_for(tiny, 5.0);
        assert!(canvas.width() >= 0.0 && canvas.height() >= 0.0, "实际 {canvas:?}");
        assert_eq!(canvas.height(), 10.0, "仍要保持满高");
    }

    /// 缩放过场要用的"上一档"必须保留，但比例不能无限堆积（第五轮验收：相邻页闪白）。
    #[test]
    fn retain_preview_scales_keeps_current_and_one_old_scale() {
        let page = PageIndex::new(0);
        let other = PageIndex::new(1);
        let key = |page: PageIndex, scale: f32, rotation: Rotation| {
            RenderKey::new(page, Scale::from_zoom(scale), rotation)
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

        store.retain_preview_scales(&[(page, Rotation::None), (other, Rotation::None)], 2000);

        assert!(store.contains(&key(page, 2.0, Rotation::None)), "当前比例必须保留");
        assert!(
            store.contains(&key(page, 1.5, Rotation::None)),
            "最近的旧比例必须保留（缩放过场要用它顶替，否则闪白）"
        );
        assert!(!store.contains(&key(page, 1.25, Rotation::None)), "更旧的档应丢弃");
        assert!(!store.contains(&key(page, 1.0, Rotation::None)));
        assert!(!store.contains(&key(page, 3.0, Rotation::None)));
        assert!(!store.contains(&key(page, 2.0, Rotation::Clockwise90)), "旧旋转必须丢弃");
        assert!(
            !store.contains(&key(PageIndex::new(2), 2.0, Rotation::None)),
            "不在保留列表里的页应丢弃"
        );
        // 其它可见页同样"当前 + 最近旧档"
        assert!(store.contains(&key(other, 2.0, Rotation::None)));
        assert!(store.contains(&key(other, 1.5, Rotation::None)));
    }
}
