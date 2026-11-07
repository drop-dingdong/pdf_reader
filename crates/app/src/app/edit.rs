//! `app::edit`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{float_size_for_body, FLOAT_TITLE_HEIGHT};
use eframe::egui;

impl PdfReaderApp {
    /// 跳到第 `index` 条搜索命中。
    ///
    /// **把命中位置放到视口中心**（第二轮验收的统一跳转语义）：
    /// 跳页是"顶部对齐"，搜索命中则是"居中"，因为用户关心的是那一行文字而不是页首。
    pub(super) fn goto_hit(&mut self, index: usize) {
        let Some(hit) = self.hits.get(index) else {
            return;
        };
        self.active_hit = Some(index);
        let page = hit.page;
        self.commands.push(ViewerCommand::GoToPage(page));
        if let Some(rect) = hit.rects.first() {
            self.commands.push(ViewerCommand::CenterOnPagePoint {
                page,
                point: rect.center(),
            });
        }
    }

    /// 打开文档（装配会话：LRU 缓存 + 后台调度器 + 纵向布局 + 选区模型）。
    /// 应用一条编辑（`record = true` 时同时记入历史）。
    ///
    /// 只有这一处修改"标注 + 配对正文"，撤销/重做因此共用同一条代码路径 ——
    /// 否则"删除标注忘了删正文"这类不对称会从两个方向各犯一次。
    pub(super) fn apply_edit(&mut self, edit: AppEdit, record: bool) {
        let description = edit.describe();
        match &edit {
            AppEdit::AddAnnotation { annotation } => {
                self.annotations.insert(annotation.clone());
                // 外部注释被删除并写回后，对象已不在 PDF 里；撤销恢复时必须
                // 当作新注释重新写回，而不是继续按旧对象号做“外部更新”。
                // 删除并写回后，旧对象已不在 PDF；撤销恢复时必须按新注释写回。
                // 这不只对外部注释成立：自有注释的旧 pdf_ref 同样会失效。
                if let Some(reference) = annotation.pdf_ref {
                    if !self.annotation_pdf.opened_pdf_refs.contains(&reference) {
                        self.annotation_pdf.external_ids.remove(&annotation.id);
                        if let Some(restored) = self.annotations.get_mut(annotation.id) {
                            restored.pdf_ref = None;
                        }
                    }
                }
            }
            AppEdit::DeleteAnnotation { annotation } => {
                self.annotations.remove(annotation.id);
                // 关闭这条注释的正文浮窗（不会碰 CustomPreview）
                self.floats
                    .retain(|window| window.owner != FloatOwner::Annotation(annotation.id));
            }
            AppEdit::RestyleAnnotation { before, after } => {
                self.annotations.remove(before.id);
                self.annotations.insert(after.clone());
                for window in &mut self.floats {
                    if window.owner == FloatOwner::Annotation(before.id) {
                        window.owner = FloatOwner::Annotation(after.id);
                    }
                }
                if let Some(reference) = after.pdf_ref {
                    if !self.annotation_pdf.opened_pdf_refs.contains(&reference) {
                        if let Some(restored) = self.annotations.get_mut(after.id) {
                            restored.pdf_ref = None;
                        }
                    }
                }
            }
            AppEdit::MoveAnnotation { before, after } => {
                self.annotations.remove(before.id);
                self.annotations.insert(after.clone());
            }
            AppEdit::SetBody { id, after, .. } => {
                self.annotations.set_body(*id, after.clone());
                if let Some(annotation) = self.annotations.get_mut(*id) {
                    annotation.modified = Some(unix_now());
                }
                // 正文变化可能带来新的公式错误；让下一帧重新渲染/提示。
                self.markdown.math_cache.borrow_mut().clear();
            }
            AppEdit::DeletePreview { key, .. } => {
                if let Some(index) = self.preview_index(*key) {
                    self.custom_previews.remove(index);
                    self.shift_float_custom_indices(index);
                }
            }
            AppEdit::AddPreview {
                key,
                position,
                preview,
            } => {
                let at = (*position).min(self.custom_previews.len());
                let mut preview = (**preview).clone();
                preview.id = key.id;
                self.custom_previews.insert(at, preview);
                for window in &mut self.floats {
                    if let FloatOwner::Custom(index) = window.owner {
                        if index >= at {
                            window.owner = FloatOwner::Custom(index + 1);
                        }
                    }
                }
            }
            AppEdit::SetPreviewTitle { key, after, .. } => {
                if let Some(index) = self.preview_index(*key) {
                    if let Some(preview) = self.custom_previews.get_mut(index) {
                        preview.title = after.clone();
                    }
                }
            }
            AppEdit::SetPreviewBody { key, after, .. } => {
                if let Some(index) = self.preview_index(*key) {
                    if let Some(preview) = self.custom_previews.get_mut(index) {
                        preview.text = after.clone();
                    }
                }
                self.markdown.math_cache.borrow_mut().clear();
            }
            AppEdit::ResizePreview { key, after, .. } => {
                if let Some(index) = self.preview_index(*key) {
                    if let Some(preview) = self.custom_previews.get_mut(index) {
                        preview.body_size = *after;
                    }
                    for window in &mut self.floats {
                        if window.custom_index() == Some(index) {
                            window.body = *after;
                            window.rect = egui::Rect::from_min_size(
                                window.rect.min,
                                float_size_for_body(*after, FLOAT_TITLE_HEIGHT),
                            );
                        }
                    }
                }
            }
            AppEdit::ReplacePreview { key, after, .. } => {
                if let Some(index) = self.preview_index(*key) {
                    let mut replacement = (**after).clone();
                    replacement.id = key.id;
                    let body = replacement.body_size;
                    let snapshot = replacement.snapshot.clone();
                    self.custom_previews[index] = replacement;
                    if let Some(snapshot) = snapshot {
                        for window in &mut self.floats {
                            if window.custom_index() == Some(index) {
                                window.content = FloatContent::Image(snapshot.clone());
                                window.body = body;
                                window.rect = egui::Rect::from_min_size(
                                    window.rect.min,
                                    float_size_for_body(body, FLOAT_TITLE_HEIGHT),
                                );
                            }
                        }
                    } else {
                        // 回到“占位截图”状态：关掉仍显示旧图/新图的浮窗。
                        self.floats
                            .retain(|window| window.custom_index() != Some(index));
                    }
                }
            }
            AppEdit::MovePreview { key, from, to } => {
                if let Some(index) = self.preview_index(*key) {
                    let target = (*to).min(self.custom_previews.len().saturating_sub(1));
                    if index != target {
                        let preview = self.custom_previews.remove(index);
                        self.custom_previews.insert(target, preview);
                    }
                    let _ = (*from, index);
                }
            }
            AppEdit::EditDestMark {
                page, index, after, ..
            } => {
                if let Some(slot) = self
                    .dest_marks
                    .get_mut(page.get() as usize)
                    .and_then(|marks| marks.get_mut(*index))
                {
                    *slot = (**after).clone();
                    for window in &mut self.floats {
                        if window.origin == Some((*page, *index)) {
                            window.body = after.body_size;
                            window.rect = egui::Rect::from_min_size(
                                window.rect.min,
                                float_size_for_body(after.body_size, FLOAT_TITLE_HEIGHT),
                            );
                        }
                    }
                }
            }
            AppEdit::ResizeDestMark {
                page, index, after, ..
            } => {
                if let Some(slot) = self
                    .dest_marks
                    .get_mut(page.get() as usize)
                    .and_then(|marks| marks.get_mut(*index))
                {
                    slot.body_size = *after;
                    for window in &mut self.floats {
                        if window.origin == Some((*page, *index)) {
                            window.body = *after;
                            window.rect = egui::Rect::from_min_size(
                                window.rect.min,
                                float_size_for_body(*after, FLOAT_TITLE_HEIGHT),
                            );
                        }
                    }
                }
            }
        }
        if record {
            let weight = edit.weight();
            let affects_pdf = edit.affects_pdf();
            self.history
                .push_tagged(description, edit, weight, affects_pdf);
        }
        self.refresh_dirty();
    }

