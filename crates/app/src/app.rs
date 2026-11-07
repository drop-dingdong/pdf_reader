//! egui/eframe 外壳：把 [`DocumentSession`] 的"命令进、事件出"模型接到界面上。
//!
//! ## 本模块的定位
//! **只做胶水**：坐标换算、输入映射、纹理存储都在 [`crate::view`] 里（可单测）。
//! 剩下的部分需要真实窗口才能验证，按 agent.md 规则 2 **交由用户验收**。
//!
//! ## 每帧流程（与 `docs/architecture.md` §4.2 的契约一致）
//! 1. 采集输入 → 命令；2. `handle` 命令；3. `pump` 取回渲染结果；
//! 4. 画页面（就绪则贴纹理，未就绪则白底页框）；5. 画高亮；6. 请求可见页渲染。
//!
//! ## 与 egui 0.37 的适配
//! 0.37 的 `eframe::App` 只提供 `fn ui(&mut self, ui: &mut Ui, frame)`（不再给 `Context`），
//! 侧边/上下栏统一为 [`egui::Panel`]，因此本模块从 `ui.ctx()` 取 `Context` 做纹理与剪贴板操作。

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use egui::{Color32, Vec2 as EVec2};
use pdfcore::annotation::{AnnotationId, AnnotationStore};
use pdfcore::prelude::*;

use crate::ink::InkStroke;

mod annotate;
mod body_editor;
mod canvas;
mod capture;
mod dialogs;
mod document;
mod edit;
mod floats;
mod frame;
mod hover;
mod model;
mod modes;
mod paint;
/// 无窗口回放器（脚本驱动真实 `App::ui`，用于回归断言与性能基线，见 D-103）。
pub mod replay;
mod save;
mod sidebar;
mod state;
mod textures;
mod toolbar;

use crate::view::{commands_for_destination, outline_node_count, Desired, KeyedStore, PlanStamp};
pub(super) use model::*;
use paint::*;
use state::{AnnotationPdfState, MarkdownState, RuntimeState, SaveFlowState};

