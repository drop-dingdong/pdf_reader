//! `app::hover`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    body_size_of, clamp_float_rect, find_float_for_target, fit_inside, float_size_for_body,
    hover_preview_allowed, overflow_layout, pointer_over_any, tip_rect_for, FLOAT_MIN_SIZE,
    FLOAT_TITLE_HEIGHT,
};
use eframe::egui;
use egui::{Align2, Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Vec2 as EVec2};

impl PdfReaderApp {
    /// 悬停预览浮窗：检测鼠标下的页内链接 → 找到对应被引用点 → 必要时**渲染一次**截图 →
    /// 画一个临时浮窗。指针离开链接与浮窗后消失；点击它则转为永久浮窗。
    pub(super) fn update_hover_preview(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        canvas: egui::Rect,
        scroll: Vec2,
    ) {
        let pointer = ui.ctx().pointer_hover_pos();

        // 统一归属：画布内的链接 hover 也必须先排除编辑窗口/弹层/面板/浮窗。
        // 之前只判断了滚动、浮窗和画墨迹，编辑窗口盖住的链接仍会触发临时预览。
        //
        // 例外：指针按在**临时预览自身**上时必须保留 hover_tip，否则按下帧就把
        // tip 清掉，release 时 `draw_hover_preview` 看不到它，点击无法固定。
        let over_tip = pointer.is_some_and(|position| {
            self.hover_tip
                .as_ref()
                .and_then(|tip| tip.rect)
                .is_some_and(|rect| rect.contains(position))
        });
        let pointer_down = ui.input(|input| input.pointer.any_down());
        let pointer_allowed = pointer.is_some_and(|position| {
            self.canvas_hover_allowed(ui.ctx(), canvas, position, pointer_down && !over_tip)
        });
        if !pointer_allowed {
            self.hover_tip = None;
            return;
        }

        // 闸门（用户要求）：
        // 1. 滚动/缩放的静默窗口内不显示；
        // 2. 指针落在**任何浮窗**（永久或临时）之上时不显示 —— 浮窗屏蔽主视图动作，
        //    被浮窗盖住的链接不应再有悬停反馈。
        let scrolling = self
            .scroll_quiet_until
            .is_some_and(|deadline| self.runtime.clock.now() < deadline);
        let float_rects: Vec<egui::Rect> = self.floats.iter().map(|window| window.rect).collect();
        let over_float = pointer.is_some_and(|position| pointer_over_any(position, &float_rects));
        // 3. **画墨迹期间**不弹出（用户实测：笔画压住链接时不该出现目的地预览）。
        //    `ink` 要等指针移过拖动阈值才建，所以"按住但还没成笔"这一小段也算绘制中。
        let drawing = self.tool == Tool::Ink
            && (self.ink.is_some() || ui.input(|input| input.pointer.primary_down()));
        if !hover_preview_allowed(scrolling, over_float, drawing) {
            self.hover_tip = None;
            return;
        }

        let mut found: Option<((PageIndex, usize), egui::Pos2)> = None;

        if let (Some(pointer), Some(navigation)) = (pointer, self.navigation.clone()) {
            'outer: for page in self.visible.clone() {
                let Some(placement) = session.placement(page).copied() else {
                    continue;
                };
                let Some(registered) = self.link_marks.get(page.get() as usize) else {
                    continue;
                };
                for (index, link) in navigation.links_on(page).iter().enumerate() {
                    // 链接 → 被引用点：**直接取登记表**（建表时已解析）
                    let Some(Some(target)) = registered.get(index).copied() else {
                        continue;
                    };
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
            (Some(tip), Some(pointer)) => tip.rect.is_some_and(|rect| rect.contains(pointer)),
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
                self.hover_tip = Some(HoverTip {
                    target,
                    anchor,
                    rect: None,
                });
            }
            None if keep_existing => {}
            None => {
                self.hover_tip = None;
                return;
            }
        }

        // 需要时**按记录的分辨率**生成一次（之后复用；缺像素则提交渲染、下帧重试）
        let Some(tip) = self.hover_tip.as_ref() else {
            return;
        };
        let (page, index) = tip.target;
        let pending = self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
            .filter(|mark| mark.snapshot.is_none())
            .map(|mark| (mark.rect, mark.scale));
        if let Some((saved_rect, scale)) = pending {
            let image_ref = self
                .dest_marks
                .get(page.get() as usize)
                .and_then(|marks| marks.get(index))
                .filter(|mark| !mark.image_id.is_empty())
                .map(|mark| crate::image_store::ImageRef {
                    record_id: mark.image_id.clone(),
                    revision: mark.image_revision,
                });
            let texture = self.preview_texture(
                ui,
                session,
                page,
                saved_rect,
                scale,
                format!("dest-hover-{}-{}", page.get(), index),
                "被引用点",
                image_ref,
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
        {}
    }

    /// 绘制临时预览浮窗并处理"点击固定"（本函数在所有固定浮窗之后调用 → 位于最上层）。
    pub(super) fn draw_hover_preview(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        let Some(tip) = self.hover_tip.as_ref() else {
            return;
        };
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
        let response = ui.interact(rect, Id::unique("hover-tip"), Sense::click_and_drag());
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
        painter.rect_filled(
            title_rect,
            CornerRadius::same(4),
            Color32::from_rgb(54, 58, 66),
        );
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
                owner: FloatOwner::None,
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
    pub(super) fn draw_sidebar_hover_float(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        let Some((index, row)) = self.custom_hover else {
            return;
        };
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
        // 临时预览也要能点击/拖动固定；之前只画不注册交互，点击会落回画布。
        let response = ui.interact(rect, Id::unique("custom-hover"), Sense::click_and_drag());
        let painter = ui.painter().clone();
        painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(30, 32, 36));
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_gray(150)),
            StrokeKind::Inside,
        );
        let title_rect =
            egui::Rect::from_min_size(rect.min, EVec2::new(rect.width(), FLOAT_TITLE_HEIGHT));
        painter.rect_filled(
            title_rect,
            CornerRadius::same(4),
            Color32::from_rgb(54, 58, 66),
        );
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
                if self.markdown_floats && self.hover_preview_markdown {
                    self.show_hover_markdown(ui, body, index, &text);
                } else {
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
                    painter
                        .with_clip_rect(body)
                        .galley(body.min, galley, Color32::from_gray(225));
                }
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
        if response.clicked() || response.drag_started() {
            let target = clamp_float_rect(
                rect.translate(response.drag_delta()),
                canvas,
                FLOAT_MIN_SIZE,
            );
            self.spawn_custom_float(index, target);
            self.custom_hover = None;
            ui.ctx().request_repaint();
        }
    }