    /// 撤销一步（反向应用栈顶命令）。
    pub(super) fn undo_edit(&mut self) {
        self.last_preview_resize = None;
        self.last_dest_resize = None;
        let Some(entry) = self.history.undo() else {
            self.message = "没有可撤销的编辑".to_string();
            return;
        };
        let inverse = match entry.command {
            AppEdit::AddAnnotation { annotation } => AppEdit::DeleteAnnotation { annotation },
            AppEdit::DeleteAnnotation { annotation } => AppEdit::AddAnnotation { annotation },
            AppEdit::RestyleAnnotation { before, after } => AppEdit::RestyleAnnotation {
                before: after,
                after: before,
            },
            AppEdit::MoveAnnotation { before, after } => AppEdit::MoveAnnotation {
                before: after,
                after: before,
            },
            AppEdit::SetBody {
                id,
                page,
                before,
                after,
            } => AppEdit::SetBody {
                id,
                page,
                before: after,
                after: before,
            },
            AppEdit::DeletePreview {
                key,
                position,
                preview,
            } => AppEdit::AddPreview {
                key,
                position,
                preview,
            },
            AppEdit::AddPreview {
                key,
                position,
                preview,
            } => AppEdit::DeletePreview {
                key,
                position,
                preview,
            },
            AppEdit::SetPreviewTitle { key, before, after } => AppEdit::SetPreviewTitle {
                key,
                before: after,
                after: before,
            },
            AppEdit::SetPreviewBody { key, before, after } => AppEdit::SetPreviewBody {
                key,
                before: after,
                after: before,
            },
            AppEdit::ResizePreview { key, before, after } => AppEdit::ResizePreview {
                key,
                before: after,
                after: before,
            },
            AppEdit::ReplacePreview { key, before, after } => AppEdit::ReplacePreview {
                key,
                before: after,
                after: before,
            },
            AppEdit::MovePreview { key, from, to } => AppEdit::MovePreview {
                key,
                from: to,
                to: from,
            },
            AppEdit::EditDestMark {
                page,
                index,
                before,
                after,
            } => AppEdit::EditDestMark {
                page,
                index,
                before: after,
                after: before,
            },
            AppEdit::ResizeDestMark {
                page,
                index,
                before,
                after,
            } => AppEdit::ResizeDestMark {
                page,
                index,
                before: after,
                after: before,
            },
        };
        self.apply_edit(inverse, false);
        self.message = format!("已撤销：{}", entry.description);
        self.show_history = true;
    }

