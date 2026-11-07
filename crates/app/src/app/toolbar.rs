//! `app::toolbar`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    checkbox_row, fit_height_zoom, page_index_from_text, parse_zoom_percent, zoom_percent_text,
    CUSTOM_SYMBOL_IMAGE, CUSTOM_SYMBOL_TEXT,
};
use eframe::egui;
use egui::{Id, Key, RichText};

impl PdfReaderApp {
    pub(super) fn draw_toolbar(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        if self.fullscreen {
            // 全屏：顶栏**不占布局空间**，只在指针靠近顶端时悬浮出现（遮住下方内容）
            if self.reveal_toolbar {
                egui::Area::new(Id::unique("toolbar-overlay"))
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        // Area 内 `available_rect_before_wrap()` 宽度接近 0（Area 按内容收缩），
                        // 会导致右侧内容被挤掉 —— 必须用窗口视口宽度。
                        let width = ui
                            .ctx()
                            .input(|input| {
                                input
                                    .viewport()
                                    .inner_rect
                                    .map(|rect| rect.width())
                                    .unwrap_or(1200.0)
                            })
                            .max(320.0);
                        egui::Frame::side_top_panel(&ui.style().clone()).show(ui, |ui| {
                            ui.set_width(width);
                            self.toolbar_rows(ui, session);
                        });
                    });
            }
            return;
        }
        egui::Panel::top("toolbar").show(ui, |ui| {
            self.toolbar_rows(ui, session);
        });
    }

    /// 切换全屏：窗口最大化 + 去装饰，并让顶栏改为悬浮显隐。
    pub(super) fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        self.reveal_toolbar = self.fullscreen;
        let on = self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!on));
    }

    /// 顶栏的三行内容（正常模式与全屏悬浮共用）。
    pub(super) fn toolbar_rows(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        self.toolbar_row_file(ui);
        self.toolbar_row_view(ui);
        self.toolbar_row_nav(ui, session);
    }
    /// 底部状态栏：页码与缩放已移到顶栏（用户要求），这里只留消息、选中字数与诊断。
    pub(super) fn draw_status(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.separator();
                ui.separator();
                ui.label(format!("纹理 {} 个", self.textures.len()));
                if let Some(kind) = self.custom_pin {
                    ui.separator();
                    ui.colored_label(
                        Color32::from_rgb(230, 180, 60),
                        match kind {
                            CustomKind::Text => "当前自定义preview：文字（点击页面确定位置）",
                            CustomKind::Image => "当前自定义preview：图片（点击页面确定位置）",
                        },
                    );
                }
                if !self.message.is_empty() {
                    ui.separator();
                    // 状态栏最多一行：长文本用省略号，不能让消息把窗口撑高。
                    ui.add(egui::Label::new(&self.message).truncate());
                }
                if let Some(error) = &self.error {
                    ui.separator();
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error).color(Color32::from_rgb(200, 60, 60)),
                        )
                        .truncate(),
                    );
                }
            });
        });
    }
}

