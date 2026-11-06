//! `app::floats`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    body_size_of, clamp_float_rect, drag_float_rect, find_float_for_custom, fit_inside,
    float_rect_at_row, float_size_for_body, move_to_last, overflow_layout, CUSTOM_SYMBOL_EDIT,
    FLOAT_HANDLE_SIZE, FLOAT_MIN_SIZE, FLOAT_TITLE_HEIGHT,
};
use eframe::egui;
use egui::{Align2, Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Vec2 as EVec2};

impl PdfReaderApp {
    /// 由"侧栏行的屏幕矩形"决定浮窗位置（委托给已测试的 `view::float_rect_at_row`）。
    pub(super) fn float_rect_at_row(&self, row: egui::Rect, size: egui::Vec2) -> egui::Rect {
        float_rect_at_row(self.canvas_rect, row, size, FLOAT_MIN_SIZE)
    }

    /// 只为**当前打开的文字浮窗**解析 Markdown 里的 `pd-img`，缺图时让 owner 懒渲染。
    pub(super) fn ensure_markdown_image_owners(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
    ) {
        if !self.markdown_floats {
            return;
        }
        let mut refs = Vec::new();
        for window in &self.floats {
            if let FloatContent::Text(text) = &window.content {
                if !text.is_empty() {
                    let cached = self
                        .markdown_prepared
                        .get(&window.id)
                        .filter(|prepared| prepared.source_hash == markdown_source_hash(text));
                    let image_refs = cached
                        .map(|prepared| prepared.image_refs.clone())
                        .unwrap_or_else(|| crate::image_store::markdown_refs(text));
                    for reference in image_refs {
                        if !refs.contains(&reference) {
                            refs.push(reference);
                        }
                    }
                }
            }
        }
        // hover Markdown 也按用户开关参与图片解析；这是明确的 opt-in 路径。
        if self.hover_preview_markdown {
            if let Some((index, _)) = self.custom_hover {
                if let Some(preview) = self.custom_previews.get(index) {
                    if let Some(text) = &preview.text {
                        for reference in crate::image_store::markdown_refs(text) {
                            if !refs.contains(&reference) {
                                refs.push(reference);
                            }
                        }
                    }
                }
            }
        }
        for reference in refs {
            // 自定义图片 preview
            if let Some(index) = self
                .custom_previews
                .iter()
                .position(|preview| preview.image_id == reference.record_id)
            {
                let Some(preview) = self.custom_previews.get(index) else {
                    continue;
                };
                if preview.kind != CustomKind::Image {
                    continue;
                }
                let current = crate::image_store::ImageRef {
                    record_id: preview.image_id.clone(),
                    revision: preview.image_revision,
                };
                self.images
                    .set_revision(&current.record_id, current.revision);
                if current == reference && self.images.get(&reference).is_some() {
                    continue;
                }
                if current != reference {
                    if let Some(png) = self.images.get(&current) {
                        self.images.insert(&reference, png);
                        ui.ctx().forget_image(&crate::image_store::image_uri(
                            &reference.record_id,
                            reference.revision,
                        ));
                        continue;
                    }
                }
                if preview.snapshot.is_some() {
                    // 字节被 LRU 淘汰：丢弃显示纹理，重新生成并登记。
                    if let Some(preview) = self.custom_previews.get_mut(index) {
                        preview.snapshot = None;
                    }
                }
                self.ensure_custom_snapshot(ui, session, index);
                // 旧 Markdown 引用可能还带旧 revision：把当前图作为别名登记，
                // 避免改正文；下次编辑/导出时可再迁移 URI。
                if current != reference {
                    if let Some(png) = self.images.get(&current) {
                        self.images.insert(&reference, png);
                        ui.ctx().forget_image(&crate::image_store::image_uri(
                            &reference.record_id,
                            reference.revision,
                        ));
                    }
                }
                continue;
            }
            // 被引用点
            let found = self
                .dest_marks
                .iter()
                .enumerate()
                .find_map(|(page, marks)| {
                    marks
                        .iter()
                        .position(|mark| mark.image_id == reference.record_id)
                        .map(|index| (page, index))
                });
            if let Some((page_index, index)) = found {
                let Some(mark) = self
                    .dest_marks
                    .get(page_index)
                    .and_then(|marks| marks.get(index))
                else {
                    continue;
                };
                let (rect, scale, image_id, revision) = (
                    mark.rect,
                    mark.scale,
                    mark.image_id.clone(),
                    mark.image_revision,
                );
                let current = crate::image_store::ImageRef {
                    record_id: image_id.clone(),
                    revision,
                };
                self.images
                    .set_revision(&current.record_id, current.revision);
                if current == reference && self.images.get(&reference).is_some() {
                    continue;
                }
                if current != reference {
                    if let Some(png) = self.images.get(&current) {
                        self.images.insert(&reference, png);
                        ui.ctx().forget_image(&crate::image_store::image_uri(
                            &reference.record_id,
                            reference.revision,
                        ));
                        continue;
                    }
                }
                if mark.snapshot.is_some() {
                    if let Some(mark) = self
                        .dest_marks
                        .get_mut(page_index)
                        .and_then(|marks| marks.get_mut(index))
                    {
                        mark.snapshot = None;
                    }
                }
                let register = current.clone();
                let texture = self.preview_texture(
                    ui,
                    session,
                    PageIndex::new(page_index as u32),
                    rect,
                    scale,
                    format!("dest-ref-{page_index}-{index}"),
                    "被引用点",
                    Some(register.clone()),
                );
                if let Some(texture) = texture {
                    if let Some(mark) = self
                        .dest_marks
                        .get_mut(page_index)
                        .and_then(|marks| marks.get_mut(index))
                    {
                        mark.snapshot = Some(texture);
                    }
                }
                if current != reference {
                    if let Some(png) = self.images.get(&register) {
                        self.images.insert(&reference, png);
                        ui.ctx().forget_image(&crate::image_store::image_uri(
                            &reference.record_id,
                            reference.revision,
                        ));
                    }
                }
            }
        }
    }

