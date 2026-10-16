//! 会话层：把前面所有零件装配成 UI 唯一需要打交道的东西。
//!
//! ## 这个模块的角色
//! 它是"用 trait 拼成阅读器"的**装配点**：把 [`OpenedDocument`]（后端）、[`RenderCache`]、
//! [`RenderScheduler`]、[`PageLayout`]、[`TextSelection`] 五个可替换零件组合成一个
//! [`DocumentSession`]，并对外只暴露两种东西：
//!
//! * [`ViewerCommand`]：UI → 会话（"翻页""缩放""开始选择""请求可见页"…）；
//! * [`SessionEvent`]：会话 → UI（"位图就绪""当前页变了""请把这些文本写入剪贴板"…）。
//!
//! 这种"命令进、事件出"的设计带来三个好处：
//! 1. **UI 无状态**：egui 每帧重绘，不需要把状态存在控件里，会话就是唯一事实来源；
//! 2. **可测试**：单测里发一串命令、收一串事件即可断言行为，不需要窗口与 GPU；
//! 3. **不依赖平台**：剪贴板、文件对话框这类平台能力通过事件交回 UI，会话本身保持纯粹。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::document::{Capabilities, NavigationIndex, OpenedDocument};
use crate::error::{CoreError, Result};
use crate::geometry::{Insets, Point, Rect, Rotation, Size, Vec2};
use crate::ids::{JobId, PageIndex};
use crate::image::RasterImage;
use crate::layout::{
    DocumentLayout, LayoutParams, PageGeometry, PageLayout, PageLayoutMode, PagePlacement,
    ReadingDirection, ScrollAnchor, Viewport,
};
use crate::render::{
    tiles_covering, RenderCache, RenderEvent, RenderKey, RenderPriority, RenderRequest,
    RenderScheduler, Scale, TileSpec, TilingPolicy,
};
use crate::text::{
    Highlight, HighlightKind, PageText, SearchHit, SearchQuery, SelectionGranularity, TextPosition,
    TextSelection,
};

/// 最小缩放倍数。
pub const MIN_ZOOM: f32 = 0.1;
/// 最大缩放倍数。
pub const MAX_ZOOM: f32 = 8.0;
/// 每次"放大/缩小"命令的步进倍率。
pub const ZOOM_STEP: f32 = 1.25;

/// 侧边栏标签页。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarTab {
    /// 关闭侧边栏。
    #[default]
    None,
    /// 目录（大纲）。
    Outline,
    /// 缩略图。
    Thumbnails,
    /// 搜索结果。
    Search,
}

/// 视图状态：UI 需要读写的一切"用户意图"，与文档内容无关。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerState {
    /// 缩放倍数（1.0 = 100%）。
    pub zoom: f32,
    /// 排布模式。
    pub layout_mode: PageLayoutMode,
    /// 阅读方向。
    pub reading_direction: ReadingDirection,
    /// 当前页（由滚动位置与跳页命令维护）。
    pub current_page: PageIndex,
    /// 用户旋转（叠加在每页自带的 `/Rotate` 之上）。
    pub user_rotation: Rotation,
    /// 侧边栏标签页。
    pub sidebar: SidebarTab,
    /// 选择粒度（单击 / 双击 / 三击切换）。
    pub selection_granularity: SelectionGranularity,
    /// 页间距。
    pub page_gap: f32,
    /// 页面留白。
    pub margin: Insets,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            layout_mode: PageLayoutMode::Continuous,
            reading_direction: ReadingDirection::LeftToRight,
            current_page: PageIndex::FIRST,
            user_rotation: Rotation::None,
            sidebar: SidebarTab::None,
            selection_granularity: SelectionGranularity::Char,
            page_gap: 12.0,
            margin: Insets::all(12.0),
        }
    }
}