    /// 重做一步。
    pub(super) fn redo_edit(&mut self) {
        self.last_preview_resize = None;
        self.last_dest_resize = None;
        let Some(entry) = self.history.redo() else {
            self.message = "没有可重做的编辑".to_string();
            return;
        };
        self.apply_edit(entry.command, false);
        self.message = format!("已重做：{}", entry.description);
        self.show_history = true;
    }

    /// `Ctrl+S`：保存**所有**有脏标记的文件（主存档 + 标注图层）。
    ///
    /// 顺序固定：先主存档（正文在里面）、再图层；两份各自原子写，互不依赖，
    /// 所以即使图层写失败，正文也已经落盘（加载时缺图层只是"没有标注"，不会崩）。
    /// 原地写入重开之后，**等视口尺寸生效**再恢复页/缩放/滚动。
    /// 把当前文档的页码/缩放/排版记到设置里（换文档、关闭窗口时）。
    pub(super) fn remember_document_view(&mut self) {
        let Some(path) = self.document_path.clone() else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let scroll = session.viewport().scroll;
        let state = crate::settings::DocumentState {
            path: settings_key(&path),
            page: session.state().current_page.get(),
            zoom: session.state().zoom,
            // 页面锚点：视口左上角在内容空间的位置，和页码一起才能回到原处。
            anchor: Some([scroll.x.max(0.0), scroll.y.max(0.0)]),
            horizontal: self.horizontal_layout,
            opened_at: unix_now(),
        };
        self.settings.remember_document(state);
        self.settings_dirty = true;
    }

