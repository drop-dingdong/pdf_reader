//! `app::modes`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use eframe::egui;
use pdfcore::annotation::{AnnotationId, AnnotationKind};

impl PdfReaderApp {
    /// 进入 Pin 状态：**预览模式回到平移**、退出截图模式（用户要求）。
    pub(super) fn enter_pin(&mut self, kind: CustomKind) {
        self.tool = Tool::Pan;
        self.capture = None;
        self.custom_pin = Some(kind);
        self.message = "当前自定义preview：点击页面确定位置（Esc 取消）".to_string();
    }

    /// Pin 状态：在页面某处点击 → 结束选点并弹出对应的输入对话框。
    pub(super) fn finish_pin(
        &mut self,
        session: &DocumentSession,
        canvas: egui::Rect,
        scroll: Vec2,
        pointer: egui::Pos2,
    ) {
        let Some(kind) = self.custom_pin else { return };
        let content = canvas_to_content(canvas, pointer, scroll);
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let point = placement.content_to_page(content);
            self.custom_dialog = Some(CustomDialog {
                kind,
                page,
                point,
                title: String::new(),
                content: String::new(),
            });
            self.custom_pin = None;
            return;
        }
    }

    /// Alt+拖拽：命中一条自有墨迹时开始平移。
    pub(super) fn begin_ink_move(
        &mut self,
        session: &DocumentSession,
        content: Point,
        required: Option<AnnotationId>,
    ) -> bool {
        let Some(id) = self.annotation_at(session, content) else {
            return false;
        };
        if required.is_some_and(|required| required != id) {
            return false;
        }
        if self.annotation_pdf.external_ids.contains(&id) {
            self.message = "外部注释暂不支持移动".to_string();
            return false;
        }
        let Some(annotation) = self.annotations.get(id).cloned() else {
            return false;
        };
        let AnnotationKind::Ink { points, .. } = &annotation.kind else {
            return false;
        };
        let original = points.clone();
        let Some(placement) = session.placement(annotation.page) else {
            return false;
        };
        let start = placement.content_to_page(content);
        self.ink_move = Some(InkMoveState {
            id,
            before: annotation,
            start,
            original,
        });
        self.message = "移动墨迹：拖到新位置，松手提交（Ctrl+Z 可回原位）".to_string();
        true
    }

    /// 拖动中：按"当前页面点 − 起点"平移原始点集（每次从原始点算，不累积误差）。
    pub(super) fn update_ink_move(&mut self, session: &DocumentSession, content: Point) {
        let Some(state) = self.ink_move.as_ref() else {
            return;
        };
        let Some(placement) = session.placement(state.before.page) else {
            return;
        };
        let current = placement.content_to_page(content);
        let dx = current.x - state.start.x;
        let dy = current.y - state.start.y;
        let points: Vec<Point> = state
            .original
            .iter()
            .map(|point| Point::new(point.x + dx, point.y + dy))
            .collect();
        let width = state
            .before
            .kind
            .ink()
            .map(|(_, width)| width)
            .unwrap_or(2.0);
        if let Some(annotation) = self.annotations.get_mut(state.id) {
            annotation.kind = AnnotationKind::Ink { points, width };
        }
    }

    /// 松手：几何变了就记一条 `MoveAnnotation`；没动就不记历史。
    pub(super) fn finish_ink_move(&mut self) {
        let Some(state) = self.ink_move.take() else {
            return;
        };
        let Some(current) = self.annotations.get(state.id).cloned() else {
            return;
        };
        if current.kind == state.before.kind {
            return;
        }
        let mut after = current;
        // 几何变了 → 写回必须重建对象（旧对象进 deletes）。
        after.pdf_ref = None;
        after.modified = Some(unix_now());
        self.apply_edit(
            AppEdit::MoveAnnotation {
                before: state.before,
                after,
            },
            true,
        );
        self.message = "已移动墨迹（Ctrl+Z 可回原位）".to_string();
    }

    /// 进入便签选点：下一次点击页面时弹出输入框。
    pub(super) fn enter_note_mode(&mut self) {
        // 便签与任何阅读模式都没有关联：**不切换工具**（否则会高亮选字/平移），
        // 只记录进入前的阅读模式 A，Esc 时回到 A。
        if self.annotation_tool.is_some() || self.tool == Tool::Ink || self.note_pin {
            // 从其它注释模式进入：保留原来的 A，退出当前注释模式（丢弃半笔墨迹）。
            self.annotation_tool = None;
            self.ink = None;
            self.drag = None;
        } else {
            self.tool_before_annotation = Some(self.tool);
        }
        self.note_pin = true;
        self.custom_pin = None;
        self.message = "便签：点击页面位置，然后输入正文（Esc 取消）".to_string();
    }

    /// 退出便签模式：回到进入前的阅读模式 A。
    pub(super) fn exit_note_mode(&mut self) {
        if !self.note_pin {
            return;
        }
        self.note_pin = false;
        if let Some(previous) = self.tool_before_annotation.take() {
            self.switch_tool(previous);
        }
        self.message = "已取消便签".to_string();
    }

    /// 进入墨迹模式：只有从阅读模式进入才记录 A；从其它注释模式切过来保留原 A。
    pub(super) fn enter_ink_mode(&mut self) {
        if self.tool == Tool::Ink && self.annotation_tool.is_none() && !self.note_pin {
            return;
        }
        let came_from_annotation =
            self.annotation_tool.is_some() || self.tool == Tool::Ink || self.note_pin;
        let remembered = self.tool_before_annotation;
        let previous = self.tool;
        self.switch_tool(Tool::Ink);
        self.tool_before_annotation = if came_from_annotation {
            remembered
        } else {
            Some(previous)
        };
        self.note_pin = false;
        self.annotation_tool = None;
    }

    /// 便签选点：找到被点中的页与页面坐标，弹出输入框。
    pub(super) fn finish_note_pin(
        &mut self,
        session: &DocumentSession,
        canvas: egui::Rect,
        scroll: Vec2,
        pointer: egui::Pos2,
    ) {
        let content = canvas_to_content(canvas, pointer, scroll);
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else {
                continue;
            };
            if !placement.rect.contains(content) {
                continue;
            }
            let point = placement.content_to_page(content);
            let half = NOTE_ICON_SIZE * 0.5;
            self.note_dialog = Some(NoteDraft {
                page,
                rect: Rect::new(
                    point.x - half,
                    point.y - half,
                    NOTE_ICON_SIZE,
                    NOTE_ICON_SIZE,
                ),
                text: String::new(),
            });
            self.note_pin = false;
            return;
        }
        self.message = "便签：请点击页面内部".to_string();
    }

    /// 新建一条便签（正文可空）。返回是否成功；整个创建是**一条**撤销记录。
    pub(super) fn add_note_annotation(&mut self, page: PageIndex, rect: Rect, text: &str) -> bool {
        let body = text.trim().to_string();
        let Some(id) = self.insert_internal_annotation(page, AnnotationKind::Note { rect }) else {
            return false;
        };
        if let Some(annotation) = self.annotations.get_mut(id) {
            annotation.body = (!body.is_empty()).then_some(body);
            annotation.author = backend_pdfium::pdf_patch::pdf_author();
            let now = unix_now();
            annotation.created = Some(now);
            annotation.modified = Some(now);
        }
        let Some(annotation) = self.annotations.get(id).cloned() else {
            self.annotations.remove(id);
            return false;
        };
        self.annotations.remove(id);
        self.apply_edit(AppEdit::AddAnnotation { annotation }, true);
        true
    }
}
