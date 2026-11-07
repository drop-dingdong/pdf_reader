//! `app::dialogs`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::persist;
use crate::view::zero_rect_at;
use eframe::egui;
use egui::{Color32, Id, Key, RichText};
use std::path::PathBuf;

impl PdfReaderApp {
    /// 原地写入的确认框（S5c）。确认后覆盖原文件；取消则什么都不做。
    pub(super) fn draw_in_place_dialog(&mut self, ctx: &egui::Context) {
        if !self.save_flow.pending_in_place {
            return;
        }
        let Some(path) = self.document_path.clone() else {
            self.save_flow.pending_in_place = false;
            return;
        };
        let sidecar = persist::sidecar_path(&path);
        let pdf = self.pdf_dirty;
        let data = self.data_dirty;
        if !pdf && !data {
            self.save_flow.pending_in_place = false;
            self.message = "无改动".to_string();
            return;
        }
        let (ref_updates, ref_deletes) = self.pdf_ref_changes();
        let (inline_updates, inline_deletes) = self.inline_changes();
        let new_count = self
            .annotations
            .iter()
            .filter(|annotation| {
                !self.external_ids.contains(&annotation.id) && annotation.pdf_ref.is_none()
            })
            .count();
        let updated = ref_updates.len() + inline_updates.len();
        let deleted = ref_deletes.len() + inline_deletes.len();
        let conflict = pdf && self.pdf_conflict();
        let error = self.save_flow.in_place_error.clone();
        let mut decision: Option<u8> = None;
        egui::Modal::new(Id::unique("in-place-confirm")).show(ctx, |ui| {
            ui.set_width(520.0);
            let heading = match (pdf, data) {
                (true, true) => "保存",
                (true, false) => "保存到 PDF",
                _ => "保存自定义数据",
            };
            ui.heading(heading);
            if pdf {
                ui.label(format!("将注释写入 PDF：{}", path.display()));
                ui.label(format!(
                    "注释：新增 {new_count}、更新 {updated}、删除 {deleted} 条"
                ));
                if self.backup_before_inplace {
                    ui.label("写入前会在同目录保留一份 .bak 备份。");
                } else {
                    ui.label("不会额外保留备份（可在「文件」菜单开启）。");
                }
            }
            if data {
                ui.label(format!("将自定义数据写入：{}", sidecar.display()));
            }
            if conflict {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    "原 PDF 已被其他程序修改：直接保存会丢弃对方的修改。请另存为或取消。",
                );
            }
            if let Some(error) = &error {
                ui.colored_label(Color32::from_rgb(200, 60, 60), error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    decision = Some(0);
                }
                if pdf && ui.button("另存为…").clicked() {
                    decision = Some(1);
                }
                if ui.button("保存").clicked() {
                    decision = Some(2);
                }
            });
        });
        match decision {
            Some(0) => {
                self.save_flow.pending_in_place = false;
                self.save_flow.in_place_error = None;
            }
            Some(1) => {
                self.save_flow.pending_in_place = false;
                self.save_flow.in_place_error = None;
                self.open_save_as_dialog();
            }
            Some(2) => match self.save_in_place_now(false) {
                Ok(()) => {
                    self.save_flow.pending_in_place = false;
                    self.save_flow.in_place_error = None;
                }
                // 失败/冲突：保留对话框与错误，用户可以直接改走另存为。
                Err(error) => self.save_flow.in_place_error = Some(error),
            },
            _ => {}
        }
    }

    /// 关闭/切换前的未保存提示。
    pub(super) fn draw_pending_action_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.save_flow.pending_action.as_ref() else {
            return;
        };
        // 另存为对话框已经在上面时，先让它处理，避免两个模态叠在一起。
        if self.save_flow.save_as.is_some() {
            return;
        }
        let action = match &pending.kind {
            PendingActionKind::Quit => "关闭窗口".to_string(),
            PendingActionKind::Open(path) => format!("打开 {}", path.display()),
        };
        let (ref_updates, ref_deletes) = self.pdf_ref_changes();
        let (inline_updates, inline_deletes) = self.inline_changes();
        let updated = ref_updates.len() + inline_updates.len();
        let deleted = ref_deletes.len() + inline_deletes.len();
        let new_count = self
            .annotations
            .iter()
            .filter(|annotation| {
                !self.external_ids.contains(&annotation.id) && annotation.pdf_ref.is_none()
            })
            .count();
        let conflict = self.pdf_dirty && self.pdf_conflict();
        let mut decision: Option<u8> = None;
        egui::Modal::new(Id::unique("pending-action")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("有未保存的改动");
            ui.label(format!("即将{action}，但当前还有改动没有保存。"));
            if self.pdf_dirty {
                ui.label(format!(
                    "注释：新增 {new_count}、更新 {updated}、删除 {deleted} 条"
                ));
            }
            if self.data_dirty {
                ui.label("自定义数据（preview / 引用点）：有未保存改动");
            }
            if conflict {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    "原 PDF 已被其他程序修改：原地保存会丢弃对方的修改。",
                );
            }
            if let Some(error) = &self.save_flow.pending_action_error {
                ui.colored_label(Color32::from_rgb(200, 60, 60), error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if !conflict {
                    if ui.button("保存并继续").clicked() {
                        decision = Some(0);
                    }
                } else if ui.button("覆盖保存…").clicked() {
                    decision = Some(1);
                }
                if ui.button("另存为…").clicked() {
                    decision = Some(2);
                }
                if ui.button("不保存").clicked() {
                    decision = Some(3);
                }
                if ui.button("取消").clicked() {
                    decision = Some(4);
                }
            });
        });
        match decision {
            Some(0) => match self.save_in_place_now(false) {
                Ok(()) => self.continue_pending_action(ctx),
                Err(error) => self.save_flow.pending_action_error = Some(error),
            },
            Some(1) => {
                // 覆盖保存 = 另存为到原路径；勾选覆盖后走同一条写回。
                if let Some(path) = self.document_path.clone() {
                    self.open_save_as_dialog_at(path);
                }
            }
            Some(2) => self.open_save_as_dialog(),
            Some(3) => self.continue_pending_action(ctx),
            Some(4) => {
                self.save_flow.pending_action = None;
                self.save_flow.pending_action_error = None;
            }
            _ => {}
        }
    }

    /// 打开「整理副本」对话框，默认同目录 `<stem>.compact.pdf`（已存在则递增后缀）。
    pub(super) fn open_compact_dialog(&mut self) {
        let Some(pdf) = self.document_path.clone() else {
            return;
        };
        let stem = pdf
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "document".to_string());
        let mut candidate = crate::compact::default_path(&pdf);
        let mut index = 1u32;
        while candidate.exists() {
            index += 1;
            candidate = pdf.with_file_name(format!("{stem}.compact-{index}.pdf"));
        }
        self.save_flow.compact_dialog = Some(candidate.to_string_lossy().to_string());
        self.save_flow.compact_error = None;
    }

    /// 「整理副本」对话框：路径输入 + 未保存注释的提示 + 错误信息。
    pub(super) fn draw_compact_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.save_flow.compact_dialog.clone() else {
            return;
        };
        let red = Color32::from_rgb(200, 60, 60);
        let mut decision: Option<bool> = None;
        egui::Modal::new(Id::unique("compact-copy")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("整理副本");
            ui.label("只保留最新一次修订，输出为更小的单段 PDF；原文件与数据文件都不动。");
            ui.add_space(6.0);
            let response = ui.add(
                egui::TextEdit::singleline(&mut draft)
                    .desired_width(f32::INFINITY)
                    .hint_text("目标 .pdf 路径"),
            );
            if response.gained_focus() {
                self.save_flow.compact_error = None;
            }
            let mut target = PathBuf::from(draft.trim());
            if target.extension().is_none() {
                target.set_extension("pdf");
            }
            if let Some(source) = &self.document_path {
                if target == *source {
                    ui.colored_label(red, "目标路径与原文件相同；整理副本必须写新文件。");
                }
            }
            if !draft.trim().is_empty() && target.exists() {
                ui.colored_label(red, format!("目标已存在：{}（不覆盖）", target.display()));
            }
            if let Some(hint) = crate::view::compact_unsaved_hint(self.pdf_dirty) {
                ui.colored_label(red, hint);
            }
            if let Some(error) = &self.save_flow.compact_error {
                ui.colored_label(red, error);
            }
            ui.label(
                RichText::new("整理后的文件不带历史修订；带数字签名或加密的文档会被拒绝。")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    decision = Some(false);
                }
                if ui.button("整理").clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => {
                if draft.trim().is_empty() {
                    self.save_flow.compact_error = Some("目标路径不能为空".to_string());
                    self.save_flow.compact_dialog = Some(draft);
                    return;
                }
                let mut target = PathBuf::from(draft.trim());
                if target.extension().is_none() {
                    target.set_extension("pdf");
                }
                match self.perform_compact(target) {
                    Ok(message) => {
                        self.message = message;
                        self.save_flow.compact_dialog = None;
                        self.save_flow.compact_error = None;
                    }
                    Err(error) => {
                        self.save_flow.compact_dialog = Some(draft);
                        self.save_flow.compact_error = Some(error);
                    }
                }
            }
            Some(false) => {
                self.save_flow.compact_dialog = None;
                self.save_flow.compact_error = None;
            }
            None => self.save_flow.compact_dialog = Some(draft),
        }
    }

    /// 在子进程里执行整理；返回状态栏要显示的一句话。
    pub(super) fn perform_compact(
        &mut self,
        target: PathBuf,
    ) -> std::result::Result<String, String> {
        let Some(source) = self.document_path.clone() else {
            return Err("没有打开的文档".to_string());
        };
        crate::compact::validate_target(&source, &target)?;
        crate::compact::run_child(&source, &target)
    }

    /// 「另存为」路径输入与确认框。
    pub(super) fn draw_save_as_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.save_flow.save_as.clone() else {
            return;
        };
        let mut decision: Option<bool> = None;
        egui::Modal::new(Id::unique("save-as")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("另存为");
            ui.label("把当前 PDF 与注释写到新文件；成功后切换到新文件，原文件保持不变。");
            ui.add_space(6.0);
            let response = ui.add(
                egui::TextEdit::singleline(&mut draft)
                    .desired_width(f32::INFINITY)
                    .hint_text("目标 .pdf 路径"),
            );
            if response.gained_focus() {
                self.save_flow.save_as_error = None;
            }
            let mut target_preview = PathBuf::from(draft.trim());
            if target_preview.extension().is_none() {
                target_preview.set_extension("pdf");
            }
            let target_exists = target_preview.exists();
            if target_exists {
                ui.checkbox(
                    &mut self.save_flow.save_as_overwrite,
                    "目标已存在：覆盖它（同时按下面的勾选覆盖数据文件）",
                );
            }
            let has_custom_data =
                self.data_dirty || !self.custom_previews.is_empty() || self.sidecar_migrated;
            let source_sidecar = self
                .document_path
                .as_ref()
                .map(|path| persist::sidecar_path(path))
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "（原 PDF 数据文件）".to_string());
            let target_sidecar = persist::sidecar_path(&target_preview).display().to_string();
            ui.add_space(4.0);
            ui.checkbox(
                &mut self.save_flow.save_as_write_original,
                "写入原 PDF 的数据文件（归档/同步原路径）",
            );
            ui.label(
                RichText::new(source_sidecar)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.checkbox(
                &mut self.save_flow.save_as_write_target,
                "写入新 PDF 的数据文件（切换后新文档使用）",
            );
            ui.label(
                RichText::new(&target_sidecar)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            let warnings = save_as_warnings(
                self.save_flow.save_as_write_original,
                self.save_flow.save_as_write_target,
                self.data_dirty,
                has_custom_data,
            );
            if warnings.new_missing_data {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    "新文件不会写入自定义数据（preview / 被引用点尺寸等）；切换后这些数据仍保留在内存中，并标记为未保存；链接会从 PDF 重新解析。",
                );
            }
            if warnings.unsaved_new {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    "未保存的改动已写入原数据文件（归档），但不会写入新文件。",
                );
            }
            if warnings.no_data_file {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    "当前自定义数据不会写入任何数据文件（原文件和新文件都不写）。",
                );
            }
            if warnings.original_stale {
                ui.label(
                    RichText::new("原 PDF 的数据文件不会更新。")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
            if let Some(source) = &self.document_path {
                if target_preview == *source {
                    ui.colored_label(Color32::from_rgb(200, 60, 60), "目标路径与原文件相同。");
                }
            }
            if target_preview.exists() {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    format!("目标 PDF 已存在：{}", target_preview.display()),
                );
            }
            if persist::sidecar_path(&target_preview).exists() {
                ui.colored_label(
                    Color32::from_rgb(200, 60, 60),
                    format!(
                        "目标数据文件已存在：{}",
                        persist::sidecar_path(&target_preview).display()
                    ),
                );
            }
            if let Some(error) = &self.save_flow.save_as_error {
                ui.colored_label(Color32::from_rgb(200, 60, 60), error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    decision = Some(false);
                }
                if ui.button("保存副本并切换").clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => {
                if draft.trim().is_empty() {
                    self.save_flow.save_as_error = Some("目标路径不能为空".to_string());
                    return;
                }
                let mut target = PathBuf::from(draft.trim());
                if target.extension().is_none() {
                    target.set_extension("pdf");
                }
                match self.save_as_to(
                    target,
                    self.save_flow.save_as_write_original,
                    self.save_flow.save_as_write_target,
                    self.save_flow.save_as_overwrite,
                ) {
                    Ok(()) => {
                        self.save_flow.save_as = None;
                        self.save_flow.save_as_error = None;
                        self.continue_pending_action(ctx);
                    }
                    Err(error) => self.save_flow.save_as_error = Some(error),
                }
            }
            Some(false) => {
                self.save_flow.save_as = None;
                self.save_flow.save_as_error = None;
            }
            None => self.save_flow.save_as = Some(draft),
        }
    }

    /// 数学公式支持/限制说明。内容按当前后端能力生成，避免文档与实现脱节。
    pub(super) fn draw_math_help(&mut self, ctx: &egui::Context) {
        if !self.math_help {
            return;
        }
        let caps = self.markdown.math_cache.borrow().capabilities();
        let mut open = true;
        egui::Window::new("Markdown 与数学支持")
            .id(Id::unique("math-help"))
            .resizable(true)
            .collapsible(false)
            .default_width(560.0)
            .default_height(420.0)
            .min_width(360.0)
            .min_height(240.0)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("math-help-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading("Markdown");
                        ui.label(
                            "CommonMark/GFM：标题、列表、表格、任务列表、删除线、脚注、\
                             定义列表、代码块、引用；原始 HTML 不执行；图片不加载；\
                             外部链接只提示；pd-float:// 浮窗链接在应用内跳转；单换行按硬换行保留。",
                        );
                        ui.label(
                            "表格走完整渲染路径；代码块/表格/长 URL 会测最小内容宽度，\
                             超过 body 时横向滚动。",
                        );
                        ui.monospace("标题：## 标题 {#id}    跳转：[文字](#id)");
                        ui.label(
                            "内部图片：图片 preview 浮窗点 @，或在引用点标记右键复制引用；\
                             也可在正文编辑窗口「插入内部图片」。只有打开的文字浮窗会渲染引用。",
                        );
                        ui.heading("浮窗引用");
                        ui.monospace(
                            "pd-float://annotation/<id>    pd-float://preview/<页>/<id>",
                        );
                        ui.label(
                            "文字/图片 preview 与注释浮窗标题栏 ↗ 复制引用链接，→ 跳到来源页/位置；\
                             页号从 0 开始。旧写法 pd-float://preview/<id> 只在 id 唯一时可用。",
                        );
                        let allow = ui
                            .checkbox(
                                &mut self.allow_external_annotation_links,
                                "允许跨会话外部注释链接（不保证准确）",
                            )
                            .on_hover_text(
                                "默认关闭：外部注释没有稳定的 /NM，重开后 id 可能指向别的注释。\
                                 开启后允许复制；跨会话回退会校验页号/类型/创建时间，仍不保证完全准确。",
                            );
                        if allow.changed() {
                            self.settings_dirty = true;
                        }
                        ui.label(
                            "外部注释的 id 由加载顺序分配；外部编辑器改写 /NM 也会让链接失效。\
                             引用点/截图/测试浮窗没有稳定目标，不能链接。",
                        );
                        ui.separator();
                        ui.heading("数学语法");
                        ui.monospace("行内：$...$    块级：$$...$$");
                        ui.label("只支持数学模式，不是完整的 LaTeX 文档。");
                        ui.label(
                            "`~` 会按 LaTeX 的不断行空格处理（渲染前换成空格）；\
                             但 STIX 没有中文全角标点（如 `，`），缺字形会回退原文。",
                        );
                        ui.label(
                            "显示公式的 `$$` 独占一行；块内空行会在渲染前去掉，\
                             避免被 CommonMark 当作段落结束。",
                        );
                        ui.label(
                            "如果公式没有正确识别，请将行内公式 `$...$` 更换为\
                             块级公式 `$$...$$` 重试。",
                        );
                        ui.separator();
                        ui.heading("当前后端");
                        ui.label(format!(
                            "{}（CJK 文本：{}；化学式：{}；矢量 SVG：{}）",
                            caps.backend,
                            if caps.cjk_text { "支持" } else { "不支持" },
                            if caps.chemistry {
                                "支持"
                            } else {
                                "不支持"
                            },
                            if caps.vector_svg {
                                "支持"
                            } else {
                                "不支持"
                            },
                        ));
                        ui.separator();
                        ui.heading("支持");
                        ui.label(
                            "分式、根号、积分/求和、矩阵/array、align/gather/multline/cases、\
                             希腊字母/AMS 符号/箭头/运算符、重音、\\left...\\right、颜色、\
                             \\text{拉丁文本}。",
                        );
                        ui.separator();
                        ui.heading("不支持 / 限制");
                        ui.label(
                            "化学式（\\ce/\\pu）、TikZ、完整文档排版、任意自定义宏；\
                             STIX Two Math 没有 CJK 字形，\\text{中文} 会回退原文；\
                             公式源码 ≤ 4096 字节；单张纹理 ≤ 8MP。",
                        );
                        ui.separator();
                        ui.label("失败时显示原文，并在状态栏提示原因；其他消息会直接顶替。");
                        ui.label(format!(
                            "当前缓存 {} 条；超过 64 条会整体清空。",
                            self.markdown.math_cache.borrow().len()
                        ));
                    });
            });
        self.math_help = open;
    }

    /// 新建便签的输入框。    /// 新建便签的输入框。
    pub(super) fn draw_note_dialog(&mut self, ctx: &egui::Context) {
        let mut decision: Option<bool> = None;
        if let Some(draft) = self.note_dialog.as_mut() {
            egui::Modal::new(Id::unique("note-create")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("新建便签");
                ui.label("正文会写进 PDF 的 /Contents；添加后画布上出现便签图标。");
                ui.add(
                    egui::TextEdit::multiline(&mut draft.text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(5),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        decision = Some(false);
                    }
                    if ui.button("添加").clicked() {
                        decision = Some(true);
                    }
                });
            });
        }
        match decision {
            Some(false) => {
                self.note_dialog = None;
                self.message = "已取消便签".to_string();
            }
            Some(true) => {
                let Some(draft) = self.note_dialog.take() else {
                    return;
                };
                if self.add_note_annotation(draft.page, draft.rect, &draft.text) {
                    self.message = "已添加便签（Ctrl+Z 可撤销）".to_string();
                }
            }
            None => {}
        }
    }

    /// 自定义 preview 的输入对话框（文字：题目+内容；图片：题目）。    /// 自定义 preview 的输入对话框（文字：题目+内容；图片：题目）。
    pub(super) fn draw_custom_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.custom_dialog.as_ref() else {
            return;
        };
        let kind = dialog.kind;
        let page = dialog.page;
        let point = dialog.point;
        let mut title = dialog.title.clone();
        let mut content = dialog.content.clone();
        let mut save = false;
        let mut cancel = false;
        let mut shoot = false;
        egui::Modal::new(Id::unique("custom-preview-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(match kind {
                CustomKind::Text => "文字预览",
                CustomKind::Image => "图片预览",
            });
            ui.label(format!(
                "位置：第 {} 页 ({:.1}, {:.1})",
                page.display_number(),
                point.x,
                point.y
            ));
            ui.separator();
            ui.label("题目");
            ui.add(egui::TextEdit::singleline(&mut title).desired_width(f32::INFINITY));
            if kind == CustomKind::Text {
                ui.label("内容");
                ui.add(
                    egui::TextEdit::multiline(&mut content)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6),
                );
            }
            ui.separator();
            ui.horizontal(|ui| {
                match kind {
                    // 文字预览：关闭 与 save 都是保存（用户要求）
                    CustomKind::Text => {
                        if ui.button("关闭").clicked() || ui.button("save").clicked() {
                            save = true;
                        }
                    }
                    // 图片预览：关闭 与 截图 都进入截图阶段
                    CustomKind::Image => {
                        if ui.button("关闭").clicked() || ui.button("截图").clicked() {
                            shoot = true;
                        }
                    }
                }
                if ui.button("cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if let Some(state) = self.custom_dialog.as_mut() {
            state.title = title;
            state.content = content;
        }

        if save {
            if let Some(dialog) = self.custom_dialog.take() {
                let body_size = DEFAULT_CUSTOM_TEXT_BODY;
                let id = self.allocate_preview_id(dialog.page);
                let preview = CustomPreview {
                    id,
                    page: dialog.page,
                    point: dialog.point,
                    rect: zero_rect_at(dialog.point),
                    kind: CustomKind::Text,
                    title: dialog.title.clone(),
                    text: Some(dialog.content.clone()),
                    snapshot: None,
                    body_size,
                    scale: 1.0,
                    // 文字 preview 也使用 image_id 作为稳定记录 id（链接用）。
                    image_id: new_image_id("preview"),
                    image_revision: 0,
                };
                let position = self.custom_previews.len();
                self.apply_edit(
                    AppEdit::AddPreview {
                        key: PreviewKey {
                            page: preview.page,
                            id,
                        },
                        position,
                        preview: std::sync::Arc::new(preview),
                    },
                    true,
                );
                // **不**自动生成浮窗（用户要求）：只在从侧栏悬浮/点击该条目时出现
                self.message = "已保存文字预览（从侧栏「自定义preview」唤出）".to_string();
            }
        } else if shoot {
            if let Some(dialog) = self.custom_dialog.take() {
                let index = self.custom_previews.len();
                let id = self.allocate_preview_id(dialog.page);
                let preview = CustomPreview {
                    id,
                    page: dialog.page,
                    point: dialog.point,
                    rect: zero_rect_at(dialog.point),
                    kind: CustomKind::Image,
                    title: dialog.title,
                    text: None,
                    snapshot: None,
                    // 占位：截图完成时按"选区在当时缩放下的显示尺寸"覆盖
                    body_size: DEFAULT_CUSTOM_TEXT_BODY,
                    // 占位：截图完成时写入当时的显示比例
                    scale: 1.0,
                    image_id: new_image_id("custom"),
                    image_revision: 0,
                };
                self.apply_edit(
                    AppEdit::AddPreview {
                        key: PreviewKey {
                            page: preview.page,
                            id,
                        },
                        position: index,
                        preview: std::sync::Arc::new(preview),
                    },
                    true,
                );
                // 进入截图模式：结果回写到这条自定义记录
                self.capture = Some(CaptureState {
                    rect: None,
                    anchor: None,
                    target: CaptureTarget::Custom(index),
                });
                self.message = "截图：拖动框选区域，Enter 保存，Esc 取消".to_string();
            }
        } else if cancel {
            self.custom_dialog = None;
            self.message = "已取消自定义preview".to_string();
        }
    }

    /// token 失效且属性不足以完全验证的外部注释链接：让用户确认。
    pub(super) fn draw_external_link_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_external_link else {
            return;
        };
        let Some(annotation) = self.annotations.get(pending.id) else {
            self.pending_external_link = None;
            return;
        };
        let page = annotation.page;
        let kind = annotation_kind_code(&annotation.kind);
        let mut open = false;
        let mut cancel = false;
        egui::Modal::new(Id::unique("external-annotation-link")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("外部注释链接");
            ui.label("无法完全验证目标（外部注释 id 会随加载顺序变化）。");
            ui.label(format!(
                "目标：第 {} 页 · {} · #{}",
                page.display_number(),
                kind,
                pending.id.get()
            ));
            ui.label("页号与类型匹配；缺少创建时间或创建时间不可用，仍可能指向别的注释。");
            ui.horizontal(|ui| {
                if ui.button("打开").clicked() {
                    open = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
            });
        });
        if open {
            self.pending_external_link = None;
            let at = self.next_float_position();
            self.open_annotation_note(pending.id, at);
            self.message = format!("已按用户确认跳转到外部注释 #{}", pending.id.get());
        } else if cancel {
            self.pending_external_link = None;
            self.message = "已取消打开外部注释链接".to_string();
        }
    }

    /// 加密文档的密码输入框（用模态窗口阻断背后交互）。
    pub(super) fn draw_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(path) = self.password_for.clone() else {
            return;
        };
        egui::Modal::new(Id::unique("pdf-password")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("文档已加密");
            ui.label(format!("{}", path.display()));
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.password_input)
                    .password(true)
                    .hint_text("密码"),
            );
            let submitted =
                response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                if ui.button("打开").clicked() || submitted {
                    let password = self.password_input.clone();
                    self.open(&path, Some(&password));
                }
                if ui.button("取消").clicked() {
                    self.password_for = None;
                    self.message = "已取消打开加密文档".to_string();
                }
            });
        });
    }
}