/// UI → 会话的命令。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewerCommand {
    /// 视口尺寸变化（窗口缩放、侧边栏开合）。
    ResizeViewport(Size),
    /// 相对滚动（滚轮、拖拽）。
    ScrollBy(Vec2),
    /// 绝对滚动。
    SetScroll(Vec2),
    /// 跳到指定页。
    GoToPage(PageIndex),
    /// 下一页。
    NextPage,
    /// 上一页。
    PrevPage,
    /// 设置缩放。
    SetZoom(f32),
    /// 放大一档。
    ZoomIn,
    /// 缩小一档。
    ZoomOut,
    /// 按页宽适应。
    FitWidth,
    /// 切换排布模式。
    SetLayoutMode(PageLayoutMode),
    /// 设置用户旋转。
    SetUserRotation(Rotation),
    /// 顺时针旋转 90°。
    RotateClockwise,
    /// 切换侧边栏。
    SetSidebar(SidebarTab),
    /// 设置选择粒度（单击 = 字符、双击 = 单词、三击 = 整行）。
    ///
    /// UI 应在双击/三击后**先**发送本命令，再发送 [`ViewerCommand::SelectBegin`]。
    SetSelectionGranularity(SelectionGranularity),
    /// 开始选择（坐标应来自 [`DocumentSession::text_position_at`]）。
    ///
    /// 语义：按当前粒度展开成一个完整区间并立即选中 —— 因此双击一次就会选中整个单词，
    /// 单击则会选中光标所在字符。鼠标按下后如果继续拖动，UI 再发送 [`ViewerCommand::SelectExtend`]。
    SelectBegin(TextPosition),
    /// 扩展选择（把焦点移到新位置，锚点不变）。
    SelectExtend(TextPosition),
    /// 清空选择。
    SelectClear,
    /// 全选。
    SelectAll,
    /// 复制选中文本（会话以 [`SessionEvent::CopyToClipboard`] 把文本交回 UI）。
    CopySelection,
    /// 请求渲染可见页；UI 每帧调用一次。
    RequestVisible,
}

/// 会话 → UI 的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// 视口尺寸变化，UI 需要重新分配绘制区域。
    ViewportChanged,
    /// 布局发生变化（缩放、旋转、排布模式），UI 可据此重建缩略图列表。
    LayoutChanged,
    /// 滚动位置变化。
    ScrollChanged(Vec2),
    /// 当前页变化（状态栏、缩略图高亮需要更新）。
    CurrentPageChanged(PageIndex),
    /// 选区变化。
    SelectionChanged,
    /// 侧边栏变化。
    SidebarChanged(SidebarTab),
    /// 某页位图已渲染并写入缓存，UI 可调用 [`DocumentSession::image_for`] 上传纹理。
    PageRendered(RenderKey),
    /// 某页渲染失败。
    PageRenderFailed {
        /// 页号。
        page: PageIndex,
        /// 原因。
        error: CoreError,
    },
    /// 某页文本已加载（选择、搜索可用）。
    PageTextLoaded(PageIndex),
    /// 请求 UI 把文本写入系统剪贴板（会话层不依赖平台 API）。
    CopyToClipboard(String),
    /// 其它错误。
    Error(CoreError),
}

/// 阅读器会话：**所有零件的装配点**。
///
/// ## 生命周期
/// 一个 `DocumentSession` 对应一个已打开的文档。关闭文档 = drop 会话（`Arc` 计数归零，
/// 后端的文档句柄随之内释放）。
///
/// ## 线程模型
/// 会话本身**不是** `Sync`（选区与视图状态属于 UI 线程），但其中的后端、缓存、调度器
/// 都是 `Send + Sync` 的 `Arc`，因此真实的渲染发生在调度器的后台线程里；
/// UI 只通过 [`DocumentSession::pump`] 取回结果。
///
/// ## 每帧的调用顺序（egui 里通常长这样）
/// ```text
/// 1. 读输入 → session.handle(命令...)
/// 2. session.pump(&mut events)          // 取回上一帧提交的渲染结果
/// 3. 用 session.layout() 画页面：image_for(page) → 上传/复用纹理
/// 4. 画当前选区高亮：session.highlight_selection()
/// 5. session.handle(ViewerCommand::RequestVisible, &mut events)   // 为下一帧备好位图
/// ```
pub struct DocumentSession {
    opened: OpenedDocument,
    pages: Vec<PageGeometry>,
    state: ViewerState,
    viewport: Viewport,
    layout: DocumentLayout,
    cache: Arc<dyn RenderCache>,
    scheduler: Arc<dyn RenderScheduler>,
    layout_engine: Box<dyn PageLayout>,
    selection: Box<dyn TextSelection>,
    page_texts: HashMap<PageIndex, Arc<PageText>>,
    submitted: HashSet<RenderKey>,
    jobs: HashMap<RenderKey, JobId>,
    visible_scratch: Vec<PageIndex>,
    /// D-02：分块策略（默认 `AbovePixels(16 MP)`）。
    tiling: TilingPolicy,
}

