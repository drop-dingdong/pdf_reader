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

use crate::view::{
    canvas_rect_for, command_for_key, command_for_scroll, command_for_zoom_delta,
    clamp_float_rect, color_image, commands_for_destination, destination_points, fit_inside,
    compose_snapshot, screen_rect_to_content, veil_bands, view_rect_to_page_rect, SnapshotPage,
    flatten_outline, link_overlay_visible, link_rect_in_page_bounds, outline_node_count,
    texture_pages, wheel_targets_canvas, FLOAT_HANDLE_SIZE, FLOAT_MIN_SIZE, FLOAT_TITLE_HEIGHT,
    tile_view_rect, tile_view_rect_scaled,
    view_rect_for, view_to_content, KeyedStore,
};

/// 纹理缓存容量（条目数）。真正的淘汰依据是"是否可见"，容量只是兜底。
const TEXTURE_CAPACITY: usize = 64;

/// 侧栏宽度范围（内容空间像素）。宽度由 app 自己保存 —— 见 `sidebar_width` 的说明。
const SIDEBAR_MIN_WIDTH: f32 = 160.0;
const SIDEBAR_MAX_WIDTH: f32 = 640.0;
/// 侧栏拖拽条的宽度（由 app 自己处理拖拽，见 `draw_sidebar` 的说明）。
const SIDEBAR_HANDLE_WIDTH: f32 = 5.0;

/// 显示链接热区框所需的最低缩放：更小的时候热区框会糊成一片，反而干扰阅读（用户要求）。
const LINK_OVERLAY_MIN_ZOOM: f32 = 0.75;
/// 大纲每行的固定高度（虚拟化行渲染要求等行高）。
const OUTLINE_ROW_HEIGHT: f32 = 18.0;
/// 链接热区框的颜色（空心红线，用户指定）。
const LINK_OVERLAY_COLOR: Color32 = Color32::from_rgb(220, 40, 40);

/// 每帧最多上传几张纹理。
///
/// 一张整页位图的 RGBA 转换 + GPU 上传在毫秒量级；一帧内连传多张就会掉帧。
/// 设上限后开销被摊到多帧，视觉上是"相邻页稍后补齐"，比"翻页顿一下"好得多。
const MAX_UPLOADS_PER_FRAME: usize = 2;

/// 缩放/旋转后进入"预览窗口"的时长：这段时间内不提交新渲染、只显示旧图拉伸。
///
/// 取 160ms：比人手连续滚轮的间隔（约 50–120ms）略长，因此一次连续缩放只会
/// 在最后渲染一次；又不至于让单次缩放的清晰图来得太迟。
const ZOOM_PREVIEW_WINDOW: std::time::Duration = std::time::Duration::from_millis(160);

/// 拖拽中：区分"平移画面"与"选择文字"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    /// 拖动画面（起点没有命中文字）。
    Pan,
    /// 拖选文字（起点命中了字符）。
    Select,
}

/// 鼠标拖拽的语义（第二轮验收：去掉不可靠的"自动"模式，只留两种明确行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    /// 总是平移画面（默认：拖拽永远不会意外改动选区）。
    #[default]
    Pan,
    /// 总是选字（拖到空白处不产生选区）。
    Select,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Self::Pan => "平移",
            Self::Select => "选字",
        }
    }
}

/// 截图模式的状态。
///
/// 进入后：主视图的拖动/滚动/缩放/链接跳转等**一律不响应**（瓦片加载仍继续），
/// 浮窗与所有叠层隐藏，主视图蒙上半透明亮灰；鼠标拖出矩形，矩形内透明（"挖洞"）。
/// `Enter` 退出并在终端输出区域信息，`Esc` 退出且无输出。
#[derive(Default)]
struct CaptureState {
    /// 已画出的选区（视图空间）；`None` = 尚未开始
    rect: Option<egui::Rect>,
    /// 正在拖拽的起点（视图空间）
    anchor: Option<egui::Pos2>,
}

/// 浮窗内容：纯文字，或一张**自包含的截图**。
enum FloatContent {
    /// 纯文字。
    Text(String),
    /// 截图（独立纹理，与主视图状态解耦；改窗口大小只改变显示尺寸，不改变内容）。
    Image(egui::TextureHandle),
}

/// 视图区域内的浮窗（始终在画布内部、可移动/改大小/关闭）。
struct Float {
    /// 唯一 id（用于 egui 的交互标识）。
    id: u64,
    /// 标题栏文字。
    title: String,
    /// 内容（文字或截图）。
    content: FloatContent,
    /// 在**视图区域**（画布）内的矩形；每帧会被夹取回区域内。
    rect: egui::Rect,
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
    /// 拖拽语义（工具栏可切换）。
    tool: Tool,
    /// 是否画出链接热区框（工具栏 checkbox，默认关闭）。
    show_links: bool,
    /// 大纲的**扁平行**（打开文档时摊平一次；侧栏用虚拟化行渲染，避免每帧布局上千行）。
    outline_rows: Vec<crate::view::OutlineRow>,
    /// 视图区域内的浮窗；**后加入的排在后面 → 绘制与交互都在更上层**（用户要求"后出现的浮窗更高"）。
    floats: Vec<Float>,
    /// 下一个浮窗 id。
    next_float_id: u64,
    /// 是否标注"被引用的点"（链接跳转目的地）。
    show_dest_points: bool,
    /// 每个目标页上被引用的点（按 0.5pt 去重），下标 = 页号；打开文档时算一次。
    dest_points: Vec<Vec<Point>>,
    /// 截图模式状态（`Some` = 处于该模式）。
    capture: Option<CaptureState>,
    /// 本帧累计的"像素格式转换"与"load_texture"耗时（诊断用，定位 150ms 到底在哪一段）。
    last_convert_ms: f64,
    last_upload_ms: f64,
    /// 侧栏宽度：**由 app 自己保存**。
    ///
    /// egui 的面板尺寸以 `parent_ui.scope_id().with_salt(...)` 为键持久化，
    /// 而 eframe 每帧给的是一个新的根 `Ui`，于是 egui 侧的持久化读不回来 ——
    /// 人工验收看到的"拖动边框、松手回弹"就是这个原因。改为每帧把上次观察到的宽度
    /// 作为 `default_size` 回填，尺寸便由我们自己保住。
    sidebar_width: f32,
    /// 待输入的密码（文档加密时）。
    password_for: Option<PathBuf>,
    password_input: String,
    search_input: String,
    /// 页码输入框（第二轮验收：支持键入页码跳转）。
    page_input: String,
    hits: Vec<SearchHit>,
    active_hit: Option<usize>,
    show_outline: bool,
    /// 状态栏文本与错误提示。
    message: String,
    error: Option<String>,
    /// 运行时诊断输出（环境变量 `PDF_READER_DIAGNOSE=1` 开启）。
    ///
    /// 存在的理由：像"800% 时看不到内容"这类问题只在真实窗口 + 真实 GPU 上出现，
    /// 无头测试无法复现；把每帧的绘制决策打到 stderr，用户就能把实况数据交回来
    /// （agent.md 规则 2：UI 相关验证交给用户，但要让用户能看到可诊断的信息）。
    diagnose: bool,
    /// 上次诊断输出的时间（限流到每秒一次）。
    last_report: std::time::Instant,
    /// 缩放/旋转"刚变过"的截止时间：在此之前不提交新渲染，全部可见页统一显示旧图拉伸。
    ///
    /// 连续滚轮缩放会产生一串中间比例（100→120→140→150），逐个渲染既浪费时间
    /// 又每步都掉一次帧（第三轮验收"缩放总是有顿感"）。等用户停手再渲染最终比例，
    /// 中间的过场用旧图顶上 —— 这是"预览质量阶梯"的轻量版。
    preview_until: Option<std::time::Instant>,
    /// 上一帧观察到的缩放与旋转，用于检测"刚变过"。
    last_zoom: f32,
    last_rotation: Rotation,
    /// 进程启动时刻：所有时序诊断都按"相对它的秒数"输出（用户要求：秒 + 浮点）。
    started_at: std::time::Instant,
    /// 本次拖动开始时刻（拖动结束时清空）。
    drag_started_at: Option<std::time::Instant>,
    /// 上一次"拖动位移生效"的诊断时刻，用于给出帧间隔。
    last_pan_at: Option<std::time::Instant>,
    /// 上一次记录的可见页集，用于检测"因拖动而重新决定显示内容"。
    last_visible: Vec<PageIndex>,
}