impl PdfReaderApp {
    pub(super) fn toolbar_row_file(&mut self, ui: &mut egui::Ui) {
        // ---- 第 0 行：文件（路径 + 保存自定义数据）----
        ui.horizontal(|ui| {
            let path_text = self
                .document_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "（未打开文件）".to_string());
            let response = ui.add(
                egui::Label::new(RichText::new(truncate_middle(&path_text, 70)).small()).truncate(),
            );
            response.on_hover_text(&path_text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                // 文件操作收进一个菜单：少了 5 个按钮，窄窗口不再挤。
                let can_write = self.document_path.is_some() && self.is_dirty();
                let can_compact = self.document_path.is_some();
                let marker = if self.pdf_dirty || self.data_dirty {
                    " *"
                } else {
                    ""
                };
                ui.menu_button(format!("文件{marker}"), |ui| {
                    let save = ui.add_enabled(can_write, egui::Button::new("保存"));
                    let save = if can_write {
                        save.on_hover_text(
                            "把注释写进原 PDF、自定义数据写进 .json（原子替换；Ctrl+S）",
                        )
                    } else {
                        save.on_disabled_hover_text("没有需要保存的改动")
                    };
                    if save.clicked() {
                        self.save_flow.pending_in_place = true;
                        self.save_flow.in_place_error = None;
                        ui.close();
                    }
                    let save_as =
                        ui.add_enabled(self.document_path.is_some(), egui::Button::new("另存为…"));
                    let save_as = if self.document_path.is_some() {
                        save_as.on_hover_text("另存为新的 PDF（可带注释），当前文件保持不变")
                    } else {
                        save_as.on_disabled_hover_text("需要先打开一个 PDF")
                    };
                    if save_as.clicked() {
                        self.open_save_as_dialog();
                        ui.close();
                    }
                    let can_summary = !self.annotations.is_empty();
                    let summary = ui.add_enabled(can_summary, egui::Button::new("导出注释摘要…"));
                    let summary = if can_summary {
                        summary.on_hover_text(
                            "导出 Markdown 摘要：同目录 <原名>.annotations.md（不覆盖）",
                        )
                    } else {
                        summary.on_disabled_hover_text("当前没有注释")
                    };
                    if summary.clicked() {
                        self.export_annotation_summary();
                        ui.close();
                    }
                    let compact = ui.add_enabled(can_compact, egui::Button::new("整理副本…"));
                    let compact = if can_compact {
                        compact.on_hover_text(
                            "去掉历史修订，生成更小的单段 PDF；输出为新文件，注释与数据文件都不动",
                        )
                    } else {
                        compact.on_disabled_hover_text("需要先打开一个 PDF")
                    };
                    if compact.clicked() {
                        self.open_compact_dialog();
                        ui.close();
                    }
                    ui.separator();
                    ui.checkbox(
                        &mut self.backup_before_inplace,
                        "原地保存前保留一份 .bak 备份",
                    )
                    .on_hover_text("默认关闭；关闭时只做临时文件 + 原子替换");
                    ui.separator();
                    ui.menu_button("最近打开", |ui| {
                        let documents = self.settings.documents.clone();
                        if documents.is_empty() {
                            ui.label("（还没有最近文件）");
                        }
                        for document in documents.iter().take(8) {
                            let name = document
                                .path
                                .file_name()
                                .map(|name| name.to_string_lossy().to_string())
                                .unwrap_or_else(|| document.path.display().to_string());
                            let label = format!("{name}  ·  第 {} 页", document.page + 1);
                            let exists = document.path.exists();
                            let item = ui.add_enabled(exists, egui::Button::new(label));
                            let item = if exists {
                                item.on_hover_text(document.path.display().to_string())
                            } else {
                                item.on_disabled_hover_text(format!(
                                    "文件不存在：{}",
                                    document.path.display()
                                ))
                            };
                            if item.clicked() {
                                // 这里还在绘制中（`session` 被借出去了），不能直接 open。
                                self.request_open(document.path.clone());
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
    }

    pub(super) fn toolbar_row_view(&mut self, ui: &mut egui::Ui) {
        // ---- 第 1 行：侧栏 / 自定义 preview / Pin 悬浮响应（全部左对齐）----
        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.show_outline, "侧栏")
                .on_hover_text("显示/隐藏侧边栏（目录 / 自定义preview）");
            ui.separator();
            ui.label("添加预览：");
            if ui
                .button(CUSTOM_SYMBOL_TEXT)
                .on_hover_text("添加文字预览")
                .clicked()
            {
                self.enter_pin(CustomKind::Text);
            }
            if ui
                .button(CUSTOM_SYMBOL_IMAGE)
                .on_hover_text("添加图片预览")
                .clicked()
            {
                self.enter_pin(CustomKind::Image);
            }
            ui.separator();
            ui.checkbox(&mut self.pin_hover_enabled, "Pin悬浮响应");
            ui.checkbox(&mut self.horizontal_scroll, "横向滚动")
                .on_hover_text("勾选后纵向滚轮改为横向滚动（拖动不受影响）");
            ui.separator();
            if ui
                .checkbox(&mut self.markdown_floats, "Markdown")
                .on_hover_text("文字浮窗按 Markdown 渲染；关闭则显示原文")
                .changed()
            {
                // 顶栏开关立即写回设置（与样式菜单同一套脏位）。
                self.settings.ui.markdown_floats = self.markdown_floats;
                self.settings_dirty = true;
            }
            if ui
                .checkbox(&mut self.hover_preview_markdown, "hover MD")
                .on_hover_text(
                    "侧栏 hover 临时预览也按 Markdown 渲染。\n\
                         会触发公式/图片 owner 渲染，可能造成 hover 卡顿；\n\
                         临时预览中的链接/滚动交互也可能与“点击固定”竞争。",
                )
                .changed()
            {
                self.settings.ui.hover_preview_markdown = self.hover_preview_markdown;
                self.settings_dirty = true;
            }
            if ui
                .small_button("?")
                .on_hover_text("Markdown 与数学公式支持/限制")
                .clicked()
            {
                self.math_help = true;
            }
        });
    }

    pub(super) fn toolbar_row_nav(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        // ---- 第 2 行：页 / 缩放 / 旋转 / 预览模式（左） + 引用显示样式（右）----
        ui.horizontal(|ui| {
            // 全屏切换（现阶段纯文字）
            let fullscreen_label = if self.fullscreen {
                "退出全屏"
            } else {
                "全屏"
            };
            if ui
                .button(fullscreen_label)
                .on_hover_text("全屏/退出全屏（F11）")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.toggle_fullscreen(&ctx);
            }
            egui::ComboBox::from_id_salt("layout-direction")
                .selected_text(if self.horizontal_layout {
                    "横向"
                } else {
                    "纵向"
                })
                .show_ui(ui, |ui| {
                    // 无条件提交：即使标记已经是该值，也强制重排一次，
                    // 这样"状态错位"（UI 说横向、实际纵向）点一下就能纠正。
                    if ui
                        .selectable_label(!self.horizontal_layout, "纵向")
                        .clicked()
                    {
                        self.horizontal_layout = false;
                        self.pending_layout = Some(false);
                    }
                    if ui
                        .selectable_label(self.horizontal_layout, "横向")
                        .clicked()
                    {
                        self.horizontal_layout = true;
                        self.pending_layout = Some(true);
                    }
                });
            ui.separator();
            // 上一页 / 页码（双击原位编辑） / 下一页
            if ui.button("上一页").clicked() {
                self.commands.push(ViewerCommand::PrevPage);
            }
            let current = session.state().current_page;
            if let Some(draft) = self.page_edit.as_mut() {
                let response = ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(44.0)
                        .hint_text("页码"),
                );
                if self.focus_edit == Some(EditField::Page) {
                    // 双击后当帧就把焦点交给输入框 → 一步进入真编辑
                    response.request_focus();
                    self.focus_edit = None;
                }
                let commit = response.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter));
                if commit {
                    match page_index_from_text(draft, session.page_count()) {
                        Some(page) => {
                            self.commands.push(ViewerCommand::GoToPage(page));
                            self.error = None;
                            self.page_edit = None;
                        }
                        None => {
                            self.error = Some(format!("页码需在 1–{} 之间", session.page_count()));
                            self.page_edit = None;
                        }
                    }
                }
            } else {
                let label = format!("{} / {}", current.display_number(), session.page_count());
                if ui
                    .add(egui::Label::new(label).sense(egui::Sense::click()))
                    .on_hover_text("双击可直接输入页码")
                    .double_clicked()
                {
                    self.page_edit = Some(current.display_number().to_string());
                    self.focus_edit = Some(EditField::Page);
                }
            }
            if ui.button("下一页").clicked() {
                self.commands.push(ViewerCommand::NextPage);
            }

            ui.separator();

            // 缩放比（双击原位编辑）+ 缩放模式菜单
            let zoom = session.state().zoom;
            if let Some(draft) = self.zoom_edit.as_mut() {
                let response = ui.add(egui::TextEdit::singleline(draft).desired_width(52.0));
                if self.focus_edit == Some(EditField::Zoom) {
                    response.request_focus();
                    self.focus_edit = None;
                }
                let commit = response.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter));
                if commit {
                    match parse_zoom_percent(draft) {
                        Some(value) => {
                            self.commands.push(ViewerCommand::SetZoom(value));
                            self.zoom_mode = ZoomMode::Custom;
                            self.error = None;
                        }
                        None => self.error = Some("缩放比需为正数百分比".to_string()),
                    }
                    self.zoom_edit = None;
                }
            } else {
                if ui
                    .add(egui::Label::new(zoom_percent_text(zoom)).sense(egui::Sense::click()))
                    .on_hover_text("双击可直接输入缩放百分比")
                    .double_clicked()
                {
                    self.zoom_edit = Some(format!("{:.0}", zoom * 100.0));
                    self.focus_edit = Some(EditField::Zoom);
                }
            }
            egui::ComboBox::from_id_salt("zoom-mode")
                .selected_text(self.zoom_mode.label())
                .show_ui(ui, |ui| {
                    if ui.selectable_label(false, "适应宽度").clicked() {
                        self.commands.push(ViewerCommand::FitWidth);
                        self.zoom_mode = ZoomMode::FitWidth;
                    }
                    if ui.selectable_label(false, "适应高度").clicked() {
                        if let Some(placement) = session.placement(current) {
                            if let Some(fit) = fit_height_zoom(
                                session.viewport().size.height,
                                placement.rect.height,
                            ) {
                                self.commands.push(ViewerCommand::SetZoom(zoom * fit));
                            }
                        }
                        self.zoom_mode = ZoomMode::FitHeight;
                    }
                    if ui.selectable_label(false, "100%").clicked() {
                        self.commands.push(ViewerCommand::SetZoom(1.0));
                        self.zoom_mode = ZoomMode::Actual;
                    }
                });

            ui.separator();

            // 旋转：顺时针 / 逆时针
            if ui.button("\u{21bb}").on_hover_text("顺时针旋转").clicked() {
                self.commands.push(ViewerCommand::RotateClockwise);
            }
            if ui.button("\u{21ba}").on_hover_text("逆时针旋转").clicked() {
                self.commands.push(ViewerCommand::RotateCounterClockwise);
            }

            ui.separator();

            // 基础工具：平移 / 选字；墨迹按钮和文本标记放在一起
            for tool in [Tool::Pan, Tool::Select] {
                // 便签模式与阅读模式无关联：不高亮任何阅读模式。
                let active = self.tool == tool && !self.note_pin;
                let response = ui.selectable_label(active, tool.label()).on_hover_text(
                    if tool == Tool::Select {
                        "选字/复制；Ctrl/Alt+拖拽可移动墨迹（也可右键「移动」）"
                    } else {
                        "平移画面；Ctrl/Alt+拖拽可移动墨迹（也可右键「移动」）"
                    },
                );
                if response.clicked() {
                    self.switch_tool(tool);
                }
            }

            // 验收辅助入口（D-38 截图 / D-36 测试浮窗）：默认**不显示**，
            // 设 `PDF_READER_DEV_ENTRY=1` 才出现在顶栏。代码保留（见 D-114）：
            // 它们是手工验证"截图流程 / 浮窗拖动改大小层级"最快的入口。
            if dev_entries_enabled() {
                ui.separator();
                if ui
                    .button("截图")
                    .on_hover_text("框选区域 → Enter 保存为浮窗，Esc 取消")
                    .clicked()
                {
                    self.capture = Some(CaptureState {
                        rect: None,
                        anchor: None,
                        target: CaptureTarget::Plain,
                    });
                    self.message = "截图模式：拖出选区，Enter 保存，Esc 取消".to_string();
                }
                if ui
                    .button("测试浮窗")
                    .on_hover_text("添加一个纯文字浮窗（验收浮窗拖动/改大小/层级）")
                    .clicked()
                {
                    self.add_test_float();
                }
            }

            ui.separator();
            // 注释工具收进一个弹出菜单；样式是独立按钮（进入模式与改样式是两次操作）。
            // 不加外框，只用竖线分组。
            let tool_label = match self.annotation_tool {
                Some(tool) => format!("{} ▾", tool.label()),
                None if self.tool == Tool::Ink => "墨迹 ▾".to_string(),
                None => "注释工具 ▾".to_string(),
            };
            ui.menu_button(tool_label, |ui| {
                for tool in [
                    MarkupTool::Highlight,
                    MarkupTool::Underline,
                    MarkupTool::StrikeOut,
                ] {
                    let active = self.annotation_tool == Some(tool);
                    if ui
                        .selectable_label(active, tool.label())
                        .on_hover_text(format!(
                            "进入{}模式：拖选文字后施加；Esc 退出",
                            tool.label()
                        ))
                        .clicked()
                    {
                        self.toggle_markup_tool(tool);
                        ui.close();
                    }
                }
                let ink_active =
                    self.tool == Tool::Ink && self.annotation_tool.is_none() && !self.note_pin;
                if ui
                    .selectable_label(ink_active, "墨迹")
                    .on_hover_text("墨迹模式：拖拽画一笔；Esc 退出")
                    .clicked()
                {
                    self.enter_ink_mode();
                    ui.close();
                }
                if ui
                    .selectable_label(self.note_pin, "便签")
                    .on_hover_text("点击页面位置，输入正文后添加；Esc 取消")
                    .clicked()
                {
                    self.enter_note_mode();
                    ui.close();
                }
            });
            ui.menu_button("样式", |ui| {
                ui.label("标记颜色");
                ui.horizontal(|ui| {
                    for (name, rgb) in [
                        ("黄", [255u8, 214, 0]),
                        ("红", [235u8, 70, 70]),
                        ("绿", [70u8, 180, 90]),
                        ("蓝", [70u8, 130, 220]),
                    ] {
                        if color_swatch(ui, name, rgb, self.markup_color == rgb).clicked() {
                            self.markup_color = rgb;
                        }
                    }
                });
                ui.add(
                    egui::Slider::new(&mut self.markup_opacity, 0.1..=1.0)
                        .text("高亮透明度")
                        .fixed_decimals(2),
                );
                ui.separator();
                ui.label("墨迹颜色");
                ui.horizontal(|ui| {
                    for (name, rgb) in [
                        ("黑", [25u8, 25, 25]),
                        ("红", [200u8, 30, 30]),
                        ("绿", [30u8, 150, 70]),
                        ("蓝", [30u8, 90, 210]),
                    ] {
                        if color_swatch(ui, name, rgb, self.ink_color == rgb).clicked() {
                            self.ink_color = rgb;
                        }
                    }
                });
                ui.add(
                    egui::Slider::new(&mut self.ink_width, 0.5..=6.0)
                        .text("墨迹线宽")
                        .fixed_decimals(1),
                );
                ui.label("下划线/删除线固定不透明；线宽随缩放。");
            });
            self.sync_style_settings();
            ui.separator();

            // 引用显示样式（链接框 + 引用点 合并为一个菜单）
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("display-style")
                    .selected_text("引用显示样式")
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                self.show_links,
                                checkbox_row("链接框", self.show_links),
                            )
                            .clicked()
                        {
                            self.show_links = !self.show_links;
                        }
                        if ui
                            .selectable_label(
                                self.show_dest_points,
                                checkbox_row("引用点", self.show_dest_points),
                            )
                            .clicked()
                        {
                            self.show_dest_points = !self.show_dest_points;
                        }
                        if ui
                            .selectable_label(
                                self.show_annotations,
                                checkbox_row("标注", self.show_annotations),
                            )
                            .clicked()
                        {
                            self.show_annotations = !self.show_annotations;
                        }
                    });
            });
        });
    }
}
