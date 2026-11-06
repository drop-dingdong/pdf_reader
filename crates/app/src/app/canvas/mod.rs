//! `app::canvas`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    body_size_changed, canvas_rect_for, command_for_scroll, command_for_zoom_delta, link_box_color,
    link_overlay_visible, link_rect_in_page_bounds, needs_underlay, page_rect_to_screen, plan_page,
    tile_view_rect, view_rect_for, wheel_targets_canvas_with_float, DrawInput, PageDraw, PlanStamp,
    Preview,
};
use eframe::egui;
use egui::{Color32, CornerRadius, Id, Sense, Stroke, StrokeKind, Vec2 as EVec2};
use pdfcore::annotation::{AnnotationKind, INK_COLOR, INK_OPACITY};

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
            let (alt, ctrl) = ui.ctx().input(|input| (input.modifiers.alt, input.modifiers.ctrl));
            if (alt || ctrl || self.move_ink_armed.is_some())
                && self.annotation_tool.is_none()
                && matches!(self.tool, Tool::Select | Tool::Pan)
            {
                if let Some(position) = ui.ctx().pointer_hover_pos() {
                    if canvas.contains(position) {
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
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
                    (input.smooth_scroll_delta, input.zoom_delta(), input.modifiers)
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
                self.handle_ink_pointer(ui, canvas, &response, session, session.viewport().scroll);
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
                        let content =
                            canvas_to_content(canvas, position, session.viewport().scroll);
                        moving = self.begin_ink_move(session, content, armed);
                    }
                }
                self.move_ink_armed = None;
                if moving {
                    self.drag = Some(DragMode::MoveInk);
                } else if let Some(position) = response.interact_pointer_pos() {
                    let content =
                        canvas_to_content(canvas, position, session.viewport().scroll);
                    let hit = session.text_position_at(content);
                    match (select_now, hit) {
                        (true, Some(text_position)) => {
                            self.drag = Some(DragMode::Select);
                            self.selection_started_on_text = true;
                            session.handle(
                                ViewerCommand::SetSelectionGranularity(SelectionGranularity::Char),
                                &mut self.events,
                            );
                            session.handle(
                                ViewerCommand::SelectBegin(text_position),
                                &mut self.events,
                            );
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
                        self.scroll_quiet_until =
                            Some(self.clock.now() + HOVER_SCROLL_QUIET);
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
                                    visible.iter().map(|page| page.display_number()).collect::<Vec<_>>()
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

            // 1) 页面
            let visible = self.visible.clone();
            let previewing = self.previewing();
            let mut report: Vec<String> = Vec::new();
            for page in visible {
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
                    self.textures.preview_source(page, placement.rotation, whole_key.scale_milli);
                // D-96：波段切换后"旧内容 vs 新块"的判据（缺一块就不翻页，另有超时兜底）
                let waited_ms = self.note_band_wait(page, whole_key.scale_milli, ready);
                // D-104：同一波段内已经切到瓦片就不再退回预览（否则波段内滚动会整页变糊）
                let showing_tiles = self.last_plan.get(&page).is_some_and(|stamp| {
                    stamp.band == whole_key.scale_milli
                        && matches!(stamp.plan, PageDraw::Tiles)
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
                    self.textures.underlay_source(
                        page,
                        placement.rotation,
                        whole_key.scale_milli,
                    )
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
                                    let rect = tile_view_rect(
                                        grid, &placement, col, row, render.0, scroll,
                                    );
                                    let key =
                                        RenderKey::tiled(page, render, placement.rotation, tile);
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
                let previous = self.last_plan.insert(page, stamp).filter(|prev| *prev != stamp);
                if let Some(previous) = previous.filter(|_| self.diag.enabled()) {
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
                    self.diag.line(line);
                }
                if self.diag.enabled() {
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

            // 2) 高亮（选区 + 搜索命中）
            for highlight in session.highlight_selection() {
                paint_highlight(&painter, session, &highlight, scroll, canvas.min);
            }
            let active = self.active_hit;
            for highlight in session.highlights_for_search(&self.hits, active) {
                paint_highlight(&painter, session, &highlight, scroll, canvas.min);
            }

            // 链接热区诊断（PDF_READER_DIAGNOSE=1）：热区越界 = 坐标空间不一致
            if self.diag.enabled() {
                if let Some(navigation) = &self.navigation {
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else { continue };
                        let links = navigation.links_on(page);
                        if links.is_empty() {
                            continue;
                        }
                        let size = placement.page_size;
                        let first = links[0].rect;
                        self.diag.line(format!(
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
                        let Some(placement) = session.placement(page).copied() else { continue };
                        for link in navigation.links_on(page) {
                            // 与高亮同一条换算链：PDF 页面空间 → 内容空间 → 视图空间
                            let content = placement.rect_to_content(link.rect);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
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

            // 2b') 标注叠加层（D-118）：几何在页面空间，这里换算到屏幕
            if self.show_annotations && self.capture.is_none() {
                let pointer = ui.ctx().pointer_hover_pos();
                self.hovered_annotation = None; // 每帧重算（指针离开后立即失效）
                for page in self.visible.clone() {
                    let Some(placement) = session.placement(page).copied() else { continue };
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
                                    && annotation.contains(
                                        placement.content_to_page(content_point),
                                        tolerance,
                                    )
                                {
                                    // 命中/删除只看几何，与"有没有正文"无关
                                    self.hovered_annotation = Some(annotation.id);
                                    if let Some(window) = self
                                        .floats
                                        .iter()
                                        .find(|window| {
                                            window.owner == FloatOwner::Annotation(annotation.id)
                                        })
                                    {
                                        self.highlight_float = Some(window.id);
                                    }
                                }
                            }
                            continue;
                        }
                        // 便签：没有 quads，单独画图标（图标 = 便签矩形；正文在浮窗里）。
                        if let AnnotationKind::Note { rect } = annotation.kind {
                            let content = placement.rect_to_content(rect);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                                EVec2::new(content.width, content.height),
                            );
                            paint_note_icon(&painter, screen, color);
                            if let Some(pointer) = pointer {
                                if screen.expand(2.0).contains(pointer) {
                                    self.hovered_annotation = Some(annotation.id);
                                    if let Some(window) = self
                                        .floats
                                        .iter()
                                        .find(|window| {
                                            window.owner == FloatOwner::Annotation(annotation.id)
                                        })
                                    {
                                        self.highlight_float = Some(window.id);
                                    }
                                }
                            }
                            continue;
                        }
                        for quad in annotation.kind.quads() {
                            let content = placement.rect_to_content(*quad);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
                                EVec2::new(content.width, content.height),
                            );
                            let to_screen = |point: Point| {
                                let content = placement.page_to_content(point);
                                egui::pos2(
                                    canvas.min.x + content.x - scroll.x,
                                    canvas.min.y + content.y - scroll.y,
                                )
                            };
                            // 悬停：把对应浮窗高亮一帧（离开即熄灭；点击另有时长高亮）
                            if let Some(pointer) = pointer {
                                if screen.contains(pointer) {
                                    // 命中/删除只看几何，与"有没有正文"无关
                                    self.hovered_annotation = Some(annotation.id);
                                    if let Some(window) = self
                                        .floats
                                        .iter()
                                        .find(|window| {
                                            window.owner == FloatOwner::Annotation(annotation.id)
                                        })
                                    {
                                        self.highlight_float = Some(window.id);
                                    }
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
                                    let y = quad.y
                                        + quad.height
                                            * pdfcore::annotation::STRIKE_FRACTION;
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
                    if self.clock.now() >= until {
                        self.annotation_focus = None;
                    } else if let Some(annotation) = self.annotations.get(id) {
                        if let (Some(bounds), Some(placement)) =
                            (annotation.bounds(), session.placement(annotation.page))
                        {
                            let content = placement.rect_to_content(bounds);
                            let screen = egui::Rect::from_min_size(
                                canvas.min
                                    + EVec2::new(content.x - scroll.x, content.y - scroll.y),
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
                    let Some(placement) = session.placement(page).copied() else { continue };
                    let Some(marks) = self.dest_marks.get(page.get() as usize) else { continue };
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
                                    ui.ctx().copy_text(crate::image_store::image_uri(
                                        &record_id, revision,
                                    ));
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
                                &placement,
                                mark.rect,
                                scroll,
                                canvas.min,
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
                                self.commands.push(ViewerCommand::SelectExtend(text_position));
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
                        self.commands.push(ViewerCommand::SelectBegin(text_position));
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
                                self.commands.push(ViewerCommand::SelectBegin(text_position));
                            }
                            None => self.commands.push(ViewerCommand::SelectClear),
                        }
                    }
                }
            }

            // 记下画布矩形：侧栏（在该函数之前绘制）需要它来给浮窗定位
            self.canvas_rect = canvas;

            // 悬停预览：**先只做检测与状态更新**（含"同一目的地已固定 → 高亮 + 置顶"）
            if self.capture.is_none() && self.custom_pin.is_none() {
                self.update_hover_preview(ui, session, canvas, scroll);
            }

            // 浮窗尺寸变化 → 回写正文尺寸到来源数据（用户要求：改大小即记住正文面积）
            // 先收集，再统一走 apply_edit，保证一次 resize 进入撤销栈。
            let mut preview_resizes: Vec<(PreviewKey, egui::Vec2, egui::Vec2)> = Vec::new();
            let mut dest_resizes: Vec<(PageIndex, usize, DestMark, DestMark)> = Vec::new();
            for window in std::mem::take(&mut self.floats) {
                let body = window.body;
                if let Some((page, index)) = window.origin {
                    if let Some(mark) = self
                        .dest_marks
                        .get(page.get() as usize)
                        .and_then(|marks| marks.get(index))
                    {
                        if body_size_changed(mark.body_size, body) {
                            let before = mark.clone();
                            let mut after = before.clone();
                            after.body_size = body;
                            dest_resizes.push((page, index, before, after));
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
                self.apply_edit(
                    AppEdit::EditDestMark {
                        page,
                        index,
                        before: std::sync::Arc::new(before),
                        after: std::sync::Arc::new(after),
                    },
                    true,
                );
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
        });
        self.draw_annotation_context_menu(ui.ctx());
    }
}