impl PdfReaderApp {
    /// 构造；`initial` 为命令行传入的文件路径。
    ///
    /// 这里顺手关掉 egui 内建的"Ctrl/⌘ + `+`/`-`/`0` 缩放整个界面"快捷键
    /// （`Options::zoom_with_keyboard`）：否则按 `Ctrl`+`+` 会**同时**放大 UI 与文档页面
    /// （人工验收反馈的现象）。关掉之后这些键由本应用统一解释为**文档缩放**。
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx.options_mut(|options| options.zoom_with_keyboard = false);

        let mut app = Self {
            session: None,
            navigation: None,
            textures: KeyedStore::new(TEXTURE_CAPACITY),
            commands: Vec::new(),
            events: Vec::new(),
            visible: Vec::new(),
            drag: None,
            tool: Tool::default(),
            show_links: false,
            outline_rows: Vec::new(),
            floats: Vec::new(),
            next_float_id: 1,
            show_dest_points: false,
            dest_points: Vec::new(),
            capture: None,
            last_convert_ms: 0.0,
            last_upload_ms: 0.0,
            sidebar_width: 260.0,
            password_for: None,
            password_input: String::new(),
            search_input: String::new(),
            page_input: String::new(),
            hits: Vec::new(),
            active_hit: None,
            show_outline: true,
            message: String::new(),
            error: None,
            diagnose: std::env::var_os("PDF_READER_DIAGNOSE").is_some(),
            last_report: std::time::Instant::now(),
            preview_until: None,
            last_zoom: 1.0,
            last_rotation: Rotation::None,
            started_at: std::time::Instant::now(),
            drag_started_at: None,
            last_pan_at: None,
            last_visible: Vec::new(),
        };
        if let Some(path) = initial {
            app.open(&path, None);
        }
        app
    }

    /// 跳到第 `index` 条搜索命中。
    ///
    /// **把命中位置放到视口中心**（第二轮验收的统一跳转语义）：
    /// 跳页是"顶部对齐"，搜索命中则是"居中"，因为用户关心的是那一行文字而不是页首。
    fn goto_hit(&mut self, index: usize) {
        let Some(hit) = self.hits.get(index) else { return };
        self.active_hit = Some(index);
        let page = hit.page;
        self.commands.push(ViewerCommand::GoToPage(page));
        if let Some(rect) = hit.rects.first() {
            self.commands.push(ViewerCommand::CenterOnPagePoint { page, point: rect.center() });
        }
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
                        // 摊平一次即可（虚拟化行渲染需要行列表）
                        self.outline_rows = self
                            .navigation
                            .as_ref()
                            .map(|navigation| flatten_outline(&navigation.outline))
                            .unwrap_or_default();
                        // 被引用的点（链接目的地）也只算一次：按目标页分组、按位置去重
                        self.dest_points = self
                            .navigation
                            .as_ref()
                            .map(|navigation| {
                                destination_points(
                                    &navigation.links_by_page,
                                    session.page_count() as usize,
                                )
                            })
                            .unwrap_or_default();
                        self.floats.clear();
                        self.next_float_id = 1;
                        self.capture = None;
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
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
                .collect()
        });
        // 复制：egui 把 Ctrl/Cmd+C 翻译成 `Event::Copy`（**不是** Key 事件），
        // 因此必须单独识别它 —— 这就是第二轮验收里"复制按钮可用但 Ctrl+C 无反应"的原因。
        let copy_event = ctx.input(|input| {
            input.events.iter().any(|event| matches!(event, egui::Event::Copy))
        });
        if copy_event {
            self.commands.push(ViewerCommand::CopySelection);
        }
        for (key, modifiers) in pressed {
            // 已经由 Event::Copy 处理过时不重复处理（避免复制两次）
            if copy_event && key == Key::C {
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
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
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
                SessionEvent::CurrentPageChanged(page) => {
                    // 页码输入框跟随真实当前页，避免显示过期数字
                    self.page_input = page.display_number().to_string();
                }
                _ => {}
            }
        }
    }

    /// 为可见页（以及相邻页）准备纹理，并裁剪不再需要的纹理。
    ///
    /// 两个细节都来自第二轮人工验收的"翻页边界处卡顿"：
    /// 1. **提前上传相邻页**：会话已按 `PREFETCH_PAGE_RADIUS` 预渲染，这里把它们也传成纹理，
    ///    于是"新页进入视口"那一刻不再有 4 MiB 量级的转换 + GPU 上传开销；
    /// 2. **每帧限量上传**：一帧内同时上传多页会造成明显掉帧，因此设上限、分帧摊开。
    ///
    /// `allow_upload = false` 时只更新可见页与裁剪，**跳过转换与上传** ——
    /// 拖动期间用它把"新页进入"那一帧约 150ms 的逐像素转换挪到松手之后（用户要求）。
    fn ensure_textures(
        &mut self,
        ctx: &egui::Context,
        session: &DocumentSession,
        allow_upload: bool,
    ) -> usize {
        self.visible.clear();
        session.visible_pages(&mut self.visible);

        // 上传目标 = 保留目标（**同一个集合**，见 `view::texture_pages`）：
        // 曾经两者不一致（上传"可见页 ±1"、裁剪只留"可见页"），导致相邻页纹理
        // 每帧被反复上传又丢弃 —— 200% 缩放下实测每帧 300ms 全耗在这里。
        let targets = texture_pages(
            &self.visible,
            session.state().current_page,
            session.page_count(),
        );

        let mut uploaded = 0usize;
        if allow_upload {
            for page in &targets {
                if uploaded >= MAX_UPLOADS_PER_FRAME {
                    break;
                }
                uploaded += self.upload_page(ctx, session, *page);
            }
        }

        // 裁剪：只保留可见页（±1）与当前页，并且每页只保留"当前比例 + 最近一个旧比例"
        // —— 旧比例是缩放过场要用的（第四轮验收：丢掉其它页的旧图会让它们闪白）。
        //
        // 旋转按页取：同一份文档里各页的 /Rotate 可能不同。
        let pages: Vec<(PageIndex, Rotation)> = targets
            .iter()
            .filter_map(|page| session.placement(*page).map(|placement| (*page, placement.rotation)))
            .collect();
        let current = session.state().current_page;
        let current_scale_milli = session
            .placement(current)
            .map(|placement| {
                RenderKey::new(current, placement.scale, placement.rotation).scale_milli
            })
            .unwrap_or(1000);
        self.textures
            .retain_preview_scales(&pages, current_scale_milli);
        uploaded
    }

    /// 上传某一页当前可用的位图（整页或瓦片），返回新上传的纹理数量。
    fn upload_page(
        &mut self,
        ctx: &egui::Context,
        session: &DocumentSession,
        page: PageIndex,
    ) -> usize {
        let Some(placement) = session.placement(page).copied() else {
            return 0;
        };
        let mut count = 0;
        // 整页路径
        if let Some(image) = session.image_for(page) {
            let key = RenderKey::new(page, placement.scale, placement.rotation);
            if !self.textures.contains(&key) {
                self.upload(ctx, key, &image);
                count += 1;
            }
        }
        // 分块路径
        for (tile, image) in session.tiles_for(page) {
            let key = RenderKey::tiled(page, placement.scale, placement.rotation, tile);
            if !self.textures.contains(&key) {
                self.upload(ctx, key, &image);
                count += 1;
            }
        }
        count
    }

    /// 上传一张位图为纹理（已存在则跳过）。
    fn upload(&mut self, ctx: &egui::Context, key: RenderKey, image: &Arc<RasterImage>) {
        if self.textures.contains(&key) {
            return;
        }
        // 全项目唯一的像素格式转换点：**一遍**完成（见 view::color_image 的说明）
        let started = std::time::Instant::now();
        let color = color_image(image.format, image.data(), image.width, image.height);
        self.last_convert_ms += started.elapsed().as_secs_f64() * 1000.0;

        let started = std::time::Instant::now();
        let texture = ctx.load_texture(
            format!("page-{}-{}-{:?}", key.page.get(), key.scale_milli, key.tile),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.last_upload_ms += started.elapsed().as_secs_f64() * 1000.0;
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
                // 页码输入：键入数字后回车跳转（第二轮验收要求）
                ui.label("跳到：");
                let page_response = ui.add(
                    egui::TextEdit::singleline(&mut self.page_input)
                        .desired_width(56.0)
                        .hint_text("页码"),
                );
                if page_response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    match self.page_input.trim().parse::<u32>() {
                        Ok(number) if number >= 1 && number <= session.page_count() => {
                            self.commands.push(ViewerCommand::GoToPage(PageIndex::new(number - 1)));
                            self.error = None;
                        }
                        Ok(number) => {
                            self.error =
                                Some(format!("页码 {number} 超出范围（1–{}）", session.page_count()))
                        }
                        Err(_) => self.error = Some("请输入数字页码".to_string()),
                    }
                }
                ui.separator();
                // 拖拽语义：平移 / 选字 两档（"自动"模式不可靠，已按反馈移除）
                ui.label("拖拽：");
                for tool in [Tool::Pan, Tool::Select] {
                    ui.selectable_value(&mut self.tool, tool, tool.label());
                }
                ui.separator();
                // 复制：除了 Ctrl+C，也提供一个可见入口，便于确认选区内容
                if ui.button("复制").clicked() {
                    self.commands.push(ViewerCommand::CopySelection);
                }
                ui.toggle_value(&mut self.show_outline, "目录");
                // 链接热区框（用户需求）：勾选后用红色空心框显示链接 bbox
                ui.checkbox(&mut self.show_links, "链接框");
                // 被引用的点（链接目的地）：与链接框同样的缩放阈值（过小不显示）
                ui.checkbox(&mut self.show_dest_points, "引用点");
                if ui.button("添加测试浮窗").clicked() {
                    self.add_test_float();
                }
                // 截图模式：进入后主视图只用于框选区域
                if ui.button("截图").clicked() {
                    self.capture = Some(CaptureState::default());
                    self.message = "截图模式：拖动框选区域，Enter 输出并退出，Esc 退出".to_string();
                }

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
                            // 0 命中往往意味着"扫描件没有文本层"，而不是搜索坏了 —— 说清楚
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
                    // 上一个 / 下一个（第二轮验收要求补"上一个"）
                    if ui.button("上一个").clicked() {
                        let index = match self.active_hit {
                            Some(0) | None => self.hits.len() - 1,
                            Some(index) => index - 1,
                        };
                        self.goto_hit(index);
                    }
                    if ui.button("下一个").clicked() {
                        let index = self.active_hit.map(|index| index + 1).unwrap_or(0) % self.hits.len();
                        self.goto_hit(index);
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
        let width = self.sidebar_width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        // 尺寸由我们自己控制（`exact_size`），可拖拽的分隔条在画布左侧 —— 见 `draw_canvas`。
        //
        // 不用 egui 内建拖拽的原因：面板状态以 `parent_ui.scope_id().with_salt(id)` 为键持久化，
        // 而 eframe 每帧传入的是**新的根 `Ui`**，于是拖拽结果写进去、下一帧读不回来 ——
        // 第三、四轮验收两次反馈"空有动作没有结果"，根因就在这里。
        egui::Panel::left("outline")
            .resizable(false)
            .exact_size(width)
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
                // **虚拟化行**：只布局可见的那十几行。
                // 实测 1292 个节点时，每帧全量布局要花约 42ms（与缩放无关，纯 UI 开销）。
                let rows = &self.outline_rows;
                let row_height = OUTLINE_ROW_HEIGHT;
                egui::ScrollArea::vertical().show_rows(ui, row_height, rows.len(), |ui, range| {
                    for index in range {
                        let Some((depth, title, destination)) = rows.get(index) else { continue };
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
                .map_or(false, |position| canvas.contains(position));
            if self.capture.is_none() && wheel_targets_canvas(pointer_over_canvas) {
                let (scroll_delta, zoom_delta, modifiers) = ui.input(|input| {
                    (input.smooth_scroll_delta, input.zoom_delta(), input.modifiers)
                });
                if let Some(command) = command_for_scroll(
                    scroll_delta,
                    modifiers.ctrl || modifiers.command,
                    session.state().zoom,
                ) {
                    session.handle(command, &mut self.events);
                }
                if let Some(command) = command_for_zoom_delta(zoom_delta, session.state().zoom) {
                    session.handle(command, &mut self.events);
                }
                ui.ctx().request_repaint();
            }

            // ---- 拖动**当帧生效** ----
            //
            // 拖动命令原先要等下一帧 `apply_and_pump` 才执行，于是画面永远落后指针一帧；
            // 快速拖动时就是上百像素的偏差（用户反馈"落后鼠标移动过多"）。
            // 这里在读取 scroll / 绘制**之前**先处理拖动，内容与指针同帧同步。
            let inverted_now = ui.input(|input| input.modifiers.shift);
            let select_now = match self.tool {
                Tool::Select => !inverted_now,
                Tool::Pan => inverted_now,
            };
            if self.capture.is_none() && response.drag_started() {
                self.drag_started_at = Some(std::time::Instant::now());
                self.last_pan_at = None;
                if self.diagnose {
                    eprintln!(
                        "[时序] 拖动开始 t={:.6}s",
                        self.started_at.elapsed().as_secs_f64()
                    );
                }
                if let Some(position) = response.interact_pointer_pos() {
                    let content =
                        canvas_to_content(canvas, position, session.viewport().scroll);
                    let hit = session.text_position_at(content);
                    match (select_now, hit) {
                        (true, Some(text_position)) => {
                            self.drag = Some(DragMode::Select);
                            session.handle(
                                ViewerCommand::SetSelectionGranularity(SelectionGranularity::Char),
                                &mut self.events,
                            );
                            session.handle(
                                ViewerCommand::SelectBegin(text_position),
                                &mut self.events,
                            );
                        }
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
                        if self.diagnose && after != before {
                            let now = std::time::Instant::now();
                            let since_start = self
                                .drag_started_at
                                .map(|start| now.duration_since(start).as_secs_f64() * 1000.0)
                                .unwrap_or(0.0);
                            let since_last = self
                                .last_pan_at
                                .map(|last| now.duration_since(last).as_secs_f64() * 1000.0)
                                .unwrap_or(f64::NAN);
                            eprintln!(
                                "[时序] 拖动位移生效 t={:.6}s Δ起={:.1}ms Δ上次={:.1}ms scroll=({:.1},{:.1})",
                                self.started_at.elapsed().as_secs_f64(),
                                since_start,
                                since_last,
                                after.x,
                                after.y
                            );
                            self.last_pan_at = Some(now);
                            // "因为拖动而重新决定显示内容"= 可见页集合发生变化（要换/补位图）
                            let mut visible = Vec::new();
                            session.visible_pages(&mut visible);
                            if visible != self.last_visible {
                                eprintln!(
                                    "[时序] 可见页集变化 t={:.6}s Δ起={:.1}ms 可见页={:?}",
                                    self.started_at.elapsed().as_secs_f64(),
                                    since_start,
                                    visible.iter().map(|page| page.display_number()).collect::<Vec<_>>()
                                );
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

                let whole_key = RenderKey::new(page, placement.scale, placement.rotation);
                let has_previous = self
                    .textures
                    .best_page_texture(page, placement.rotation, whole_key.scale_milli)
                    .is_some();

                // 先决策、再落笔：这段"画法"判定同时用于诊断输出，
                // 出问题时能直接从日志看出走的是哪条路径（agent.md 规则 2：UI 现象靠实测数据定位）。
                enum Plan {
                    Whole,
                    Stretch,
                    Tiles(TileSpec, TileSpec),
                    /// 用上一档缩放的**瓦片**拼出预览（高倍缩放下没有整页图可拉伸）。
                    TilePreview(u32),
                    Blank,
                }
                let plan = if self.textures.contains(&whole_key) {
                    Plan::Whole
                } else if let Some((first, last)) = session.visible_tiles(page) {
                    // 优先级：当前比例的真瓦片 → 上一档的整页图 → 上一档的瓦片 → 白纸。
                    // 只要当前比例还有内容没到齐，就尽量用旧内容顶上，绝不让页面闪白
                    // （第四轮验收：中心页不闪、相邻页闪白，就是因为相邻页没有旧图可用）。
                    if self.textures.has_tiles(&whole_key) {
                        Plan::Tiles(first, last)
                    } else if has_previous {
                        Plan::Stretch
                    } else if let Some(old_scale) = self.textures.best_tile_scale(
                        page,
                        placement.rotation,
                        whole_key.scale_milli,
                    ) {
                        Plan::TilePreview(old_scale)
                    } else {
                        Plan::Tiles(first, last)
                    }
                } else if has_previous {
                    Plan::Stretch
                } else if let Some(old_scale) = self.textures.best_tile_scale(
                    page,
                    placement.rotation,
                    whole_key.scale_milli,
                ) {
                    Plan::TilePreview(old_scale)
                } else {
                    Plan::Blank
                };

                let (label, ready, total) = match plan {
                    Plan::Whole => {
                        let texture = self.textures.get(&whole_key).expect("已判定存在");
                        paint_image(&painter, texture, screen);
                        ("整页纹理", 1u32, 1u32)
                    }
                    Plan::Stretch => {
                        let texture = self
                            .textures
                            .best_page_texture(page, placement.rotation, whole_key.scale_milli)
                            .expect("已判定存在");
                        paint_image(&painter, texture, screen);
                        ("旧图拉伸", 0, 0)
                    }
                    Plan::TilePreview(old_scale_milli) => {
                        let old_page = placement
                            .page_size
                            .rotated(placement.rotation)
                            .scale(old_scale_milli as f32 / 1000.0);
                        let new_page = placement.pixel_size();
                        let mut count = 0u32;
                        for (tile, texture) in
                            self.textures.tiles_at(page, placement.rotation, old_scale_milli)
                        {
                            let pixels = tile.pixel_rect(old_page);
                            let rect = tile_view_rect_scaled(
                                page_rect,
                                pixels,
                                old_page,
                                new_page,
                            );
                            let tile_screen = egui::Rect::from_min_size(
                                canvas.min + EVec2::new(rect.x, rect.y),
                                EVec2::new(rect.width, rect.height),
                            );
                            paint_image(&painter, texture, tile_screen);
                            count += 1;
                        }
                        ("旧瓦片预览", count, count)
                    }
                    Plan::Tiles(first, last) => {
                        // 视口覆盖到的**全部**瓦片都要画：就绪的贴图、未就绪的补白。
                        // v1 只遍历"已缓存"的瓦片，未就绪区域直接露出深色画布 ——
                        // 高倍缩放时看起来就是"视野全黑"（第二轮验收 §15）。
                        let pixel_size = placement.pixel_size();
                        let mut ready = 0u32;
                        let mut total = 0u32;
                        for row in first.row..=last.row {
                            for col in first.col..=last.col {
                                total += 1;
                                let tile = TileSpec::new(col, row);
                                let pixels = tile.pixel_rect(pixel_size);
                                // 不变量：内容空间单位 == 该页位图像素 → 像素偏移直接相加（D-17）
                                let rect = tile_view_rect(page_rect, pixels);
                                let key = RenderKey::tiled(
                                    page,
                                    placement.scale,
                                    placement.rotation,
                                    tile,
                                );
                                let tile_screen = egui::Rect::from_min_size(
                                    canvas.min + EVec2::new(rect.x, rect.y),
                                    EVec2::new(rect.width, rect.height),
                                );
                                match self.textures.get(&key) {
                                    Some(texture) => {
                                        ready += 1;
                                        paint_image(&painter, texture, tile_screen);
                                    }
                                    None => paint_blank(&painter, tile_screen),
                                }
                            }
                        }
                        ("瓦片", ready, total)
                    }
                    Plan::Blank => {
                        // 纸张（白底 + 页框）在循环开头已经铺好，这里无需再画任何东西 ——
                        // 尤其不要画瓦片边框（那会变成满屏网格线）。
                        ("白底页框", 0, 0)
                    }
                };

                if self.diagnose {
                    report.push(format!(
                        "p{} 视图矩形=({:.0},{:.0} {:.0}×{:.0}) 画法={label} 瓦片={ready}/{total}",
                        page.display_number(),
                        page_rect.x,
                        page_rect.y,
                        page_rect.width,
                        page_rect.height
                    ));
                }
            }

            // 诊断输出（PDF_READER_DIAGNOSE=1）：每秒一次，足够定位"看不到内容"这类问题
            if self.diagnose
                && self.last_report.elapsed() >= std::time::Duration::from_secs(1)
            {
                self.last_report = std::time::Instant::now();
                let max_side = ui.ctx().input(|input| input.max_texture_side);
                eprintln!(
                    "[诊断] 缩放={:.0}% 视口={:.0}×{:.0} scroll=({:.1},{:.1}) 当前页={} 可见页={:?} \
                     纹理={} 缓存在飞={} 预览中={} max_texture_side={} | {}",
                    session.state().zoom * 100.0,
                    viewport.size.width,
                    viewport.size.height,
                    scroll.x,
                    scroll.y,
                    session.state().current_page.display_number(),
                    self.visible.iter().map(|page| page.display_number()).collect::<Vec<_>>(),
                    self.textures.len(),
                    session.pending_renders(),
                    previewing,
                    max_side,
                    report.join(" | ")
                );
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
            if self.diagnose {
                if let Some(navigation) = &self.navigation {
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else { continue };
                        let links = navigation.links_on(page);
                        if links.is_empty() {
                            continue;
                        }
                        let size = placement.page_size;
                        let first = links[0].rect;
                        eprintln!(
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
                        );
                    }
                }
            }

            // 2b) 链接热区框（可开关；缩放过小时不画）
            if self.capture.is_none()
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
                                Stroke::new(1.5, LINK_OVERLAY_COLOR),
                                StrokeKind::Outside,
                            );
                        }
                    }
                }
            }

            // 2c) 被引用的点（链接跳转目的地）：按位置去重后画记号；
            //     与链接框同样在缩放过小时不显示（用户要求）。
            if self.capture.is_none()
                && link_overlay_visible(
                    self.show_dest_points,
                    session.state().zoom,
                    LINK_OVERLAY_MIN_ZOOM,
                )
            {
                for page in self.visible.clone() {
                    let Some(placement) = session.placement(page).copied() else { continue };
                    let Some(points) = self.dest_points.get(page.get() as usize) else { continue };
                    for point in points {
                        // PDF 页面空间 → 内容空间 → 视图空间
                        let content = placement.page_to_content(*point);
                        let center = canvas.min + EVec2::new(content.x - scroll.x, content.y - scroll.y);
                        painter.circle_stroke(center, 5.0, Stroke::new(1.5, LINK_OVERLAY_COLOR));
                        painter.circle_filled(center, 1.5, LINK_OVERLAY_COLOR);
                    }
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
                    Some(DragMode::Pan) => {
                        // 已在绘制之前当帧处理（见"拖动当帧生效"），这里不再重复提交
                    }
                    None => {}
                }
            }

            // 拖动过程诊断：把 egui 报的状态与下一帧要执行的滚动量打出来（PDF_READER_DIAGNOSE=1）
            if self.diagnose
                && (response.drag_started()
                    || response.dragged()
                    || response.drag_stopped()
                    || response.clicked())
            {
                eprintln!(
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
                );
            }
            if response.drag_stopped() {
                if self.diagnose {
                    let total = self
                        .drag_started_at
                        .map(|start| start.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or(0.0);
                    eprintln!(
                        "[时序] 拖动结束 t={:.6}s 全程={:.1}ms",
                        self.started_at.elapsed().as_secs_f64(),
                        total
                    );
                }
                self.drag = None;
                self.drag_started_at = None;
                self.last_pan_at = None;
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
            } else if self.capture.is_none() && response.clicked() {
                if let Some(position) = response.interact_pointer_pos() {
                    let content = canvas_to_content(canvas, position, scroll);
                    // 链接优先：命中热区就跳转（不论当前工具）—— 目的地有坐标时居中（用户要求）
                    if !self.follow_link(session, content) && select_tool {
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

            // 浮窗**最后**绘制与注册 → 位于最上层；后加入的在更上层（见 draw_floats）。
            // 截图模式下所有浮窗不显现（连交互也不注册，避免吃掉框选拖动）。
            if self.capture.is_none() {
                self.draw_floats(ui, canvas);
            } else {
                self.draw_capture(ui, canvas, session, scroll);
            }
        });
    }

    /// 添加一个"测试浮窗"（带任意文字），位置按已有浮窗数量级联偏移。
    fn add_test_float(&mut self) {
        let id = self.next_float_id;
        self.next_float_id += 1;
        let cascade = ((id as f32 - 1.0) % 6.0) * 24.0;
        let rect = egui::Rect::from_min_size(
            egui::pos2(24.0 + cascade, 24.0 + cascade),
            EVec2::new(280.0, 150.0),
        );
        self.floats.push(Float {
            id,
            title: format!("测试浮窗 #{id}"),
            content: FloatContent::Text(format!(
                "这是第 {id} 个测试浮窗。\n目前只显示文字。\n\n可以拖动标题栏移动、\n拖右下角改变大小、\n点右上角 ✕ 关闭。"
            )),
            rect,
        });
        self.message = format!("已添加测试浮窗 #{id}");
    }

    /// 截图模式：框选区域 + 半透明蒙层（矩形内透明）+ Enter/Esc 退出。
    ///
    /// * 蒙层用"绕开选区的四块"实现（egui 没有擦除混合，见 `view::veil_bands`）；
    /// * 拖动即框选；松开后选区保留，可再次拖动重画；
    /// * `Enter` → 终端输出区域信息（视图坐标 + 覆盖页的页面空间矩形）并退出；
    /// * `Esc` → 直接退出，无输出。
    fn draw_capture(
        &mut self,
        ui: &mut egui::Ui,
        canvas: egui::Rect,
        session: &DocumentSession,
        scroll: Vec2,
    ) {
        // ---- 框选 ----
        let response = ui.interact(canvas, Id::unique("capture-area"), Sense::drag());
        if let Some(position) = response.interact_pointer_pos() {
            if response.drag_started() {
                self.capture.as_mut().unwrap().anchor = Some(position);
                self.capture.as_mut().unwrap().rect = None;
            }
            if response.dragged() {
                if let Some(anchor) = self.capture.as_ref().and_then(|state| state.anchor) {
                    let rect = egui::Rect::from_two_pos(anchor, position).intersect(canvas);
                    self.capture.as_mut().unwrap().rect = Some(rect);
                }
                ui.ctx().request_repaint();
            }
        }
        if response.drag_stopped() {
            self.capture.as_mut().unwrap().anchor = None;
        }

        // ---- 蒙层（选区透明）+ 选区边框 ----
        let selection = self.capture.as_ref().and_then(|state| state.rect);
        let veil_color = Color32::from_rgba_unmultiplied(225, 225, 225, 110);
        for band in veil_bands(canvas, selection) {
            ui.painter().rect_filled(band, CornerRadius::ZERO, veil_color);
        }
        if let Some(selection) = selection {
            ui.painter().rect_stroke(
                selection,
                CornerRadius::ZERO,
                Stroke::new(1.5, Color32::from_rgb(60, 120, 220)),
                StrokeKind::Inside,
            );
        }

        // ---- Enter / Esc ----
        let (enter, escape) = ui.ctx().input(|input| {
            (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape))
        });
        if enter {
            match selection {
                Some(selection) => {
                    // 屏幕矩形 → 内容空间矩形（供快照与页面空间换算使用）
                    let selection_rect =
                        screen_rect_to_content(canvas.min, selection, scroll);
                    // 1) 把选区做成**自包含快照**并放进一个新浮窗（用户需求）
                    self.capture_into_float(ui, session, selection_rect);
                    let mut parts: Vec<String> = Vec::new();
                    for page in self.visible.clone() {
                        let Some(placement) = session.placement(page).copied() else { continue };
                        // 注意：`selection_rect` 已是**内容空间**，故这里传零滚动
                        if let Some(page_rect) =
                            view_rect_to_page_rect(&placement, selection_rect, Vec2::ZERO)
                        {
                            parts.push(format!(
                                "页 {} 页空间 ({:.1},{:.1} {:.1}×{:.1})",
                                page.display_number(),
                                page_rect.x,
                                page_rect.y,
                                page_rect.width,
                                page_rect.height
                            ));
                        }
                    }
                    eprintln!(
                        "[截图] 视图区域 ({:.1},{:.1} {:.1}×{:.1}){}",
                        selection.min.x,
                        selection.min.y,
                        selection.width(),
                        selection.height(),
                        if parts.is_empty() {
                            String::new()
                        } else {
                            format!(" | {}", parts.join(" | "))
                        }
                    );
                    self.message = format!(
                        "截图区域已输出：{:.0}×{:.0}",
                        selection.width(),
                        selection.height()
                    );
                }
                None => {
                    eprintln!("[截图] 未框选任何区域");
                    self.message = "截图模式：未框选区域".to_string();
                }
            }
            self.capture = None;
        } else if escape {
            self.capture = None;
            self.message = "已退出截图模式".to_string();
        }
    }

    /// 把选中的区域做成截图快照，放入一个新的浮窗。
    ///
    /// 快照来源是**已经渲染好的位图**（整页或瓦片，直接从缓存取），因此无需重新渲染；
    /// 拷贝成独立纹理后与主视图状态完全解耦：之后缩放/滚动/换页都不会改变浮窗内容。
    fn capture_into_float(
        &mut self,
        ui: &mut egui::Ui,
        session: &DocumentSession,
        // `content_rect` 已经是**内容空间**矩形（调用方用 `screen_rect_to_content` 换算过）
        content_rect: Rect,
    ) {
        // `content_rect` 由调用方从屏幕坐标换算而来（**必须减去画布原点**，见 view::screen_rect_to_content）

        // 收集覆盖到的页及其**已缓存**位图（先持有 Arc，再借用给合成函数）
        let mut owned: Vec<(Rect, Option<Arc<RasterImage>>, Vec<(TileSpec, Arc<RasterImage>)>)> =
            Vec::new();
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else { continue };
            if placement.rect.intersection(content_rect).is_none() {
                continue;
            }
            owned.push((placement.rect, session.image_for(page), session.tiles_for(page)));
        }
        let sources: Vec<SnapshotPage<'_>> = owned
            .iter()
            .map(|(rect, whole, tiles)| SnapshotPage {
                rect: *rect,
                whole: whole.as_deref(),
                tiles: tiles.iter().map(|(tile, image)| (*tile, image.as_ref())).collect(),
            })
            .collect();

        let Some(snapshot) = compose_snapshot(&sources, content_rect) else {
            self.message = "截图：该区域还没有已渲染的像素（请稍候再试）".to_string();
            return;
        };

        let id = self.next_float_id;
        self.next_float_id += 1;
        let texture = ui.ctx().load_texture(
            format!("snapshot-{id}"),
            snapshot,
            egui::TextureOptions::LINEAR,
        );
        // 默认窗口大小按区域尺寸给（夹取在合理范围内），之后可自由改大小
        let size = EVec2::new(
            content_rect.width.clamp(FLOAT_MIN_SIZE.x, 640.0),
            content_rect.height.clamp(FLOAT_MIN_SIZE.y, 640.0) + FLOAT_TITLE_HEIGHT,
        );
        let cascade = ((id as f32 - 1.0) % 6.0) * 24.0;
        self.floats.push(Float {
            id,
            title: format!("截图 #{id}"),
            content: FloatContent::Image(texture),
            rect: egui::Rect::from_min_size(egui::pos2(32.0 + cascade, 32.0 + cascade), size),
        });
        self.message = format!("已把截图放入浮窗 #{id}");
    }

    /// 绘制并处理浮窗：移动（拖标题栏）、改大小（拖右下角）、关闭（右上角 ✕）。
    ///
    /// 注册顺序即层级：按 `floats` 顺序注册 → **后加入的浮窗既画在上面、也优先拿到鼠标**
    /// （用户要求"后出现的浮窗更高"）。子区域（标题栏/把手/关闭按钮）在主提之后注册，
    /// 因此优先于主体，不会被主体吞掉。
    fn draw_floats(&mut self, ui: &mut egui::Ui, canvas: egui::Rect) {
        if self.floats.is_empty() {
            return;
        }
        let mut close: Option<u64> = None;
        for index in 0..self.floats.len() {
            // 每帧夹取回视图区域内（用户要求：浮窗始终在视图区域内部）
            let rect = clamp_float_rect(self.floats[index].rect, canvas, FLOAT_MIN_SIZE);
            self.floats[index].rect = rect;
            let id = self.floats[index].id;

            // 主体：占住点击，避免"点在浮窗上却给文档落了光标"；不参与拖动
            let _ = ui.interact(rect, Id::unique(("float", id)), Sense::click());

            // 标题栏：拖动移动
            let title_rect =
                egui::Rect::from_min_size(rect.min, EVec2::new(rect.width(), FLOAT_TITLE_HEIGHT));
            let title = ui.interact(title_rect, Id::unique(("float-title", id)), Sense::drag());
            if title.dragged() {
                self.floats[index].rect = rect.translate(title.drag_delta());
            }

            // 右下角把手：拖动改大小（不小于最小尺寸）
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - FLOAT_HANDLE_SIZE, rect.bottom() - FLOAT_HANDLE_SIZE),
                EVec2::splat(FLOAT_HANDLE_SIZE),
            );
            let handle = ui.interact(handle_rect, Id::unique(("float-resize", id)), Sense::drag());
            if handle.dragged() {
                let delta = handle.drag_delta();
                self.floats[index].rect = egui::Rect::from_min_size(
                    rect.min,
                    EVec2::new(
                        (rect.width() + delta.x).max(FLOAT_MIN_SIZE.x),
                        (rect.height() + delta.y).max(FLOAT_MIN_SIZE.y),
                    ),
                );
            }

            // 右上角关闭按钮：**画成带底色的圆**，悬停/按下变红 —— 否则只有一个"✕"太不显眼
            let close_center = egui::pos2(title_rect.right() - 13.0, title_rect.center().y);
            let close_rect = egui::Rect::from_center_size(close_center, EVec2::splat(18.0));
            let close_response = ui.interact(close_rect, Id::unique(("float-close", id)), Sense::click());
            let close_color = if close_response.is_pointer_button_down_on() {
                Color32::from_rgb(170, 45, 45)
            } else if close_response.hovered() {
                Color32::from_rgb(205, 70, 70)
            } else {
                Color32::from_gray(95)
            };
            ui.painter().circle_filled(close_center, 8.5, close_color);
            ui.painter().text(
                close_center,
                Align2::CENTER_CENTER,
                "✕",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            if close_response.on_hover_text("关闭浮窗").clicked() {
                close = Some(id);
            }

            // ---- 绘制 ----
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(30, 32, 36));
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(1.0, Color32::from_gray(130)),
                StrokeKind::Inside,
            );
            painter.rect_filled(title_rect, CornerRadius::same(4), Color32::from_rgb(54, 58, 66));
            painter.text(
                title_rect.left_center() + EVec2::new(8.0, 0.0),
                Align2::LEFT_CENTER,
                &self.floats[index].title,
                egui::FontId::proportional(13.0),
                Color32::WHITE,
            );
            // 缩放手柄示意
            painter.rect_filled(handle_rect.shrink(5.0), CornerRadius::ZERO, Color32::from_gray(120));
            match &self.floats[index].content {
                FloatContent::Text(text) => {
                    painter.text(
                        rect.min + EVec2::new(10.0, FLOAT_TITLE_HEIGHT + 8.0),
                        Align2::LEFT_TOP,
                        text,
                        egui::FontId::proportional(13.0),
                        Color32::from_gray(225),
                    );
                }
                FloatContent::Image(texture) => {
                    // 正文区域：按**保持长宽比**放进窗口（居中、不拉伸变形）；
                    // 改变窗口大小只影响显示尺寸，图片内容本身不变。
                    let body = egui::Rect::from_min_max(
                        rect.min + EVec2::new(4.0, FLOAT_TITLE_HEIGHT + 4.0),
                        rect.max - EVec2::new(4.0, 4.0),
                    );
                    let size = texture.size_vec2();
                    let target = fit_inside(size, body);
                    painter.image(
                        texture.id(),
                        target,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
        }
        if let Some(id) = close {
            self.floats.retain(|window| window.id != id);
            self.message = format!("已关闭浮窗 #{id}");
        }
    }

    /// 命中当前页的链接热区则发出跳转命令，返回是否命中。
    ///
    /// 热区判断（点在哪个链接里）在核心层：[`DocumentSession::link_at`]；
    /// 这里只负责"把内容空间的点交给它"，以及把结果翻译成命令
    /// （[`crate::view::commands_for_destination`]：有落点 → 居中，无落点 → 顶部对齐）。
    fn follow_link(&mut self, session: &DocumentSession, content: Point) -> bool {
        for page in self.visible.clone() {
            let Some(placement) = session.placement(page).copied() else { continue };
            if !placement.rect.contains(content) {
                continue;
            }
            let page_point = placement.content_to_page(content);
            if let Some(destination) = session.link_at(page, page_point) {
                match &destination {
                    Destination::Uri(uri) => {
                        self.message = format!("外部链接：{uri}（打开浏览器暂未实现）");
                    }
                    Destination::Page { .. } => {
                        self.commands.extend(commands_for_destination(&destination));
                    }
                }
                return true;
            }
            return false; // 点在页面上但没有链接
        }
        false
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

        // 拖放打开：**空状态下也必须处理**（v1 只在已有会话时读取，导致启动后拖入无反应）
        self.handle_dropped_files(&ctx);

        // 把会话临时取出，避免与 `self` 的其它字段发生借用冲突
        let Some(mut session) = self.session.take() else {
            self.draw_empty(ui);
            self.draw_password_prompt(&ctx);
            return;
        };

        let frame_start = std::time::Instant::now();
        self.collect_commands(&ctx, &session);
        if self.capture.is_some() {
            // 丢弃工具栏等来源的命令：截图期间不改动主视图内容；
            // 注意仍然要跑 `apply_and_pump` —— 它负责 `pump`，瓦片加载得以继续。
            self.commands.clear();
        }
        self.apply_and_pump(&ctx, &mut session);
        let after_input = frame_start.elapsed();

        // 检测"缩放/旋转刚刚变过"：连续缩放期间不提交渲染，只显示旧图拉伸（见 ZOOM_PREVIEW_WINDOW）
        let zoom_now = session.state().zoom;
        let rotation_now = session.state().user_rotation;
        if (zoom_now - self.last_zoom).abs() > f32::EPSILON || rotation_now != self.last_rotation {
            self.last_zoom = zoom_now;
            self.last_rotation = rotation_now;
            self.preview_until = Some(std::time::Instant::now() + ZOOM_PREVIEW_WINDOW);
        }
        let previewing = self.previewing();

        self.last_convert_ms = 0.0;
        self.last_upload_ms = 0.0;
        // 拖动期间**暂缓**转换与上传：把"新页进入"那一帧的逐像素开销挪到松手之后，
        // 拖动本身只需保证画面跟着走（未就绪的区域显示旧图或白纸）。
        let dragging = self.drag.is_some();
        let uploaded = self.ensure_textures(&ctx, &session, !dragging);
        let after_textures = frame_start.elapsed();
        if uploaded > 0 {
            // 本帧上传过 → 继续重绘，把"松手后补齐"的过程走完（拖动期间暂缓的上传会在这里补上）
            ctx.request_repaint();
        }

        self.draw_toolbar(ui, &session);
        self.draw_sidebar(ui, &session);
        self.draw_status(ui, &session);
        self.draw_canvas(ui, &mut session);
        let after_draw = frame_start.elapsed();

        // 每帧耗时分解：拖动期间（或单帧明显慢时）打印，用来定位"不跟手"到底花在哪
        if self.diagnose {
            let total_ms = after_draw.as_secs_f64() * 1000.0;
            if self.drag.is_some() || total_ms > 25.0 {
                eprintln!(
                    "[帧] 总={:.1}ms 输入+pump={:.1}ms 纹理={:.1}ms(转换={:.1} 上传={:.1}) 绘制={:.1}ms 纹理数={} 命中={} 可见页={} 大纲={} 在飞={}",
                    total_ms,
                    after_input.as_secs_f64() * 1000.0,
                    (after_textures - after_input).as_secs_f64() * 1000.0,
                    self.last_convert_ms,
                    self.last_upload_ms,
                    (after_draw - after_textures).as_secs_f64() * 1000.0,
                    self.textures.len(),
                    self.hits.len(),
                    self.visible.len(),
                    self.navigation
                        .as_ref()
                        .map(|navigation| outline_node_count(&navigation.outline))
                        .unwrap_or(0),
                    session.pending_renders()
                );
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
            if let Some(deadline) = self.preview_until {
                ctx.request_repaint_after(deadline.saturating_duration_since(std::time::Instant::now()));
            }
        } else if pending > 0 {
            ctx.request_repaint();
        }
        self.draw_password_prompt(&ctx);
    }
}

impl PdfReaderApp {
    /// 是否正处于"缩放/旋转预览窗口"内（此刻只显示旧图，不提交渲染）。
    fn previewing(&self) -> bool {
        self.preview_until
            .map_or(false, |deadline| std::time::Instant::now() < deadline)
    }

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

/// 只铺白色、**不画任何边框**。
///
/// 用于"尚未就绪的瓦片"：整页已经铺过纸张（`paint_placeholder`），若每块缺失瓦片再描一圈灰边，
/// 用户就会看到满屏网格线（第四轮验收反馈）。
fn paint_blank(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
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


/// 让 `Align2` 与 `FontId` 在需要时可用（保持导入稳定）。
#[allow(dead_code)]
fn _unused_imports() -> (Align2, egui::FontId) {
    (Align2::CENTER_CENTER, egui::FontId::proportional(12.0))
}
