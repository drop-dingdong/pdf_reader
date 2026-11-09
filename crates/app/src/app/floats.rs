//! `app::floats`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    body_size_of, clamp_float_rect, drag_float_rect, find_float_for_custom, fit_inside,
    float_rect_at_row, float_size_for_body, move_to_last, overflow_layout, parse_float_link,
    FloatLinkTarget, FLOAT_HANDLE_SIZE, FLOAT_MIN_SIZE, FLOAT_TITLE_HEIGHT,
};
use eframe::egui;
use egui::{Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Vec2 as EVec2};

/// 指针是否落在画布之上的窗口/弹层（而不是画布本层）。
///
/// 不用 `Context::is_pointer_over_egui()`：它按"root Ui 剩余可用矩形"判断，
/// 而本应用用 `CentralPanel` 把剩余区域全部消费掉，导致画布上的普通 hover 也被判成
/// "在 egui 之外"。也不能简单判断 `order != Background`：回放/某些布局里画布本身在
/// Middle 层，必须和 `canvas_layer` 比较；只有**别的**非 Background 层才算遮住画布。
/// `layer_id_at` 用上一帧图层，最多晚一帧。
pub(super) fn pointer_over_overlay(
    ctx: &egui::Context,
    canvas_layer: egui::LayerId,
    position: egui::Pos2,
) -> bool {
    ctx.layer_id_at(position)
        .is_some_and(|layer| layer != canvas_layer && layer.order != egui::Order::Background)
}