/// 应用状态。
pub struct PdfReaderApp {
    session: Option<DocumentSession>,
    navigation: Option<Arc<NavigationIndex>>,
    textures: KeyedStore<egui::TextureHandle>,
    /// D-96：每页"当前波段从何时开始"（`(scale_milli, 起始时刻)`），用于就绪门槛的超时兜底。
    band_since: std::collections::HashMap<PageIndex, (u32, std::time::Instant)>,
    /// 每页上一帧的画法（结构化）。既用于 `[画法]` 的边沿触发输出，也供回放器读取。
    last_plan: std::collections::HashMap<PageIndex, PlanStamp>,
    /// 本次打开是否发生过**存档版本迁移**（旧版本 → 当前版本）：是则置脏，提示保存。
    sidecar_migrated: bool,
    /// 同目录存档由**更新版本**写入：不加载、也不允许覆盖（`(存档版本, 本程序版本)`，D-107）。
    sidecar_locked: Option<(u32, u32)>,
    /// 每帧临时持有的命令与事件缓冲（避免反复分配）。
    commands: Vec<ViewerCommand>,
    events: Vec<SessionEvent>,
    visible: Vec<PageIndex>,
    drag: Option<DragMode>,
    /// 拖拽语义（工具栏可切换）。
    tool: Tool,
    /// 是否画出链接热区框（工具栏 checkbox，默认关闭）。
    show_links: bool,
    /// 是否绘制标注（D-118；默认开启）。
    show_annotations: bool,
    /// 页面标注（会话内；S2 才落盘）。
    annotations: AnnotationStore,
    /// 下一次 `open` 是写回后的重开：保留撤销栈与对象号映射。
    reopening_after_write: bool,
    /// 正在画的一笔墨迹（S4b）；**抬笔之前它不是标注**，取消只要丢掉它。
    ink: Option<InkStroke>,
    /// "下一步把当前选区变成标注"（快捷键一次性动作）。
    pending_annotate: Option<MarkupTool>,
    /// 当前持有哪种标记工具：点按钮进入，拖选文字后自动施加，Esc/再点退出。
    annotation_tool: Option<MarkupTool>,
    /// 本次拖选是否从文字上开始（避免在空白处拖拽时误用旧选区）。
    selection_started_on_text: bool,
    /// 新文本标记的颜色（样式栏）。
    markup_color: [u8; 3],
    /// 新高亮的透明度（下划线/删除线固定不透明）。
    markup_opacity: f32,
    /// 新墨迹的颜色。
    ink_color: [u8; 3],
    /// 进入标记模式前的工具，退出时恢复。
    tool_before_annotation: Option<Tool>,
    /// 原地写入重开后要恢复的视图（页、缩放、滚动、剩余帧数）。
    ///
    /// 延迟两帧：**先让新会话收到真实的视口尺寸**，否则 `SetScroll` 会被按旧/默认视口夹紧，
    /// 表现为"缩放对了、滚动偏了一点"（用户实测）。
    pending_view_restore: Option<(PageIndex, f32, Option<Vec2>, u8)>,
    /// 注释是否有未写入 PDF 的新增/正文/删除。
    pdf_dirty: bool,
    /// 上次成功写 PDF / 打开时的历史修订号（与当前修订号比较得出脏位）。
    saved_pdf_revision: u64,
    /// 原地保存前是否留一份 `.bak` 副本（设置项，默认关闭）。
    backup_before_inplace: bool,
    /// 上次成功保存自定义数据 / 打开时自定义数据的指纹。
    data_clean_hash: Option<u64>,
    /// 编辑历史（注释 + 重选截图/引用点，单栈时间序）。
    history: crate::history::History<AppEdit>,
    /// 是否显示"编辑历史"可视列表（首次撤销时自动打开）。
    show_history: bool,
    /// 本帧指针悬停在哪个标注上（`Delete` 用它决定删谁）。
    hovered_annotation: Option<AnnotationId>,
    /// 大纲的**扁平行**（打开文档时摊平一次；侧栏用虚拟化行渲染，避免每帧布局上千行）。
    outline_rows: Vec<crate::view::OutlineRow>,
    /// 视图区域内的浮窗；**后加入的排在后面 → 绘制与交互都在更上层**（用户要求"后出现的浮窗更高"）。
    floats: Vec<Float>,
    /// 下一个浮窗 id。
    next_float_id: u64,
    /// 是否标注"被引用的点"（链接跳转目的地）。
    show_dest_points: bool,
    /// 每个目标页上被引用的点（按 0.5pt 去重）及其矩形/截图，下标 = 页号；打开文档时算一次。
    dest_marks: Vec<Vec<DestMark>>,
    /// 截图模式状态（`Some` = 处于该模式）。
    capture: Option<CaptureState>,
    /// 悬停预览浮窗（鼠标在页内链接上时出现）。
    hover_tip: Option<HoverTip>,
    /// 上一帧鼠标所在链接框的尺寸（仅用于计算临时浮窗位置）
    hover_link_size: egui::Vec2,
    /// 滚动/缩放"刚刚发生过"的截止时刻：这段时间内不显示悬停预览（用户要求）。
    scroll_quiet_until: Option<std::time::Instant>,
    /// 本帧需要**高亮**的浮窗 id（悬停到已固定的同一目的地时设置，仅维持一帧）。
    highlight_float: Option<u64>,
    /// 当前文档的路径（用于保存/查找同目录的存档文件）。
    document_path: Option<std::path::PathBuf>,
    /// 打开时记录的文件身份；写回前若变化则拒绝覆盖。
    pdf_file_stamp: Option<PdfFileStamp>,
    /// 便签模式：下一次点击页面时在该位置新建便签。
    note_pin: bool,
    /// 新建便签的输入对话框。
    note_dialog: Option<NoteDraft>,
    /// 正在 Alt/Ctrl+拖拽平移的墨迹。
    ink_move: Option<InkMoveState>,
    /// 右键「移动」选中的墨迹：下一次拖动它时不需要按修饰键。
    move_ink_armed: Option<AnnotationId>,
    /// 自定义 preview 的 Pin 状态（`Some` = 正在选点；此时不显示浮窗、不响应链接点击）。
    custom_pin: Option<CustomKind>,
    /// 自定义 preview 的输入对话框。
    custom_dialog: Option<CustomDialog>,
    /// 已保存的自定义 preview。
    custom_previews: Vec<CustomPreview>,
    /// 每页下一个可用的 custom preview id（会话内）。
    next_preview_id: Vec<u64>,
    /// 连续的 preview resize 合并窗口。
    last_preview_resize: Option<(PreviewKey, std::time::Instant)>,
    /// 连续的引用点浮窗 resize 合并窗口。
    last_dest_resize: Option<((PageIndex, usize), std::time::Instant)>,
    /// 自定义数据是否有未保存的修改（影响保存按钮文案）。
    data_dirty: bool,
    /// 页码原位编辑草稿（`Some` = 正在编辑）。
    page_edit: Option<String>,
    /// 缩放比原位编辑草稿。
    zoom_edit: Option<String>,
    /// 缩放菜单当前的模式标题。
    zoom_mode: ZoomMode,
    /// 哪个原位编辑框需要在**下一帧**获得焦点（双击后直接进入真编辑）。
    focus_edit: Option<EditField>,
    /// 正文原位编辑是否需要在当帧获得焦点（与 `focus_edit` 同理，但目标在浮窗里）。
    focus_body_edit: bool,
    /// 搜索框是否需要在当帧获得焦点（Ctrl+F 直接可输入）。
    focus_search: bool,
    /// 是否处于全屏模式（窗口最大化、去标题栏、顶栏改为悬浮显示）。
    fullscreen: bool,
    /// 全屏下顶栏是否**悬浮显示**（指针靠近窗口顶端时出现）。
    reveal_toolbar: bool,
    /// 是否横向浏览（页从左到右排成一行）。
    horizontal_layout: bool,
    /// 勾选后：**纵向滚动一律转为横向滚动**（原始横向分量丢弃）。
    horizontal_scroll: bool,
    /// 待应用的布局切换（`Some(true)` = 横向），在拿到 `&mut session` 时执行。
    pending_layout: Option<bool>,
    /// 底部搜索框是否显示（Ctrl+F 切换）。
    search_open: bool,
    /// 正在编辑题目 `(下标, 草稿)`。
    editing_title: Option<(usize, String)>,
    /// 正在编辑正文 `(下标, 草稿)`（文字预览：固定浮窗内双击正文进入）。
    editing_body: Option<(FloatOwner, String)>,
    /// **链接 → 被引用点** 的登记表（与 `navigation.links_by_page` 同构）：
    /// 下标与 `links_on(page)` 一一对应。打开文档时解析一次，
    /// 之后悬停与跳转都直接用它（用户建议：记住索引，而不是每次按位置查）。
    ///
    /// 前提：打开后 PDF 内容不再变化（不会插入新链接）—— 用户已确认该假设成立。
    link_marks: Vec<Vec<Option<(PageIndex, usize)>>>,
    /// 侧栏宽度：**由 app 自己保存**。
    ///
    /// egui 的面板尺寸以 `parent_ui.scope_id().with_salt(...)` 为键持久化，
    /// 而 eframe 每帧给的是一个新的根 `Ui`，于是 egui 侧的持久化读不回来 ——
    /// 人工验收看到的"拖动边框、松手回弹"就是这个原因。改为每帧把上次观察到的宽度
    /// 作为 `default_size` 回填，尺寸便由我们自己保住。
    sidebar_width: f32,
    /// 用户设置（注释默认样式 / 每个文件的阅读状态 / 窗口尺寸）。
    settings: crate::settings::Settings,
    /// 设置文件路径；`None` = 不读写（无窗口/回放/测试）。
    settings_path: Option<PathBuf>,
    /// 设置是否有未落盘的修改。
    settings_dirty: bool,
    /// 首帧要恢复的窗口尺寸（`None` = 不动）。
    pending_window_size: Option<[f32; 2]>,
    /// 在绘制过程中请求打开的文件：必须等 `session` 放回 `self` 之后再打开，
    /// 否则 `ui()` 末尾会把旧 session 又塞回去（菜单里直接 `open` 的坑）。
    pending_open: Option<PathBuf>,
    /// 新墨迹的线宽（来自设置）。
    ink_width: f32,
    /// 待输入的密码（文档加密时）。
    password_for: Option<PathBuf>,
    password_input: String,
    search_input: String,
    /// 页码输入框（第二轮验收：支持键入页码跳转）。
    page_input: String,
    hits: Vec<SearchHit>,
    active_hit: Option<usize>,
    /// 侧栏是否显示（原"目录"按钮，现在叫"侧栏"）。
    show_outline: bool,
    /// 侧栏当前标签页。
    sidebar_tab: SidebarPane,
    /// 注释列表：按类型过滤。
    annotation_show_highlight: bool,
    annotation_show_underline: bool,
    annotation_show_strike: bool,
    annotation_show_ink: bool,
    annotation_show_note: bool,
    /// 注释列表：只看外部/只读注释。
    annotation_only_external: bool,
    annotation_only_readonly: bool,
    /// 注释列表视图（按页 / 最近修改）。
    annotation_view: AnnotationListView,
    /// 注释列表搜索词（正文/作者，忽略大小写）。
    annotation_search: String,
    /// 按页视图里被折叠的页。
    collapsed_annotation_pages: std::collections::HashSet<u32>,
    /// 注释列表点击后的中性定位轮廓（id + 截止时刻）。
    annotation_focus: Option<(AnnotationId, std::time::Instant)>,
    /// 画布右键菜单锚定的注释（菜单/子菜单打开期间保持）。
    context_annotation: Option<AnnotationId>,
    /// 自定义右键菜单的屏幕位置；`None` = 关闭。
    context_menu_pos: Option<egui::Pos2>,
    /// Pin 条目悬浮响应开关（勾选 = 悬浮时显示临时预览/高亮旧浮窗）。
    pin_hover_enabled: bool,
    /// 文字浮窗是否以 Markdown 渲染（`settings.json`，默认关闭）。
    markdown_floats: bool,
    /// 侧栏 hover 临时预览是否也按 Markdown 渲染（设置项）。
    hover_preview_markdown: bool,
    /// 是否允许复制外部注释的链接（跨会话不保证准确；设置项，默认关闭）。
    allow_external_annotation_links: bool,
    /// 本次文档加载的 token；外部注释链接跨会话/重开时失效。
    document_load_token: u64,
    /// 等待用户确认的外部注释链接（属性不足以完全验证时）。
    pending_external_link: Option<PendingExternalLink>,
    /// 拖入文件 hover 日志只打一次，避免 Wayland 调度每帧刷屏。
    drag_hover_logged: bool,
    /// 数学公式支持/限制帮助窗口是否打开。
    math_help: bool,
    /// 正文编辑会话号：每次打开换一个 ScrollArea id，避免不同文本继承滚动位置。
    body_edit_session: u64,
    /// 首次打开编辑器时要放置的字符光标（双击位置映射；✎ 为 0）。
    body_edit_cursor: Option<usize>,
    /// 侧栏 Pin 列表里正在被悬浮的条目（下标 + 该行的屏幕矩形，用于定位临时浮窗）。
    custom_hover: Option<(usize, egui::Rect)>,
    /// 上一帧主视图（画布）的矩形：侧栏先于画布绘制，用它给浮窗定位。
    canvas_rect: egui::Rect,
    /// 画布所在图层；用于区分画布本层和盖在上面的编辑窗口/弹层。
    canvas_layer: egui::LayerId,
    /// 运行时诊断/时间状态（不持久化）。
    runtime: RuntimeState,
    /// 保存/关闭流程的对话框与一次性状态。
    save_flow: SaveFlowState,
    /// Markdown/数学/内部图片的会话内缓存。
    markdown: MarkdownState,
    /// 注释/PDF 对象号的会话内簿记。
    annotation_pdf: AnnotationPdfState,
    /// 点击 Pin 条目后的一次性高亮（浮窗 id + 截止时刻）。
    click_highlight: Option<(u64, std::time::Instant)>,
    /// 状态栏文本与错误提示。
    message: String,
    error: Option<String>,
}

