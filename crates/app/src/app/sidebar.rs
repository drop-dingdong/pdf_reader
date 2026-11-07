//! `app::sidebar`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{annotation_time_line, overflow_layout};
use eframe::egui;
use egui::{Align2, CornerRadius, Id, Key, RichText, Sense, Vec2 as EVec2};
use pdfcore::annotation::AnnotationKind;

impl PdfReaderApp {
    /// 搜索框（Ctrl+F 呼出）：关键词、上一个/下一个、命中计数。
    ///
    /// 它在**中央区域**里作为底部面板创建，因此宽度与画布一致 —— 不会横跨到侧栏下方。
    pub(super) fn draw_search_bar_in_center(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
    ) {
        if !self.search_open {
            return;
        }
        egui::Panel::bottom("search-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("搜索：");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_input)
                        .desired_width(180.0)
                        .hint_text("关键词"),
                );
                if self.focus_search {
                    response.request_focus();
                    self.focus_search = false;
                }
                let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (submitted || ui.button("查找").clicked()) && !self.search_input.is_empty() {
                    match session.search(&SearchQuery::new(self.search_input.clone())) {
                        Ok(hits) => {
                            self.message = if hits.is_empty() {
                                format!(
                                    "未找到“{}”（若 PDF 是扫描件则没有文本层，无法搜索）",
                                    self.search_input
                                )
                            } else {
                                format!("找到 {} 处", hits.len())
                            };
                            self.hits = hits;
                            self.error = None;
                            if self.hits.is_empty() {
                                self.active_hit = None;
                            } else {
                                self.goto_hit(0);
                            }
                        }
                        Err(error) => self.error = Some(format!("搜索失败：{error}")),
                    }
                }
                if !self.hits.is_empty() {
                    if ui.button("上一个").clicked() {
                        let index = match self.active_hit {
                            Some(0) | None => self.hits.len() - 1,
                            Some(index) => index - 1,
                        };
                        self.goto_hit(index);
                    }
                    if ui.button("下一个").clicked() {
                        let index =
                            self.active_hit.map(|index| index + 1).unwrap_or(0) % self.hits.len();
                        self.goto_hit(index);
                    }
                    ui.label(format!(
                        "{} / {}",
                        self.active_hit.map(|index| index + 1).unwrap_or(0),
                        self.hits.len()
                    ));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("关闭").clicked() {
                        self.search_open = false;
                    }
                });
            });
        });
    }

    pub(super) fn draw_sidebar(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        if !self.show_outline {
            return;
        }
        let navigation = self.navigation.clone();
        let width = self
            .sidebar_width
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        // 尺寸由我们自己控制（`exact_size`），可拖拽的分隔条在画布左侧 —— 见 `draw_canvas`。
        //
        // 不用 egui 内建拖拽的原因：面板状态以 `parent_ui.scope_id().with_salt(id)` 为键持久化，
        // 而 eframe 每帧传入的是**新的根 `Ui`**，于是拖拽结果写进去、下一帧读不回来 ——
        // 第三、四轮验收两次反馈"空有动作没有结果"，根因就在这里。
        egui::Panel::left("outline")
            .resizable(false)
            .exact_size(width)
            .show(ui, |ui| {
                // 两个标签页：目录 / 自定义preview（Pin）。同一时刻只显示一个。
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.sidebar_tab == SidebarPane::Outline, "目录")
                        .clicked()
                    {
                        self.sidebar_tab = SidebarPane::Outline;
                    }
                    if ui
                        .selectable_label(self.sidebar_tab == SidebarPane::Pins, "自定义preview")
                        .clicked()
                    {
                        self.sidebar_tab = SidebarPane::Pins;
                    }
                    if ui
                        .selectable_label(self.sidebar_tab == SidebarPane::Annotations, "注释")
                        .clicked()
                    {
                        self.sidebar_tab = SidebarPane::Annotations;
                    }
                });
                ui.separator();
                if self.sidebar_tab == SidebarPane::Pins {
                    self.draw_pin_list(ui, session);
                    return;
                }
                if self.sidebar_tab == SidebarPane::Annotations {
                    self.draw_annotation_list(ui, session);
                    return;
                }
                let Some(navigation) = navigation else {
                    ui.label("（无目录）");
                    return;
                };
                if navigation.outline.is_empty() {
                    ui.label("（本文档没有书签）");
                    return;
                }
                // **虚拟化行**：只布局可见的那十几行。
                // 实测 1292 个节点时，每帧全量布局要花约 42ms（与缩放无关，纯 UI 开销）。
                let rows = &self.outline_rows;
                let row_height = OUTLINE_ROW_HEIGHT;
                egui::ScrollArea::vertical().show_rows(ui, row_height, rows.len(), |ui, range| {
                    for index in range {
                        let Some((depth, title, destination)) = rows.get(index) else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            ui.add_space(*depth as f32 * 12.0);
                            let clickable = destination.is_some();
                            let label = if clickable {
                                RichText::new(title).strong()
                            } else {
                                RichText::new(title)
                            };
                            if ui
                                .add_enabled(clickable, egui::Button::new(label).frame(false))
                                .clicked()
                            {
                                if let Some(Destination::Page { page, .. }) = destination {
                                    if page.get() < session.page_count() {
                                        self.commands.push(ViewerCommand::GoToPage(*page));
                                    }
                                }
                            }
                        });
                    }
                });
            });
    }

    /// 侧栏「注释」标签页：按页分组、类型/正文摘要、筛选与定位。
    pub(super) fn draw_annotation_list(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
    ) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("annotation-list-view")
                .selected_text(self.annotation_view.label())
                .width(88.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.annotation_view,
                        AnnotationListView::ByPage,
                        "按页",
                    );
                    ui.selectable_value(
                        &mut self.annotation_view,
                        AnnotationListView::Recent,
                        "最近修改",
                    );
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.annotation_search)
                    .hint_text("搜索正文/作者")
                    .desired_width(120.0),
            );
            if !self.annotation_search.is_empty()
                && ui.small_button("×").on_hover_text("清空搜索").clicked()
            {
                self.annotation_search.clear();
            }
            ui.menu_button("筛选", |ui| {
                ui.checkbox(&mut self.annotation_show_highlight, "高亮");
                ui.checkbox(&mut self.annotation_show_underline, "下划线");
                ui.checkbox(&mut self.annotation_show_strike, "删除线");
                ui.checkbox(&mut self.annotation_show_ink, "墨迹");
                ui.checkbox(&mut self.annotation_show_note, "便签");
                ui.separator();
                ui.checkbox(&mut self.annotation_only_external, "仅外部");
                ui.checkbox(&mut self.annotation_only_readonly, "仅只读");
            });
        });
        ui.separator();

        let query = self.annotation_search.trim().to_lowercase();
        let mut rows: Vec<AnnotationRow> = Vec::new();
        for annotation in self.annotations.iter() {
            let (label, visible) = annotation_kind_label(&annotation.kind);
            if !visible {
                continue;
            }
            let enabled = match &annotation.kind {
                AnnotationKind::Highlight { .. } => self.annotation_show_highlight,
                AnnotationKind::Underline { .. } => self.annotation_show_underline,
                AnnotationKind::StrikeOut { .. } => self.annotation_show_strike,
                AnnotationKind::Ink { .. } => self.annotation_show_ink,
                AnnotationKind::Note { .. } => self.annotation_show_note,
            };
            if !enabled {
                continue;
            }
            let external = self.annotation_pdf.external_ids.contains(&annotation.id);
            if self.annotation_only_external && !external {
                continue;
            }
            let readonly = external && annotation.pdf_ref.is_none();
            if self.annotation_only_readonly && !readonly {
                continue;
            }
            if !query.is_empty() && !annotation_matches_query(annotation, &query) {
                continue;
            }
            let summary = body_summary(annotation.body.as_deref());
            let preview = body_tooltip(annotation.body.as_deref());
            rows.push(AnnotationRow {
                page: annotation.page,
                id: annotation.id,
                label,
                readonly,
                external,
                summary,
                preview,
                author: annotation.author.clone(),
                created: annotation.created,
                modified: annotation.modified,
                bounds: annotation.bounds(),
            });
        }
        if rows.is_empty() {
            ui.label(if self.annotations.is_empty() {
                "（本文档还没有注释）"
            } else {
                "（没有匹配的注释）"
            });
            return;
        }
        match self.annotation_view {
            AnnotationListView::ByPage => rows.sort_by(by_page_order),
            AnnotationListView::Recent => rows.sort_by(recent_order),
        }
        let recent = self.annotation_view == AnnotationListView::Recent;
        let total = rows.len();
        let mut counts: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
        for row in &rows {
            *counts.entry(row.page.get()).or_default() += 1;
        }
        ui.label(if recent {
            format!("{total} 条 · 按修改时间")
        } else {
            format!("{total} 条")
        });
        let mut last_page: Option<PageIndex> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in rows {
                    let AnnotationRow {
                        page,
                        id,
                        label,
                        readonly,
                        external,
                        summary,
                        preview,
                        author,
                        created,
                        modified,
                        ..
                    } = row;
                    if !recent && last_page != Some(page) {
                        if last_page.is_some() {
                            ui.add_space(6.0);
                        }
                        let collapsed = self.collapsed_annotation_pages.contains(&page.get());
                        let count = counts.get(&page.get()).copied().unwrap_or(0);
                        let arrow = if collapsed { "▸" } else { "▾" };
                        let text = RichText::new(format!(
                            "{arrow} 第 {} 页（{count} 条）",
                            page.display_number()
                        ))
                        .strong()
                        .color(ui.visuals().weak_text_color());
                        if ui.selectable_label(false, text).clicked() {
                            if collapsed {
                                self.collapsed_annotation_pages.remove(&page.get());
                            } else {
                                self.collapsed_annotation_pages.insert(page.get());
                            }
                        }
                        last_page = Some(page);
                    }
                    if !recent && self.collapsed_annotation_pages.contains(&page.get()) {
                        continue;
                    }
                    let mut text = if recent {
                        format!("第 {} 页 · [{label}] {summary}", page.display_number())
                    } else {
                        format!("[{label}] {summary}")
                    };
                    if readonly {
                        text.push_str("  [内联]");
                    }
                    // 与 preview 行一致：先按整行宽度占位，再在行内做交互；
                    // 不再嵌套 `ui.horizontal`，否则 available_width 会随文字宽度变化，
                    // 右侧 ⋯ 每行越挪越靠右。
                    let width = ui.available_width();
                    let (row_rect, _) = ui
                        .allocate_exact_size(EVec2::new(width, OUTLINE_ROW_HEIGHT), Sense::hover());
                    let rect =
                        egui::Rect::from_min_max(row_rect.min + EVec2::new(8.0, 0.0), row_rect.max);
                    let response = ui.interact(
                        rect,
                        Id::unique(("annotation-row", id.get())),
                        Sense::click(),
                    );
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::ZERO,
                            ui.visuals().widgets.hovered.weak_bg_fill,
                        );
                    }
                    let menu_width = 24.0;
                    let text_width = (rect.width() - menu_width - 8.0).max(24.0);
                    let job = overflow_layout(
                        &text,
                        egui::FontId::proportional(13.0),
                        ui.visuals().strong_text_color(),
                        text_width,
                        1,
                    );
                    let galley = ui.painter().layout_job(job);
                    ui.painter().galley(
                        egui::pos2(rect.min.x, rect.center().y - galley.size().y * 0.5),
                        galley,
                        ui.visuals().strong_text_color(),
                    );
                    let menu_rect = egui::Rect::from_center_size(
                        egui::pos2(rect.max.x - 10.0, rect.center().y),
                        EVec2::new(menu_width, OUTLINE_ROW_HEIGHT),
                    );
                    let menu_response = ui.put(menu_rect, egui::Button::new("").frame(false));
                    let color = if menu_response.hovered() {
                        ui.visuals().strong_text_color()
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    self.paint_icon(ui, icons::Icon::More, menu_rect.center(), 16.0, color);
                    let menu_response = menu_response.on_hover_text("更多操作");
                    egui::Popup::menu(&menu_response).show(|ui| {
                        self.annotation_row_menu(ui, id);
                    });

                    let response = response.on_hover_text({
                        // 列表已按页分组，"第 N 页"冗余：这里换成作者；时间单独一行。
                        let mut text = format!(
                            "{preview}\n\n{label} · {}{}{}",
                            author.as_deref().unwrap_or("未署名"),
                            if external { " · 外部" } else { " · 自有" },
                            if readonly { " · 内联" } else { "" }
                        );
                        if let Some(line) = annotation_time_line(created, modified) {
                            text.push('\n');
                            text.push_str(&line);
                        }
                        text
                    });
                    response.context_menu(|ui| {
                        self.annotation_row_menu(ui, id);
                    });
                    if response.clicked() {
                        self.focus_annotation(id, session);
                    }
                }
            });
    }

    /// 注释行的 ⋯ / 右键菜单。
    fn annotation_row_menu(&mut self, ui: &mut egui::Ui, id: AnnotationId) {
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
            ui.close();
        }
        if ui.button("跳到位置").clicked() {
            self.jump_to_annotation(id);
            ui.close();
        }
        if ui.button("删除注释（可撤销）").clicked() {
            self.delete_annotation(id);
            ui.close();
        }
    }

    /// preview 行的 ⋯ / 右键菜单。
    fn preview_row_menu(&mut self, ui: &mut egui::Ui, index: usize) {
        let Some(preview) = self.custom_previews.get(index) else {
            return;
        };
        let page = preview.page;
        let point = preview.point;
        let kind = preview.kind;
        let image_ref = (kind == CustomKind::Image && !preview.image_id.is_empty())
            .then(|| (preview.image_id.clone(), preview.image_revision));
        let title = preview.title.clone();
        if let Some(uri) = self.preview_link_uri(index) {
            if ui.button("复制引用链接").clicked() {
                ui.ctx().copy_text(uri);
                self.message = "已复制自定义preview引用链接，可粘到正文".to_string();
                ui.close();
            }
        }
        if ui.button("跳到位置").clicked() {
            self.commands.push(ViewerCommand::GoToPage(page));
            self.commands
                .push(ViewerCommand::CenterOnPagePoint { page, point });
            self.message = format!("已跳到第 {} 页", page.display_number());
            ui.close();
        }
        if ui.button("编辑标题").clicked() {
            self.editing_title = Some((index, title));
            ui.close();
        }
        if let Some((record_id, revision)) = image_ref {
            if ui.button("复制图片引用").clicked() {
                ui.ctx()
                    .copy_text(crate::image_store::image_uri(&record_id, revision));
                self.message = "已复制图片引用，可粘到正文".to_string();
                ui.close();
            }
        }
        if ui.button("删除条目（可撤销）").clicked() {
            self.delete_custom_preview(index);
            ui.close();
        }
    }

    /// 侧栏的"自定义preview（Pin）"列表：
    /// 标题左对齐、页码右对齐、过长标题截断；双击跳转；悬浮显示临时浮窗；
    /// 单击生成/唤出持久浮窗（已有则高亮并置顶，一次性高亮有时限）。
    pub(super) fn draw_pin_list(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        if self.custom_previews.is_empty() {
            ui.label("（还没有自定义preview）");
            return;
        }
        let row_height = OUTLINE_ROW_HEIGHT;
        let mut hovered: Option<(usize, egui::Rect)> = None;
        let count = self.custom_previews.len();
        egui::ScrollArea::vertical().show_rows(ui, row_height, count, |ui, range| {
            for index in range {
                let Some(preview) = self.custom_previews.get(index) else {
                    continue;
                };
                // 行首：类型符号 + 标题（长标题截断）+ 右侧页码
                let kind_icon = match preview.kind {
                    CustomKind::Text => icons::Icon::TextPreview,
                    CustomKind::Image => icons::Icon::ImagePreview,
                };
                let title = if preview.title.is_empty() {
                    "（无题）".to_string()
                } else {
                    preview.title.clone()
                };
                let page_label = format!("第 {} 页", preview.page.display_number());

                // 1) 先占位（整行宽度），2) 再画，3) 最后在该矩形上注册交互。
                // 顺序很重要：交互最后注册才会盖在内容之上、稳定收到点击。
                let width = ui.available_width();
                let (rect, _) =
                    ui.allocate_exact_size(EVec2::new(width, row_height), Sense::hover());
                let response = ui.interact(rect, Id::unique(("pin-row", index)), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::ZERO,
                        ui.visuals().widgets.hovered.weak_bg_fill,
                    );
                    // 悬浮响应可整体关闭（用户要求）：关掉后既不出临时预览、也不点亮旧浮窗
                    if self.pin_hover_enabled {
                        // 该条目已有正式浮窗 → **只**高亮并置顶那个浮窗，不再显示临时预览；
                        // 没有正式浮窗时才显示临时预览浮窗。
                        if self.highlight_existing_custom_float(index).is_none() {
                            // 缺图（例如刚从存档恢复）→ 现在按当前缩放合成；未就绪则下帧自动重试
                            self.ensure_custom_snapshot(ui, session, index);
                            hovered = Some((index, rect));
                        }
                    }
                }
                let painter = ui.painter();
                let menu_width = 20.0;
                let page_width = 56.0;
                let icon_width = 22.0;
                let title_width =
                    (rect.width() - page_width - menu_width - icon_width - 8.0).max(24.0);
                self.paint_icon(
                    ui,
                    kind_icon,
                    egui::pos2(rect.min.x + 10.0, rect.center().y),
                    14.0,
                    ui.visuals().text_color(),
                );
                let job = overflow_layout(
                    &title,
                    egui::FontId::proportional(13.0),
                    ui.visuals().strong_text_color(),
                    title_width,
                    1,
                );
                let galley = painter.layout_job(job);
                painter.galley(
                    egui::pos2(
                        rect.min.x + icon_width,
                        rect.center().y - galley.size().y * 0.5,
                    ),
                    galley,
                    ui.visuals().strong_text_color(),
                );
                painter.text(
                    egui::pos2(rect.max.x - menu_width - 4.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    &page_label,
                    egui::FontId::proportional(11.0),
                    ui.visuals().weak_text_color(),
                );

                if response.clicked() {
                    // 单击立即生成/唤出浮窗；跳转/编辑/删除/复制链接都收进 ⋯ 菜单。
                    self.ensure_custom_snapshot(ui, session, index);
                    self.activate_custom_preview(index, rect);
                }

                let menu_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.max.x - 10.0, rect.center().y),
                    EVec2::new(menu_width, row_height),
                );
                let menu_response = ui.put(menu_rect, egui::Button::new("").frame(false));
                let color = if menu_response.hovered() {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().weak_text_color()
                };
                self.paint_icon(ui, icons::Icon::More, menu_rect.center(), 16.0, color);
                let menu_response = menu_response.on_hover_text("更多操作");
                egui::Popup::menu(&menu_response).show(|ui| {
                    self.preview_row_menu(ui, index);
                });
                response.context_menu(|ui| {
                    self.preview_row_menu(ui, index);
                });
            }
        });
        // 悬浮条目 → 只显示临时浮窗（离开即关）；同时记录行矩形用于定位
        self.custom_hover = hovered;
    }
}
