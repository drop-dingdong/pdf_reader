//! `app::canvas::interact`：画布输入、右键、点击/拖动与帧尾浮窗。

use super::super::*;
use super::*;

use crate::app::paint::canvas_to_content;
use eframe::egui;

impl PdfReaderApp {
    /// 滚轮缩放/滚动、拖动当帧生效、墨迹/选字/平移的拖拽起点。
    pub(super) fn handle_canvas_input(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        canvas: egui::Rect,
        response: &egui::Response,
    ) {
        // ---- 滚轮（在面板绘制之后读，且仅当指针在画布上）----
        //
        // 1. **必须等侧栏/工具栏绘制完再读**：egui 的 `ScrollArea` 会消费滚动增量，
        //    消费后 `smooth_scroll_delta` 归零；此前在面板绘制前读取，于是"侧栏滚动时文档也动"。
        // 2. 加指针位置判据，双保险（且行为可预期）。
        // 3. 当帧直接执行，滚轮少延迟一帧。
        let pointer_over_canvas = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|position| canvas.contains(position));
        // 浮窗是画布上的不透明覆盖层：指针在浮窗上时，滚轮/缩放只归浮窗内部
        // （Markdown 的 ScrollArea 会自己消费）。否则文档会跟着一起滚。
        let over_float = ui.ctx().pointer_hover_pos().is_some_and(|position| {
            self.floats
                .iter()
                .any(|window| window.rect.contains(position))
                || self
                    .hover_tip
                    .as_ref()
                    .and_then(|tip| tip.rect)
                    .is_some_and(|rect| rect.contains(position))
        });
        // 指针在编辑窗口/帮助窗口等 Area 上时，滚轮归该窗口（例如编辑区的
        // ScrollArea），不能先被画布消费。`layer_id_at` 用的是上一帧的图层，
        // 对正在打开的窗口最多晚一帧，可接受。
        let over_window = ui.ctx().pointer_hover_pos().is_some_and(|position| {
            ui.ctx()
                .layer_id_at(position)
                .is_some_and(|layer| layer != egui::LayerId::background())
        });
        // Alt 悬停在墨迹上：抓手光标，提示可以拖动。
        let (alt, ctrl) = ui
            .ctx()
            .input(|input| (input.modifiers.alt, input.modifiers.ctrl));
        if (alt || ctrl || self.move_ink_armed.is_some())
            && self.annotation_tool.is_none()
            && matches!(self.tool, Tool::Select | Tool::Pan)
        {
            if let Some(position) = ui.ctx().pointer_hover_pos() {
                if canvas.contains(position) {
                    let content = canvas_to_content(canvas, position, session.viewport().scroll);
                    if self.annotation_at(session, content).is_some_and(|id| {
                        !self.external_ids.contains(&id)
                            && self
                                .annotations
                                .get(id)
                                .is_some_and(|annotation| annotation.kind.ink().is_some())
                    }) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                }
            }
        }
        // 便签选点：十字光标 + Esc 取消
        if self.note_pin {
            if pointer_over_canvas {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                self.exit_note_mode();
            }
        }
        // Pin 状态（自定义 preview 选点）：十字光标 + 状态栏提示
        if self.custom_pin.is_some() {
            if pointer_over_canvas {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                self.custom_pin = None;
                self.message = "已退出自定义preview".to_string();
            }
        }
        if self.capture.is_none()
            && wheel_targets_canvas_with_float(pointer_over_canvas && !over_window, over_float)
        {
            let (scroll_delta, zoom_delta, modifiers) = ui.input(|input| {
                (
                    input.smooth_scroll_delta,
                    input.zoom_delta(),
                    input.modifiers,
                )
            });
            // 注意：**只有真的产生了滚动/缩放命令**才开启静默窗口。
            // 曾经把开窗写在"指针在画布上"的分支里，结果静止悬停时也每帧刷新，
            // 静默窗口永不结束 → 正常悬停也看不到预览。
            let mut view_changed = false;
            // 勾选"横向滚动"：纵向分量改作横向，横向分量丢弃（用户要求）；拖动不受影响
            let scroll_delta = if self.horizontal_scroll {
                egui::vec2(scroll_delta.y, 0.0)
            } else {
                egui::vec2(scroll_delta.x, scroll_delta.y)
            };
            let scroll_before = session.viewport().scroll;
            if let Some(command) = command_for_scroll(
                scroll_delta,
                modifiers.ctrl || modifiers.command,
                session.state().zoom,
            ) {
                session.handle(command, &mut self.events);
                view_changed = true;
                if self.diag.enabled() {
                    let after = session.viewport().scroll;
                    let viewport = session.viewport().size;
                    self.diag.line(format!(
                            "[滚动] 滚轮 delta=({:.1},{:.1}) scroll ({:.1},{:.1})→({:.1},{:.1}) 视口={}x{} 横向布局={}",
                            scroll_delta.x, scroll_delta.y, scroll_before.x, scroll_before.y,
                            after.x, after.y, viewport.width, viewport.height,
                            self.horizontal_layout
                        ));
                }
            }
            if let Some(command) = command_for_zoom_delta(zoom_delta, session.state().zoom) {
                session.handle(command, &mut self.events);
                view_changed = true;
            }
            if view_changed {
                // 视图刚被滚动/缩放 → 静默一段时间，期间不显示悬停预览（用户要求）
                self.scroll_quiet_until = Some(self.clock.now() + HOVER_SCROLL_QUIET);
                ui.ctx().request_repaint();
            }
        }

