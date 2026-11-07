//! `app::canvas::paint`：画布上的页面/选区/链接/标注/引用点绘制。

use super::super::*;
use super::*;

use eframe::egui;
use egui::{Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Vec2 as EVec2};
use pdfcore::annotation::{AnnotationKind, INK_COLOR, INK_OPACITY};

impl PdfReaderApp {
    /// 画可见页：纸张 → 底料 → 主图层（预览/瓦片），并返回诊断行。
    pub(super) fn paint_pages(
        &mut self,
        session: &DocumentSession,
        ctx: &CanvasPaint,
        visible: &[PageIndex],
    ) -> Vec<String> {
        let painter = ctx.painter.clone();
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        let mut report: Vec<String> = Vec::new();
        for &page in visible {
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
                self.textures
                    .preview_source(page, placement.rotation, whole_key.scale_milli);
            // D-96：波段切换后"旧内容 vs 新块"的判据（缺一块就不翻页，另有超时兜底）
            let waited_ms = self.note_band_wait(page, whole_key.scale_milli, ready);
            // D-104：同一波段内已经切到瓦片就不再退回预览（否则波段内滚动会整页变糊）
            let showing_tiles = self.last_plan.get(&page).is_some_and(|stamp| {
                stamp.band == whole_key.scale_milli && matches!(stamp.plan, PageDraw::Tiles)
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
                self.textures
                    .underlay_source(page, placement.rotation, whole_key.scale_milli)
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
                                let rect =
                                    tile_view_rect(grid, &placement, col, row, render.0, scroll);
                                let key = RenderKey::tiled(page, render, placement.rotation, tile);
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
            let previous = self
                .last_plan
                .insert(page, stamp)
                .filter(|prev| *prev != stamp);
            if let Some(previous) = previous.filter(|_| self.runtime.diag.enabled()) {
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
                self.runtime.diag.line(line);
            }
            if self.runtime.diag.enabled() {
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
        report
    }

    /// 选区与搜索命中高亮。
    pub(super) fn paint_highlights(&self, session: &DocumentSession, ctx: &CanvasPaint) {
        let painter = ctx.painter.clone();
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        // 2) 高亮（选区 + 搜索命中）
        for highlight in session.highlight_selection() {
            paint_highlight(&painter, session, &highlight, scroll, canvas.min);
        }
        let active = self.active_hit;
        for highlight in session.highlights_for_search(&self.hits, active) {
            paint_highlight(&painter, session, &highlight, scroll, canvas.min);
        }
    }

    /// 链接热区框（含热区越界诊断）。
    pub(super) fn paint_links(&mut self, session: &DocumentSession, ctx: &CanvasPaint) {
        let painter = ctx.painter.clone();
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        // 链接热区诊断（PDF_READER_DIAGNOSE=1）：热区越界 = 坐标空间不一致
        if self.runtime.diag.enabled() {
            if let Some(navigation) = &self.navigation {
                for page in self.visible.clone() {
                    let Some(placement) = session.placement(page).copied() else {
                        continue;
                    };
                    let links = navigation.links_on(page);
                    if links.is_empty() {
                        continue;
                    }
                    let size = placement.page_size;
                    let first = links[0].rect;
                    self.runtime.diag.line(format!(
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
                    let Some(placement) = session.placement(page).copied() else {
                        continue;
                    };
                    for link in navigation.links_on(page) {
                        // 与高亮同一条换算链：PDF 页面空间 → 内容空间 → 视图空间
                        let content = placement.rect_to_content(link.rect);
                        let screen = egui::Rect::from_min_size(
                            canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
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
    }

    /// 标注叠加层（高亮/下划线/删除线/墨迹/便签 + 悬停命中）。
    pub(super) fn paint_annotations(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        ctx: &CanvasPaint,
    ) {
        let painter = ctx.painter.clone();
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        // 2b') 标注叠加层（D-118）：几何在页面空间，这里换算到屏幕
        //
        // 每帧先清空悬停目标：`Delete` 只删除**本帧**指针真正位于画布注释上的那一条。
        // 这一步必须在 `show_annotations`/截图判断之外，否则关掉标注或进入截图后
        // 旧的悬停目标会残留（侧栏 hover + Delete 误删就是这条路径）。
        self.hovered_annotation = None;
        if self.show_annotations && self.capture.is_none() {
            // 画布注释不是 egui widget：命中判断自己裁剪到 canvas，并避开弹层/拖拽/浮窗。
            let using_pointer = ui.ctx().egui_is_using_pointer();
            let pointer = ui.ctx().pointer_hover_pos().filter(|position| {
                self.canvas_hover_allowed(ui.ctx(), canvas, *position, using_pointer)
            });
            for page in self.visible.clone() {
                let Some(placement) = session.placement(page).copied() else {
                    continue;
                };
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
                                && annotation
                                    .contains(placement.content_to_page(content_point), tolerance)
                            {
                                // 命中/删除只看几何，与"有没有正文"无关
                                self.hovered_annotation = Some(annotation.id);
                            }
                        }
                        continue;
                    }
                    // 便签：没有 quads，单独画图标（图标 = 便签矩形；正文在浮窗里）。
                    if let AnnotationKind::Note { rect } = annotation.kind {
                        let content = placement.rect_to_content(rect);
                        let screen = egui::Rect::from_min_size(
                            canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                            EVec2::new(content.width, content.height),
                        );
                        paint_note_icon(&painter, screen, color);
                        if let Some(pointer) = pointer {
                            if screen.expand(2.0).contains(pointer) {
                                self.hovered_annotation = Some(annotation.id);
                            }
                        }
                        continue;
                    }
                    for quad in annotation.kind.quads() {
                        let content = placement.rect_to_content(*quad);
                        let screen = egui::Rect::from_min_size(
                            canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                            EVec2::new(content.width, content.height),
                        );
                        let to_screen = |point: Point| {
                            let content = placement.page_to_content(point);
                            egui::pos2(
                                canvas.min.x + content.x - scroll.x,
                                canvas.min.y + content.y - scroll.y,
                            )
                        };
                        // 悬停：只记录 Delete 目标；不置顶、不改变浮窗高亮（点击才打开/置顶）
                        if let Some(pointer) = pointer {
                            if screen.contains(pointer) {
                                // 命中/删除只看几何，与"有没有正文"无关
                                self.hovered_annotation = Some(annotation.id);
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
                                let y = quad.y + quad.height * pdfcore::annotation::STRIKE_FRACTION;
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
                if self.runtime.clock.now() >= until {
                    self.annotation_focus = None;
                } else if let Some(annotation) = self.annotations.get(id) {
                    if let (Some(bounds), Some(placement)) =
                        (annotation.bounds(), session.placement(annotation.page))
                    {
                        let content = placement.rect_to_content(bounds);
                        let screen = egui::Rect::from_min_size(
                            canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
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
    }

    /// 被引用的点（链接跳转目的地）。
    pub(super) fn paint_dest_marks(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        ctx: &CanvasPaint,
    ) {
        let painter = ctx.painter.clone();
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
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
                let Some(placement) = session.placement(page).copied() else {
                    continue;
                };
                let Some(marks) = self.dest_marks.get(page.get() as usize) else {
                    continue;
                };
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
                                ui.ctx()
                                    .copy_text(crate::image_store::image_uri(&record_id, revision));
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
                            &placement, mark.rect, scroll, canvas.min,
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
    }
}