/// 从 `pd-float://...?a=b&c=d` 里取一个查询参数。
fn query_value<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|part| {
        part.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

/// 浮窗的正文区域：标题栏以下、整个浮窗宽度。
///
/// 标题栏（含 ✎/✕/@ 按钮）不属于正文；双击判定与正文编辑入口都用它。
pub(super) fn float_body_rect(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + FLOAT_TITLE_HEIGHT),
        rect.max,
    )
}

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
                        .markdown
                        .prepared
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
            // 每次 owner 扫描都重新判定“这张图还能不能获取”。如果来源 preview 被删除、
            // 或 dest mark 已不存在，就不能继续用 ImageStore 里的旧缓存（否则文字浮窗
            // 仍显示已删除的图，且不会重新检测）。
            let has_owner = self.custom_previews.iter().any(|preview| {
                preview.kind == CustomKind::Image && preview.image_id == reference.record_id
            }) || self.dest_marks.iter().any(|marks| {
                marks
                    .iter()
                    .any(|mark| mark.image_id == reference.record_id)
            });
            if !has_owner {
                for revision in self.markdown.images.remove_record(&reference.record_id) {
                    ui.ctx().forget_image(&crate::image_store::image_uri(
                        &reference.record_id,
                        revision,
                    ));
                }
                ui.ctx().forget_image(&crate::image_store::image_uri(
                    &reference.record_id,
                    reference.revision,
                ));
                continue;
            }
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
                self.markdown
                    .images
                    .set_revision(&current.record_id, current.revision);
                if current == reference && self.markdown.images.get(&reference).is_some() {
                    continue;
                }
                if current != reference {
                    if let Some(png) = self.markdown.images.get(&current) {
                        self.markdown.images.insert(&reference, png);
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
                    if let Some(png) = self.markdown.images.get(&current) {
                        self.markdown.images.insert(&reference, png);
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
                self.markdown
                    .images
                    .set_revision(&current.record_id, current.revision);
                if current == reference && self.markdown.images.get(&reference).is_some() {
                    continue;
                }
                if current != reference {
                    if let Some(png) = self.markdown.images.get(&current) {
                        self.markdown.images.insert(&reference, png);
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
                    if let Some(png) = self.markdown.images.get(&register) {
                        self.markdown.images.insert(&reference, png);
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

    /// 确保显示中的 dest 浮窗有截图；重选 rect 后提交 `snapshot=None`，
    /// 这里按 `preview_texture` 的 overscan 分辨率生成，避免固定浮窗比 hover 糊。
    pub(super) fn ensure_pending_dest_snapshots(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
    ) {
        let mut shown: Vec<(PageIndex, usize)> = self
            .floats
            .iter()
            .filter_map(|window| window.origin)
            .collect();
        shown.sort_unstable();
        shown.dedup();
        for (page, index) in shown {
            let missing = self
                .dest_marks
                .get(page.get() as usize)
                .and_then(|marks| marks.get(index))
                .is_some_and(|mark| mark.snapshot.is_none());
            if missing {
                self.ensure_dest_snapshot(ui, session, page, index);
            }
        }
    }

    /// 按记录的分辨率生成一条 dest 截图。
    pub(super) fn ensure_dest_snapshot(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        page: PageIndex,
        index: usize,
    ) {
        let Some(mark) = self
            .dest_marks
            .get(page.get() as usize)
            .and_then(|marks| marks.get(index))
        else {
            return;
        };
        if mark.snapshot.is_some() {
            return;
        }
        let (rect, scale) = (mark.rect, mark.scale);
        let image_ref = (!mark.image_id.is_empty()).then(|| crate::image_store::ImageRef {
            record_id: mark.image_id.clone(),
            revision: mark.image_revision,
        });
        let Some(texture) = self.preview_texture(
            ui,
            session,
            page,
            rect,
            scale,
            format!("dest-image-{}-{}", page.get(), index),
            "被引用点",
            image_ref,
        ) else {
            return;
        };
        if let Some(mark) = self
            .dest_marks
            .get_mut(page.get() as usize)
            .and_then(|marks| marks.get_mut(index))
        {
            mark.snapshot = Some(texture);
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
        let now = self.runtime.clock.now();
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

    /// 指针下**最上层**的浮窗（`floats` 顺序 = z 序，后画的在上）。
    pub(super) fn topmost_float_at(&self, position: egui::Pos2) -> Option<usize> {
        self.floats
            .iter()
            .rposition(|window| window.rect.contains(position))
    }

    /// 画布内容（注释/链接/右键目标）是否允许响应指针。
    ///
    /// 画布内容不是 egui widget，命中判断必须自己裁剪：面板区域（顶栏/侧栏/状态栏）
    /// 由 `canvas` 矩形排除；弹层/窗口/模态用"指针下是否是非背景层"判断；
    /// 连续动作（拖拽/画墨迹/截图选区）期间不响应新的 hover。
    pub(super) fn canvas_hover_allowed(
        &self,
        ctx: &egui::Context,
        canvas: egui::Rect,
        position: egui::Pos2,
        using_pointer: bool,
    ) -> bool {
        canvas.contains(position)
            && !pointer_over_overlay(ctx, self.canvas_layer, position)
            && !using_pointer
            && self.topmost_float_at(position).is_none()
    }

    /// 连续拖拽同一引用点浮窗 resize 合并成一条历史（1 秒窗口）。
    pub(super) fn apply_dest_resize(
        &mut self,
        page: PageIndex,
        index: usize,
        before: egui::Vec2,
        after: egui::Vec2,
    ) {
        let now = self.runtime.clock.now();
        let target = (page, index);
        let merge = self.last_dest_resize.is_some_and(|(last, last_at)| {
            last == target
                && now.duration_since(last_at) <= std::time::Duration::from_secs(1)
                && !self.history.can_redo()
        });
        let merged = merge
            && self.history.last_mut().is_some_and(|entry| {
                matches!(
                    entry.command,
                    AppEdit::ResizeDestMark { page: p, index: i, .. } if (p, i) == target
                )
            });
        if merged {
            self.apply_edit(
                AppEdit::ResizeDestMark {
                    page,
                    index,
                    before,
                    after,
                },
                false,
            );
            if let Some(entry) = self.history.last_mut() {
                if let AppEdit::ResizeDestMark { after: last, .. } = &mut entry.command {
                    *last = after;
                }
            }
            self.refresh_dirty();
        } else {
            self.apply_edit(
                AppEdit::ResizeDestMark {
                    page,
                    index,
                    before,
                    after,
                },
                true,
            );
        }
        self.last_dest_resize = Some((target, now));
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
        // 先移除被删 preview 自己的浮窗，再给后面的下标前移。
        // 顺序反了会把“前移后恰好变成 index”的其它 preview 浮窗一起删掉
        // （用户实测：删除 preview a 会关掉 preview b 的浮窗）。
        self.floats
            .retain(|window| window.owner != FloatOwner::Custom(index));
        for window in &mut self.floats {
            if let FloatOwner::Custom(custom) = window.owner {
                if custom > index {
                    window.owner = FloatOwner::Custom(custom - 1);
                }
            }
        }
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
            self.click_highlight = Some((id, self.runtime.clock.now() + CLICK_HIGHLIGHT));
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
        self.click_highlight = Some((id, self.runtime.clock.now() + CLICK_HIGHLIGHT));
        self.message = "已生成该自定义preview浮窗".to_string();
    }

    /// 新浮窗的级联落点（没有行列上下文时使用，例如从 `pd-float://` 链接打开）。
    pub(super) fn next_float_position(&self) -> egui::Pos2 {
        let cascade = (self.floats.len() % 6) as f32 * 24.0;
        let base = if self.canvas_rect.width() > 1.0 {
            self.canvas_rect.min
        } else {
            egui::pos2(24.0, 24.0)
        };
        base + egui::vec2(24.0 + cascade, 24.0 + cascade)
    }

    /// 注释的 `pd-float://` URI；外部注释默认返回 None（设置关闭时不给复制入口）。
    pub(super) fn annotation_link_uri(&self, id: AnnotationId) -> Option<String> {
        let annotation = self.annotations.get(id)?;
        let external = self.annotation_pdf.external_ids.contains(&id);
        if external && !self.allow_external_annotation_links {
            return None;
        }
        let mut uri = format!(
            "pd-float://annotation/{}/{}",
            annotation.page.get(),
            id.get()
        );
        if external {
            uri.push_str(&format!(
                "?s={}&k={}",
                self.document_load_token,
                annotation_kind_code(&annotation.kind)
            ));
            if let Some(created) = annotation.created {
                uri.push_str(&format!("&c={created}"));
            }
        }
        Some(uri)
    }

    /// 自定义 preview 的 `pd-float://` URI。
    pub(super) fn preview_link_uri(&self, index: usize) -> Option<String> {
        let preview = self.custom_previews.get(index)?;
        let record = if preview.image_id.is_empty() {
            // 极旧的运行时数据：退回页号 + 每页内序号（仅唯一时可用）。
            format!("{}/{}", preview.page.get(), preview.id)
        } else {
            preview.image_id.clone()
        };
        Some(format!("pd-float://preview/{record}"))
    }

    /// 这条浮窗可被 `pd-float://` 引用的 URI（只有注释/自定义 preview 有稳定目标）。
    pub(super) fn float_link_uri(&self, index: usize) -> Option<String> {
        match self.floats.get(index)?.owner {
            FloatOwner::Annotation(id) => self.annotation_link_uri(id),
            FloatOwner::Custom(index) => self.preview_link_uri(index),
            FloatOwner::None => None,
        }
    }

    /// 浮窗来源页/页内坐标（标题栏 goto 按钮用）。
    pub(super) fn float_goto_target(&self, index: usize) -> Option<(PageIndex, Point)> {
        let window = self.floats.get(index)?;
        match window.owner {
            FloatOwner::Annotation(id) => {
                let annotation = self.annotations.get(id)?;
                let point = annotation
                    .bounds()
                    .map(|bounds| bounds.center())
                    .unwrap_or(Point::new(0.0, 0.0));
                Some((annotation.page, point))
            }
            FloatOwner::Custom(index) => {
                let preview = self.custom_previews.get(index)?;
                Some((preview.page, preview.point))
            }
            FloatOwner::None => {
                let (page, mark_index) = window.origin?;
                let mark = self.dest_marks.get(page.get() as usize)?.get(mark_index)?;
                Some((page, mark.point))
            }
        }
    }

    /// 处理 Markdown 里的 `pd-float://` 链接：打开/置顶目标浮窗，断链只提示。
    pub(super) fn follow_float_link(&mut self, url: &str) {
        let Some(target) = parse_float_link(url) else {
            self.message = format!("无法识别的浮窗链接：{url}");
            return;
        };
        match target {
            FloatLinkTarget::Annotation { page, id: raw } => {
                let id = AnnotationId(raw);
                let Some(annotation) = self.annotations.get(id) else {
                    self.message = format!("断链：注释 #{} 不存在（可能来自其他文档）", id.get());
                    return;
                };
                if page.is_some_and(|page| page != annotation.page) {
                    self.message = format!(
                        "断链：注释 #{} 不在第 {} 页（可能在其它阅读器中被替换）",
                        id.get(),
                        page.map(|page| page.display_number()).unwrap_or_default()
                    );
                    return;
                }
                if self.annotation_pdf.external_ids.contains(&id) {
                    let token = query_value(url, "s").and_then(|value| value.parse::<u64>().ok());
                    if token == Some(self.document_load_token) {
                        let at = self.next_float_position();
                        self.open_annotation_note(id, at);
                        self.message = format!("已跳转到外部注释 #{}", id.get());
                        return;
                    }
                    if !self.allow_external_annotation_links {
                        self.message = format!(
                            "断链：外部注释 #{} 的链接跨会话失效（可在 ? 中开启跨会话外部注释链接）",
                            id.get()
                        );
                        return;
                    }
                    // A+B：先校验类型；created 可验证则直接打开，否则让用户确认。
                    let Some(kind) = query_value(url, "k") else {
                        self.message = "断链：外部注释链接缺少类型校验信息".to_string();
                        return;
                    };
                    if kind != annotation_kind_code(&annotation.kind) {
                        self.message = format!(
                            "断链：外部注释 #{} 的类型已变化（链接可能指向别的注释）",
                            id.get()
                        );
                        return;
                    }
                    if let Some(raw_created) = query_value(url, "c") {
                        if let (Ok(link_created), Some(current)) =
                            (raw_created.parse::<i64>(), annotation.created)
                        {
                            if link_created == current {
                                let at = self.next_float_position();
                                self.open_annotation_note(id, at);
                                self.message = format!("已跳转到外部注释 #{}", id.get());
                                return;
                            }
                            self.message = format!(
                                "断链：外部注释 #{} 的创建时间已变化（链接可能指向别的注释）",
                                id.get()
                            );
                            return;
                        }
                    }
                    // 页号 + 类型匹配，但缺少创建时间：不能完全验证，交给用户确认。
                    self.pending_external_link = Some(PendingExternalLink { id });
                    self.message = "无法完全验证外部注释目标，请在确认框中决定是否打开".to_string();
                    return;
                }
                let at = self.next_float_position();
                self.open_annotation_note(id, at);
                self.message = format!("已跳转到注释 #{}", id.get());
            }
            FloatLinkTarget::PreviewRecord(record) => {
                let mut index = self
                    .custom_previews
                    .iter()
                    .position(|preview| preview.image_id == record);
                if index.is_none() {
                    // 旧写法：preview/<id>，id 是每页内序号；仅唯一时可用。
                    if let Ok(legacy) = record.parse::<u64>() {
                        let matches: Vec<usize> = self
                            .custom_previews
                            .iter()
                            .enumerate()
                            .filter_map(|(index, preview)| (preview.id == legacy).then_some(index))
                            .collect();
                        match matches.as_slice() {
                            [only] => index = Some(*only),
                            [] => {}
                            _ => {
                                self.message = format!(
                                    "链接不明确：id={legacy} 在多页都存在，请改用 pd-float://preview/<页>/<id>"
                                );
                                return;
                            }
                        }
                    }
                }
                match index {
                    Some(index) => self.open_custom_float_by_link(index),
                    None => {
                        self.message = format!("断链：没有记录 id={record} 的自定义preview");
                    }
                }
            }
            FloatLinkTarget::PreviewLegacy { page, id } => {
                match self
                    .custom_previews
                    .iter()
                    .position(|preview| preview.page == page && preview.id == id)
                {
                    Some(index) => self.open_custom_float_by_link(index),
                    None => {
                        self.message = format!(
                            "断链：第 {} 页没有 id={} 的自定义preview",
                            page.display_number(),
                            id
                        );
                    }
                }
            }
        }
    }

    /// 从链接打开/置顶一条自定义 preview 浮窗。
    fn open_custom_float_by_link(&mut self, index: usize) {
        if let Some(id) = self.highlight_existing_custom_float(index) {
            self.click_highlight = Some((id, self.runtime.clock.now() + CLICK_HIGHLIGHT));
            self.message = "已置顶该自定义preview浮窗".to_string();
            return;
        }
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        let size = float_size_for_body(preview.body_size, FLOAT_TITLE_HEIGHT);
        let rect = egui::Rect::from_min_size(self.next_float_position(), size);
        self.spawn_custom_float(index, rect);
        if let Some(window) = self.floats.last() {
            self.click_highlight = Some((window.id, self.runtime.clock.now() + CLICK_HIGHLIGHT));
        }
        self.message = "已打开该自定义preview浮窗".to_string();
    }

    /// 关闭浮窗并清理正文编辑状态（如果编辑的正是这个浮窗）。
    pub(super) fn close_float(&mut self, id: u64) {
        let owner = self
            .floats
            .iter()
            .find(|window| window.id == id)
            .map(|w| w.owner);
        self.floats.retain(|window| window.id != id);
        self.markdown.caches.remove(&id);
        self.markdown.viewports.remove(&id);
        self.markdown.prepared.remove(&id);
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
        self.markdown.rendered = false;
        if self.floats.is_empty() {
            return;
        }
        let mut close: Option<u64> = None;
        let mut raise: Option<u64> = None;
        // 点击浮窗任意处（含链接、滚动条）→ 置顶；后画的浮窗在循环后面，
        // 因此天然覆盖先画的（顶层优先）。
        let clicked_pos = ui
            .ctx()
            .input(|input| {
                input
                    .pointer
                    .any_click()
                    .then(|| input.pointer.interact_pos())
                    .flatten()
            })
            // raw click 只看矩形，不看图层；编辑窗口/弹层覆盖处不能把下面的浮窗置顶。
            .filter(|position| !pointer_over_overlay(ui.ctx(), self.canvas_layer, *position));
        // 多个浮窗重叠时，滚轮只归 z 轴最高的那个；否则先注册的下层
        // ScrollArea 会先消费滚轮，看起来像“滚了下面的窗口”。
        let topmost_under_pointer = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|position| !pointer_over_overlay(ui.ctx(), self.canvas_layer, *position))
            .and_then(|position| {
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
            // 指针在编辑窗口/弹层上时，浮窗不应收到任何点击/拖动动作（否则窗口背景下的
            // 浮窗按钮或 body 仍可能被 egui 命中，导致置顶/编辑）。
            let interactive = !ui.ctx().pointer_hover_pos().is_some_and(|position| {
                pointer_over_overlay(ui.ctx(), self.canvas_layer, position)
            });

            // 拖动遮罩：**先于本浮窗自己的控件注册**，位于所有更低浮窗之上。
            // 只感知 drag、不处理任何动作：用来保证拖动不会落到下层浮窗的
            // 标题栏/图片正文，也不会漏到 PDF 正文（Markdown 正文的 body 只感知 click）。
            let _drag_block =
                ui.interact(rect, Id::unique(("float-drag-block", id)), Sense::drag());

            // 主体：占住点击（避免"点在浮窗上却给文档落了光标"）。
            // 纯文本模式沿用"整个主体可拖动"；Markdown 模式只允许标题栏拖动，
            // 正文留给内部滚动条/链接/双击编辑。
            let body_sense = if markdown_active {
                Sense::click()
            } else {
                Sense::click_and_drag()
            };
            let body = ui.interact(rect, Id::unique(("float", id)), body_sense);
            if interactive && (body.clicked() || body.drag_started()) {
                raise = Some(id);
            }
            if let Some(position) = clicked_pos {
                if rect.contains(position) {
                    raise = Some(id);
                }
            }
            if interactive && body.dragged() {
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
            if interactive && (title.clicked() || title.drag_started()) {
                raise = Some(id);
            }
            if interactive && title.dragged() {
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
            if interactive && handle.dragged() {
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

            let palette = float_palette(self.dark_mode);
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
                palette.muted
            };
            if interactive && close_response.on_hover_text("关闭浮窗").clicked() {
                let owner = self.floats[index].owner;
                if self.body_edit_owner() == Some(owner) && self.body_edit_dirty() {
                    self.save_flow.pending_edit_action = Some(EditDiscardAction::CloseFloat(id));
                } else {
                    close = Some(id);
                }
            }

            // ---- 绘制 ----
            // 被"再次悬停同一目的地"激发时高亮：边框加粗变橙、标题栏变暖色
            // 高亮有两个来源：悬浮（每帧判定）与点击（一次性，带截止时刻）
            let clicked_highlight = self
                .click_highlight
                .is_some_and(|(target, until)| target == id && self.runtime.clock.now() < until);
            let highlighted = self.highlight_float == Some(id) || clicked_highlight;
            let (border, border_width, title_color) = if highlighted {
                (
                    Color32::from_rgb(255, 170, 60),
                    2.0,
                    Color32::from_rgb(140, 96, 36),
                )
            } else {
                (palette.border, 1.0, palette.title)
            };
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(4), palette.bg);
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(border_width, border),
                StrokeKind::Inside,
            );
            painter.rect_filled(title_rect, CornerRadius::same(4), title_color);
            // 关闭按钮必须在背景/标题填充之后画，否则会被后面的标题栏盖住。
            ui.painter().circle_filled(close_center, 8.5, close_color);
            self.paint_icon(ui, icons::Icon::Close, close_center, 13.0, Color32::WHITE);
            // 标题栏按钮按固定槽位排：close（固定）→ 主按钮 → link → goto。
            // 主按钮是 edit（文字浮窗）或 @（图片 preview）；dest 浮窗没有主按钮，
            // 只有 close + goto，因此 goto 直接占第一槽，不留空洞。
            let owner = self.floats[index].owner;
            let editable = text_float_is_editable(owner, &self.floats[index].content);
            let image_at = self.floats[index]
                .custom_index()
                .and_then(|index| self.custom_previews.get(index))
                .is_some_and(|preview| {
                    preview.kind == CustomKind::Image && !preview.image_id.is_empty()
                });
            let dest_image = match owner {
                FloatOwner::None => self.floats[index].origin.and_then(|(page, mark_index)| {
                    self.dest_marks
                        .get(page.get() as usize)
                        .and_then(|marks| marks.get(mark_index))
                        .filter(|mark| !mark.image_id.is_empty())
                        .map(|mark| crate::image_store::ImageRef {
                            record_id: mark.image_id.clone(),
                            revision: mark.image_revision,
                        })
                }),
                _ => None,
            };
            let has_primary = editable || image_at || dest_image.is_some();
            let link_uri = if matches!(owner, FloatOwner::Annotation(_) | FloatOwner::Custom(_)) {
                self.float_link_uri(index)
            } else {
                None
            };
            let goto_target = match owner {
                FloatOwner::Annotation(_) | FloatOwner::Custom(_) => self.float_goto_target(index),
                // dest 浮窗：owner=None + origin 指向被引用点；截图/测试浮窗没有 origin。
                FloatOwner::None if self.floats[index].origin.is_some() => {
                    self.float_goto_target(index)
                }
                FloatOwner::None => None,
            };
            let mut button_count = usize::from(has_primary)
                + usize::from(link_uri.is_some())
                + usize::from(goto_target.is_some());
            button_count = button_count.max(1);
            let reserved = 54.0 + 26.0 * (button_count.saturating_sub(1)) as f32;
            let link_center_x = if has_primary {
                close_center.x - 52.0
            } else {
                close_center.x - 26.0
            };
            let goto_center_x = if link_uri.is_some() {
                link_center_x - 26.0
            } else if has_primary {
                close_center.x - 52.0
            } else {
                close_center.x - 26.0
            };
            // 标题：**单行截断**（长标题不再压到关闭按钮上）
            let title_font = egui::FontId::proportional(13.0);
            let title_text_color = if highlighted {
                Color32::WHITE
            } else {
                palette.title_text
            };
            let title_job = overflow_layout(
                &self.floats[index].title,
                title_font,
                title_text_color,
                (title_rect.width() - reserved).max(24.0),
                1,
            );
            let title_galley = painter.layout_job(title_job);
            painter.galley(
                egui::pos2(
                    title_rect.min.x + 8.0,
                    title_rect.center().y - title_galley.size().y * 0.5,
                ),
                title_galley,
                title_text_color,
            );
            // 缩放手柄示意
            painter.rect_filled(handle_rect.shrink(5.0), CornerRadius::ZERO, palette.handle);
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
            // 引用点浮窗：重选 dest rect 后 snapshot 已更新，已有浮窗也要换图，
            // 否则浮窗一直显示旧截图（用户实测）。
            if let Some((page, mark_index)) = self.floats[index].origin {
                if let Some(mark) = self
                    .dest_marks
                    .get(page.get() as usize)
                    .and_then(|marks| marks.get(mark_index))
                {
                    if let Some(texture) = mark.snapshot.clone() {
                        let stale = match &self.floats[index].content {
                            FloatContent::Image(current) => current.id() != texture.id(),
                            FloatContent::Text(_) => true,
                        };
                        if stale {
                            self.floats[index].content = FloatContent::Image(texture);
                        }
                    }
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
            if editable && self.editing_body.is_none() {
                let edit_center = egui::pos2(close_center.x - 26.0, title_rect.center().y);
                let edit_rect = egui::Rect::from_center_size(edit_center, EVec2::splat(18.0));
                let edit_response =
                    ui.interact(edit_rect, Id::unique(("float-edit", id)), Sense::click());
                let edit_color = if edit_response.hovered() {
                    palette.icon_hover
                } else {
                    palette.icon
                };
                self.paint_icon(ui, icons::Icon::Edit, edit_center, 13.0, edit_color);
                if interactive && edit_response.on_hover_text("编辑正文").clicked() {
                    if let FloatContent::Text(text) = &self.floats[index].content {
                        let owner = self.floats[index].owner;
                        let text = text.clone();
                        self.open_body_editor(owner, text, None, ui.ctx());
                    }
                }
            }
            // 浮窗：复制 `pd-float://` 引用链接（注释/文字/图片 preview）。
            if let Some(uri) = link_uri {
                let center = egui::pos2(link_center_x, title_rect.center().y);
                let button_rect = egui::Rect::from_center_size(center, EVec2::splat(18.0));
                let response =
                    ui.interact(button_rect, Id::unique(("float-link", id)), Sense::click());
                let color = if response.hovered() {
                    palette.icon_hover
                } else {
                    palette.icon
                };
                self.paint_icon(ui, icons::Icon::Link, center, 13.0, color);
                if interactive && response.on_hover_text("复制引用链接").clicked() {
                    ui.ctx().copy_text(uri);
                    self.message = "已复制引用链接，可粘到正文".to_string();
                }
            }
            // 浮窗：跳到来源页/位置（与侧栏菜单同一动作）。
            if let Some((page, point)) = goto_target {
                let center = egui::pos2(goto_center_x, title_rect.center().y);
                let button_rect = egui::Rect::from_center_size(center, EVec2::splat(18.0));
                let response =
                    ui.interact(button_rect, Id::unique(("float-goto", id)), Sense::click());
                let color = if response.hovered() {
                    palette.icon_hover
                } else {
                    palette.icon
                };
                self.paint_icon(ui, icons::Icon::Goto, center, 13.0, color);
                if interactive && response.on_hover_text("跳到来源页/位置").clicked() {
                    self.commands.push(ViewerCommand::GoToPage(page));
                    self.commands
                        .push(ViewerCommand::CenterOnPagePoint { page, point });
                    if let FloatOwner::Annotation(annotation) = self.floats[index].owner {
                        self.annotation_focus =
                            Some((annotation, self.runtime.clock.now() + ANNOTATION_FOCUS));
                    }
                    self.message = format!("已跳转到来源（第 {} 页）", page.display_number());
                }
            }
            // 引用点/dest 浮窗：如果该点有内部图片记录，也可以复制图片引用。
            if let Some(image_ref) = dest_image {
                let uri = crate::image_store::image_uri(&image_ref.record_id, image_ref.revision);
                let center = egui::pos2(close_center.x - 26.0, title_rect.center().y);
                let button_rect = egui::Rect::from_center_size(center, EVec2::splat(18.0));
                let response = ui.interact(
                    button_rect,
                    Id::unique(("float-dest-image-ref", id)),
                    Sense::click(),
                );
                let color = if response.hovered() {
                    palette.icon_hover
                } else {
                    palette.icon
                };
                self.paint_icon(ui, icons::Icon::ImageRef, center, 13.0, color);
                if interactive && response.on_hover_text("复制 Markdown 图片引用").clicked() {
                    ui.ctx().copy_text(uri);
                    self.message = "已复制图片引用，可粘到正文".to_string();
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
                            let color = if response.hovered() {
                                palette.icon_hover
                            } else {
                                palette.icon
                            };
                            self.paint_icon(ui, icons::Icon::ImageRef, center, 13.0, color);
                            if interactive
                                && response.on_hover_text("复制 Markdown 图片引用").clicked()
                            {
                                ui.ctx().copy_text(uri);
                                self.message = "已复制图片引用，可粘到正文".to_string();
                            }
                        }
                    }
                }
            }
            self.draw_float_content(ui, canvas, rect, index, &body, wheel_owner);
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
    /// 添加一个"测试浮窗"（带任意文字），位置按已有浮窗数量级联偏移。
    ///
    /// 入口在顶栏第二行（「测试浮窗」按钮）；用于不依赖文档内容地验证浮窗的
    /// 拖动/改大小/关闭/层级。
    pub(super) fn add_test_float(&mut self) {
        let id = self.next_float_id;
        self.next_float_id += 1;
        let cascade = ((id as f32 - 1.0) % 6.0) * 24.0;
        let rect = egui::Rect::from_min_size(
            egui::pos2(24.0 + cascade, 24.0 + cascade),
            EVec2::new(280.0, 150.0),
        );
        self.floats.push(Float {
            id,
            title: format!("测试浮窗 #{id}"),
            content: FloatContent::Text(format!(
                "这是第 {id} 个测试浮窗。\n目前只显示文字。\n\n可以拖动标题栏移动、\n拖右下角改变大小、\n点右上角 ✕ 关闭。"
            )),
            origin: None,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            rect,
        });
        self.message = format!("已添加测试浮窗 #{id}");
    }

    /// 安装 `pd-img` bytes loader 与 PNG image loader。
    pub(super) fn ensure_image_loaders(&self, ctx: &egui::Context) {
        if !ctx.is_loader_installed(crate::image_store::InternalImageLoader::ID) {
            ctx.add_bytes_loader(std::sync::Arc::new(
                crate::image_store::InternalImageLoader::new(self.markdown.images.clone()),
            ));
        }
        egui_extras::install_image_loaders(ctx);
    }

    /// 取走公式后端的首次失败错误，写入状态栏（短暂提示，后续消息可顶替）。
    pub(super) fn take_math_error(&mut self) {
        let after_save = std::mem::take(&mut self.markdown.math_recheck_pending);
        if let Some(error) = self.markdown.math_cache.borrow_mut().take_error() {
            let line = error.status_line();
            self.message = if after_save {
                format!("已更新内容；{line}")
            } else {
                line
            };
        }
    }

    /// 取走本帧 Markdown 链接产生的 `OpenUrl` 命令，改为状态栏提示。
    ///
    /// `egui_commonmark` 的外部链接最终走 `ui.hyperlink_to` → `ctx.open_url`；
    /// 项目当前对 PDF URI 也只提示、不打开浏览器（见 `follow_link`）。在
    /// `egui_commonmark` 没有链接钩子的情况下，这是官方文档给出的检查方式
    /// （`ctx.output_mut` 遍历 `OutputCommand::OpenUrl`）。
    pub(super) fn consume_markdown_links(&mut self, ctx: &egui::Context) {
        if !self.markdown.rendered {
            return;
        }
        self.markdown.rendered = false;
        let mut opened = Vec::new();
        ctx.output_mut(|output| {
            output.commands.retain(|command| match command {
                egui::OutputCommand::OpenUrl(open) => {
                    opened.push(open.url.clone());
                    false
                }
                _ => true,
            });
        });
        let mut external = None;
        for url in opened {
            if url.starts_with("pd-float://") {
                self.follow_float_link(&url);
            } else {
                external = Some(url);
            }
        }
        if let Some(url) = external {
            self.message = format!("外部链接：{url}（打开浏览器暂未实现）");
        }
    }

    /// 命中当前页的链接热区则发出跳转命令，返回是否命中。
    ///
    /// 热区判断（点在哪个链接里）在核心层：[`DocumentSession::link_at`]；
    /// 这里只负责"把内容空间的点交给它"，以及把结果翻译成命令
    /// （[`crate::view::commands_for_destination`]：有落点 → 居中，无落点 → 顶部对齐）。
    pub(super) fn follow_link(&mut self, session: &DocumentSession, content: Point) -> bool {
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let page_point = placement.content_to_page(content);
            // 找出命中的链接下标（登记表与 links_on 同构，因此能直接查）
            let Some(navigation) = self.navigation.clone() else {
                return false;
            };
            let hit = navigation
                .links_on(page)
                .iter()
                .position(|link| link.rect.contains(page_point));
            let Some(index) = hit else { return false };
            let destination = navigation.links_on(page)[index].destination.clone();
            match &destination {
                Destination::Uri(uri) => {
                    self.message = format!("外部链接：{uri}（打开浏览器暂未实现）");
                }
                Destination::Page { .. } => {
                    // 优先：跳到**被引用点矩形的中心**（用户要求；登记表在打开时解析）
                    let target = self
                        .link_marks
                        .get(page.get() as usize)
                        .and_then(|registered| registered.get(index))
                        .copied()
                        .flatten();
                    let mark_center = target.and_then(|(mark_page, mark_index)| {
                        self.dest_marks
                            .get(mark_page.get() as usize)?
                            .get(mark_index)
                            .map(|mark| (mark_page, mark.rect.center()))
                    });
                    match mark_center {
                        Some((mark_page, center)) => {
                            self.commands.push(ViewerCommand::GoToPage(mark_page));
                            self.commands.push(ViewerCommand::CenterOnPagePoint {
                                page: mark_page,
                                point: center,
                            });
                        }
                        // 没有登记被引用点（例如仅 /Fit 的目标）→ 退回原有语义
                        None => self.commands.extend(commands_for_destination(&destination)),
                    }
                }
            }
            return true;
        }
        false
    }
    /// 绘制浮窗正文（Markdown/纯文本/图片）与正文双击入口。
    pub(super) fn draw_float_content(
        &mut self,
        ui: &mut egui::Ui,
        canvas: egui::Rect,
        rect: egui::Rect,
        index: usize,
        body: &egui::Response,
        wheel_owner: bool,
    ) {
        let id = self.floats[index].id;
        let palette = float_palette(self.dark_mode);
        let painter = ui.painter();
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
                    .markdown
                    .viewports
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
                    self.markdown.rendered = true;
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
                    let prepared = self.markdown.prepared.remove(&id);
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
                        &self.markdown.images,
                    );
                    self.markdown.prepared.insert(id, prepared);
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
                    let mut md_cache = self.markdown.caches.remove(&id).unwrap_or_default();
                    ui.scope_builder(
                        egui::UiBuilder::new().max_rect(body_area).id_salt(view_id),
                        |ui| {
                            ui.set_clip_rect(body_area.intersect(canvas));
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                            ui.style_mut().url_in_tooltip = true;
                            let mut visuals = if self.dark_mode {
                                egui::Visuals::dark()
                            } else {
                                egui::Visuals::light()
                            };
                            visuals.override_text_color = Some(palette.text);
                            visuals.hyperlink_color = palette.link;
                            *ui.visuals_mut() = visuals;
                            ui.spacing_mut().item_spacing = EVec2::new(0.0, 4.0);
                            // 数学公式走 render_math_fn（egui_commonmark 会在
                            // 有回调时启用 ENABLE_MATH）；缓存按公式/行内/DPI。
                            let math_cache = self.markdown.math_cache.clone();
                            let math_fn = move |ui: &mut egui::Ui, tex: &str, inline: bool| {
                                crate::math::paint(ui, &math_cache, tex, inline, palette.text);
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
                            self.markdown.viewports.insert(
                                id,
                                (
                                    scroll_output.state.clamped_offset(),
                                    scroll_output.content_size,
                                ),
                            );
                        },
                    );
                    self.markdown.caches.insert(id, md_cache);
                    self.markdown.rendered = true;
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
                        palette.text,
                        body_area.width(),
                        rows,
                    );
                    let galley = painter.layout_job(job);
                    painter
                        .with_clip_rect(body_area)
                        .galley(body_area.min, galley, palette.text);
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
}