    /// 为"**正在显示**、但还缺预览图"的自定义图片 preview 重试生成。
    ///
    /// 只处理已被浮窗引用的条目：不能对存档里的每一条都预渲染（那会在打开文档时
    /// 把几十个区域全提交渲染）。
    pub(super) fn ensure_pending_custom_snapshots(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
    ) {
        let mut shown: Vec<usize> = self
            .floats
            .iter()
            .filter_map(|window| window.custom_index())
            .collect();
        shown.sort_unstable();
        shown.dedup();
        for index in shown {
            let missing = self.custom_previews.get(index).is_some_and(|preview| {
                preview.kind == CustomKind::Image && preview.snapshot.is_none()
            });
            if missing {
                self.ensure_custom_snapshot(ui, session, index);
            }
        }
    }

    /// 确保某个自定义**图片** preview 有可用截图（缺则按**记录的分辨率**重新生成）。
    pub(super) fn ensure_custom_snapshot(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        index: usize,
    ) {
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        if preview.kind != CustomKind::Image || preview.snapshot.is_some() {
            return;
        }
        let (page, page_rect, scale) = (preview.page, preview.rect, preview.scale);
        let preview_id = preview.id;
        let image_ref = (!preview.image_id.is_empty()).then(|| crate::image_store::ImageRef {
            record_id: preview.image_id.clone(),
            revision: preview.image_revision,
        });
        let Some(texture) = self.preview_texture(
            ui,
            session,
            page,
            page_rect,
            scale,
            format!("custom-image-{preview_id}"),
            "自定义图片",
            image_ref,
        ) else {
            return;
        };
        if let Some(preview) = self.custom_previews.get_mut(index) {
            preview.snapshot = Some(texture);
        }
    }

    /// 连续拖拽同一 preview 的 resize 合并成一条历史（1 秒窗口）。
    pub(super) fn apply_preview_resize(
        &mut self,
        key: PreviewKey,
        before: egui::Vec2,
        after: egui::Vec2,
    ) {
        let now = self.clock.now();
        let merge = self.last_preview_resize.is_some_and(|(last_key, last_at)| {
            last_key == key
                && now.duration_since(last_at) <= std::time::Duration::from_secs(1)
                && !self.history.can_redo()
        });
        let merged = merge
            && self.history.last_mut().is_some_and(
                |entry| matches!(entry.command, AppEdit::ResizePreview { key: k, .. } if k == key),
            );
        if merged {
            self.apply_edit(AppEdit::ResizePreview { key, before, after }, false);
            if let Some(entry) = self.history.last_mut() {
                if let AppEdit::ResizePreview { after: last, .. } = &mut entry.command {
                    *last = after;
                }
            }
            self.refresh_dirty();
        } else {
            self.apply_edit(AppEdit::ResizePreview { key, before, after }, true);
        }
        self.last_preview_resize = Some((key, now));
    }

