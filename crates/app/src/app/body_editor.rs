//! `app::body_editor`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::FLOAT_TITLE_HEIGHT;
use eframe::egui;
use egui::{Color32, Id, Sense, Vec2 as EVec2};

impl PdfReaderApp {
    /// 打开正文编辑窗口；每次换一个会话号，使编辑区滚动位置从顶部开始。
    pub(super) fn open_body_editor(
        &mut self,
        owner: FloatOwner,
        text: String,
        cursor: Option<usize>,
        ctx: &egui::Context,
    ) {
        self.body_edit_session = self.body_edit_session.wrapping_add(1).max(1);
        self.body_edit_cursor = Some(cursor.unwrap_or(0));
        self.editing_body = Some((owner, text));
        self.focus_body_edit = true;
        ctx.request_repaint();
    }

    /// 浮窗正文的正式编辑窗口：双击/✎ 打开，显式保存或取消。
    ///
    /// 不再使用 `ui.put(TextEdit)` 塞进浮窗 body：body 尺寸由用户决定，
    /// 长文本会被截断且无法滚动。这里用可调整大小的 `Window` + 内部
    /// `ScrollArea`，编辑区独立于浮窗尺寸；保存仍走 `SetBody`/`SetPreviewBody`。
    pub(super) fn draw_body_editor(&mut self, ctx: &egui::Context) {
        let Some((owner, _)) = self.editing_body.clone() else {
            return;
        };
        let title = match owner {
            FloatOwner::Custom(index) => match self.custom_previews.get(index) {
                Some(preview) if preview.title.is_empty() => "自定义preview".to_string(),
                Some(preview) => preview.title.clone(),
                None => {
                    self.editing_body = None;
                    return;
                }
            },
            FloatOwner::Annotation(id) => match self.annotations.get(id) {
                Some(_) => format!("标注 #{}", id.get()),
                None => {
                    self.editing_body = None;
                    return;
                }
            },
            FloatOwner::None => {
                self.editing_body = None;
                return;
            }
        };
        let Some((_, draft)) = self.editing_body.take() else {
            return;
        };
        let mut text = draft;
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let mut focus = self.focus_body_edit;
        let mut initial_cursor = self.body_edit_cursor.take();
        let image_choices = self.image_choices();
        let session = self.body_edit_session;
        let text_edit_id = Id::unique(("edit-body-text", session));
        egui::Window::new(format!("编辑正文 · {title}"))
            .id(Id::unique("edit-body"))
            .resizable(true)
            .collapsible(false)
            .default_width(560.0)
            .default_height(420.0)
            .min_width(360.0)
            .min_height(240.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("原始 Markdown；保存后按现有路径写回注释或 preview。");
                ui.collapsing("插入内部图片", |ui| {
                    if image_choices.is_empty() {
                        ui.label("还没有可引用的图片 preview 或引用点。");
                    } else {
                        for (label, uri) in &image_choices {
                            if ui
                                .small_button(label)
                                .on_hover_text("插入到当前光标位置")
                                .clicked()
                            {
                                let alt: String = label
                                    .chars()
                                    .map(|ch| match ch {
                                        '[' | ']' | '\n' | '\r' => ' ',
                                        _ => ch,
                                    })
                                    .collect();
                                insert_at_text_cursor(
                                    ui.ctx(),
                                    text_edit_id,
                                    &mut text,
                                    &format!("\n![{alt}]({uri})\n"),
                                );
                                focus = true;
                            }
                        }
                    }
                });
                // 编辑区跟随窗口：按剩余高度算行数，窗口拉大/拉小都填满，
                // 单行短文本也不会出现无意义的滚动条。
                // TextEdit 的实际行高就是字体行高；不要再加 item_spacing，
                // 否则窗口越大，算出的行数越少，底部空白越明显。
                let line_height = ui.text_style_height(&egui::TextStyle::Monospace).max(10.0);
                let editor_height = (ui.available_height() - 46.0).max(60.0);
                let rows = (((editor_height - 8.0) / line_height).floor() as usize).max(1);
                egui::ScrollArea::vertical()
                    .id_salt(("edit-body-scroll", session))
                    .auto_shrink([false, false])
                    .max_height(editor_height)
                    .show(ui, |ui| {
                        let response = ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .id(text_edit_id)
                                .cursor_at_end(false)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(rows),
                        );
                        if focus {
                            response.request_focus();
                            focus = false;
                        }
                        // 在控件绘制之后再写入光标状态：TextEdit 本帧先按旧状态
                        // 绘制，下一帧会加载这里写入的 cursor，并保持滚动可见。
                        if let Some(index) = initial_cursor.take() {
                            let mut state =
                                egui::text_edit::TextEditState::load(ui.ctx(), text_edit_id)
                                    .unwrap_or_default();
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::one(
                                    egui::text::CCursor::new(index),
                                )));
                            state.store(ui.ctx(), text_edit_id);
                            response.request_focus();
                            let galley = ui.ctx().fonts_mut(|fonts| {
                                fonts.layout(
                                    text.clone(),
                                    egui::FontId::monospace(13.0),
                                    Color32::from_gray(225),
                                    ui.available_width(),
                                )
                            });
                            let cursor_rect =
                                galley.pos_from_cursor(egui::text::CCursor::new(index));
                            ui.scroll_to_rect(cursor_rect, Some(egui::Align::Center));
                        }
                    });
                if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
                if ui.input(|input| {
                    (input.modifiers.command || input.modifiers.ctrl)
                        && input.key_pressed(egui::Key::Enter)
                }) {
                    save = true;
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        self.focus_body_edit = focus;
        if save {
            self.commit_body_edit(owner, text);
            self.editing_body = None;
            self.math_recheck_pending = true;
            self.message = "已更新内容".to_string();
            return;
        }
        // 点窗口右上角 X：如果草稿有改动，先确认，不静默丢弃。
        // Esc/「取消」按钮是明确的放弃操作，不弹二次确认。
        if !open {
            if self.body_text_dirty(owner, &text) {
                self.pending_edit_action = Some(EditDiscardAction::CloseEditor);
                self.editing_body = Some((owner, text));
                return;
            }
            cancel = true;
        }
        if cancel {
            self.editing_body = None;
            self.message = "已取消编辑".to_string();
        } else {
            self.editing_body = Some((owner, text));
        }
    }

    /// 提交正文编辑；注释走 `SetBody`（可撤销），preview 走 `SetPreviewBody`。
    pub(super) fn commit_body_edit(&mut self, owner: FloatOwner, text: String) {
        match owner {
            FloatOwner::Custom(index) => {
                if let Some(preview) = self.custom_previews.get(index) {
                    let before = preview.text.clone();
                    let after = Some(text);
                    if before != after {
                        self.apply_edit(
                            AppEdit::SetPreviewBody {
                                key: PreviewKey {
                                    page: preview.page,
                                    id: preview.id,
                                },
                                before,
                                after,
                            },
                            true,
                        );
                    }
                }
            }
            FloatOwner::Annotation(id) => {
                let before = self.annotations.get(id).and_then(|a| a.body.clone());
                let after = if text.is_empty() { None } else { Some(text) };
                if before != after {
                    let page = self
                        .annotations
                        .get(id)
                        .map(|a| a.page)
                        .unwrap_or(PageIndex::FIRST);
                    self.apply_edit(
                        AppEdit::SetBody {
                            id,
                            page,
                            before,
                            after,
                        },
                        true,
                    );
                }
            }
            FloatOwner::None => {}
        }
    }

    pub(super) fn body_edit_owner(&self) -> Option<FloatOwner> {
        self.editing_body.as_ref().map(|(owner, _)| *owner)
    }

    /// 给定草稿是否与已保存正文不同。
    pub(super) fn body_text_dirty(&self, owner: FloatOwner, draft: &str) -> bool {
        let saved = match owner {
            FloatOwner::Custom(index) => self
                .custom_previews
                .get(index)
                .and_then(|preview| preview.text.clone())
                .unwrap_or_default(),
            FloatOwner::Annotation(id) => self
                .annotations
                .get(id)
                .and_then(|annotation| annotation.body.clone())
                .unwrap_or_default(),
            FloatOwner::None => return false,
        };
        draft != saved
    }

    /// 编辑草稿是否与已保存正文不同。
    pub(super) fn body_edit_dirty(&self) -> bool {
        let Some((owner, draft)) = self.editing_body.as_ref() else {
            return false;
        };
        self.body_text_dirty(*owner, draft)
    }

    /// 正文草稿未保存时，关闭浮窗/切换文件前的确认框。
    pub(super) fn draw_edit_discard_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_edit_action.clone() else {
            return;
        };
        let what = match self.body_edit_owner() {
            Some(FloatOwner::Annotation(_)) => "注释正文",
            Some(FloatOwner::Custom(_)) => "文字 preview 正文",
            _ => "正文",
        };
        let reason = match &action {
            EditDiscardAction::CloseEditor => "关闭编辑窗口前",
            EditDiscardAction::CloseFloat(_) => "关闭浮窗或切换文件前",
            EditDiscardAction::Open(_) => "关闭浮窗或切换文件前",
            EditDiscardAction::Quit => "关闭软件前",
        };
        let mut save = false;
        let mut discard = false;
        let mut cancel = false;
        egui::Modal::new(Id::unique("edit-discard")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(format!("{what}尚未保存"));
            ui.label(format!("{reason}，是否保存当前{what}？"));
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("保存并继续").clicked() {
                    save = true;
                }
                if ui.button("放弃修改").clicked() {
                    discard = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel {
            self.pending_edit_action = None;
            return;
        }
        if save {
            if let Some((owner, text)) = self.editing_body.take() {
                self.commit_body_edit(owner, text);
                self.math_recheck_pending = true;
                self.message = "已更新内容".to_string();
            }
        } else if discard {
            self.editing_body = None;
            self.message = "已放弃正文修改".to_string();
        }
        if save || discard {
            self.pending_edit_action = None;
            self.run_edit_action(action, ctx);
        }
    }

    /// 双击浮窗正文 → 进入原位编辑（自定义文字 preview 或注释正文）。
    pub(super) fn detect_body_double_click(&mut self, ui: &mut egui::Ui) {
        if self.editing_body.is_some() {
            return;
        }
        // Markdown 模式的正文有自己的内容控件（链接/滚动条）。这个全窗 click
        // 层注册在浮窗之后，会按"后注册优先"挡住它们；双击改由
        // `draw_floats` 里的 body 响应当场处理。
        if self.markdown_floats {
            return;
        }
        let targets: Vec<(u64, FloatOwner, egui::Rect)> = self
            .floats
            .iter()
            .filter_map(|window| match window.owner {
                FloatOwner::Custom(index) => {
                    let preview = self.custom_previews.get(index)?;
                    (preview.kind == CustomKind::Text).then_some((
                        window.id,
                        window.owner,
                        window.rect,
                    ))
                }
                FloatOwner::Annotation(_) => Some((window.id, window.owner, window.rect)),
                FloatOwner::None => None,
            })
            .collect();
        for (id, owner, rect) in targets {
            let body_rect = egui::Rect::from_min_max(
                rect.min + EVec2::new(10.0, FLOAT_TITLE_HEIGHT + 8.0),
                rect.max - EVec2::new(10.0, 8.0),
            );
            let hit = ui.interact(
                body_rect,
                Id::unique(("float-body-edit", id)),
                Sense::click(),
            );
            if hit.double_clicked() {
                let text = match owner {
                    FloatOwner::Custom(index) => self
                        .custom_previews
                        .get(index)
                        .and_then(|preview| preview.text.clone())
                        .unwrap_or_default(),
                    FloatOwner::Annotation(id) => self
                        .annotations
                        .get(id)
                        .and_then(|annotation| annotation.body.clone())
                        .unwrap_or_default(),
                    FloatOwner::None => String::new(),
                };
                let cursor = hit.interact_pointer_pos().map(|position| {
                    body_cursor_from_pointer(
                        ui.ctx(),
                        &text,
                        body_rect,
                        position,
                        false,
                        egui::Vec2::ZERO,
                        None,
                    )
                });
                self.open_body_editor(owner, text, cursor, ui.ctx());
                return;
            }
        }
    }

    /// 编辑题目 / 正文的对话框（✎ 按钮与"固定浮窗内双击文字"共用）。
    pub(super) fn draw_edit_dialogs(&mut self, ctx: &egui::Context) {
        // ---- 正文编辑（正式窗口）----
        self.draw_body_editor(ctx);
        self.draw_edit_discard_dialog(ctx);
        self.draw_math_help(ctx);

        // ---- 题目 ----
        if let Some((index, draft)) = self.editing_title.clone() {
            let mut text = draft;
            let mut close = false;
            egui::Modal::new(Id::unique("edit-title")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.heading("修改题目");
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("save").clicked() || ui.button("关闭").clicked() {
                        close = true;
                    }
                    if ui.button("cancel").clicked() {
                        text = self
                            .custom_previews
                            .get(index)
                            .map(|preview| preview.title.clone())
                            .unwrap_or_default();
                        close = true;
                    }
                });
            });
            self.editing_title = Some((index, text.clone()));
            if close {
                if let Some(preview) = self.custom_previews.get(index) {
                    let before = preview.title.clone();
                    if before != text {
                        self.apply_edit(
                            AppEdit::SetPreviewTitle {
                                key: PreviewKey {
                                    page: preview.page,
                                    id: preview.id,
                                },
                                before,
                                after: text,
                            },
                            true,
                        );
                    }
                }
                self.editing_title = None;
                self.message = "已更新题目".to_string();
            }
        }
    }
    pub(super) fn run_edit_action(&mut self, action: EditDiscardAction, ctx: &egui::Context) {
        match action {
            EditDiscardAction::CloseEditor => self.editing_body = None,
            EditDiscardAction::CloseFloat(id) => self.close_float(id),
            EditDiscardAction::Open(path) => self.request_open(path),
            EditDiscardAction::Quit => {
                if self.is_dirty() {
                    // 正文保存后可能让文档变脏；交给已有的未保存文档提示。
                    self.pending_action = Some(PendingAction {
                        kind: PendingActionKind::Quit,
                    });
                } else {
                    self.closing_after_decision = true;
                    self.remember_document_view();
                    self.flush_settings(Some(ctx));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }
}
