//! `app::capture`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    body_size_of, compose_snapshot, pages_intersecting, screen_rect_to_content, veil_bands,
    view_rect_to_page_rect, SnapshotLayer, SnapshotPage, FLOAT_MIN_SIZE, FLOAT_TITLE_HEIGHT,
};
use eframe::egui;
use egui::{Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Vec2 as EVec2};

impl PdfReaderApp {
    /// 截图模式：框选区域 + 半透明蒙层（矩形内透明）+ Enter/Esc 退出。
    ///
    /// * 蒙层用"绕开选区的四块"实现（egui 没有擦除混合，见 `view::veil_bands`）；
    /// * 拖动即框选；松开后选区保留，可再次拖动重画；
    /// * `Enter` → 终端输出区域信息（视图坐标 + 覆盖页的页面空间矩形）并退出；
    /// * `Esc` → 直接退出，无输出。
    pub(super) fn draw_capture(
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
    pub(super) fn commit_dest_mark(
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
        let png = crate::image_store::encode_png(&snapshot);
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
            after.image_revision = old.image_revision.saturating_add(1);
            if let Some(png) = png.clone() {
                let image_ref = crate::image_store::ImageRef {
                    record_id: after.image_id.clone(),
                    revision: after.image_revision,
                };
                self.images.insert(&image_ref, png);
                ui.ctx().forget_image(&crate::image_store::image_uri(
                    &image_ref.record_id,
                    image_ref.revision,
                ));
            }
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
    pub(super) fn snapshot_for(
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
    pub(super) fn capture_into_float(
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
    /// 把截图结果存进某个**自定义图片预览**。
    ///
    /// **不再**自动生成浮窗（用户要求）：浮窗只在用户从侧栏悬浮/点击该条目时出现。
    pub(super) fn commit_custom_image(
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
        let png = crate::image_store::encode_png(&snapshot);
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
        let Some(before) = self.custom_previews.get(index).cloned() else {
            return;
        };
        let key = PreviewKey {
            page: before.page,
            id: before.id,
        };
        let mut after = before.clone();
        after.snapshot = Some(texture);
        after.rect = page_rect;
        after.body_size = egui::vec2(overlap.width, overlap.height);
        after.image_revision = before.image_revision.saturating_add(1);
        if let Some(png) = png {
            let image_ref = crate::image_store::ImageRef {
                record_id: after.image_id.clone(),
                revision: after.image_revision,
            };
            self.images.insert(&image_ref, png);
            ui.ctx().forget_image(&crate::image_store::image_uri(
                &image_ref.record_id,
                image_ref.revision,
            ));
        }
        // ▲v2：记录这张图的分辨率（截图时的显示比例）—— 之后重建预览时按它请求渲染
        after.scale = session.state().zoom;
        self.apply_edit(
            AppEdit::ReplacePreview {
                key,
                before: std::sync::Arc::new(before),
                after: std::sync::Arc::new(after),
            },
            true,
        );
        self.message = format!(
            "已保存图片预览（取第 {} 页，交叠 {:.0}×{:.0}）",
            page.display_number(),
            overlap.width,
            overlap.height
        );
    }
}