    /// 预留的顺序调整接口：当前侧栏还没有拖拽 UI。
    #[allow(dead_code)]
    pub(super) fn move_custom_preview(&mut self, key: PreviewKey, to: usize) {
        if let Some(from) = self.preview_index(key) {
            self.apply_edit(AppEdit::MovePreview { key, from, to }, true);
        }
    }

    /// 按稳定键查找 preview 的全局下标。
    pub(super) fn preview_index(&self, key: PreviewKey) -> Option<usize> {
        self.custom_previews
            .iter()
            .position(|preview| preview.id == key.id && preview.page == key.page)
    }

    /// 删除 `custom_previews[index]` 之后，把浮窗里的下标引用左移并移除指向它的浮窗。
    pub(super) fn shift_float_custom_indices(&mut self, index: usize) {
        for window in &mut self.floats {
            if let FloatOwner::Custom(custom) = window.owner {
                if custom > index {
                    window.owner = FloatOwner::Custom(custom - 1);
                }
            }
        }
        self.floats
            .retain(|window| window.owner != FloatOwner::Custom(index));
    }

    pub(super) fn spawn_custom_float(&mut self, index: usize, rect: egui::Rect) {
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        // 权威尺寸直接取记录值：与记录完全一致，天然不会误判"已修改"
        let body = preview.body_size;
        let id = self.next_float_id;
        self.next_float_id += 1;
        let content = match (preview.kind, preview.snapshot.clone()) {
            (CustomKind::Image, Some(texture)) => FloatContent::Image(texture),
            _ => FloatContent::Text(preview.text.clone().unwrap_or_default()),
        };
        let title = if preview.title.is_empty() {
            "自定义preview".to_string()
        } else {
            preview.title.clone()
        };
        self.floats.push(Float {
            id,
            title,
            content,
            rect,
            owner: FloatOwner::Custom(index),
            body,
            origin: None,
        });
        self.highlight_float = Some(id);
    }

    /// 分配本页新的 preview id；删除不回收，保证会话内 `(page, id)` 唯一。
    pub(super) fn allocate_preview_id(&mut self, page: PageIndex) -> u64 {
        let index = page.get() as usize;
        if self.next_preview_id.len() <= index {
            self.next_preview_id.resize(index + 1, 0);
        }
        let id = self.next_preview_id[index];
        self.next_preview_id[index] += 1;
        id
    }

    /// 删除某个自定义 preview：同时清掉它的浮窗，并把后续条目的 `custom` 下标左移。
    pub(super) fn delete_custom_preview(&mut self, index: usize) {
        if index >= self.custom_previews.len() {
            return;
        }
        let preview = self.custom_previews[index].clone();
        let key = PreviewKey {
            page: preview.page,
            id: preview.id,
        };
        self.apply_edit(
            AppEdit::DeletePreview {
                key,
                position: index,
                preview: std::sync::Arc::new(preview),
            },
            true,
        );
        self.message = "已删除该自定义preview（Ctrl+Z 可撤销）".to_string();
    }

    /// 若某个自定义 preview 已有**正式浮窗**，把它高亮并置顶（侧栏悬浮/点击共用）。
    pub(super) fn highlight_existing_custom_float(&mut self, index: usize) -> Option<u64> {
        let customs: Vec<Option<usize>> = self.floats.iter().map(|f| f.custom_index()).collect();
        let position = find_float_for_custom(&customs, index)?;
        let window = self.floats.remove(position);
        let id = window.id;
        self.floats.push(window);
        self.highlight_float = Some(id);
        Some(id)
    }