impl DocumentSession {
    /// 装配一个会话。
    ///
    /// 五个零件全部由调用方注入：后端产物、缓存实现、调度实现、布局引擎、选区模型。
    /// 读取页几何（尺寸 + 每页 `/Rotate`）会访问后端，因此返回 `Result`。
    pub fn new(
        opened: OpenedDocument,
        cache: Arc<dyn RenderCache>,
        scheduler: Arc<dyn RenderScheduler>,
        layout_engine: Box<dyn PageLayout>,
        selection: Box<dyn TextSelection>,
    ) -> Result<Self> {
        // D-01：一次拿到全部页几何（后端在 open 时已备好批量表），
        // 取代原先"循环 N 次 page_size/page_rotation"的逐页跨 FFI 查询。
        let pages: Vec<PageGeometry> = opened.document.page_geometries()?.to_vec();

        let mut session = Self {
            opened,
            pages,
            state: ViewerState::default(),
            viewport: Viewport::default(),
            layout: DocumentLayout::default(),
            cache,
            scheduler,
            layout_engine,
            selection,
            page_texts: HashMap::new(),
            submitted: HashSet::new(),
            jobs: HashMap::new(),
            visible_scratch: Vec::new(),
            tiling: TilingPolicy::default(),
        };
        session.recompute_layout();
        Ok(session)
    }

    // ---------------------------------------------------------------- 只读访问

    /// 文档元数据与能力声明。
    pub fn opened(&self) -> &OpenedDocument {
        &self.opened
    }

    /// 后端能力。
    pub fn capabilities(&self) -> Capabilities {
        self.opened.capabilities
    }

    /// 页数。
    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    /// 视图状态。
    pub fn state(&self) -> &ViewerState {
        &self.state
    }

    /// 当前布局。
    pub fn layout(&self) -> &DocumentLayout {
        &self.layout
    }

    /// 当前视口。
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// 某页的摆放结果。
    pub fn placement(&self, page: PageIndex) -> Option<&PagePlacement> {
        self.layout.placement(page)
    }

    /// 某页当前缩放下的**整页**位图（若已在缓存中）。
    ///
    /// UI 拿它做纹理上传：`RasterImage::to_rgba8()` 之后即可交给 `egui::ColorImage`。
    ///
    /// **分块时返回 `None`** —— 该页的像素请改用 [`DocumentSession::tiles_for`] 拼接。
    /// 判断依据是 [`DocumentSession::tiling_policy`]，UI 也可据此决定走哪条路径。
    pub fn image_for(&self, page: PageIndex) -> Option<Arc<RasterImage>> {
        let placement = self.layout.placement(page)?;
        if self.tiling.requires_tiling(placement.pixel_size()) {
            return None;
        }
        let key = RenderKey::new(page, placement.scale, placement.rotation);
        self.cache.get(&key)
    }