impl PdfReaderApp {
    /// 构造；`initial` 为命令行传入的文件路径。
    ///
    /// 这里顺手关掉 egui 内建的"Ctrl/⌘ + `+`/`-`/`0` 缩放整个界面"快捷键
    /// （`Options::zoom_with_keyboard`）：否则按 `Ctrl`+`+` 会**同时**放大 UI 与文档页面
    /// （人工验收反馈的现象）。关掉之后这些键由本应用统一解释为**文档缩放**。
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        let settings_path = crate::settings::Settings::path();
        let had_settings = settings_path.as_ref().is_some_and(|path| path.exists());
        let (settings, warning) = crate::settings::Settings::load();
        let mut app = Self::with_clock(
            crate::clock::Clock::system(),
            crate::diag::Diagnostics::from_env(),
            initial,
            settings,
            settings_path,
            warning,
        );
        // 窗口系统在创建时还不知道设置；只有**已有设置文件**时才恢复尺寸，
        // 首次启动保持 main 的默认窗口大小。
        if had_settings {
            app.pending_window_size = Some(app.settings.ui.window_size);
        }
        app
    }

    /// 设置纹理层字节预算（0 = 不限制）。给回放/调参用；正常路径由构造时读取。
    pub fn set_texture_budget(&mut self, bytes: u64) {
        self.textures.set_max_bytes(bytes);
        self.runtime.budget_warned = false;
    }

    /// **无窗口**构造：自己提供时钟与诊断去处（回放器 / 集成测试用）。
    ///
    /// 与 [`PdfReaderApp::new`] 的唯一区别就是不碰 `CreationContext`；其余行为完全一致。
    pub fn new_headless(
        clock: crate::clock::Clock,
        diag: crate::diag::Diagnostics,
        initial: Option<PathBuf>,
    ) -> Self {
        let mut app = Self::with_clock(
            clock,
            diag,
            initial,
            crate::settings::Settings::default(),
            None,
            None,
        );
        // 无窗口时没有窗口尺寸，给一个明确的默认视口（回放脚本也可以覆盖它）。
        app.commands
            .push(ViewerCommand::ResizeViewport(Size::new(1100.0, 900.0)));
        app
    }

    fn with_clock(
        clock: crate::clock::Clock,
        diag: crate::diag::Diagnostics,
        initial: Option<PathBuf>,
        settings: crate::settings::Settings,
        settings_path: Option<PathBuf>,
        settings_warning: Option<String>,
    ) -> Self {
        let style = settings.annotation.clone();
        let sidebar_width = settings.ui.sidebar_width;
        let ui_settings = settings.ui.clone();
        let save_settings = settings.save.clone();
        let mut app = Self {
            session: None,
            navigation: None,
            textures: {
                let mut textures =
                    KeyedStore::with_bytes(TEXTURE_CAPACITY, crate::view::texture_bytes);
                textures.set_max_bytes(texture_budget_bytes());
                textures
            },
            band_since: std::collections::HashMap::new(),
            last_plan: std::collections::HashMap::new(),
            sidecar_migrated: false,
            sidecar_locked: None,
            commands: Vec::new(),
            events: Vec::new(),
            visible: Vec::new(),
            drag: None,
            tool: Tool::default(),
            show_links: ui_settings.show_links,
            show_annotations: ui_settings.show_annotations,
            annotations: AnnotationStore::new(),
            reopening_after_write: false,
            ink: None,
            pending_annotate: None,
            annotation_tool: None,
            selection_started_on_text: false,
            markup_color: style.markup_color,
            markup_opacity: style.highlight_opacity,
            ink_color: style.ink_color,
            ink_width: style.ink_width,
            settings,
            settings_path,
            settings_dirty: false,
            pending_window_size: None,
            pending_open: None,
            tool_before_annotation: None,
            pending_view_restore: None,
            pdf_dirty: false,
            saved_pdf_revision: 0,
            backup_before_inplace: save_settings.backup_in_place,
            data_clean_hash: None,
            history: crate::history::History::new(),
            show_history: false,
            hovered_annotation: None,
            outline_rows: Vec::new(),
            floats: Vec::new(),
            next_float_id: 1,
            show_dest_points: ui_settings.show_dest_points,
            dest_marks: Vec::new(),
            capture: None,
            hover_tip: None,
            hover_link_size: EVec2::new(160.0, 20.0),
            scroll_quiet_until: None,
            highlight_float: None,
            document_path: None,
            pdf_file_stamp: None,
            note_pin: false,
            note_dialog: None,
            ink_move: None,
            move_ink_armed: None,
            custom_pin: None,
            custom_dialog: None,
            custom_previews: Vec::new(),
            next_preview_id: Vec::new(),
            last_preview_resize: None,
            last_dest_resize: None,
            data_dirty: false,
            page_edit: None,
            zoom_edit: None,
            zoom_mode: ZoomMode::Custom,
            focus_edit: None,
            focus_body_edit: false,
            focus_search: false,
            fullscreen: false,
            reveal_toolbar: false,
            horizontal_layout: false,
            horizontal_scroll: ui_settings.horizontal_scroll,
            pending_layout: None,
            search_open: false,
            editing_title: None,
            editing_body: None,
            link_marks: Vec::new(),
            sidebar_width,
            password_for: None,
            password_input: String::new(),
            search_input: String::new(),
            page_input: String::new(),
            hits: Vec::new(),
            active_hit: None,
            show_outline: true,
            sidebar_tab: SidebarPane::Outline,
            annotation_show_highlight: true,
            annotation_show_underline: true,
            annotation_show_strike: true,
            annotation_show_ink: true,
            annotation_show_note: true,
            annotation_only_external: false,
            annotation_only_readonly: false,
            annotation_view: AnnotationListView::default(),
            annotation_search: String::new(),
            collapsed_annotation_pages: std::collections::HashSet::new(),
            annotation_focus: None,
            context_annotation: None,
            context_menu_pos: None,
            pin_hover_enabled: ui_settings.pin_hover_enabled,
            markdown_floats: ui_settings.markdown_floats,
            hover_preview_markdown: ui_settings.hover_preview_markdown,
            allow_external_annotation_links: ui_settings.allow_external_annotation_links,
            document_load_token: 0,
            pending_external_link: None,
            drag_hover_logged: false,
            math_help: false,
            body_edit_session: 1,
            body_edit_cursor: None,
            custom_hover: None,
            canvas_rect: egui::Rect::NOTHING,
            canvas_layer: egui::LayerId::background(),
            runtime: RuntimeState::new(clock, diag),
            save_flow: SaveFlowState::default(),
            markdown: MarkdownState::default(),
            annotation_pdf: AnnotationPdfState::default(),
            click_highlight: None,
            message: String::new(),
            error: None,
        };
        if let Some(warning) = settings_warning {
            app.message = warning;
        }
        app.saved_pdf_revision = app.history.current_pdf_revision();
        app.data_clean_hash = Some(app.data_fingerprint());
        if let Some(path) = initial {
            app.open(&path, None);
        }
        app
    }
}

impl eframe::App for PdfReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

#[cfg(test)]
mod tests;