    /// 侧栏条目被点击：生成正式浮窗；若已存在则高亮 + 置顶（一次性高亮有时限）。
    pub(super) fn activate_custom_preview(&mut self, index: usize, row: egui::Rect) {
        let customs: Vec<Option<usize>> = self.floats.iter().map(|f| f.custom_index()).collect();
        if let Some(position) = find_float_for_custom(&customs, index) {
            let window = self.floats.remove(position);
            let id = window.id;
            self.floats.push(window);
            // 一次性高亮（点击动作需要有限时长）+ 当帧也置为高亮
            self.click_highlight = Some((id, self.clock.now() + CLICK_HIGHLIGHT));
            self.highlight_float = Some(id);
            self.message = "已唤出该自定义preview浮窗".to_string();
            return;
        }
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        let id = self.next_float_id;
        self.next_float_id += 1;
        // 正文尺寸使用该条目**储存的 body_size**（用户要求）
        let size = float_size_for_body(preview.body_size, FLOAT_TITLE_HEIGHT);
        let rect = self.float_rect_at_row(row, size);
        self.spawn_custom_float(index, rect);
        self.click_highlight = Some((id, self.clock.now() + CLICK_HIGHLIGHT));
        self.message = "已生成该自定义preview浮窗".to_string();
    }

    /// 关闭浮窗并清理正文编辑状态（如果编辑的正是这个浮窗）。
    pub(super) fn close_float(&mut self, id: u64) {
        let owner = self
            .floats
            .iter()
            .find(|window| window.id == id)
            .map(|w| w.owner);
        self.floats.retain(|window| window.id != id);
        self.markdown_caches.remove(&id);
        self.markdown_viewports.remove(&id);
        self.markdown_prepared.remove(&id);
        if owner.is_some() && self.body_edit_owner() == owner {
            self.editing_body = None;
        }
        self.message = format!("已关闭浮窗 #{id}");
    }

