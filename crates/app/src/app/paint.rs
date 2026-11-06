//! `app::paint`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{overflow_layout, tile_view_rect_scaled, view_to_content, KeyedStore, Preview};
use eframe::egui;
use egui::{Color32, CornerRadius, Stroke, StrokeKind, Vec2 as EVec2};

/// 把浮窗正文上的双击位置映射成源码字符下标。
///
/// 纯文本用与绘制相同的 `overflow_layout`，命中精确；Markdown 的渲染结果与
/// 源码行结构不同，只能按纵向比例估算（打开后仍可继续用滚动条精确定位）。
pub(super) fn body_cursor_from_pointer(
    ctx: &egui::Context,
    text: &str,
    body: egui::Rect,
    pointer: egui::Pos2,
    markdown: bool,
    scroll: egui::Vec2,
    content_size: Option<egui::Vec2>,
) -> usize {
    if text.is_empty() || body.width() <= 0.0 || body.height() <= 0.0 {
        return 0;
    }
    let font = egui::FontId::proportional(13.0);
    let color = Color32::from_gray(225);
    let local = pointer - body.min;
    let galley = if markdown {
        ctx.fonts_mut(|fonts| fonts.layout(text.to_owned(), font, color, body.width()))
    } else {
        let row_height = 13.0 * 1.3;
        let rows = ((body.height() / row_height).floor() as usize).max(1);
        let job = overflow_layout(text, font, color, body.width(), rows);
        ctx.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let position = if markdown {
        let content_height = content_size
            .map(|size| size.y)
            .filter(|height| *height > 0.0)
            .unwrap_or(body.height());
        let content_y = (scroll.y + local.y).max(0.0);
        let y_ratio = (content_y / content_height).clamp(0.0, 1.0);
        let content_x = (scroll.x + local.x).max(0.0);
        egui::vec2(content_x, y_ratio * galley.size().y)
    } else {
        egui::vec2(local.x.max(0.0), local.y.max(0.0))
    };
    let index: usize = galley.cursor_from_pos(position).index.into();
    index.min(text.chars().count())
}

/// 画一张纹理（UV 取满）。
pub(super) fn paint_image(
    painter: &egui::Painter,
    texture: &egui::TextureHandle,
    rect: egui::Rect,
) {
    painter.image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// 便签图标：填色方块 + 折角。大小为页面空间矩形换算后的屏幕矩形。
pub(super) fn paint_note_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let rect = egui::Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width().max(10.0), rect.height().max(10.0)),
    );
    painter.rect_filled(rect, CornerRadius::same(2), color);
    painter.rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0, Color32::from_rgb(70, 70, 70)),
        StrokeKind::Inside,
    );
    let corner = 6.0_f32.min(rect.width() * 0.4).min(rect.height() * 0.4);
    painter.line_segment(
        [
            egui::pos2(rect.right() - corner, rect.top()),
            egui::pos2(rect.right(), rect.top() + corner),
        ],
        Stroke::new(1.0, Color32::from_rgb(70, 70, 70)),
    );
}

pub(super) fn paint_placeholder(painter: &egui::Painter, rect: egui::Rect) {
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
pub(super) fn paint_preview(
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
pub(super) fn paint_highlight(
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
pub(super) fn canvas_to_content(
    canvas: egui::Rect,
    screen_position: egui::Pos2,
    scroll: Vec2,
) -> Point {
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
pub(super) fn paint_ink(
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
