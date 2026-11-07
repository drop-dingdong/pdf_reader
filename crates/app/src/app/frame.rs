//! `app::frame`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::command_for_key;
use eframe::egui;
use egui::{Color32, Key, Modifiers, RichText};

impl PdfReaderApp {
    /// 采集输入 → 命令。
    pub(super) fn collect_commands(&mut self, ctx: &egui::Context, session: &DocumentSession) {
        // 保存是全局动作：不能被“输入框有焦点”或截图模式吞掉。
        // 正文编辑有未提交草稿时先提示，避免 Ctrl+S 保存了旧内容却让用户以为草稿已落盘。
        let save_shortcut = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: Key::S,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.ctrl || modifiers.command
                )
            })
        });
        if save_shortcut {
            if self.editing_body.is_none() {
                // 本函数持有的是 `session` 的借用，而 `save_all` 需要 `self.session`
                // （此刻字段是 None）；延后到本帧末尾 session 放回后再写。
                self.pending_save = true;
            } else {
                self.message = "正文编辑器有未保存草稿：请用 Ctrl+Enter 保存正文".to_string();
            }
            return;
        }
        // **文本输入获得焦点时，键盘归输入框**：否则在编辑框里敲空格/方向键会同时滚动页面
        // （用户反馈）。`Memory::focused()` 在有 TextEdit 聚焦时返回其 Id，
        // 覆盖搜索框、题目/正文编辑框等所有输入控件。
        if ctx.memory(|memory| memory.focused().is_some()) {
            return;
        }
        // 正文编辑窗口打开时，键盘归编辑窗口（Esc 取消、Ctrl+Enter 保存）。
        if self.editing_body.is_some() {
            return;
        }
        // 截图模式下不采集任何"影响主视图显示内容"的命令（滚动/缩放/翻页/复制…）：
        // 键盘的 Enter/Esc 与框选都在 `draw_canvas` 里处理（那里才有画布矩形与滚动量）。
        if self.capture.is_some() {
            return;
        }
        let page_count = session.page_count();

        // 键盘
        let pressed: Vec<(Key, Modifiers)> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        // 复制：egui 把 Ctrl/Cmd+C 翻译成 `Event::Copy`（**不是** Key 事件），
        // 因此必须单独识别它 —— 这就是第二轮验收里"复制按钮可用但 Ctrl+C 无反应"的原因。
        let copy_event = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
        });
        if copy_event {
            self.commands.push(ViewerCommand::CopySelection);
        }
        for (key, modifiers) in pressed {
            // 已经由 Event::Copy 处理过时不重复处理（避免复制两次）
            if copy_event && key == Key::C {
                continue;
            }
            let command_modifier = modifiers.ctrl || modifiers.command;
            // 文本框有焦点时把按键让给文本框（它自己的撤销/删除更贴合当前输入）
            let text_focused = ctx.memory(|memory| memory.focused().is_some());
            if key == Key::Z && command_modifier && !text_focused {
                if modifiers.shift {
                    self.redo_edit();
                } else {
                    self.undo_edit();
                }
                continue;
            }
            if (key == Key::Delete || key == Key::Backspace)
                && !text_focused
                && self.delete_hovered_annotation()
            {
                continue;
            }
            if key == Key::H && !modifiers.any() {
                self.pending_annotate = Some(MarkupTool::Highlight);
                continue;
            }
            if key == Key::Escape && self.move_ink_armed.is_some() {
                self.move_ink_armed = None;
                self.message = "已取消移动".to_string();
                continue;
            }
            if key == Key::Escape && self.context_menu_pos.is_some() {
                self.context_annotation = None;
                self.context_menu_pos = None;
                continue;
            }
            if key == Key::Escape && self.annotation_tool.is_some() {
                self.exit_markup_tool();
                self.message = "已退出标记模式".to_string();
                self.drag = None;
                continue;
            }
            if key == Key::Escape && self.tool == Tool::Ink && self.ink.is_none() {
                let previous = self.tool_before_annotation.take().unwrap_or(Tool::Select);
                self.switch_tool(previous);
                self.message = "已退出墨迹模式".to_string();
                continue;
            }
            if key == Key::Escape && self.ink.is_some() {
                // 整笔丢弃：不进标注集合、不产生命令，所以撤销栈里不会留下半条
                self.ink = None;
                self.drag = None;
                self.message = "已取消这一笔".to_string();
                continue;
            }
            if let Some(command) = command_for_key(key, modifiers, page_count) {
                self.commands.push(command);
            }
        }

        // 滚轮与 Ctrl+滚轮的处理**不在这里**：它必须等侧栏等面板绘制完再读
        // （egui 的 ScrollArea 会消费滚动增量），并只在指针位于画布上时生效 ——
        // 否则"在侧栏里滚动，文档也跟着动"。见 `draw_canvas` 里的"滚轮"段。
    }

    /// 处理"把文件拖进窗口"。
    ///
    /// **必须在没有文档时也执行**：v1 把它放在"已有会话"的分支里，
    /// 导致启动后（空状态）拖入 PDF 毫无反应 —— 人工验收反馈的缺陷之一。
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let (hovered, dropped) = ctx.input(|input| {
            (
                input
                    .raw
                    .hovered_files
                    .iter()
                    .map(|file| (file.path.clone(), file.mime.clone()))
                    .collect::<Vec<_>>(),
                input
                    .raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_path_buf())
                    .collect::<Vec<_>>(),
            )
        });
        if !hovered.is_empty() && !self.drag_hover_logged {
            eprintln!("[drag] hovered files: {hovered:?}");
            self.drag_hover_logged = true;
        } else if hovered.is_empty() {
            self.drag_hover_logged = false;
        }
        if !dropped.is_empty() {
            eprintln!("[drag] dropped files: {dropped:?}");
        }
        if let Some(path) = dropped.into_iter().next() {
            self.request_open(path);
        }
    }

    /// 把命令交给会话，并把事件翻译成界面动作。
    pub(super) fn apply_and_pump(&mut self, ctx: &egui::Context, session: &mut DocumentSession) {
        for command in std::mem::take(&mut self.commands) {
            session.handle(command, &mut self.events);
        }
        session.pump(&mut self.events);

        for event in std::mem::take(&mut self.events) {
            match event {
                SessionEvent::CopyToClipboard(text) => {
                    ctx.copy_text(text.clone());
                    self.message = format!("已复制 {} 个字符", text.chars().count());
                }
                SessionEvent::PageRenderFailed { page, error } => {
                    // 切布局/跳页会取消在途渲染；"任务已取消"不是失败，别弹给用户。
                    if !error.is_cancelled() {
                        self.error =
                            Some(format!("第 {} 页渲染失败：{error}", page.display_number()));
                    }
                }
                SessionEvent::Error(error) => self.error = Some(error.to_string()),
                SessionEvent::CurrentPageChanged(page) => {
                    // 页码输入框跟随真实当前页，避免显示过期数字
                    self.page_input = page.display_number().to_string();
                }
                _ => {}
            }
        }
    }

    /// 是否正处于"缩放/旋转预览窗口"内（此刻只显示旧图，不提交渲染）。
    /// 返回"该页当前波段**第一块就绪**以来"的毫秒数（0 = 还没有任何块就绪）。
    ///
    /// 计时起点刻意选在"第一块就绪"而不是"波段变化"：
    /// * 慢手势（滚轮按住 1 秒以上）期间不断改波段，若从波段变化开始计时，
    ///   等渲染真正开始时早已超时 —— 第一块一到就触发兜底，白块又回来了；
    /// * 从"第一块"开始计时，语义正好是"用户已经看到半张图多久了"。
    ///
    /// 波段不变就不重置：同波段内滚动时新块本来就要现渲染，
    /// 等过 [`view::READY_TIMEOUT_MS`] 后应显示"已就绪 + 补白"，而不是一直糊着（D-96）。
    pub(super) fn note_band_wait(
        &mut self,
        page: PageIndex,
        scale_milli: u32,
        ready: usize,
    ) -> u64 {
        let now = self.runtime.clock.now();
        if ready == 0 {
            self.band_since.remove(&page);
            return 0;
        }
        match self.band_since.get(&page) {
            Some((milli, since)) if *milli == scale_milli => {
                now.saturating_duration_since(*since).as_millis() as u64
            }
            _ => {
                self.band_since.insert(page, (scale_milli, now));
                0
            }
        }
    }

    pub(super) fn previewing(&self) -> bool {
        self.runtime
            .preview_until
            .is_some_and(|deadline| self.runtime.clock.now() < deadline)
    }

    /// 没有打开文档时的空状态界面。
    pub(super) fn draw_empty(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(
                        "把 PDF 文件拖到这里，或用命令行参数打开：\n\n    cargo run -p app -- <文件路径>",
                    )
                    .size(18.0)
                    .color(Color32::GRAY),
                );
            });
        });
        if let Some(error) = &self.error {
            egui::Panel::bottom("empty-status").show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(error).color(Color32::from_rgb(200, 60, 60)),
                    )
                    .truncate(),
                );
            });
        }
    }
}