    /// 绘制并处理浮窗：移动（拖标题栏）、改大小（拖右下角）、关闭（右上角 ✕）。
    ///
    /// 注册顺序即层级：按 `floats` 顺序注册 → **后加入的浮窗既画在上面、也优先拿到鼠标**
    /// （用户要求"后出现的浮窗更高"）。子区域（标题栏/把手/关闭按钮）在主提之后注册，
    /// 因此优先于主体，不会被主体吞掉。
    pub(super) fn draw_floats(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        self.markdown_rendered = false;
        if self.floats.is_empty() {
            return;
        }
        let mut close: Option<u64> = None;
        let mut raise: Option<u64> = None;
        // 点击浮窗任意处（含链接、滚动条）→ 置顶；后画的浮窗在循环后面，
        // 因此天然覆盖先画的（顶层优先）。
        let clicked_pos = ui.ctx().input(|input| {
            input
                .pointer
                .any_click()
                .then(|| input.pointer.interact_pos())
                .flatten()
        });
        // 多个浮窗重叠时，滚轮只归 z 轴最高的那个；否则先注册的下层
        // ScrollArea 会先消费滚轮，看起来像“滚了下面的窗口”。
        let topmost_under_pointer = ui.ctx().pointer_hover_pos().and_then(|position| {
            self.floats
                .iter()
                .rev()
                .find(|window| window.rect.contains(position))
                .map(|window| window.id)
        });
        for index in 0..self.floats.len() {
            // 每帧夹取回视图区域内（用户要求：浮窗始终在视图区域内部）
            let rect = clamp_float_rect(self.floats[index].rect, canvas, FLOAT_MIN_SIZE);
            self.floats[index].rect = rect;
            let id = self.floats[index].id;
            // 空正文也要保留“可编辑”语义：标题栏 ✎ / 双击进入编辑器，不能因为
            // 文本为空就把命中区域或入口一起省掉。
            let has_text = matches!(&self.floats[index].content, FloatContent::Text(_));
            let markdown_active = self.markdown_floats && has_text;
            let wheel_owner = topmost_under_pointer == Some(id);

            // 主体：占住点击（避免"点在浮窗上却给文档落了光标"）。
            // 纯文本模式沿用"整个主体可拖动"；Markdown 模式只允许标题栏拖动，
            // 正文留给内部滚动条/链接/双击编辑。
            let body_sense = if markdown_active {
                Sense::click()
            } else {
                Sense::click_and_drag()
            };
            let body = ui.interact(rect, Id::unique(("float", id)), body_sense);
            if body.clicked() || body.drag_started() {
                raise = Some(id);
            }
            if let Some(position) = clicked_pos {
                if rect.contains(position) {
                    raise = Some(id);
                }
            }
            if body.dragged() {
                self.floats[index].rect =
                    drag_float_rect(rect, body.drag_delta(), canvas, FLOAT_MIN_SIZE);
            }

            // 标题栏：拖动移动
            let title_rect =
                egui::Rect::from_min_size(rect.min, EVec2::new(rect.width(), FLOAT_TITLE_HEIGHT));
            // 标题栏：**点击或开始拖动都要置顶**（用户要求：点击区域要包含标题栏）；
            // 拖动则移动浮窗。
            let title = ui.interact(
                title_rect,
                Id::unique(("float-title", id)),
                Sense::click_and_drag(),
            );
            if title.clicked() || title.drag_started() {
                raise = Some(id);
            }
            if title.dragged() {
                self.floats[index].rect =
                    drag_float_rect(rect, title.drag_delta(), canvas, FLOAT_MIN_SIZE);
            }

            // 右下角把手：拖动改大小（不小于最小尺寸）
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(
                    rect.right() - FLOAT_HANDLE_SIZE,
                    rect.bottom() - FLOAT_HANDLE_SIZE,
                ),
                EVec2::splat(FLOAT_HANDLE_SIZE),
            );
            let handle = ui.interact(handle_rect, Id::unique(("float-resize", id)), Sense::drag());
            if handle.dragged() {
                let delta = handle.drag_delta();
                let resized = egui::Rect::from_min_size(
                    rect.min,
                    EVec2::new(
                        (rect.width() + delta.x).max(FLOAT_MIN_SIZE.x),
                        (rect.height() + delta.y).max(FLOAT_MIN_SIZE.y),
                    ),
                );
                self.floats[index].rect = resized;
                // 尺寸是**权威值**：只在真正改大小时写入（不从 rect 每帧反算）
                self.floats[index].body = body_size_of(resized, FLOAT_TITLE_HEIGHT);
            }

            // 右上角关闭按钮：**画成带底色的圆**，悬停/按下变红 —— 否则只有一个"✕"太不显眼
            let close_center = egui::pos2(title_rect.right() - 13.0, title_rect.center().y);
            let close_rect = egui::Rect::from_center_size(close_center, EVec2::splat(18.0));
            let close_response =
                ui.interact(close_rect, Id::unique(("float-close", id)), Sense::click());
            let close_color = if close_response.is_pointer_button_down_on() {
                Color32::from_rgb(170, 45, 45)
            } else if close_response.hovered() {
                Color32::from_rgb(205, 70, 70)
            } else {
                Color32::from_gray(95)
            };
            ui.painter().circle_filled(close_center, 8.5, close_color);
            ui.painter().text(
                close_center,
                Align2::CENTER_CENTER,
                "✕",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            if close_response.on_hover_text("关闭浮窗").clicked() {
                let owner = self.floats[index].owner;
                if self.body_edit_owner() == Some(owner) && self.body_edit_dirty() {
                    self.pending_edit_action = Some(EditDiscardAction::CloseFloat(id));
                } else {
                    close = Some(id);
                }
            }

            // ---- 绘制 ----
            // 被"再次悬停同一目的地"激发时高亮：边框加粗变橙、标题栏变暖色
            // 高亮有两个来源：悬浮（每帧判定）与点击（一次性，带截止时刻）
            let clicked_highlight = self
                .click_highlight
                .is_some_and(|(target, until)| target == id && self.clock.now() < until);
            let highlighted = self.highlight_float == Some(id) || clicked_highlight;
            let (border, border_width, title_color) = if highlighted {
                (
                    Color32::from_rgb(255, 170, 60),
                    2.0,
                    Color32::from_rgb(140, 96, 36),
                )
            } else {
                (Color32::from_gray(130), 1.0, Color32::from_rgb(54, 58, 66))
            };
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(30, 32, 36));
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(border_width, border),
                StrokeKind::Inside,
            );
            painter.rect_filled(title_rect, CornerRadius::same(4), title_color);
            // 标题：**单行截断**（长标题不再压到关闭按钮上）
            let title_font = egui::FontId::proportional(13.0);
            let title_job = overflow_layout(
                &self.floats[index].title,
                title_font,
                Color32::WHITE,
                title_rect.width() - 54.0,
                1,
            );
            let title_galley = painter.layout_job(title_job);
            painter.galley(
                egui::pos2(
                    title_rect.min.x + 8.0,
                    title_rect.center().y - title_galley.size().y * 0.5,
                ),
                title_galley,
                Color32::WHITE,
            );
            // 缩放手柄示意
            painter.rect_filled(
                handle_rect.shrink(5.0),
                CornerRadius::ZERO,
                Color32::from_gray(120),
            );
            // 自定义 preview 的浮窗：内容从记录里**每帧刷新**，这样"快照迟到"也能自动补上
            if let Some(custom_index) = self.floats[index].custom_index() {
                if let Some(preview) = self.custom_previews.get(custom_index) {
                    let fresh = match (preview.kind, preview.snapshot.clone()) {
                        (CustomKind::Image, Some(texture)) => Some(FloatContent::Image(texture)),
                        (CustomKind::Text, _) => {
                            Some(FloatContent::Text(preview.text.clone().unwrap_or_default()))
                        }
                        _ => None,
                    };
                    if let Some(fresh) = fresh {
                        self.floats[index].content = fresh;
                    }
                    // 题目也要同步：否则从 ✎ 改完题目，已有浮窗仍显示旧标题
                    let title = if preview.title.is_empty() {
                        "自定义preview".to_string()
                    } else {
                        preview.title.clone()
                    };
                    self.floats[index].title = title;
                }
            }
            // 注释正文：每帧从 Annotation.body 刷新（编辑/撤销后立即同步）
            if let FloatOwner::Annotation(id) = self.floats[index].owner {
                if let Some(annotation) = self.annotations.get(id) {
                    self.floats[index].content =
                        FloatContent::Text(annotation.body.clone().unwrap_or_default());
                    self.floats[index].title = format!("标注 #{}", id.get());
                }
            }
            // 标题栏编辑按钮：双击正文在 Markdown 模式下可能被文字选择/链接抢走，
            // 这里提供一个稳定入口；图标与侧栏 Pin 条目的「编辑标题」一致。
            let editable =
                text_float_is_editable(self.floats[index].owner, &self.floats[index].content);
            if editable && self.editing_body.is_none() {
                let edit_center = egui::pos2(close_center.x - 26.0, title_rect.center().y);
                let edit_rect = egui::Rect::from_center_size(edit_center, EVec2::splat(18.0));
                let edit_response =
                    ui.interact(edit_rect, Id::unique(("float-edit", id)), Sense::click());
                let edit_color = if edit_response.hovered() {
                    Color32::from_gray(235)
                } else {
                    Color32::from_gray(190)
                };
                ui.painter().text(
                    edit_center,
                    Align2::CENTER_CENTER,
                    CUSTOM_SYMBOL_EDIT,
                    egui::FontId::proportional(13.0),
                    edit_color,
                );
                if edit_response.on_hover_text("编辑正文").clicked() {
                    if let FloatContent::Text(text) = &self.floats[index].content {
                        let owner = self.floats[index].owner;
                        let text = text.clone();
                        self.open_body_editor(owner, text, None, ui.ctx());
                    }
                }
            }
            // 图片 preview 浮窗：@ 复制内部图片引用，便于粘进注释正文。
            if let FloatOwner::Custom(custom_index) = self.floats[index].owner {
                if matches!(&self.floats[index].content, FloatContent::Image(_)) {
                    if let Some(preview) = self.custom_previews.get(custom_index) {
                        if !preview.image_id.is_empty() {
                            let uri = crate::image_store::image_uri(
                                &preview.image_id,
                                preview.image_revision,
                            );
                            let center = egui::pos2(close_center.x - 26.0, title_rect.center().y);
                            let button_rect =
                                egui::Rect::from_center_size(center, EVec2::splat(18.0));
                            let response = ui.interact(
                                button_rect,
                                Id::unique(("float-image-ref", id)),
                                Sense::click(),
                            );
                            ui.painter().text(
                                center,
                                Align2::CENTER_CENTER,
                                "@",
                                egui::FontId::proportional(13.0),
                                if response.hovered() {
                                    Color32::from_gray(235)
                                } else {
                                    Color32::from_gray(190)
                                },
                            );
                            if response.on_hover_text("复制 Markdown 图片引用").clicked() {
                                ui.ctx().copy_text(uri);
                                self.message = "已复制图片引用，可粘到正文".to_string();
                            }
                        }
                    }
                }
            }
            // Markdown 模式：正文的 body 先于内容控件注册，双击普通文字会落到 body；
            // 链接/滚动条的单次点击由它们自己优先，不会被双击层吞掉。
            let markdown_active = self.markdown_floats
                && matches!(&self.floats[index].content, FloatContent::Text(text) if !text.is_empty());
            if markdown_active && body.double_clicked() && self.editing_body.is_none() {
                if let FloatContent::Text(text) = &self.floats[index].content {
                    let owner = self.floats[index].owner;
                    let text = text.clone();
                    let body_area = egui::Rect::from_min_max(
                        rect.min + EVec2::new(8.0, FLOAT_TITLE_HEIGHT + 6.0),
                        rect.max - EVec2::new(18.0, 16.0),
                    );
                    let viewport = self
                        .markdown_viewports
                        .get(&self.floats[index].id)
                        .copied()
                        .unwrap_or((egui::Vec2::ZERO, egui::Vec2::ZERO));
                    let cursor = body.interact_pointer_pos().map(|position| {
                        body_cursor_from_pointer(
                            ui.ctx(),
                            &text,
                            body_area,
                            position,
                            true,
                            viewport.0,
                            Some(viewport.1),
                        )
                    });
                    self.open_body_editor(owner, text, cursor, ui.ctx());
                }
            }
            match &self.floats[index].content {
                FloatContent::Text(text) if markdown_active => {
                    // Markdown 正文：子 Ui + 内部 ScrollArea（可滚动、可点链接）。
                    // 右侧留出滚动条位置，底部留出缩放手柄位置。
                    let body_area = egui::Rect::from_min_max(
                        rect.min + EVec2::new(8.0, FLOAT_TITLE_HEIGHT + 6.0),
                        rect.max - EVec2::new(18.0, 16.0),
                    );
                    if text.is_empty() {
                        // 空正文没有 CommonMark 内容；不创建 ScrollArea（它会吞掉双击），
                        // 用一个明确的正文点击层作为唯一响应。
                        self.markdown_rendered = true;
                        if body_area.width() > 0.0 && body_area.height() > 0.0 {
                            let hit = ui.interact(
                                body_area,
                                Id::unique(("float-md-empty-edit", id)),
                                Sense::click(),
                            );
                            if hit.double_clicked() && self.editing_body.is_none() {
                                let owner = self.floats[index].owner;
                                self.open_body_editor(owner, String::new(), None, ui.ctx());
                            }
                        }
                    } else if body_area.width() > 0.0 && body_area.height() > 0.0 {
                        let source_hash = markdown_source_hash(text);
                        let prepared = self.markdown_prepared.remove(&id);
                        let prepared = match prepared {
                            Some(prepared) if prepared.source_hash == source_hash => prepared,
                            _ => {
                                let normalized = crate::markdown::normalize_display_math(
                                    &crate::markdown::preserve_single_line_breaks(text),
                                );
                                let image_refs = crate::image_store::markdown_refs(&normalized);
                                let min_width = {
                                    let mut measure = |line: &str, monospace: bool| {
                                        let font = if monospace {
                                            egui::FontId::monospace(13.0)
                                        } else {
                                            egui::FontId::proportional(13.0)
                                        };
                                        ui.fonts_mut(|fonts| {
                                            fonts
                                                .layout_no_wrap(
                                                    line.to_owned(),
                                                    font,
                                                    Color32::from_gray(225),
                                                )
                                                .size()
                                                .x
                                        })
                                    };
                                    crate::markdown::min_unwrapped_width(&normalized, &mut measure)
                                };
                                PreparedMarkdown {
                                    source_hash,
                                    normalized,
                                    image_refs,
                                    min_width,
                                }
                            }
                        };
                        let min_width = prepared.min_width;
                        let text = crate::image_store::canonicalize_markdown(
                            &prepared.normalized,
                            &self.images,
                        );
                        self.markdown_prepared.insert(id, prepared);
                        let view_id = Id::unique(("float-md", id));
                        let default_width = if min_width > body_area.width() {
                            Some(
                                (min_width.ceil() as usize)
                                    .max(1)
                                    .min(canvas.width().max(1.0) as usize),
                            )
                        } else {
                            None
                        };
                        let mut md_cache = self.markdown_caches.remove(&id).unwrap_or_default();
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(body_area).id_salt(view_id),
                            |ui| {
                                ui.set_clip_rect(body_area.intersect(canvas));
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                                ui.style_mut().url_in_tooltip = true;
                                let mut visuals = egui::Visuals::dark();
                                visuals.override_text_color = Some(Color32::from_gray(225));
                                visuals.hyperlink_color = Color32::from_rgb(120, 180, 255);
                                *ui.visuals_mut() = visuals;
                                ui.spacing_mut().item_spacing = EVec2::new(0.0, 4.0);
                                // 数学公式走 render_math_fn（egui_commonmark 会在
                                // 有回调时启用 ENABLE_MATH）；缓存按公式/行内/DPI。
                                let math_cache = self.math_cache.clone();
                                let math_fn = move |ui: &mut egui::Ui, tex: &str, inline: bool| {
                                    crate::math::paint(ui, &math_cache, tex, inline);
                                };
                                let scroll_source = egui::scroll_area::ScrollSource {
                                    mouse_wheel: wheel_owner,
                                    ..Default::default()
                                };
                                let scroll_output = egui::ScrollArea::both()
                                    .id_salt(view_id.with("scroll"))
                                    .scroll_source(scroll_source)
                                    .auto_shrink([false, false])
                                    .content_margin(egui::Margin::same(4))
                                    .show(ui, |ui| {
                                        egui_commonmark::CommonMarkViewer::new()
                                            .default_width(default_width)
                                            .enable_scroll_to_heading(true)
                                            .render_math_fn(Some(&math_fn))
                                            .show(ui, &mut md_cache, &text);
                                    });
                                self.markdown_viewports.insert(
                                    id,
                                    (
                                        scroll_output.state.clamped_offset(),
                                        scroll_output.content_size,
                                    ),
                                );
                            },
                        );
                        self.markdown_caches.insert(id, md_cache);
                        self.markdown_rendered = true;
                    }
                }
                FloatContent::Text(text) => {
                    // 正文：**自动换行 + 行数上限（放不下就截断并显示 …）**
                    let body_origin = rect.min + EVec2::new(10.0, FLOAT_TITLE_HEIGHT + 8.0);
                    let body_area =
                        egui::Rect::from_min_max(body_origin, rect.max - EVec2::new(10.0, 8.0));
                    if body_area.width() > 0.0 && body_area.height() > 0.0 {
                        // 行高按字体 1.3 倍估算；至少保留 1 行
                        let row_height = 13.0 * 1.3;
                        let rows = ((body_area.height() / row_height).floor() as usize).max(1);
                        let job = overflow_layout(
                            text,
                            egui::FontId::proportional(13.0),
                            Color32::from_gray(225),
                            body_area.width(),
                            rows,
                        );
                        let galley = painter.layout_job(job);
                        painter.with_clip_rect(body_area).galley(
                            body_area.min,
                            galley,
                            Color32::from_gray(225),
                        );
                    }
                }
                FloatContent::Image(texture) => {
                    // 正文区域：按**保持长宽比**放进窗口（居中、不拉伸变形）；
                    // 改变窗口大小只影响显示尺寸，图片内容本身不变。
                    let body = egui::Rect::from_min_max(
                        rect.min + EVec2::new(4.0, FLOAT_TITLE_HEIGHT + 4.0),
                        rect.max - EVec2::new(4.0, 4.0),
                    );
                    let size = texture.size_vec2();
                    let target = fit_inside(size, body);
                    painter.image(
                        texture.id(),
                        target,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
        }
        if let Some(id) = close {
            self.close_float(id);
        } else if let Some(id) = raise {
            // 关闭按钮优先；否则把被点击的浮窗移到末尾（最上层），其余顺序不变
            if let Some(index) = self.floats.iter().position(|window| window.id == id) {
                move_to_last(&mut self.floats, index);
            }
        }
    }
}
