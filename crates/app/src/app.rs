//! egui/eframe 外壳：把 [`DocumentSession`] 的"命令进、事件出"模型接到界面上。
//!
//! ## 本模块的定位
//! **只做胶水**：坐标换算、输入映射、纹理存储都在 [`crate::view`] 里（可单测）。
//! 剩下的部分需要真实窗口才能验证，按 agent.md 规则 2 **交由用户验收**。
//!
//! ## 每帧流程（与 architecture.md §4.2 的契约一致）
//! 1. 采集输入 → 命令；2. `handle` 命令；3. `pump` 取回渲染结果；
//! 4. 画页面（就绪则贴纹理，未就绪则白底页框）；5. 画高亮；6. 请求可见页渲染。
//!
//! ## 与 egui 0.37 的适配
//! 0.37 的 `eframe::App` 只提供 `fn ui(&mut self, ui: &mut Ui, frame)`（不再给 `Context`），
//! 侧边/上下栏统一为 [`egui::Panel`]，因此本模块从 `ui.ctx()` 取 `Context` 做纹理与剪贴板操作。

use std::path::PathBuf;
use std::sync::Arc;

use backend_pdfium::PdfiumBackend;
use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, Id, Key, Modifiers, RichText, Sense, Stroke, StrokeKind,
    Vec2 as EVec2,
};
use pdfcore::prelude::*;

use crate::view::{command_for_key, command_for_scroll, view_rect_for, view_to_content, KeyedStore};

/// 纹理缓存容量（条目数）。真正的淘汰依据是"是否可见"，容量只是兜底。
const TEXTURE_CAPACITY: usize = 64;

/// 拖拽中：区分"平移画面"与"选择文字"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    /// 拖动画面（起点没有命中文字）。
    Pan,
    /// 拖选文字（起点命中了字符）。
    Select,
}

/// 应用状态。
pub struct PdfReaderApp {
    session: Option<DocumentSession>,
    navigation: Option<Arc<NavigationIndex>>,
    textures: KeyedStore<egui::TextureHandle>,
    /// 每帧临时持有的命令与事件缓冲（避免反复分配）。
    commands: Vec<ViewerCommand>,
    events: Vec<SessionEvent>,
    visible: Vec<PageIndex>,
    drag: Option<DragMode>,
    /// 待输入的密码（文档加密时）。
    password_for: Option<PathBuf>,
    password_input: String,
    search_input: String,
    hits: Vec<SearchHit>,
    active_hit: Option<usize>,
    show_outline: bool,
    /// 状态栏文本与错误提示。
    message: String,
    error: Option<String>,
}

impl PdfReaderApp {
    /// 构造；`initial` 为命令行传入的文件路径。
    pub fn new(initial: Option<PathBuf>) -> Self {
        let mut app = Self {
            session: None,
            navigation: None,
            textures: KeyedStore::new(TEXTURE_CAPACITY),
            commands: Vec::new(),
            events: Vec::new(),
            visible: Vec::new(),
            drag: None,
            password_for: None,
            password_input: String::new(),
            search_input: String::new(),
            hits: Vec::new(),
            active_hit: None,
            show_outline: true,
            message: String::new(),
            error: None,
        };
        if let Some(path) = initial {
            app.open(&path, None);
        }
        app
    }