impl PdfReaderApp {
    pub(super) fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.ensure_image_loaders(&ctx);
        self.apply_pending_window_size(&ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.closing_after_decision {
                self.remember_document_view();
                self.flush_settings(Some(&ctx));
            } else if self.body_edit_dirty() {
                // 正文窗口里有未保存草稿：先取消关闭，交给编辑确认框。
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.pending_edit_action.is_none() {
                    self.pending_edit_action = Some(EditDiscardAction::Quit);
                }
            } else if !self.is_dirty() {
                self.remember_document_view();
                self.flush_settings(Some(&ctx));
            } else {
                // 有未保存改动：取消这次关闭；提示已经开着也不能放行。
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.pending_action.is_none() {
                    self.pending_action = Some(PendingAction {
                        kind: PendingActionKind::Quit,
                    });
                    self.pending_action_error = None;
                }
            }
        }

        // 拖放打开：**空状态下也必须处理**（v1 只在已有会话时读取，导致启动后拖入无反应）
        self.handle_dropped_files(&ctx);
        // 原地写入的确认框必须在借用 session 之前处理：确认后要**关掉会话**才能替换文件
        self.draw_pending_action_dialog(&ctx);
        self.draw_in_place_dialog(&ctx);
        self.draw_save_as_dialog(&ctx);
        self.draw_compact_dialog(&ctx);
        self.apply_pending_view_restore(&ctx);
        self.draw_history_window(&ctx);