    /// 可插入 Markdown 的内部图片记录（来自 preview/被引用点，不要求已生成快照）。
    pub(super) fn image_choices(&self) -> Vec<(String, String)> {
        let mut choices = Vec::new();
        for preview in &self.custom_previews {
            if preview.kind == CustomKind::Image && !preview.image_id.is_empty() {
                choices.push((
                    if preview.title.is_empty() {
                        "图片 preview".to_string()
                    } else {
                        preview.title.clone()
                    },
                    crate::image_store::image_uri(&preview.image_id, preview.image_revision),
                ));
            }
        }
        for (page, marks) in self.dest_marks.iter().enumerate() {
            for (index, mark) in marks.iter().enumerate() {
                if !mark.image_id.is_empty() {
                    choices.push((
                        format!("引用点 第{}页 #{}", page + 1, index + 1),
                        crate::image_store::image_uri(&mark.image_id, mark.image_revision),
                    ));
                }
            }
        }
        choices
    }

    /// hover 临时预览的 Markdown 渲染（设置开启时）。
    pub(super) fn show_hover_markdown(
        &mut self,
        ui: &mut egui::Ui,
        body: egui::Rect,
        index: usize,
        text: &str,
    ) {
        if self.markdown.hover_index != Some(index) {
            self.markdown.hover_cache = egui_commonmark::CommonMarkCache::default();
            self.markdown.hover_index = Some(index);
        }
        self.markdown.rendered = true;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(body)
                .id_salt(("hover-md", index)),
            |ui| {
                ui.set_clip_rect(body.intersect(ui.clip_rect()));
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                let mut visuals = egui::Visuals::dark();
                visuals.override_text_color = Some(Color32::from_gray(225));
                visuals.hyperlink_color = Color32::from_rgb(120, 180, 255);
                *ui.visuals_mut() = visuals;
                ui.spacing_mut().item_spacing = EVec2::new(0.0, 4.0);
                let math_cache = self.markdown.math_cache.clone();
                let math_fn = move |ui: &mut egui::Ui, tex: &str, inline: bool| {
                    crate::math::paint(ui, &math_cache, tex, inline);
                };
                egui::ScrollArea::both()
                    .id_salt(("hover-md-scroll", index))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui_commonmark::CommonMarkViewer::new()
                            .enable_scroll_to_heading(true)
                            .render_math_fn(Some(&math_fn))
                            .show(ui, &mut self.markdown.hover_cache, text);
                    });
            },
        );
    }
}
