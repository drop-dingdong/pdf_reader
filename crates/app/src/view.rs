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

/// 合成快照时"一页可用的已渲染位图"。
///
/// 位图像素与内容空间**同尺度**（D-17 的不变量），因此像素偏移可以直接相加。
pub struct SnapshotPage<'a> {
    /// 该页在内容空间的矩形（其尺寸就是位图像素尺寸）。
    pub rect: Rect,
    /// 整页位图（未分块且已缓存时可用）。
    pub whole: Option<&'a RasterImage>,
    /// 已缓存的瓦片（分块时可用）。
    pub tiles: Vec<(TileSpec, &'a RasterImage)>,
}

/// 把 `content_rect` 覆盖到的**已渲染像素**拷成一张自包含图像（截图快照）。
///
/// ## 为什么存成内存快照，而不是"引用已渲染纹理的对应区域"
/// 1. 主视图一旦缩放或换页，原纹理会被淘汰或换成另一比例 —— 浮窗内容会失效或显示错区域；
/// 2. 区域可能跨页、跨留白，单张纹理根本无法表达；
/// 3. 快照自包含，与主视图状态完全解耦，单页/多页走**同一条**代码路径。
///
/// 代价只有一次区域像素拷贝（区域 ≤ 一屏）。未被页面覆盖或**尚未就绪**的地方保持白色。
/// 完全没有可用像素时返回 `None`（调用方据此提示）。
pub fn compose_snapshot(pages: &[SnapshotPage<'_>], content_rect: Rect) -> Option<egui::ColorImage> {
    let width = content_rect.width.ceil().max(1.0) as usize;
    let height = content_rect.height.ceil().max(1.0) as usize;
    // 白底：页面未覆盖处（页间距、页边距）与未就绪处都留白
    let mut rgba = vec![255u8; width * height * 4];
    let origin = content_rect.min();

    /// 把源位图的一个子矩形拷进目标缓冲；越界一律裁剪。
    fn blit(
        image: &RasterImage,
        source_origin: (i64, i64),
        source: (i64, i64, i64, i64),
        out: &mut [u8],
        out_size: (usize, usize),
        dest: (i64, i64),
    ) {
        let (sx, sy, sw, sh) = source;
        let stride = image.format.bytes_per_pixel() as i64;
        for row in 0..sh {
            let source_y = sy + row - source_origin.1;
            let dest_y = dest.1 + row;
            if source_y < 0 || source_y >= image.height as i64 {
                continue;
            }
            if dest_y < 0 || dest_y >= out_size.1 as i64 {
                continue;
            }
            for column in 0..sw {
                let source_x = sx + column - source_origin.0;
                let dest_x = dest.0 + column;
                if source_x < 0 || source_x >= image.width as i64 {
                    continue;
                }
                if dest_x < 0 || dest_x >= out_size.0 as i64 {
                    continue;
                }
                let from = ((source_y * image.width as i64 + source_x) * stride) as usize;
                let to = ((dest_y * out_size.0 as i64 + dest_x) * 4) as usize;
                let data = image.data();
                let (r, g, b) = match image.format {
                    PixelFormat::Bgra8 => (data[from + 2], data[from + 1], data[from]),
                    PixelFormat::Rgba8 => (data[from], data[from + 1], data[from + 2]),
                    PixelFormat::Gray8 => (data[from], data[from], data[from]),
                };
                out[to] = r;
                out[to + 1] = g;
                out[to + 2] = b;
                out[to + 3] = 255;
            }
        }
    }

    let mut painted = false;
    for page in pages {
        let Some(overlap) = page.rect.intersection(content_rect) else { continue };
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            continue;
        }
        let destination = (
            (overlap.x - origin.x).round() as i64,
            (overlap.y - origin.y).round() as i64,
        );
        let source_x = (overlap.x - page.rect.x).round() as i64;
        let source_y = (overlap.y - page.rect.y).round() as i64;
        let width_px = overlap.width.round() as i64;
        let height_px = overlap.height.round() as i64;

        if let Some(image) = page.whole {
            blit(
                image,
                (0, 0),
                (source_x, source_y, width_px, height_px),
                &mut rgba,
                (width, height),
                destination,
            );
            painted = true;
            continue;
        }

        // 分块：逐块与源矩形求交后拷贝
        for (tile, image) in &page.tiles {
            let tile_rect = tile.pixel_rect(page.rect.size());
            let tile_origin = (tile_rect.x.round() as i64, tile_rect.y.round() as i64);
            let left = source_x.max(tile_origin.0);
            let top = source_y.max(tile_origin.1);
            let right = (source_x + width_px).min(tile_origin.0 + image.width as i64);
            let bottom = (source_y + height_px).min(tile_origin.1 + image.height as i64);
            if left >= right || top >= bottom {
                continue;
            }
            blit(
                image,
                tile_origin,
                (left, top, right - left, bottom - top),
                &mut rgba,
                (width, height),
                (destination.0 + (left - source_x), destination.1 + (top - source_y)),
            );
            painted = true;
        }
    }

    if !painted {
        return None;
    }
    Some(color_image(PixelFormat::Rgba8, &rgba, width as u32, height as u32))
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

/// 侧栏条目的"单击延迟"窗口：与 egui 的双击判定时间一致。
///
/// 背景：egui 在双击的**第一下**就会给出 `clicked()`，所以"单击生成浮窗 + 双击跳转"
/// 会同时触发。做法是把单击动作**延迟一个双击窗口**再执行；窗口内若收到双击，
/// 就取消那一次单击（用户要求：双击只跳转）。
pub const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(300);

/// 延迟的单击是否已到执行时刻（`elapsed` 为从点击发生到现在的时间）。
pub fn single_click_deferred(elapsed: std::time::Duration) -> bool {
    elapsed >= DOUBLE_CLICK_WINDOW
}

/// 侧栏 Pin 列表行首的**类型符号**：文字 / 截图（用户指定的 unicode）。
///
/// 用 Rust 的 `\u{...}` 转义写出，确保码点与用户要求一致：
/// 文字 `U+1F5B9`、截图 `U+1F5BB`。若渲染成方块，说明当前字体缺这两个字形
/// （与码点无关，换个带符号的字体即可）。
pub const CUSTOM_SYMBOL_TEXT: &str = "\u{1F5B9}";
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
        egui::TextFormat { font_id: font, color, ..Default::default() },
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
            let Destination::Page { page, offset: Some(point) } = &link.destination else {
                continue;
            };
            let Some(bucket) = out.get_mut(page.get() as usize) else { continue };
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

    /// 单击延迟：窗口内不执行（给双击留机会），到点才执行。
    #[test]
    fn single_click_waits_out_the_double_click_window() {
        assert!(!single_click_deferred(std::time::Duration::from_millis(0)));
        assert!(!single_click_deferred(std::time::Duration::from_millis(299)));
        assert!(single_click_deferred(DOUBLE_CLICK_WINDOW));
        assert!(single_click_deferred(std::time::Duration::from_millis(1000)));
        assert_eq!(DOUBLE_CLICK_WINDOW, std::time::Duration::from_millis(300));
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
        assert!((rect.min.x - (canvas.min.x + 16.0)).abs() < 1e-3, "紧贴侧边栏");
        assert!((rect.center().y - row.center().y).abs() < 1e-3, "与条目行垂直居中");

        // 顶部行 → 会被夹回视图内（不会超出上边）
        let top = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(260.0, 18.0));
        let rect = float_rect_at_row(canvas, top, size, min);
        assert!(rect.min.y >= canvas.min.y - 1e-3);

        // 画布未知 → 退回固定位置，不 panic
        let rect = float_rect_at_row(egui::Rect::NOTHING, row, size, min);
        assert_eq!(rect.min, egui::pos2(48.0, 48.0));
    }

    /// 侧栏行首类型符号：码点必须与用户指定的完全一致。
    #[test]
    fn custom_preview_symbols_are_the_requested_codepoints() {
        assert_eq!(CUSTOM_SYMBOL_TEXT.chars().next().map(u32::from), Some(0x1F5B9));
        assert_eq!(CUSTOM_SYMBOL_IMAGE.chars().next().map(u32::from), Some(0x1F5BB));
        assert_eq!(CUSTOM_SYMBOL_TEXT.chars().count(), 1);
        assert_eq!(CUSTOM_SYMBOL_IMAGE.chars().count(), 1);
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
        assert_eq!(find_float_for_target(&origins, (page, 9)), None, "没固定过就没有");
        assert_eq!(find_float_for_target(&origins, (PageIndex::new(3), 0)), None, "页不同不算");
        assert_eq!(find_float_for_target(&[], (page, 0)), None);
    }

    /// 悬停预览的两个约束：滚动期间不出现；浮窗之下不出现。
    #[test]
    fn hover_preview_is_blocked_while_scrolling_and_under_floats() {
        assert!(hover_preview_allowed(false, false), "正常情况应出现");
        assert!(!hover_preview_allowed(true, false), "滚动/缩放静默窗口内不出现");
        assert!(!hover_preview_allowed(false, true), "指针在浮窗上不出现（浮窗屏蔽主视图）");
        assert!(!hover_preview_allowed(true, true));

        let rects = [
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 50.0)),
            egui::Rect::from_min_size(egui::pos2(200.0, 200.0), egui::vec2(100.0, 50.0)),
        ];
        assert!(pointer_over_any(egui::pos2(50.0, 30.0), &rects));
        assert!(pointer_over_any(egui::pos2(250.0, 220.0), &rects));
        assert!(!pointer_over_any(egui::pos2(150.0, 150.0), &rects), "浮窗之间不算遮挡");
        assert!(!pointer_over_any(egui::pos2(50.0, 30.0), &[]), "没有浮窗时不遮挡");
    }

    /// 临时预览浮窗：左上角放在链接框中心（保证与链接框重叠，鼠标移动路径连续）。
    #[test]
    fn tip_rect_starts_at_the_link_center_so_it_overlaps() {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let link = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(180.0, 24.0));
        let size = egui::vec2(360.0, 260.0);

        let tip = tip_rect_for(link, size, canvas);
        assert_eq!(tip.min, link.center(), "左上角 = 链接框中心");
        assert!(tip.intersects(link), "必须与链接框重叠（否则鼠标一移开预览就消失）");
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
            destination: Destination::Page { page: PageIndex::new(page), offset: Some(point) },
        };
        let links = vec![
            vec![link(1, Point::new(10.0, 20.0)), link(1, Point::new(300.0, 400.0))],
            Vec::new(),
        ];
        let points = destination_points(&links, 2);
        let first_page = &points[1];
        assert_eq!(first_page.len(), 2);

        // 用"略偏一点"的同一点去查，仍应命中（量化到同一格）
        assert_eq!(find_dest_index(first_page, Point::new(10.2, 20.2)), Some(0));
        assert_eq!(find_dest_index(first_page, Point::new(10.0, 20.0)), Some(0));
        assert_eq!(find_dest_index(first_page, Point::new(300.0, 400.0)), Some(1));
        // 另一个点 → 未命中
        assert_eq!(find_dest_index(first_page, Point::new(50.0, 60.0)), None);
        // 量化函数本身：0.4 → 0.5，0.2 → 0.0
        assert_eq!(quantize_dest_point(Point::new(10.4, 20.2)), Point::new(10.5, 20.0));
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
        assert_eq!(screen, egui::Rect::from_min_size(egui::pos2(335.0, 93.0), egui::vec2(50.0, 20.0)));
    }

    /// 回归：屏幕坐标 → 内容空间必须减去画布原点；且框选区域截到的内容必然**落在当前可见范围内**。
    #[test]
    fn screen_rect_to_content_subtracts_the_canvas_origin() {
        // 画布左上角在屏幕 (265, 40)（左侧有 260px 侧栏 + 5px 拖拽条）
        let canvas_min = egui::pos2(265.0, 40.0);
        let selection = egui::Rect::from_min_size(egui::pos2(365.0, 140.0), egui::vec2(100.0, 50.0));
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
            RasterImage::new(width, height, PixelFormat::Bgra8, std::sync::Arc::from(data))
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

        // 分块来源：瓦片是 512×512 的网格，因此页要够大才能放两块（1024×512）
        let left = make(512, 512, 50);
        let right = make(512, 512, 200);
        let tiled = vec![SnapshotPage {
            rect: Rect::new(0.0, 0.0, 1024.0, 512.0),
            whole: None,
            tiles: vec![
                (TileSpec::new(0, 0), &left),
                (TileSpec::new(1, 0), &right),
            ],
        }];
        let snapshot =
            compose_snapshot(&tiled, Rect::new(0.0, 0.0, 1024.0, 512.0)).expect("应有内容");
        assert_eq!(snapshot.size, [1024, 512]);
        assert_eq!(snapshot.pixels[0].r(), 50, "左块覆盖 x<512");
        assert_eq!(snapshot.pixels[511].r(), 50);
        assert_eq!(snapshot.pixels[512].r(), 200, "右块覆盖 x≥512");
        assert_eq!(snapshot.pixels[1024 * 10].r(), 50, "第二行左侧仍是左块");
        assert_eq!(snapshot.pixels[1024 * 10 + 512].r(), 200, "第二行右侧仍是右块");

        // 多页 + 页间距：中间留白
        let top = make(2, 2, 10);
        let bottom = make(2, 2, 20);
        let stacked = vec![
            SnapshotPage { rect: Rect::new(0.0, 0.0, 2.0, 2.0), whole: Some(&top), tiles: Vec::new() },
            SnapshotPage { rect: Rect::new(0.0, 3.0, 2.0, 2.0), whole: Some(&bottom), tiles: Vec::new() },
        ];
        let snapshot = compose_snapshot(&stacked, Rect::new(0.0, 0.0, 2.0, 5.0)).expect("应有内容");
        assert_eq!(snapshot.size, [2, 5]);
        assert_eq!(snapshot.pixels[0].r(), 10, "第一页");
        assert_eq!(snapshot.pixels[2 * 2].r(), 255, "页间距应留白");
        assert_eq!(snapshot.pixels[3 * 2].r(), 20, "第二页");
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