    /// 打开文档（装配会话：LRU 缓存 + 后台调度器 + 纵向布局 + 选区模型）。
    fn open(&mut self, path: &std::path::Path, password: Option<&str>) {
        let backend = PdfiumBackend::new();
        match backend.open(Source::Path(path), password) {
            Ok(opened) => {
                let raster = opened.raster.clone();
                let concurrency = opened.capabilities.max_concurrency as usize;
                let cache = Arc::new(LruCache::with_default_capacity());
                let scheduler = Arc::new(WorkerScheduler::new(raster, concurrency));

                match DocumentSession::new(
                    opened,
                    cache,
                    scheduler,
                    Box::new(VerticalLayout),
                    Box::new(RangeSelection::new()),
                ) {
                    Ok(session) => {
                        self.navigation = session.navigation().ok();
                        let title = session.opened().document.info().to_string();
                        self.session = Some(session);
                        self.textures.clear();
                        self.error = None;
                        self.message = format!("已打开：{title}");
                        self.password_for = None;
                        self.password_input.clear();
                        self.hits.clear();
                        self.active_hit = None;
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            // 加密文档：不是故障，弹密码框后重试
            Err(CoreError::PasswordRequired) => {
                self.password_for = Some(path.to_path_buf());
                self.password_input.clear();
            }
            Err(CoreError::IncorrectPassword) => {
                self.password_for = Some(path.to_path_buf());
                self.error = Some("密码错误，请重试".to_string());
            }
            Err(error) => self.error = Some(format!("{}：{error}", path.display())),
        }
    }

    /// 采集输入 → 命令。
    fn collect_commands(&mut self, ctx: &egui::Context, session: &DocumentSession) {
        let page_count = session.page_count();

        // 键盘
        let pressed: Vec<(Key, Modifiers)> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
                .collect()
        });
        for (key, modifiers) in pressed {
            if let Some(command) = command_for_key(key, modifiers, page_count) {
                self.commands.push(command);
            }
        }

        // 滚轮（Ctrl/Cmd 时缩放）。egui 0.37 的 `smooth_scroll_delta` 语义是"内容应移动的方向"，
        // 与 `Viewport::scroll`（视口在内容中的位置）方向相反，因此映射里取负号。
        let (scroll, modifiers) = ctx.input(|input| (input.smooth_scroll_delta, input.modifiers));
        if let Some(command) =
            command_for_scroll(scroll, modifiers.ctrl || modifiers.command, session.state().zoom)
        {
            self.commands.push(command);
        }

        // 拖入文件
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .first()
                .map(|file| file.path().to_path_buf())
        });
        if let Some(path) = dropped {
            self.open(&path, None);
        }
    }

    /// 把命令交给会话，并把事件翻译成界面动作。
    fn apply_and_pump(&mut self, ctx: &egui::Context, session: &mut DocumentSession) {
        for command in std::mem::take(&mut self.commands) {
            session.handle(command, &mut self.events);
        }
        session.pump(&mut self.events);

        let current = session.state().current_page;
        for event in std::mem::take(&mut self.events) {
            match event {
                SessionEvent::CopyToClipboard(text) => {
                    ctx.copy_text(text.clone());
                    self.message = format!("已复制 {} 个字符", text.chars().count());
                }
                SessionEvent::PageRenderFailed { page, error } => {
                    self.error = Some(format!("第 {} 页渲染失败：{error}", page.display_number()));
                }
                SessionEvent::Error(error) => self.error = Some(error.to_string()),
                SessionEvent::LayoutChanged => {
                    // 布局（缩放/旋转）变化后旧键不再被使用，留下会白占显存
                    self.textures.retain(|key| key.page == current);
                }
                _ => {}
            }
        }
    }

    /// 为可见页准备纹理（整页或瓦片），并裁剪不再可见的纹理。
    fn ensure_textures(&mut self, ctx: &egui::Context, session: &DocumentSession) {
        self.visible.clear();
        session.visible_pages(&mut self.visible);

        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else {
                continue;
            };
            // 整页路径
            if let Some(image) = session.image_for(page) {
                let key = RenderKey::new(page, placement.scale, placement.rotation);
                self.upload(ctx, key, &image);
            }
            // 分块路径
            for (tile, image) in session.tiles_for(page) {
                let key = RenderKey::tiled(page, placement.scale, placement.rotation, tile);
                self.upload(ctx, key, &image);
            }
        }

        let visible = self.visible.clone();
        let current = session.state().current_page;
        self.textures
            .retain(|key| visible.contains(&key.page) || key.page == current);
    }

    /// 上传一张位图为纹理（已存在则跳过）。
    fn upload(&mut self, ctx: &egui::Context, key: RenderKey, image: &Arc<RasterImage>) {
        if self.textures.contains(&key) {
            return;
        }
        // 全项目唯一的 BGRA→RGBA 转换点
        let rgba = image.to_rgba8();
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [image.width as usize, image.height as usize],
            &rgba,
        );
        let texture = ctx.load_texture(
            format!("page-{}-{}-{:?}", key.page.get(), key.scale_milli, key.tile),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert(key, texture);
    }

    fn draw_toolbar(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("上一页").clicked() {
                    self.commands.push(ViewerCommand::PrevPage);
                }
                if ui.button("下一页").clicked() {
                    self.commands.push(ViewerCommand::NextPage);
                }
                ui.separator();
                if ui.button("－").clicked() {
                    self.commands.push(ViewerCommand::ZoomOut);
                }
                if ui.button("＋").clicked() {
                    self.commands.push(ViewerCommand::ZoomIn);
                }
                if ui.button("100%").clicked() {
                    self.commands.push(ViewerCommand::SetZoom(1.0));
                }
                if ui.button("适应宽度").clicked() {
                    self.commands.push(ViewerCommand::FitWidth);
                }
                ui.separator();
                if ui.button("顺时针旋转").clicked() {
                    self.commands.push(ViewerCommand::RotateClockwise);
                }
                ui.separator();
                ui.toggle_value(&mut self.show_outline, "目录");

                // 搜索（同步接口：大文档可能短暂卡顿，见 decision-log D-03/D-07）
                ui.separator();
                ui.label("搜索：");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_input)
                        .desired_width(140.0)
                        .hint_text("关键词"),
                );
                let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (submitted || ui.button("查找").clicked()) && !self.search_input.is_empty() {
                    match session.search(&SearchQuery::new(self.search_input.clone())) {
                        Ok(hits) => {
                            self.message = format!("找到 {} 处", hits.len());
                            self.hits = hits;
                            self.active_hit = if self.hits.is_empty() { None } else { Some(0) };
                            if let Some(first) = self.hits.first().map(|hit| hit.page) {
                                self.commands.push(ViewerCommand::GoToPage(first));
                            }
                            self.error = None;
                        }
                        Err(error) => self.error = Some(format!("搜索失败：{error}")),
                    }
                }
                if !self.hits.is_empty() {
                    if ui.button("下一个").clicked() {
                        let next =
                            self.active_hit.map(|index| index + 1).unwrap_or(0) % self.hits.len();
                        self.active_hit = Some(next);
                        if let Some(page) = self.hits.get(next).map(|hit| hit.page) {
                            self.commands.push(ViewerCommand::GoToPage(page));
                        }
                    }
                    ui.label(format!(
                        "{} / {}",
                        self.active_hit.map(|index| index + 1).unwrap_or(0),
                        self.hits.len()
                    ));
                }
            });
        });
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui, session: &DocumentSession) {
        if !self.show_outline {
            return;
        }
        let navigation = self.navigation.clone();
        egui::Panel::left("outline")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui.heading("目录");
                ui.separator();
                let Some(navigation) = navigation else {
                    ui.label("（无目录）");
                    return;
                };
                if navigation.outline.is_empty() {
                    ui.label("（本文档没有书签）");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for node in &navigation.outline {
                        draw_outline_node(ui, node, 0, &mut self.commands, session);
                    }
                });
            });
    }

    fn draw_status(&self, ui: &mut egui::Ui, session: &DocumentSession) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "第 {} / {} 页",
                    session.state().current_page.display_number(),
                    session.page_count()
                ));
                ui.separator();
                ui.label(format!("缩放 {:.0}%", session.state().zoom * 100.0));
                ui.separator();
                ui.label(format!("纹理 {} 个", self.textures.len()));
                if !self.message.is_empty() {
                    ui.separator();
                    ui.label(&self.message);
                }
                if let Some(error) = &self.error {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(200, 60, 60), error);
                }
            });
        });
    }

    /// 画布：页面（纹理或白底页框）+ 高亮 + 鼠标交互。
    fn draw_canvas(&mut self, ui: &mut egui::Ui, session: &mut DocumentSession) {
        egui::CentralPanel::default().show(ui, |ui| {
            let size = ui.available_size();
            let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
            let canvas = response.rect;

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
            for page in visible {
                let Some(placement) = session.placement(page).copied() else { continue };
                let page_rect = view_rect_for(&placement, scroll);
                let screen = egui::Rect::from_min_size(
                    canvas.min + EVec2::new(page_rect.x, page_rect.y),
                    EVec2::new(page_rect.width, page_rect.height),
                );

                let whole_key = RenderKey::new(page, placement.scale, placement.rotation);
                if let Some(texture) = self.textures.get(&whole_key) {
                    paint_image(&painter, texture, screen);
                    continue;
                }

                // 分块路径：逐块贴；缺失的块画白底
                let tiles = session.tiles_for(page);
                if tiles.is_empty() {
                    paint_placeholder(&painter, screen);
                    continue;
                }
                let pixel_size = placement.pixel_size();
                for (tile, _) in tiles {
                    let pixels = tile.pixel_rect(pixel_size);
                    let key =
                        RenderKey::tiled(page, placement.scale, placement.rotation, tile);
                    let tile_screen = egui::Rect::from_min_size(
                        canvas.min
                            + EVec2::new(
                                page_rect.x + pixels.x / placement.scale.0,
                                page_rect.y + pixels.y / placement.scale.0,
                            ),
                        EVec2::new(
                            pixels.width / placement.scale.0,
                            pixels.height / placement.scale.0,
                        ),
                    );
                    match self.textures.get(&key) {
                        Some(texture) => paint_image(&painter, texture, tile_screen),
                        None => paint_placeholder(&painter, tile_screen),
                    }
                }
            }

            // 2) 高亮（选区 + 搜索命中）
            for highlight in session.highlight_selection() {
                paint_highlight(&painter, session, &highlight, scroll, canvas.min);
            }
            let active = self.active_hit;
            for highlight in session.highlights_for_search(&self.hits, active) {
                paint_highlight(&painter, session, &highlight, scroll, canvas.min);
            }

            // 3) 交互
            if response.drag_started() {
                if let Some(position) = response.interact_pointer_pos() {
                    let content = canvas_to_content(canvas, position, scroll);
                    match session.text_position_at(content) {
                        Some(text_position) => {
                            self.drag = Some(DragMode::Select);
                            self.commands.push(ViewerCommand::SetSelectionGranularity(
                                SelectionGranularity::Char,
                            ));
                            self.commands.push(ViewerCommand::SelectBegin(text_position));
                        }
                        None => self.drag = Some(DragMode::Pan),
                    }
                }
            }
            if response.dragged() {
                match self.drag {
                    Some(DragMode::Select) => {
                        if let Some(position) = response.interact_pointer_pos() {
                            let content = canvas_to_content(canvas, position, scroll);
                            if let Some(text_position) = session.text_position_at(content) {
                                self.commands.push(ViewerCommand::SelectExtend(text_position));
                            }
                        }
                    }
                    Some(DragMode::Pan) => {
                        let delta = response.drag_delta();
                        self.commands
                            .push(ViewerCommand::ScrollBy(Vec2::new(-delta.x, -delta.y)));
                    }
                    None => {}
                }
            }
            if response.drag_stopped() {
                self.drag = None;
            }
            if response.double_clicked() {
                if let Some(position) = response.interact_pointer_pos() {
                    let content = canvas_to_content(canvas, position, scroll);
                    if let Some(text_position) = session.text_position_at(content) {
                        self.commands.push(ViewerCommand::SetSelectionGranularity(
                            SelectionGranularity::Word,
                        ));
                        self.commands.push(ViewerCommand::SelectBegin(text_position));
                    }
                }
            } else if response.clicked() {
                self.commands.push(ViewerCommand::SelectClear);
            }
        });
    }

    /// 加密文档的密码输入框（用模态窗口阻断背后交互）。
    fn draw_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(path) = self.password_for.clone() else { return };
        egui::Modal::new(Id::unique("pdf-password")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("文档已加密");
            ui.label(format!("{}", path.display()));
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.password_input)
                    .password(true)
                    .hint_text("密码"),
            );
            let submitted = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
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