    /// 该页当前缩放下的全部**已在缓存中**的瓦片，供 UI 按 `TileSpec::pixel_rect` 拼接绘制。
    ///
    /// 未就绪的瓦片不会出现在返回值里 —— UI 应对缺失区域画白底占位（D-06）。
    pub fn tiles_for(&self, page: PageIndex) -> Vec<(TileSpec, Arc<RasterImage>)> {
        let Some(placement) = self.layout.placement(page).copied() else {
            return Vec::new();
        };
        let pixel_size = placement.pixel_size();
        if !self.tiling.requires_tiling(pixel_size) {
            return Vec::new();
        }
        let (cols, rows) = TileSpec::grid_for(pixel_size);
        let mut out = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let tile = TileSpec::new(col, row);
                let key = RenderKey::tiled(page, placement.scale, placement.rotation, tile);
                if let Some(image) = self.cache.get(&key) {
                    out.push((tile, image));
                }
            }
        }
        out
    }

    /// 当前分块策略。
    pub fn tiling_policy(&self) -> TilingPolicy {
        self.tiling
    }

    /// 切换分块策略。改变策略会作废在飞任务（缓存键含义随之变化）。
    pub fn set_tiling_policy(&mut self, policy: TilingPolicy) {
        if self.tiling != policy {
            self.tiling = policy;
            self.invalidate_render();
        }
    }

    /// 捕获当前视野中心对应的页内位置（D-05）。
    ///
    /// 中心落在页间距或页外（`page_at` 返回 `None`）时返回 `None`，
    /// 调用方应退化为"当前页居中"（见 [`DocumentSession::restore_anchor`] 的调用方）。
    pub fn capture_anchor(&self) -> Option<ScrollAnchor> {
        let center = self.viewport.center_content_point();
        let page = self.layout_engine.page_at(&self.layout, center)?;
        let placement = self.layout.placement(page)?;
        Some(ScrollAnchor { page, page_point: placement.content_to_page(center) })
    }

    /// 把锚点重新放回视口中心（`scroll_to_anchor` 内部已做夹取）。
    pub fn restore_anchor(&mut self, anchor: ScrollAnchor) {
        self.viewport.scroll = self.layout_engine.scroll_to_anchor(
            &self.layout,
            anchor.page,
            anchor.page_point,
            self.viewport.size,
        );
    }

    /// 已缓存的页文本（不会触发加载）。
    pub fn cached_page_text(&self, page: PageIndex) -> Option<Arc<PageText>> {
        self.page_texts.get(&page).cloned()
    }

    /// 当前选区的高亮矩形。
    ///
    /// **尽力而为**：文本尚未加载的页会被跳过（UI 每帧只画可见页，因此通常已经加载）。
    /// 这样高亮不会为了"全选"而把整本文档的文本一次性拉起来。
    pub fn highlight_selection(&self) -> Vec<Highlight> {
        let texts = &self.page_texts;
        let lookup = move |page: PageIndex| -> Option<Arc<PageText>> { texts.get(&page).cloned() };
        self.selection.highlight(&lookup)
    }

    /// 当前选中的文本。
    ///
    /// 与 [`DocumentSession::highlight_selection`] 不同，这里**会按需加载**选区覆盖的页文本：
    /// 复制必须完整，不能因为某页尚未加载就静默漏掉内容（"全选 + 复制"就依赖这一点）。
    /// 代价是跨页大选区会触发多页文本提取 —— 这是复制语义本身的要求。
    pub fn selected_text(&mut self) -> String {
        self.load_selection_text();
        let texts = &self.page_texts;
        let lookup = move |page: PageIndex| -> Option<Arc<PageText>> { texts.get(&page).cloned() };
        self.selection.selected_text(&lookup)
    }

    /// 把选区覆盖到的页文本全部加载进缓存（惰性、按需）。
    fn load_selection_text(&mut self) {
        let Some(selection) = self.selection.selection() else {
            return;
        };
        for (page, _) in selection.page_ranges() {
            let _ = self.page_text(page);
        }
    }

    /// 把搜索命中转成高亮；`active` 为当前跳转到的那一条（用更醒目的配色）。
    pub fn highlights_for_search(&self, hits: &[SearchHit], active: Option<usize>) -> Vec<Highlight> {
        hits.iter()
            .enumerate()
            .map(|(index, hit)| Highlight {
                page: hit.page,
                rects: hit.rects.clone(),
                kind: if Some(index) == active {
                    HighlightKind::ActiveSearchMatch
                } else {
                    HighlightKind::SearchMatch
                },
            })
            .collect()
    }

    /// 导航索引（大纲 + 逐页链接 + 命名目标，打开时已解析并合并存储）；O(1) 克隆。
    ///
    /// 取代了早期的 `outline()` 与 `links(page)`：点击链接/大纲时 **0 次后端调用**。
    pub fn navigation(&self) -> Result<Arc<NavigationIndex>> {
        self.opened.document.navigation()
    }

    /// 全文搜索（后端不支持时返回 [`CoreError::Unsupported`]）。
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        match self.opened.text.as_ref() {
            Some(layer) => layer.search(query),
            None => Err(CoreError::unsupported("全文搜索")),
        }
    }

    /// 取某页文本，必要时向后端加载并缓存。
    pub fn page_text(&mut self, page: PageIndex) -> Option<Arc<PageText>> {
        if let Some(text) = self.page_texts.get(&page) {
            return Some(text.clone());
        }
        let layer = self.opened.text.clone()?;
        match layer.page_text(page) {
            Ok(text) => {
                self.page_texts.insert(page, text.clone());
                Some(text)
            }
            Err(_) => None,
        }
    }

    /// 把内容空间坐标换算成文本位置（鼠标按下/拖拽时调用）。
    ///
    /// 这是 UI 里"点文字开始选择"的唯一入口：命中页 → 页内坐标 → 字符下标。
    pub fn text_position_at(&mut self, content_point: Point) -> Option<TextPosition> {
        let page = self.layout_engine.page_at(&self.layout, content_point)?;
        let placement = *self.layout.placement(page)?;
        let page_point = placement.content_to_page(content_point);
        let text = self.page_text(page)?;
        text.hit_test(page_point).map(|index| TextPosition::new(page, index))
    }

    /// 当前页在 100% 缩放下的显示尺寸（用于 FitWidth 之外的适配模式）。
    pub fn current_page_displayed_size(&self) -> Option<Size> {
        Some(self.pages.get(self.state.current_page.get() as usize)?.displayed_size(self.state.user_rotation))
    }

    // ---------------------------------------------------------------- 命令处理

    /// 处理一条 UI 命令，把产生的事件追加到 `events`。
    pub fn handle(&mut self, command: ViewerCommand, events: &mut Vec<SessionEvent>) {
        match command {
            ViewerCommand::ResizeViewport(size) => {
                // D-05：窗口尺寸变化同样保持视野中心 —— 用**旧**视口捕获锚点，
                // 换尺寸后把它放回新的视口中心（布局本身不随视口变化）。
                let anchor = self.capture_anchor();
                self.viewport.size = size;
                self.apply_anchor(anchor);
                events.push(SessionEvent::ViewportChanged);
                events.push(SessionEvent::ScrollChanged(self.viewport.scroll));
                self.sync_current_page(events);
            }
            ViewerCommand::ScrollBy(delta) => {
                let target = self.viewport.scroll + delta;
                self.set_scroll(target, events);
            }
            ViewerCommand::SetScroll(offset) => self.set_scroll(offset, events),
            ViewerCommand::GoToPage(page) => self.goto_page(page, events),
            ViewerCommand::NextPage => {
                let next = self.state.current_page.get() + 1;
                if next < self.page_count() {
                    self.goto_page(PageIndex::new(next), events);
                }
            }
            ViewerCommand::PrevPage => {
                if let Some(previous) = self.state.current_page.prev() {
                    self.goto_page(previous, events);
                }
            }
            ViewerCommand::SetZoom(zoom) => self.set_zoom(zoom, events),
            ViewerCommand::ZoomIn => self.set_zoom(self.state.zoom * ZOOM_STEP, events),
            ViewerCommand::ZoomOut => self.set_zoom(self.state.zoom / ZOOM_STEP, events),
            ViewerCommand::FitWidth => self.fit_width(events),
            ViewerCommand::SetLayoutMode(mode) => {
                if self.state.layout_mode != mode {
                    self.state.layout_mode = mode;
                    self.relayout_keeping_anchor(events);
                }
            }
            ViewerCommand::SetUserRotation(rotation) => {
                if self.state.user_rotation != rotation {
                    self.state.user_rotation = rotation;
                    self.invalidate_render();
                    self.relayout_keeping_anchor(events);
                }
            }
            ViewerCommand::RotateClockwise => {
                self.state.user_rotation = self.state.user_rotation.compose(Rotation::Clockwise90);
                self.invalidate_render();
                self.relayout_keeping_anchor(events);
            }
            ViewerCommand::SetSidebar(tab) => {
                self.state.sidebar = tab;
                events.push(SessionEvent::SidebarChanged(tab));
            }
            ViewerCommand::SetSelectionGranularity(granularity) => {
                self.selection.set_granularity(granularity);
                self.state.selection_granularity = granularity;
            }
            ViewerCommand::SelectBegin(position) => {
                // 按粒度展开成完整区间后立即选中：单击选中字符、双击选中单词、三击选中整行。
                let (start, end) = self.expand_position(position);
                self.selection.begin(start);
                self.selection.extend(end);
                events.push(SessionEvent::SelectionChanged);
            }
            ViewerCommand::SelectExtend(position) => {
                let (_, end) = self.expand_position(position);
                self.selection.extend(end);
                events.push(SessionEvent::SelectionChanged);
            }
            ViewerCommand::SelectClear => {
                self.selection.clear();
                events.push(SessionEvent::SelectionChanged);
            }
            ViewerCommand::SelectAll => {
                if self.page_count() > 0 {
                    let last = PageIndex::new(self.page_count() - 1);
                    let end = self.page_text(last).map(|text| text.len() as u32).unwrap_or(0);
                    self.selection.begin(TextPosition::new(PageIndex::FIRST, 0));
                    self.selection.extend(TextPosition::new(last, end));
                    events.push(SessionEvent::SelectionChanged);
                }
            }
            ViewerCommand::CopySelection => {
                let text = self.selected_text();
                if !text.is_empty() {
                    events.push(SessionEvent::CopyToClipboard(text));
                }
            }
            ViewerCommand::RequestVisible => self.submit_visible(),
        }
    }

    /// 从调度器取回渲染结果（非阻塞），并把位图写入缓存。
    ///
    /// 这一步是"后台渲染 → UI 可用"的交接点：**UI 每帧都必须调用它**，
    /// 否则完成的渲染结果会一直堆在调度器的队列里。
    pub fn pump(&mut self, events: &mut Vec<SessionEvent>) {
        const MAX_EVENTS_PER_FRAME: usize = 64;
        for event in self.scheduler.drain_events(MAX_EVENTS_PER_FRAME) {
            match event {
                RenderEvent::Rendered { key, image } => {
                    self.cache.insert(key, image);
                    self.submitted.remove(&key);
                    self.jobs.remove(&key);
                    events.push(SessionEvent::PageRendered(key));
                }
                RenderEvent::Failed { page, error } => {
                    self.forget_page(page);
                    events.push(SessionEvent::PageRenderFailed { page, error });
                }
                RenderEvent::Cancelled { page } => {
                    self.forget_page(page);
                }
            }
        }
    }

    // ---------------------------------------------------------------- 内部实现

    /// 组装当前布局参数（视图状态 → 布局输入）。
    fn layout_params(&self) -> LayoutParams {
        LayoutParams {
            mode: self.state.layout_mode,
            direction: self.state.reading_direction,
            scale: Scale::from_zoom(self.state.zoom),
            page_gap: self.state.page_gap,
            margin: self.state.margin,
            user_rotation: self.state.user_rotation,
        }
    }

    /// 重新计算布局。
    fn recompute_layout(&mut self) {
        let params = self.layout_params();
        self.layout = self.layout_engine.compute(&self.pages, &params);
    }

    /// D-05：把锚点放回视口中心；锚点缺失（中心落在页间距）时退化为"当前页居中"。
    fn apply_anchor(&mut self, anchor: Option<ScrollAnchor>) {
        match anchor {
            Some(anchor) => self.restore_anchor(anchor),
            None => {
                self.viewport.scroll = self.layout_engine.scroll_to_page(
                    &self.layout,
                    self.state.current_page,
                    self.viewport.size,
                );
            }
        }
    }

    /// D-05：重算布局并保持视野中心（捕获 → 重算 → 恢复 → 事件）。
    fn relayout_keeping_anchor(&mut self, events: &mut Vec<SessionEvent>) {
        let anchor = self.capture_anchor();
        self.recompute_layout();
        self.apply_anchor(anchor);
        events.push(SessionEvent::LayoutChanged);
        events.push(SessionEvent::ScrollChanged(self.viewport.scroll));
        self.sync_current_page(events);
    }

    /// 缩放/旋转变化后，旧的渲染任务与缓存键都失效了。
    fn invalidate_render(&mut self) {
        self.scheduler.cancel_all();
        self.submitted.clear();
        self.jobs.clear();
    }

    /// 忘记某一页的所有在飞任务记录。
    fn forget_page(&mut self, page: PageIndex) {
        self.submitted.retain(|key| key.page != page);
        self.jobs.retain(|key, _| key.page != page);
    }

    fn set_scroll(&mut self, target: Vec2, events: &mut Vec<SessionEvent>) {
        let clamped = self.layout_engine.clamp_scroll(&self.layout, self.viewport.size, target);
        if clamped == self.viewport.scroll {
            return;
        }
        self.viewport.scroll = clamped;
        events.push(SessionEvent::ScrollChanged(clamped));
        self.sync_current_page(events);
    }

    fn goto_page(&mut self, page: PageIndex, events: &mut Vec<SessionEvent>) {
        if page.get() >= self.page_count() {
            events.push(SessionEvent::Error(CoreError::PageOutOfRange {
                page,
                page_count: self.page_count(),
            }));
            return;
        }
        self.state.current_page = page;
        self.viewport.scroll =
            self.layout_engine.scroll_to_page(&self.layout, page, self.viewport.size);
        events.push(SessionEvent::CurrentPageChanged(page));
        events.push(SessionEvent::ScrollChanged(self.viewport.scroll));
        // 目标页优先渲染，用户翻页后不必等调度器轮询
        self.request_page(page, RenderPriority::Interactive);
    }

    fn set_zoom(&mut self, zoom: f32, events: &mut Vec<SessionEvent>) {
        let clamped = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (clamped - self.state.zoom).abs() < f32::EPSILON {
            return;
        }
        // D-05：锚点必须在改状态**之前**捕获（用旧布局），改完再放回。
        let anchor = self.capture_anchor();
        self.state.zoom = clamped;
        self.invalidate_render();
        self.recompute_layout();
        self.apply_anchor(anchor);
        events.push(SessionEvent::LayoutChanged);
        events.push(SessionEvent::ScrollChanged(self.viewport.scroll));
        self.sync_current_page(events);
    }

    fn fit_width(&mut self, events: &mut Vec<SessionEvent>) {
        let Some(geometry) = self.pages.get(self.state.current_page.get() as usize).copied()
        else {
            return;
        };
        let displayed = geometry.displayed_size(self.state.user_rotation);
        let available = (self.viewport.size.width - self.state.margin.horizontal()).max(32.0);
        if displayed.width > 0.0 {
            self.set_zoom(available / displayed.width, events);
        }
    }

    /// 视口顶部所在的页 —— 这是"当前页"的判定依据。
    fn sync_current_page(&mut self, events: &mut Vec<SessionEvent>) {
        let probe = Point::new(
            self.viewport.scroll.x + self.viewport.size.width * 0.5,
            self.viewport.scroll.y + 1.0,
        );
        let found = self
            .layout_engine
            .page_at(&self.layout, probe)
            .or_else(|| self.layout_engine.page_at(&self.layout, self.viewport.content_rect().center()));
        if let Some(page) = found {
            if page != self.state.current_page {
                self.state.current_page = page;
                events.push(SessionEvent::CurrentPageChanged(page));
            }
        }
    }

    /// 按当前粒度把鼠标命中的字符位置展开成 `(起点, 终点)`。
    ///
    /// 这是"粒度"唯一的落地点：选区模型（[`TextSelection`]）本身完全不知道
    /// 什么叫"单词"，它只做区间记账 —— 因此它可以被独立单测。
    fn expand_position(&mut self, position: TextPosition) -> (TextPosition, TextPosition) {
        let granularity = self.selection.granularity();
        let Some(text) = self.page_text(position.page) else {
            return (position, position);
        };
        let range = text.range_at(position.index, granularity);
        (
            TextPosition::new(position.page, range.start),
            TextPosition::new(position.page, range.end),
        )
    }

    /// 请求渲染一页（已缓存或已在飞则跳过）。
    ///
    /// D-02：按分块策略分流 —— 小页走整页渲染，超大页只提交"视口覆盖到的瓦片"。
    /// 关键一致性：请求里的 `scale` / `rotation` 必须与布局给出的完全一致，
    /// 否则渲染结果会与布局对不上（画面错位、高亮偏移）。
    fn request_page(&mut self, page: PageIndex, priority: RenderPriority) {
        let Some(placement) = self.layout.placement(page).copied() else {
            return;
        };
        if self.tiling.requires_tiling(placement.pixel_size()) {
            self.request_tiles(placement, priority);
        } else {
            let key = RenderKey::new(placement.page, placement.scale, placement.rotation);
            let request = RenderRequest::new(placement.page, placement.scale, placement.rotation)
                .with_priority(priority);
            self.submit(request, key);
        }
    }

    /// 分块路径：只为"视口在该页位图中覆盖到的瓦片"提交请求。
    ///
    /// 不整页提交（130 块）的原因：分块的目的是省内存，预取整页会把收益抵消掉。
    fn request_tiles(&mut self, placement: PagePlacement, priority: RenderPriority) {
        let pixel_size = placement.pixel_size();
        let grid = TileSpec::grid_for(pixel_size);
        let Some(visible) = self.viewport.content_rect().intersection(placement.rect) else {
            return;
        };
        // 内容空间 → 该页位图内的像素矩形
        let visible_px = Rect::new(
            visible.x - placement.rect.x,
            visible.y - placement.rect.y,
            visible.width,
            visible.height,
        );
        let Some((first, last)) = tiles_covering(visible_px, grid) else {
            return;
        };
        for row in first.row..=last.row {
            for col in first.col..=last.col {
                let tile = TileSpec::new(col, row);
                let key = RenderKey::tiled(placement.page, placement.scale, placement.rotation, tile);
                let request =
                    RenderRequest::tiled(placement.page, placement.scale, placement.rotation, tile)
                        .with_priority(priority);
                self.submit(request, key);
            }
        }
    }

    /// 提交任务（已缓存或已在飞则跳过）。
    fn submit(&mut self, request: RenderRequest, key: RenderKey) {
        if self.submitted.contains(&key) || self.jobs.contains_key(&key) {
            return;
        }
        if self.cache.get(&key).is_some() {
            return;
        }
        let job = self.scheduler.submit(request);
        self.submitted.insert(key);
        self.jobs.insert(key, job);
    }

    /// 把可见页（以及前后各一页预取）交给调度器。
    fn submit_visible(&mut self) {
        let mut visible = std::mem::take(&mut self.visible_scratch);
        self.layout_engine.visible_pages(&self.layout, &self.viewport, &mut visible);

        for page in &visible {
            self.request_page(*page, RenderPriority::Visible);
        }

        if let (Some(first), Some(last)) = (visible.first().copied(), visible.last().copied()) {
            let mut prefetch = Vec::with_capacity(2);
            if first.get() > 0 {
                prefetch.push(PageIndex::new(first.get() - 1));
            }
            if (last.get() as usize) + 1 < self.pages.len() {
                prefetch.push(PageIndex::new(last.get() + 1));
            }
            for page in prefetch {
                self.request_page(page, RenderPriority::Prefetch);
            }
        }

        self.visible_scratch = visible;
    }
}

impl std::fmt::Debug for DocumentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentSession")
            .field("page_count", &self.page_count())
            .field("state", &self.state)
            .field("viewport", &self.viewport)
            .field("cached_texts", &self.page_texts.len())
            .field("in_flight", &self.jobs.len())
            .finish()
    }
}

/// 便于 UI 直接构造缩放命令。
impl ViewerCommand {
    /// 适应宽度。
    pub fn fit_width() -> Self {
        Self::FitWidth
    }
}