        // ---- 拖动**当帧生效** ----
        //
        // 拖动命令原先要等下一帧 `apply_and_pump` 才执行，于是画面永远落后指针一帧；
        // 快速拖动时就是上百像素的偏差（用户反馈"落后鼠标移动过多"）。
        // 这里在读取 scroll / 绘制**之前**先处理拖动，内容与指针同帧同步。
        let inverted_now = ui.input(|input| input.modifiers.shift);
        let select_now = self.annotation_tool.is_some()
            || match self.tool {
                Tool::Select => !inverted_now,
                Tool::Pan => inverted_now,
                Tool::Ink => false,
            };
        // 墨迹工具独占指针：拖拽既不滚页也不选字（S4b）；便签模式下不画墨迹。
        if self.tool == Tool::Ink && !self.note_pin {
            self.handle_ink_pointer(ui, canvas, response, session, session.viewport().scroll);
        } else if self.capture.is_none() && response.drag_started() && !self.note_pin {
            self.selection_started_on_text = false;
            self.drag_started_at = Some(self.clock.now());
            self.last_pan_at = None;
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[时序] 拖动开始 t={:.6}s",
                    self.started_at.elapsed().as_secs_f64()
                ));
            }
            // Alt+拖拽墨迹 = 平移（v1 只支持墨迹）。命中才进入，否则照旧选字/平移。
            // Alt 在不少 Linux 桌面被窗口管理器抢去拖窗口，所以同时支持 Ctrl；
            // 也可以先用右键菜单「移动」选中，再直接拖。
            let (alt, ctrl) = ui.input(|input| (input.modifiers.alt, input.modifiers.ctrl));
            let armed = self.move_ink_armed;
            let mut moving = false;
            if (alt || ctrl || armed.is_some())
                && self.annotation_tool.is_none()
                && matches!(self.tool, Tool::Select | Tool::Pan)
            {
                if let Some(position) = response.interact_pointer_pos() {
                    let content = canvas_to_content(canvas, position, session.viewport().scroll);
                    moving = self.begin_ink_move(session, content, armed);
                }
            }
            self.move_ink_armed = None;
            if moving {
                self.drag = Some(DragMode::MoveInk);
            } else if let Some(position) = response.interact_pointer_pos() {
                let content = canvas_to_content(canvas, position, session.viewport().scroll);
                let hit = session.text_position_at(content);
                match (select_now, hit) {
                    (true, Some(text_position)) => {
                        self.drag = Some(DragMode::Select);
                        self.selection_started_on_text = true;
                        session.handle(
                            ViewerCommand::SetSelectionGranularity(SelectionGranularity::Char),
                            &mut self.events,
                        );
                        session.handle(ViewerCommand::SelectBegin(text_position), &mut self.events);
                    }
                    // 选字模式下即使起点不在文字上，也**不做平移**（用户反馈）：
                    // 拖动过程中指针移到文字上时仍可继续扩选；不在文字上则什么也不发生。
                    (true, None) => self.drag = Some(DragMode::Select),
                    _ => self.drag = Some(DragMode::Pan),
                }
            }
        }
        if self.capture.is_none() && response.dragged() {
            ui.ctx().request_repaint();
            match self.drag {
                Some(DragMode::Pan) => {
                    let delta = response.drag_delta();
                    let before = session.viewport().scroll;
                    session.handle(
                        ViewerCommand::ScrollBy(Vec2::new(-delta.x, -delta.y)),
                        &mut self.events,
                    );
                    let after = session.viewport().scroll;
                    // 拖动也是"视图在动" → 同样静默，避免预览在拖动中闪现
                    self.scroll_quiet_until = Some(self.clock.now() + HOVER_SCROLL_QUIET);
                    if self.diag.enabled() && after != before {
                        let now = self.clock.now();
                        let since_start = self
                            .drag_started_at
                            .map(|start| now.duration_since(start).as_secs_f64() * 1000.0)
                            .unwrap_or(0.0);
                        let since_last = self
                            .last_pan_at
                            .map(|last| now.duration_since(last).as_secs_f64() * 1000.0)
                            .unwrap_or(f64::NAN);
                        self.diag.line(format!(
                                "[时序] 拖动位移生效 t={:.6}s Δ起={:.1}ms Δ上次={:.1}ms scroll=({:.1},{:.1})",
                                self.started_at.elapsed().as_secs_f64(),
                                since_start,
                                since_last,
                                after.x,
                                after.y
                            ));
                        self.last_pan_at = Some(now);
                        // "因为拖动而重新决定显示内容"= 可见页集合发生变化（要换/补位图）
                        let mut visible = Vec::new();
                        session.visible_pages(&mut visible);
                        if visible != self.last_visible {
                            self.diag.line(format!(
                                "[时序] 可见页集变化 t={:.6}s Δ起={:.1}ms 可见页={:?}",
                                self.started_at.elapsed().as_secs_f64(),
                                since_start,
                                visible
                                    .iter()
                                    .map(|page| page.display_number())
                                    .collect::<Vec<_>>()
                            ));
                            self.last_visible = visible;
                        }
                    }
                }
                Some(DragMode::Select) => {
                    if let Some(position) = response.interact_pointer_pos() {
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
                        if let Some(text_position) = session.text_position_at(content) {
                            session.handle(
                                ViewerCommand::SelectExtend(text_position),
                                &mut self.events,
                            );
                        }
                    }
                }
                // 墨迹已在上面由 `handle_ink_pointer` 逐帧累积采样点
                Some(DragMode::MoveInk) => {
                    if let Some(position) = response.interact_pointer_pos() {
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
                        self.update_ink_move(session, content);
                    }
                }
                Some(DragMode::Ink) => {}
                None => {}
            }
        }
    }

    /// 右键：记录指针下的注释，菜单由 `draw_annotation_context_menu` 绘制。
    pub(super) fn handle_secondary_click(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        ctx: &CanvasPaint,
    ) {
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        // 2a'') 右键：记录指针下的注释；菜单由 draw_annotation_context_menu 绘制。
        // 用原始指针事件，不受 popup 捕获影响；在另一条注释上右键可直接切换。
        if self.capture.is_none() && ui.input(|i| i.pointer.secondary_clicked()) {
            if let Some(position) = ui.input(|i| i.pointer.interact_pos()) {
                if canvas.contains(position) {
                    let content = canvas_to_content(canvas, position, scroll);
                    let id = self.annotation_at(session, content);
                    self.context_annotation = id;
                    self.context_menu_pos = id.map(|_| position);
                }
            }
        }
    }

    /// 点击/双击/拖动结束：选字、便签选点、Pin、打开注释、跟随链接。
    pub(super) fn handle_canvas_interaction(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        ctx: &CanvasPaint,
        response: &egui::Response,
    ) {
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        // 3) 交互
        //
        // 在画布上按下时主动结束文本输入：否则工具栏的搜索框仍持有焦点，
        // `Ctrl+C` 会被它吃掉，用户看到的现象是"选中了却复制不出来"（人工验收反馈）。
        if response.drag_started()
            || response.clicked()
            || response.double_clicked()
            || response.triple_clicked()
        {
            ui.memory_mut(|memory| memory.stop_text_input());
        }

        // 当前生效的拖拽语义：按住 Shift 临时**反选**工具，
        // 这样想顺手复制一句时不必先切换工具（"自动"模式已按反馈移除）。
        let inverted = ui.input(|input| input.modifiers.shift);
        let select_tool = match self.tool {
            Tool::Select => !inverted,
            Tool::Pan => inverted,
            Tool::Ink => false,
        };

        // drag_started / dragged 已在绘制之前处理（当帧生效），此处不重复
        if response.dragged() {
            // 拖动期间必须持续重绘：命令在**下一帧**才生效，
            // 若这一帧之后没有新的输入事件（鼠标停住不动），画面就不会更新 ——
            // 表现出来就是"拖动过程中视野不动、松手才跳一下"。
            ui.ctx().request_repaint();
            match self.drag {
                Some(DragMode::Select) => {
                    if let Some(position) = response.interact_pointer_pos() {
                        let content = canvas_to_content(canvas, position, scroll);
                        if let Some(text_position) = session.text_position_at(content) {
                            self.commands
                                .push(ViewerCommand::SelectExtend(text_position));
                        }
                    }
                }
                Some(DragMode::MoveInk) => {
                    if let Some(position) = response.interact_pointer_pos() {
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
                        self.update_ink_move(session, content);
                    }
                }
                Some(DragMode::Pan) | Some(DragMode::Ink) => {
                    // 已在绘制之前当帧处理（见"拖动当帧生效"），这里不再重复提交
                }
                None => {}
            }
        }

        // 拖动过程诊断：把 egui 报的状态与下一帧要执行的滚动量打出来（PDF_READER_DIAGNOSE=1）
        if self.diag.enabled()
            && (response.drag_started()
                || response.dragged()
                || response.drag_stopped()
                || response.clicked())
        {
            self.diag.line(format!(
                    "[拖动] started={} dragged={} stopped={} clicked={} 模式={:?} 指针={:?} delta={:?} 当前scroll=({:.1},{:.1})",
                    response.drag_started(),
                    response.dragged(),
                    response.drag_stopped(),
                    response.clicked(),
                    self.drag,
                    response.interact_pointer_pos().map(|p| (p.x, p.y)),
                    response.drag_delta(),
                    scroll.x,
                    scroll.y
                ));
        }
        if response.drag_stopped() {
            let apply_markup = self.drag == Some(DragMode::Select)
                && self.selection_started_on_text
                && self.annotation_tool.is_some();
            self.selection_started_on_text = false;
            if self.diag.enabled() {
                let total = self
                    .drag_started_at
                    .map(|start| start.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0);
                self.diag.line(format!(
                    "[时序] 拖动结束 t={:.6}s 全程={:.1}ms",
                    self.started_at.elapsed().as_secs_f64(),
                    total
                ));
            }
            if self.drag == Some(DragMode::MoveInk) {
                self.finish_ink_move();
            }
            self.drag = None;
            self.drag_started_at = None;
            self.last_pan_at = None;
            if apply_markup {
                self.pending_annotate = self.annotation_tool;
            }
        }
        if self.capture.is_none() && response.double_clicked() {
            if let Some(position) = response.interact_pointer_pos() {
                let content = canvas_to_content(canvas, position, scroll);
                if let Some(text_position) = session.text_position_at(content) {
                    self.commands.push(ViewerCommand::SetSelectionGranularity(
                        SelectionGranularity::Word,
                    ));
                    self.commands
                        .push(ViewerCommand::SelectBegin(text_position));
                }
            }
        } else if self.capture.is_none() && self.note_pin && response.clicked() {
            if let Some(position) = response.interact_pointer_pos() {
                self.finish_note_pin(session, canvas, scroll, position);
            }
        } else if self.capture.is_none() && self.custom_pin.is_some() && response.clicked() {
            // Pin 状态：点击页面某处 → 结束选点，弹出输入对话框
            if let Some(position) = response.interact_pointer_pos() {
                self.finish_pin(session, canvas, scroll, position);
            }
        } else if self.capture.is_none() && response.clicked() {
            if let Some(position) = response.interact_pointer_pos() {
                let content = canvas_to_content(canvas, position, scroll);
                // 标注最先（它画在链接框之上，用户点到的就是它）；其次是链接热区
                if let Some(id) = self.annotation_at(session, content) {
                    self.open_annotation_note(id, position);
                } else if !self.follow_link(session, content) && select_tool {
                    // 选字工具下：点空白处清空选择，点到文字则重新落点。
                    // v1 无条件清空，于是"选完随手点一下"就会把选区清掉、复制扑空。
                    match session.text_position_at(content) {
                        Some(text_position) => {
                            self.commands.push(ViewerCommand::SetSelectionGranularity(
                                SelectionGranularity::Char,
                            ));
                            self.commands
                                .push(ViewerCommand::SelectBegin(text_position));
                        }
                        None => self.commands.push(ViewerCommand::SelectClear),
                    }
                }
            }
        }
    }

    /// 帧尾：记录画布矩形、悬停预览、浮窗尺寸回写、绘制浮窗/临时预览。
    pub(super) fn finish_canvas_frame(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        ctx: &CanvasPaint,
    ) {
        let canvas = ctx.canvas;
        let scroll = ctx.scroll;
        // 记下画布矩形：侧栏（在该函数之前绘制）需要它来给浮窗定位
        self.canvas_rect = canvas;

        // 悬停预览：**先只做检测与状态更新**（含"同一目的地已固定 → 高亮 + 置顶"）
        if self.capture.is_none() && self.custom_pin.is_none() {
            self.update_hover_preview(ui, session, canvas, scroll);
        }

        // 浮窗尺寸变化 → 回写正文尺寸到来源数据（用户要求：改大小即记住正文面积）
        // 先收集，再统一走 apply_edit，保证一次 resize 进入撤销栈。
        let mut preview_resizes: Vec<(PreviewKey, egui::Vec2, egui::Vec2)> = Vec::new();
        let mut dest_resizes: Vec<(PageIndex, usize, egui::Vec2, egui::Vec2)> = Vec::new();
        for window in std::mem::take(&mut self.floats) {
            let body = window.body;
            if let Some((page, index)) = window.origin {
                if let Some(mark) = self
                    .dest_marks
                    .get(page.get() as usize)
                    .and_then(|marks| marks.get(index))
                {
                    if body_size_changed(mark.body_size, body) {
                        dest_resizes.push((page, index, mark.body_size, body));
                    }
                }
            }
            if let Some(index) = window.custom_index() {
                if let Some(preview) = self.custom_previews.get(index) {
                    if body_size_changed(preview.body_size, body) {
                        preview_resizes.push((
                            PreviewKey {
                                page: preview.page,
                                id: preview.id,
                            },
                            preview.body_size,
                            body,
                        ));
                    }
                }
            }
            self.floats.push(window);
        }
        for (key, before, after) in preview_resizes {
            self.apply_preview_resize(key, before, after);
        }
        for (page, index, before, after) in dest_resizes {
            self.apply_dest_resize(page, index, before, after);
        }

        // 固定浮窗：绘制与注册都在此；后加入的在更上层（见 draw_floats）
        if self.capture.is_none() && self.custom_pin.is_none() {
            // 正在显示、但还没拿到预览图的自定义图片 preview：每帧重试一次。
            // 旧实现只在"悬浮/点击侧栏条目"时重试，于是"点了但渲染还没好"的浮窗
            // 会一直空着（D-101 把初次生成改成了"提交渲染 + 等待"，必须有重试入口）。
            self.ensure_pending_custom_snapshots(ui, session);
            self.ensure_markdown_image_owners(ui, session);
            self.draw_floats(ui, canvas);
            // 正文编辑：只检测双击并打开正式编辑窗口（编辑窗口在 draw_edit_dialogs 里）
            self.detect_body_double_click(ui);
            // 临时预览**最后**绘制 → 真正位于最上层（用户要求）
            self.draw_hover_preview(ui, canvas);
            // 侧栏条目悬浮 → 主视图右侧的临时浮窗
            self.draw_sidebar_hover_float(ui, canvas);
            // 公式失败只做短暂状态提示；下一条消息会直接顶替。
            self.take_math_error();
            // Markdown 链接只提示，不打开浏览器（与 PDF URI 的行为一致）。
            self.consume_markdown_links(ui.ctx());
        } else if self.capture.is_some() {
            self.draw_capture(ui, canvas, session, scroll);
        }
    }
}