impl eframe::App for PdfReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 把会话临时取出，避免与 `self` 的其它字段发生借用冲突
        let Some(mut session) = self.session.take() else {
            self.draw_empty(ui);
            self.draw_password_prompt(&ctx);
            return;
        };

        self.collect_commands(&ctx, &session);
        self.apply_and_pump(&ctx, &mut session);
        self.ensure_textures(&ctx, &session);

        self.draw_toolbar(ui, &session);
        self.draw_sidebar(ui, &session);
        self.draw_status(ui, &session);
        self.draw_canvas(ui, &mut session);

        // 为下一帧备好可见页
        session.handle(ViewerCommand::RequestVisible, &mut self.events);
        self.events.clear();

        // 只有后台还有任务时才持续重绘（避免空转）
        let pending = session.pending_renders();
        self.session = Some(session);

        if pending > 0 {
            ctx.request_repaint();
        }
        self.draw_password_prompt(&ctx);
    }
}

impl PdfReaderApp {
    /// 没有打开文档时的空状态界面。
    fn draw_empty(&mut self, ui: &mut egui::Ui) {
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
                ui.colored_label(Color32::from_rgb(200, 60, 60), error);
            });
        }
    }
}

/// 画一张纹理（UV 取满）。
fn paint_image(painter: &egui::Painter, texture: &egui::TextureHandle, rect: egui::Rect) {
    painter.image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// 画"白底页框"占位（decision-log D-06：位图未就绪时用它，而不是保留上一档拉伸图）。
fn paint_placeholder(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, Color32::from_gray(180)),
        StrokeKind::Inside,
    );
}

