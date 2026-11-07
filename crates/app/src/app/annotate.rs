//! `app::annotate`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::ink::InkStroke;
use crate::view::{body_size_of, pin_exits_on_mode_change, FLOAT_TITLE_HEIGHT};
use eframe::egui;
use egui::Vec2 as EVec2;
use pdfcore::annotation::{AnnotationId, AnnotationKind, INK_OPACITY};

impl PdfReaderApp {
    /// 按某个自定义 preview 的数据生成它的**正式浮窗**（正文尺寸取 `body_size`）。    /// 按某个自定义 preview 的数据生成它的**正式浮窗**（正文尺寸取 `body_size`）。
    /// 把当前选区变成指定文本标记，返回新增数量（D-118 S1）。
    ///
    /// 几何来自文本层的 `rects_for_range`（已按行合并），因此三种标记共用同一套算法；
    /// 标注**只存页面空间**，缩放/旋转时由布局换算，不会漂移。
    pub(super) fn annotate_selection(
        &mut self,
        session: &mut DocumentSession,
        tool: MarkupTool,
    ) -> usize {
        let highlights = session.highlight_selection();
        let mut added = 0;
        for highlight in highlights {
            let kind = tool.kind(highlight.rects.clone());
            let (color, opacity) = match tool {
                MarkupTool::Highlight => (self.markup_color, self.markup_opacity),
                MarkupTool::Underline | MarkupTool::StrikeOut => (self.markup_color, 1.0),
            };
            if self.add_annotation_styled(highlight.page, kind, color, opacity) {
                added += 1;
            }
        }
        if added > 0 {
            self.pdf_dirty = true;
        }
        if added == 0 && session.highlight_selection().is_empty() {
            self.message = format!("先选中文字，再{}", tool.label());
        } else if added > 0 {
            self.message = format!("已{} {added} 处", tool.label());
        }
        added
    }

    /// 页面矩形 → 该区域覆盖的文本行矩形（页面空间）。
    ///
    /// 会**按需加载**该页文本：标注必须先有文本层才能取几何（扫描件没有字符框，
    /// 这种页应当走"选框"类标注，而不是文本高亮）。
    pub(super) fn quads_for_page_rect(&mut self, page: PageIndex, rect: Rect) -> Option<Vec<Rect>> {
        let text = self.session.as_mut()?.page_text(page)?;
        let range = text.range_for_page_rect(rect)?;
        Some(text.rects_for_range(range))
    }

    /// 把页面上的一块矩形区域变成高亮（按文本字符范围取几何）。
    pub(super) fn annotate_page_rect(&mut self, page: PageIndex, rect: Rect) -> usize {
        let Some(quads) = self.quads_for_page_rect(page, rect) else {
            return 0;
        };
        let bounds = quads
            .iter()
            .copied()
            .reduce(|a, b| a.union(b))
            .unwrap_or(rect);
        usize::from(self.add_highlight(page, quads, bounds))
    }

    /// 进入/退出文本标记模式；进入时自动切到选字工具。
    pub(super) fn toggle_markup_tool(&mut self, tool: MarkupTool) {
        // 从便签切回标记也要算"从注释模式进入"，否则会把 A 改写成当前阅读模式。
        let came_from_annotation = self.note_pin || self.tool == Tool::Ink;
        self.note_pin = false;
        if self.annotation_tool.is_some() {
            if self.annotation_tool == Some(tool) {
                self.exit_markup_tool();
            } else {
                self.annotation_tool = Some(tool);
                self.message = format!("{}模式：拖选文字即可添加，Esc 退出", tool.label());
            }
            return;
        }
        // 从墨迹/便签切过来时**保留原来的 A**；只有从阅读模式进入才记录 A。
        let remembered = self.tool_before_annotation;
        let previous = self.tool;
        self.switch_tool(Tool::Select);
        self.tool_before_annotation = if came_from_annotation {
            remembered
        } else {
            Some(previous)
        };
        self.annotation_tool = Some(tool);
        self.message = format!("{}模式：拖选文字即可添加，Esc 退出", tool.label());
    }