        // 拖放/菜单请求的文件必须在“空状态提前返回”之前真正打开；否则
        // pending_open 只会在已有 session 的帧末尾消费，启动后拖入 PDF 没反应。
        if let Some(path) = self.pending_open.take() {
            self.open(&path, None);
            ctx.request_repaint();
        }
        // 把会话临时取出，避免与 `self` 的其它字段发生借用冲突
        let Some(mut session) = self.session.take() else {
            self.draw_empty(ui);
            self.draw_password_prompt(&ctx);
            return;
        };

        let frame_start = std::time::Instant::now();
        // 高亮只维持一帧：由本帧的悬停判定重新设置
        self.highlight_float = None;
        // `hovered_annotation` **不在这里清空**：按键处理（`collect_commands`）在本帧更早的位置
        // 读取它，清空会让"悬停 + Delete"永远看不到目标。它由标注绘制那段每帧重算，
        // 指针离开标注时自然变成 `None`。
        // 点击产生的一次性高亮（D-118）：在计时内**每帧**生效，并请求"到期时重绘"。
        // 少了这次请求，空闲时 egui 不会再画一帧 —— 高亮会一直停在屏幕上，
        // 直到下一次鼠标移动之类的事件（用户实测"移开指针后高亮不定长地持续"）。
        if let Some((id, until)) = self.click_highlight {
            let now = self.runtime.clock.now();
            if now < until {
                self.highlight_float = Some(id);
                ctx.request_repaint_after(until - now);
            } else {
                self.click_highlight = None;
            }
        }
        self.collect_commands(&ctx, &session);
        if self.capture.is_some() {
            // 丢弃工具栏等来源的命令：截图期间不改动主视图内容；
            // 注意仍然要跑 `apply_and_pump` —— 它负责 `pump`，瓦片加载得以继续。
            self.commands.clear();
        }
        // 注意：**不再**按命令白名单退出 Pin。用户最终要求是"只有切到选字/截图模式才退出"；
        // 缩放（含适应宽度、100%）、翻页、侧栏开关、checkbox 都不影响 Pin。
        // Pin 的退出由工具切换与截图按钮显式触发（见 draw_toolbar）。
        // 布局切换（纵向/横向）：拿到 &mut session 才能替换布局实现
        if let Some(horizontal) = self.pending_layout.take() {
            if horizontal {
                session.set_layout(Box::new(pdfcore::layout::HorizontalLayout));
            } else {
                session.set_layout(Box::new(pdfcore::layout::VerticalLayout));
            }
            session.handle(ViewerCommand::RequestVisible, &mut self.events);
        }
        // 新会话默认是纵向布局：若 UI 仍标记为"横向"，这里补上，
        // 否则会出现"菜单显示横向、实际纵向"的错位（用户反馈）。
        if self.horizontal_layout {
            session.set_layout(Box::new(pdfcore::layout::HorizontalLayout));
            session.handle(ViewerCommand::RequestVisible, &mut self.events);
        }
        self.apply_and_pump(&ctx, &mut session);
        let after_input = frame_start.elapsed();

        // 检测"缩放/旋转刚刚变过"：连续缩放期间不提交渲染，只显示旧图拉伸（见 ZOOM_PREVIEW_WINDOW）
        let zoom_now = session.state().zoom;
        let rotation_now = session.state().user_rotation;
        if (zoom_now - self.runtime.last_zoom).abs() > f32::EPSILON
            || rotation_now != self.runtime.last_rotation
        {
            self.runtime.last_zoom = zoom_now;
            self.runtime.last_rotation = rotation_now;
            self.runtime.preview_until = Some(self.runtime.clock.now() + ZOOM_PREVIEW_WINDOW);
        }
        let previewing = self.previewing();

