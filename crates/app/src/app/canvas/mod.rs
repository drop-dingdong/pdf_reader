//! `app::canvas`：画布编排与输入（页面/选区/链接/标注/引用点绘制见子模块）。

mod interact;
mod paint;

use super::*;

use crate::view::{
    body_size_changed, canvas_rect_for, command_for_scroll, command_for_zoom_delta, link_box_color,
    link_overlay_visible, link_rect_in_page_bounds, needs_underlay, page_rect_to_screen, plan_page,
    tile_view_rect, view_rect_for, wheel_targets_canvas_with_float, DrawInput, PageDraw, PlanStamp,
    Preview,
};
use eframe::egui;
use egui::{Color32, CornerRadius, Id, Sense, Vec2 as EVec2};

/// 画布绘制上下文：一次绘制内不变量（painter 克隆便宜）。
pub(super) struct CanvasPaint {
    pub(super) painter: egui::Painter,
    pub(super) canvas: egui::Rect,
    pub(super) scroll: Vec2,
}

impl PdfReaderApp {
    /// 画布：页面（纹理或白底页框）+ 高亮 + 鼠标交互。
    pub(super) fn draw_canvas(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
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

            self.handle_canvas_input(ui, session, canvas, &response);

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

            let visible = self.visible.clone();
            let previewing = self.previewing();
            let ctx = CanvasPaint { painter, canvas, scroll };
            let report = self.paint_pages(session, &ctx, &visible);
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
            self.paint_highlights(session, &ctx);
            self.paint_links(session, &ctx);
            self.handle_secondary_click(ui, session, &ctx);
            self.paint_annotations(ui, session, &ctx);
            self.paint_dest_marks(ui, session, &ctx);
            self.handle_canvas_interaction(ui, session, &ctx, &response);
            self.finish_canvas_frame(ui, session, &ctx);
        });
        self.draw_annotation_context_menu(ui.ctx());
    }
}