    /// 退出标记模式，恢复进入前的工具。
    pub(super) fn exit_markup_tool(&mut self) {
        let previous = self.tool_before_annotation.take().unwrap_or(Tool::Select);
        self.annotation_tool = None;
        self.switch_tool(previous);
    }

    /// 切换工具（工具栏与回放共用）：**切走墨迹时丢弃正在画的一笔**。
    pub(super) fn switch_tool(&mut self, tool: Tool) {
        if self.tool == Tool::Ink && tool != Tool::Ink {
            if self.ink.take().is_some() {
                self.message = "已切换工具，未完成的一笔已丢弃".to_string();
            }
            self.drag = None;
        }
        self.tool = tool;
        self.annotation_tool = None;
        self.tool_before_annotation = None;
        self.note_pin = false;
        self.move_ink_armed = None;
        if pin_exits_on_mode_change(tool == Tool::Select, false) {
            self.custom_pin = None;
        }
    }

    /// 内容空间点 →（所在页，页面空间点）。
    ///
    /// 墨迹用它决定"这一笔画在哪一页"：可见页按顺序取第一个包含该点的。
    pub(super) fn page_at_content(
        &self,
        session: &DocumentSession,
        content: Point,
    ) -> Option<(PageIndex, Point)> {
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page) else {
                continue;
            };
            if placement.rect.contains(content) {
                return Some((page, placement.content_to_page(content)));
            }
        }
        None
    }

    /// 墨迹工具的手势：按下起笔、拖动累积、**抬笔才入库**（D-118 S4b）。
    ///
    /// 拖拽中只更新 [`InkStroke`]（实时跟手），不进 `AnnotationStore`、不产生命令；
    /// 因此 Esc / 切工具丢弃时"不会留下半条"。
    pub(super) fn handle_ink_pointer(
        &mut self,
        ui: &egui::Ui,
        canvas: egui::Rect,
        response: &egui::Response,
        session: &DocumentSession,
        scroll: Vec2,
    ) {
        if self.capture.is_some() {
            return;
        }
        if response.drag_started() {
            // 起点必须用 `press_origin`：egui 的 `drag_started` 要等指针移过阈值才触发，
            // 用当时的指针位置会让整笔平移几个像素。
            let Some(origin) = ui.input(|input| input.pointer.press_origin()) else {
                return;
            };
            let content = canvas_to_content(canvas, origin, scroll);
            let Some((page, page_point)) = self.page_at_content(session, content) else {
                self.message = "请在页面上开始画墨迹".to_string();
                return;
            };
            let mut stroke = InkStroke::start(page, self.ink_width, page_point);
            // 触发 `drag_started` 需要先移过阈值，所以"按下点"与"当前位置"之间已经有一段了；
            // 把当前位置也采进来，否则一笔的开头会凭空少一段（快速起笔时尤其明显）。
            if let Some(position) = response.interact_pointer_pos() {
                let current = canvas_to_content(canvas, position, scroll);
                if let Some(placement) = session.placement(page) {
                    stroke.push(placement.content_to_page(current));
                }
            }
            self.ink = Some(stroke);
            self.drag = Some(DragMode::Ink);
            self.message = "正在画墨迹（Esc 取消）".to_string();
            return;
        }
        if response.dragged() {
            let Some(position) = response.interact_pointer_pos() else {
                return;
            };
            let Some(stroke) = self.ink.as_mut() else {
                return;
            };
            let Some(placement) = session.placement(stroke.page()) else {
                return;
            };
            let content = canvas_to_content(canvas, position, scroll);
            if stroke.push(placement.content_to_page(content)) {
                ui.ctx().request_repaint();
            }
            return;
        }
        if response.drag_stopped() {
            self.drag = None;
            let Some(stroke) = self.ink.take() else {
                return;
            };
            match stroke.finish() {
                Some((page, points, width)) => {
                    self.annotate_ink(page, &points, width);
                }
                None => {
                    self.message = "这一笔太短：至少需要两个点".to_string();
                }
            }
        }
    }

    /// **压力测试专用**：在页内铺开 `count` 笔短墨迹（回放动作 `seed_ink`）。
    ///
    /// 与真实绘制走同一个 `annotate_ink` 入口（所以每条都有配对正文、都进撤销栈）；
    /// 布局是确定性的：40 行，行距 18，列距 22，每笔是一个 4 点的小锯齿。
    /// 只给"1000 条以内忙帧 p99"的压力脚本用，不暴露给 UI。
    pub(super) fn seed_ink_strokes(&mut self, page: PageIndex, count: u32, width: f32) {
        for index in 0..count {
            let row = (index % 40) as f32;
            let column = (index / 40) as f32;
            let x = 40.0 + column * 22.0;
            let y = 80.0 + row * 18.0;
            let points = [
                Point::new(x, y),
                Point::new(x + 6.0, y + 6.0),
                Point::new(x + 12.0, y),
                Point::new(x + 18.0, y + 6.0),
            ];
            self.annotate_ink(page, &points, width);
        }
        self.message = format!("已造 {count} 笔墨迹（压力测试）");
    }

    /// 把页面上的一笔墨迹变成标注（页面空间折线 + 页面空间线宽，D-118 S4）。
    ///
    /// 回放动作 `ink` 与（S4c 的）墨迹工具都调用它：**抬笔时调用一次**，
    /// 因此"一笔 = 一条标注 = 一次撤销"只有这一处实现。
    pub(super) fn annotate_ink(&mut self, page: PageIndex, points: &[Point], width: f32) -> bool {
        if self.add_annotation_styled(
            page,
            AnnotationKind::Ink {
                points: points.to_vec(),
                width,
            },
            self.ink_color,
            INK_OPACITY,
        ) {
            self.message = "已添加墨迹（Ctrl+Z 可撤销）".to_string();
            true
        } else {
            false
        }
    }

    pub(super) fn add_highlight(
        &mut self,
        page: PageIndex,
        quads: Vec<Rect>,
        _bounds: Rect,
    ) -> bool {
        self.add_annotation(page, AnnotationKind::Highlight { quads })
    }

    /// 建一条标注并**记入历史**（所有"新建标注"的唯一入口）。
    /// 正文直接属于注释（初始为空；首次编辑时再写）。
    pub(super) fn add_annotation(&mut self, page: PageIndex, kind: AnnotationKind) -> bool {
        self.add_annotation_with_style(page, kind, None)
    }

    pub(super) fn add_annotation_styled(
        &mut self,
        page: PageIndex,
        kind: AnnotationKind,
        color: [u8; 3],
        opacity: f32,
    ) -> bool {
        self.add_annotation_with_style(page, kind, Some((color, opacity)))
    }

    /// 新的内部注释 id：时间戳/单调，保证大于现有 id 且不复用已删除的旧 id。
    ///
    /// **必须小于 2^53**：写回 job 用 JSON number 传 id，f64 只能精确表示 2^53 以内的整数；
    /// 超过就会在保存/重开后变成另一个 id，`pd-float://` 链接随之断开。
    fn next_internal_annotation_id(&self) -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST: AtomicU64 = AtomicU64::new(0);
        static SEQ: AtomicU64 = AtomicU64::new(1);
        // 秒 << 20：约 1.8e15（< 9.0e15 = 2^53），低 20 位给同一秒内的序号
        // （约 100 万条/秒；足够，且 2^53 上限可用到 2255 年左右）。
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed) & 0xf_ffff;
        let base = (seconds << 20) | seq;
        let floor = self
            .annotations
            .iter()
            .map(|annotation| annotation.id.get())
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        loop {
            let previous = LAST.load(Ordering::Relaxed);
            let candidate = base.max(floor).max(previous.saturating_add(1));
            if LAST
                .compare_exchange(previous, candidate, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return candidate;
            }
        }
    }

    /// 以新的内部 id 插入一条注释（不记历史）；调用方负责后续字段与 `apply_edit`。
    pub(super) fn insert_internal_annotation(
        &mut self,
        page: PageIndex,
        kind: AnnotationKind,
    ) -> Option<AnnotationId> {
        let mut annotation = pdfcore::annotation::Annotation::new(page, kind)?;
        annotation.id = AnnotationId(self.next_internal_annotation_id());
        let id = annotation.id;
        self.annotations.insert(annotation);
        Some(id)
    }

    pub(super) fn add_annotation_with_style(
        &mut self,
        page: PageIndex,
        kind: AnnotationKind,
        style: Option<([u8; 3], f32)>,
    ) -> bool {
        // 时间戳/单调 id：删除注释后旧 id 不会被新注释复用，`pd-float://` 链接
        // 才不会在重开后静默指向另一条注释。外部注释仍由读回按加载顺序分配。
        let Some(id) = self.insert_internal_annotation(page, kind) else {
            return false;
        };
        if let Some(annotation) = self.annotations.get_mut(id) {
            if let Some((color, opacity)) = style {
                annotation.color = color;
                annotation.opacity = opacity.clamp(0.0, 1.0);
            }
            annotation.author = backend_pdfium::pdf_patch::pdf_author();
            let now = unix_now();
            annotation.created = Some(now);
            annotation.modified = Some(now);
        }
        let Some(annotation) = self.annotations.get(id).cloned() else {
            return false;
        };
        // 先把"就地插入"撤掉，再走统一的 `apply_edit` 重新应用 —— 这样"新建"与"撤销后重做"
        // 走的是同一条代码路径，不会一边删了正文、另一边忘了删。
        self.annotations.remove(id);
        self.apply_edit(AppEdit::AddAnnotation { annotation }, true);
        true
    }

    /// 删除指定标注（列表右键与 `Delete` 键共用；Ctrl+Z 可撤销）。
    pub(super) fn delete_annotation(&mut self, id: AnnotationId) -> bool {
        let Some(annotation) = self.annotations.get(id).cloned() else {
            return false;
        };
        self.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        self.message = "已删除标注（Ctrl+Z 可撤销）".to_string();
        true
    }

    /// 修改自有标注的颜色。
    ///
    /// 已写入 PDF 的注释通过“旧对象 deletes + 新对象 pages”重建，避免依赖
    /// 各阅读器不同的 AP 颜色字段；外部注释暂不改色，避免把别人的注释变成自有对象。
    pub(super) fn restyle_annotation(
        &mut self,
        id: AnnotationId,
        color: Option<[u8; 3]>,
        opacity: Option<f32>,
        ink_width: Option<f32>,
    ) -> bool {
        let Some(before) = self.annotations.get(id).cloned() else {
            return false;
        };
        if self.external_ids.contains(&id) {
            self.message = "外部注释暂不支持修改样式".to_string();
            return false;
        }
        // 墨迹线宽是 kind 的一部分：改它要重建 kind。
        let mut kind = before.kind.clone();
        if let (Some(width), AnnotationKind::Ink { points, .. }) = (ink_width, &before.kind) {
            kind = AnnotationKind::Ink {
                points: points.clone(),
                width: width.clamp(0.5, 8.0),
            };
        }
        let color = color.unwrap_or(before.color);
        let opacity = opacity
            .map(|value| value.clamp(0.1, 1.0))
            .unwrap_or(before.opacity);
        if color == before.color
            && (opacity - before.opacity).abs() < f32::EPSILON
            && kind == before.kind
        {
            return false;
        }
        // **保留同一个 id**：链接（pd-float://annotation/<id>）在改样式后仍然有效。
        let mut after = before.clone();
        after.kind = kind;
        after.color = color;
        after.opacity = opacity;
        after.modified = Some(unix_now());
        // 样式变了，写回必须重建对象；旧对象由 `deletes` 清理。
        after.pdf_ref = None;
        self.apply_edit(AppEdit::RestyleAnnotation { before, after }, true);
        self.message = "已修改标注样式（Ctrl+Z 可撤销；外部注释暂不支持）".to_string();
        true
    }

    /// 改颜色（旧入口，内部走 [`Self::restyle_annotation`]）。
    pub(super) fn recolor_annotation(&mut self, id: AnnotationId, color: [u8; 3]) -> bool {
        self.restyle_annotation(id, Some(color), None, None)
    }

    /// 只跳到注释位置（不打开正文浮窗）；侧栏菜单/浮窗 goto 用。
    pub(super) fn jump_to_annotation(&mut self, id: AnnotationId) {
        let Some(annotation) = self.annotations.get(id) else {
            return;
        };
        let page = annotation.page;
        if let Some(bounds) = annotation.bounds() {
            self.commands.push(ViewerCommand::CenterOnPagePoint {
                page,
                point: bounds.center(),
            });
        } else {
            self.commands.push(ViewerCommand::GoToPage(page));
        }
        self.annotation_focus = Some((id, self.runtime.clock.now() + ANNOTATION_FOCUS));
        self.message = format!(
            "已定位到注释 #{}（第 {} 页）",
            id.get(),
            page.display_number()
        );
    }

    /// 注释右键菜单（画布与注释列表共用）：颜色、删除。
    pub(super) fn annotation_context_menu(&mut self, ui: &mut egui::Ui, id: AnnotationId) {
        let Some(annotation) = self.annotations.get(id) else {
            return;
        };
        let external = self.external_ids.contains(&id);
        let current = annotation.color;
        let current_opacity = annotation.opacity;
        let current_width = annotation.kind.ink().map(|(_, width)| width);
        let is_highlight = matches!(annotation.kind, AnnotationKind::Highlight { .. });
        let uri = self.annotation_link_uri(id);
        let copy = ui.add_enabled(uri.is_some(), egui::Button::new("复制引用链接"));
        let copy = if uri.is_some() {
            copy
        } else {
            copy.on_disabled_hover_text("外部注释默认不可引用（可在 ? 中开启跨会话外部注释链接）")
        };
        if copy.clicked() {
            if let Some(uri) = uri {
                ui.ctx().copy_text(uri);
                self.message = "已复制注释引用链接，可粘到正文".to_string();
            }
            self.context_annotation = None;
            self.context_menu_pos = None;
            ui.close();
            return;
        }
        if external {
            ui.label("外部注释暂不支持改样式（正文/删除可用）");
        } else {
            ui.menu_button("颜色", |ui| {
                for (name, rgb) in [
                    ("黄", [255u8, 214, 0]),
                    ("红", [235u8, 70, 70]),
                    ("绿", [70u8, 180, 90]),
                    ("蓝", [70u8, 130, 220]),
                ] {
                    if color_swatch(ui, name, rgb, current == rgb).clicked()
                        && self.recolor_annotation(id, rgb)
                    {
                        self.context_annotation = None;
                        self.context_menu_pos = None;
                        ui.close();
                    }
                }
            });
            if is_highlight {
                ui.menu_button("透明度", |ui| {
                    for (label, value) in [
                        ("20%", 0.2f32),
                        ("35%", 0.35),
                        ("50%", 0.5),
                        ("75%", 0.75),
                        ("100%", 1.0),
                    ] {
                        let selected = (current_opacity - value).abs() < 0.01;
                        if ui.selectable_label(selected, label).clicked()
                            && self.restyle_annotation(id, None, Some(value), None)
                        {
                            self.context_annotation = None;
                            self.context_menu_pos = None;
                            ui.close();
                        }
                    }
                });
            }
            if current_width.is_some()
                && ui
                    .button("移动（拖动）")
                    .on_hover_text("选中后直接拖动这条墨迹；Esc 取消")
                    .clicked()
            {
                self.move_ink_armed = Some(id);
                self.context_annotation = None;
                self.context_menu_pos = None;
                ui.close();
                self.message = "已选中墨迹：拖动它即可移动（Esc 取消）".to_string();
            }
            if let Some(width) = current_width {
                ui.menu_button("线宽", |ui| {
                    for value in [1.0f32, 1.5, 2.0, 3.0, 4.0, 6.0] {
                        let selected = (width - value).abs() < 0.01;
                        if ui
                            .selectable_label(selected, format!("{value:.1} pt"))
                            .clicked()
                            && self.restyle_annotation(id, None, None, Some(value))
                        {
                            self.context_annotation = None;
                            self.context_menu_pos = None;
                            ui.close();
                        }
                    }
                });
            }
        }
        if ui.button("删除注释（可撤销）").clicked() {
            self.delete_annotation(id);
            self.context_annotation = None;
            self.context_menu_pos = None;
            ui.close();
        }
    }

    /// 删除指针下的标注（`Delete` 键；Ctrl+Z 可撤销）。
    pub(super) fn delete_hovered_annotation(&mut self) -> bool {
        let Some(id) = self.hovered_annotation else {
            return false;
        };
        self.delete_annotation(id)
    }

    /// 命中标注（`content` 是内容空间点，容差按**屏幕像素**换算到页面单位）。
    pub(super) fn annotation_at(
        &self,
        session: &DocumentSession,
        content: Point,
    ) -> Option<AnnotationId> {
        let zoom = session.state().zoom.max(0.01);
        let tolerance = ANNOTATION_HIT_TOLERANCE_PX / zoom;
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page) else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let page_point = placement.content_to_page(content);
            if let Some(hit) = self
                .annotations
                .on_page(page)
                .find(|annotation| annotation.contains(page_point, tolerance))
            {
                return Some(hit.id);
            }
        }
        None
    }

    /// 从注释列表定位：跳页、给中性轮廓、打开正文浮窗。
    pub(super) fn focus_annotation(&mut self, id: AnnotationId, session: &DocumentSession) {
        let Some(annotation) = self.annotations.get(id) else {
            return;
        };
        let page = annotation.page;
        let bounds = annotation.bounds();
        if let Some(bounds) = bounds {
            self.commands.push(ViewerCommand::CenterOnPagePoint {
                page,
                point: bounds.center(),
            });
        } else {
            self.commands.push(ViewerCommand::GoToPage(page));
        }
        let mut at = egui::pos2(360.0, 160.0);
        if self.canvas_rect.width() > 1.0 {
            if let Some(placement) = session.placement(page) {
                if let Some(bounds) = bounds {
                    let content = placement.page_to_content(bounds.min());
                    let scroll = session.viewport().scroll;
                    at = egui::pos2(
                        self.canvas_rect.min.x + content.x - scroll.x,
                        self.canvas_rect.min.y + content.y - scroll.y,
                    );
                }
            }
            let max_x = (self.canvas_rect.right() - 300.0).max(self.canvas_rect.left());
            let max_y = (self.canvas_rect.bottom() - 160.0).max(self.canvas_rect.top());
            at.x = at.x.clamp(self.canvas_rect.left(), max_x);
            at.y = at.y.clamp(self.canvas_rect.top(), max_y);
        }
        self.annotation_focus = Some((id, self.runtime.clock.now() + ANNOTATION_FOCUS));
        self.open_annotation_note(id, at);
    }

    /// 点击标注：打开/置顶它的正文浮窗（正文直接属于注释，不再经过 preview）。
    pub(super) fn open_annotation_note(&mut self, id: AnnotationId, at: egui::Pos2) {
        let Some(annotation) = self.annotations.get(id) else {
            return;
        };
        let body = annotation.body.clone().unwrap_or_default();
        let title = format!("标注 #{}", id.get());
        if let Some(position) = self
            .floats
            .iter()
            .position(|window| window.owner == FloatOwner::Annotation(id))
        {
            let window = self.floats.remove(position);
            let float_id = window.id;
            self.floats.push(window);
            self.highlight_float = Some(float_id);
            self.click_highlight = Some((float_id, self.runtime.clock.now() + CLICK_HIGHLIGHT));
            return;
        }
        let rect = egui::Rect::from_min_size(at, EVec2::new(280.0, 150.0));
        let float_id = self.next_float_id;
        self.next_float_id += 1;
        self.floats.push(Float {
            id: float_id,
            title,
            content: FloatContent::Text(body),
            rect,
            owner: FloatOwner::Annotation(id),
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        self.highlight_float = Some(float_id);
        self.click_highlight = Some((float_id, self.runtime.clock.now() + CLICK_HIGHLIGHT));
    }

    /// "编辑历史"可视列表：只是撤销栈的**视图**（点击不做状态跳转）。
    pub(super) fn draw_history_window(&mut self, ctx: &egui::Context) {
        if !self.show_history || (!self.history.can_undo() && !self.history.can_redo()) {
            return;
        }
        let mut open = self.show_history;
        egui::Window::new("编辑历史")
            .open(&mut open)
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "可撤销 {} 步 · 可重做 {} 步",
                    self.history.len(),
                    if self.history.can_redo() { 1 } else { 0 }
                ));
                ui.separator();
                // 虚拟化：只排版可见行。1000 条编辑时整表渲染要 300ms+（实测），
                // 而这里按行高只画十几行 —— 历史窗口打开着也不影响帧时间。
                let row_height = ui.spacing().interact_size.y.max(HISTORY_ROW_HEIGHT);
                egui::ScrollArea::vertical()
                    .max_height(HISTORY_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show_rows(ui, row_height, self.history.len(), |ui, range| {
                        for index in range {
                            if let Some(description) = self.history.description(index) {
                                ui.label(format!("{}. {description}", index + 1));
                            }
                        }
                    });
                if self.history.dropped() > 0 {
                    ui.separator();
                    ui.weak(format!(
                        "更早的 {} 条记录已因内存上限被丢弃",
                        self.history.dropped()
                    ));
                }
                ui.separator();
                if ui.button("撤销一步").clicked() {
                    self.undo_edit();
                }
                if ui.button("重做一步").clicked() {
                    self.redo_edit();
                }
            });
        self.show_history = open;
    }

    /// 绘制自定义注释右键菜单。
    ///
    /// 不用 `Response::context_menu`：它的 popup 绑定在画布 response 上，
    /// 指针移到菜单/子菜单时菜单会丢；自定义 Area 可以按 state 保持，
    /// 并且能在另一条注释上右键时直接替换锚点。
    pub(super) fn draw_annotation_context_menu(&mut self, ctx: &egui::Context) {
        let (Some(id), Some(pos)) = (self.context_annotation, self.context_menu_pos) else {
            return;
        };
        let screen = ctx.viewport_rect();
        let pos = egui::pos2(
            pos.x.min((screen.right() - 170.0).max(screen.left())),
            pos.y.min((screen.bottom() - 24.0).max(screen.top())),
        );
        let inner = egui::Area::new(egui::Id::unique("annotation-context-menu"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(150.0);
                    self.annotation_context_menu(ui, id);
                });
            });
        let pointer = ctx.input(|i| i.pointer.interact_pos());
        let clicked = ctx.input(|i| i.pointer.any_click());
        let opening = ctx.input(|i| i.pointer.secondary_clicked());
        if clicked
            && !opening
            && pointer.is_some_and(|pointer| !inner.response.rect.contains(pointer))
            && !ctx.any_popup_open()
        {
            self.context_annotation = None;
            self.context_menu_pos = None;
        } else {
            ctx.request_repaint();
        }
    }
}