/// 画一块高亮：PDF 页面空间 → 内容空间 → 视图空间。
fn paint_highlight(
    painter: &egui::Painter,
    session: &DocumentSession,
    highlight: &Highlight,
    scroll: Vec2,
    canvas_min: egui::Pos2,
) {
    let Some(placement) = session.placement(highlight.page).copied() else { return };
    let color = match highlight.kind {
        HighlightKind::Selection => Color32::from_rgba_unmultiplied(0, 120, 255, 90),
        HighlightKind::SearchMatch => Color32::from_rgba_unmultiplied(255, 220, 0, 80),
        HighlightKind::ActiveSearchMatch => Color32::from_rgba_unmultiplied(255, 140, 0, 110),
    };
    for rect in &highlight.rects {
        // 坐标系换算链：PDF 页面空间 → 内容空间 → 视图空间
        let content = placement.rect_to_content(*rect);
        let screen = egui::Rect::from_min_size(
            canvas_min + EVec2::new(content.x - scroll.x, content.y - scroll.y),
            EVec2::new(content.width, content.height),
        );
        painter.rect_filled(screen, CornerRadius::ZERO, color);
    }
}

/// 画布坐标 → 内容坐标。
fn canvas_to_content(canvas: egui::Rect, screen_position: egui::Pos2, scroll: Vec2) -> Point {
    view_to_content(
        Point::new(screen_position.x - canvas.min.x, screen_position.y - canvas.min.y),
        scroll,
    )
}

/// 递归画大纲节点；点击时跳到目标页。
fn draw_outline_node(
    ui: &mut egui::Ui,
    node: &OutlineNode,
    depth: usize,
    commands: &mut Vec<ViewerCommand>,
    session: &DocumentSession,
) {
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 12.0);
        let clickable = node.destination.is_some();
        let label = if clickable {
            RichText::new(&node.title).strong()
        } else {
            RichText::new(&node.title)
        };
        if ui
            .add_enabled(clickable, egui::Button::new(label).frame(false))
            .clicked()
        {
            if let Some(Destination::Page { page, .. }) = &node.destination {
                let page = *page;
                if page.get() < session.page_count() {
                    commands.push(ViewerCommand::GoToPage(page));
                }
            }
        }
    });
    for child in &node.children {
        draw_outline_node(ui, child, depth + 1, commands, session);
    }
}

/// 让 `Align2` 与 `FontId` 在需要时可用（保持导入稳定）。
#[allow(dead_code)]
fn _unused_imports() -> (Align2, egui::FontId) {
    (Align2::CENTER_CENTER, egui::FontId::proportional(12.0))
}