        self.runtime.last_convert_ms = 0.0;
        self.runtime.last_upload_ms = 0.0;
        // 拖动期间**暂缓**转换与上传：把"新页进入"那一帧的逐像素开销挪到松手之后，
        // 拖动本身只需保证画面跟着走（未就绪的区域显示旧图或白纸）。
        let dragging = self.drag.is_some();
        let uploaded = self.ensure_textures(&ctx, &mut session, !dragging);
        let after_textures = frame_start.elapsed();
        if uploaded > 0 {
            // 本帧上传过 → 继续重绘，把"松手后补齐"的过程走完（拖动期间暂缓的上传会在这里补上）
            ctx.request_repaint();
        }

        if self.fullscreen {
            // 指针靠近窗口顶端才显示顶栏；离开顶栏区域即隐藏（显示区域本身不变）
            let pointer_y = ctx.pointer_hover_pos().map(|position| position.y);
            match pointer_y {
                Some(y) if y < 6.0 => self.reveal_toolbar = true,
                Some(y) if y > 120.0 => self.reveal_toolbar = false,
                _ => {}
            }
        }
        self.draw_toolbar(ui, &session);
        // Ctrl+F 切换底部搜索框；Esc 关闭
        // F11 切换全屏
        if ctx.input(|input| input.key_pressed(Key::F11)) {
            self.toggle_fullscreen(&ctx);
        }
        if ctx.input(|input| input.modifiers.command && input.key_pressed(Key::F)) {
            self.search_open = !self.search_open;
            // 呼出时直接聚焦输入框，省掉"再点一下"（用户要求）
            self.focus_search = self.search_open;
        }
        if self.search_open && ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.search_open = false;
        }
        self.draw_sidebar(ui, &mut session);
        self.draw_status(ui);
        self.draw_canvas(ui, &mut session);
        let after_draw = frame_start.elapsed();

        // 每帧耗时分解：拖动期间（或单帧明显慢时）打印，用来定位"不跟手"到底花在哪
        if self.runtime.diag.enabled() {
            let total_ms = after_draw.as_secs_f64() * 1000.0;
            if self.drag.is_some() || total_ms > 25.0 {
                self.runtime.diag.line(format!(
                    "[帧] 总={:.1}ms 输入+pump={:.1}ms 纹理={:.1}ms(转换={:.1} 上传={:.1}) 绘制={:.1}ms 纹理数={} 命中={} 可见页={} 大纲={} 在飞={}",
                    total_ms,
                    after_input.as_secs_f64() * 1000.0,
                    (after_textures - after_input).as_secs_f64() * 1000.0,
                    self.runtime.last_convert_ms,
                    self.runtime.last_upload_ms,
                    (after_draw - after_textures).as_secs_f64() * 1000.0,
                    self.textures.len(),
                    self.hits.len(),
                    self.visible.len(),
                    self.navigation
                        .as_ref()
                        .map(|navigation| outline_node_count(&navigation.outline))
                        .unwrap_or(0),
                    session.pending_renders()
                ));
            }
        }

        let pending = if previewing {
            // 预览窗口内**不请求渲染**：把这一串中间比例的钱省下来，只画最终那一次。
            // 会话在 set_zoom 里已经 cancel_all，所以队列此刻是空的，不会白等。
            0
        } else {
            session.handle(ViewerCommand::RequestVisible, &mut self.events);
            self.events.clear();
            session.pending_renders()
        };
        self.session = Some(session);

        // 预览窗口结束后必须自己醒一次，去请求最终比例的渲染
        if previewing {
            if let Some(deadline) = self.runtime.preview_until {
                ctx.request_repaint_after(
                    deadline.saturating_duration_since(self.runtime.clock.now()),
                );
            }
        } else if pending > 0 {
            ctx.request_repaint();
        }
        self.draw_password_prompt(&ctx);
        self.draw_note_dialog(&ctx);
        self.draw_custom_dialog(&ctx);
        self.draw_edit_dialogs(&ctx);
        self.draw_external_link_dialog(&ctx);
        // 样式/视图改动不抢在拖动中写盘：指针空闲时再落一次。
        // 这里**不要**再 remember_document_view：刚打开新文档时视图还在等两帧恢复，
        // 此时记下来会把"第 1 页"写进设置，反而把上次位置覆盖掉。
        if self.settings_dirty && !ctx.input(|input| input.pointer.any_down()) {
            self.flush_settings(Some(&ctx));
        }
        // Ctrl+S 同样要等 session 放回；这是它之前“按了没反应”的根因。
        if std::mem::take(&mut self.pending_save) {
            self.save_all();
            ctx.request_repaint();
        }
        // 菜单里请求的"打开另一个文件"放在最后：此时 `self.session` 已经放回，
        // 打开新文档不会被旧 session 覆盖，也不会把新文件的阅读状态写坏。
        if let Some(path) = self.pending_open.take() {
            self.open(&path, None);
            ctx.request_repaint();
        }
    }
}