    /// 把设置写到磁盘（`settings_path == None` 时什么也不做：回放/测试不碰用户配置）。
    pub(super) fn flush_settings(&mut self, ctx: Option<&egui::Context>) {
        if self.settings_path.is_none() {
            self.settings_dirty = false;
            return;
        }
        if let Some(ctx) = ctx {
            if let Some(rect) = ctx.input(|input| input.viewport().inner_rect) {
                if rect.width() > 1.0 && rect.height() > 1.0 {
                    self.settings.ui.window_size = [rect.width(), rect.height()];
                }
            }
        }
        self.settings.ui.sidebar_width = self.sidebar_width;
        if let Err(error) = self.settings.save() {
            eprintln!("[settings] 保存失败：{error}");
        }
        self.settings_dirty = false;
    }

    /// 首帧恢复上次的窗口尺寸（窗口创建时还不知道设置）。
    pub(super) fn apply_pending_window_size(&mut self, ctx: &egui::Context) {
        if let Some(size) = self.pending_window_size.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                size[0], size[1],
            )));
        }
    }

    /// 样式菜单改动后同步到设置（变了才标记脏，避免每帧写盘）。
    pub(super) fn sync_style_settings(&mut self) {
        let current = crate::settings::AnnotationDefaults {
            markup_color: self.markup_color,
            highlight_opacity: self.markup_opacity,
            ink_color: self.ink_color,
            ink_width: self.ink_width,
        };
        if current != self.settings.annotation {
            self.settings.annotation = current;
            self.settings_dirty = true;
        }
        let ui = crate::settings::UiSettings {
            window_size: self.settings.ui.window_size,
            sidebar_width: self.settings.ui.sidebar_width,
            pin_hover_enabled: self.pin_hover_enabled,
            horizontal_scroll: self.horizontal_scroll,
            show_links: self.show_links,
            show_dest_points: self.show_dest_points,
            show_annotations: self.show_annotations,
            markdown_floats: self.markdown_floats,
            hover_preview_markdown: self.hover_preview_markdown,
            allow_external_annotation_links: self.allow_external_annotation_links,
        };
        if ui != self.settings.ui {
            self.settings.ui = ui;
            self.settings_dirty = true;
        }
        if self.settings.save.backup_in_place != self.backup_before_inplace {
            self.settings.save.backup_in_place = self.backup_before_inplace;
            self.settings_dirty = true;
        }
    }

    pub(super) fn apply_pending_view_restore(&mut self, ctx: &egui::Context) {
        let Some((page, zoom, scroll, remaining)) = self.pending_view_restore else {
            return;
        };
        if self.session.is_none() {
            self.pending_view_restore = None;
            return;
        }
        if remaining == 0 {
            self.commands.push(ViewerCommand::GoToPage(page));
            self.commands.push(ViewerCommand::SetZoom(zoom));
            // 设置里只记了页码/缩放：没有滚动快照时不要 SetScroll(0)，
            // 否则会把刚跳到的页又拉回文档开头（用户 2026-11-01 反馈"页数不恢复"）。
            if let Some(scroll) = scroll {
                self.commands.push(ViewerCommand::SetScroll(scroll));
            }
            self.pending_view_restore = None;
        } else {
            self.pending_view_restore = Some((page, zoom, scroll, remaining - 1));
            ctx.request_repaint();
        }
    }
}
