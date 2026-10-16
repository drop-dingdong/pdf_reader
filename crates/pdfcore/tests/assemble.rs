//! 端到端装配测试：**不依赖 PDFium、不依赖 PDF 文件、不依赖 GPU**，
//! 用内存假后端把整条链路跑通。
//!
//! 这个测试是本次 trait 设计的核心验收项：如果它能通过，就证明
//! "后端 / 缓存 / 调度 / 布局 / 选区" 五个零件确实可以由外部拼装起来，
//! 并且 egui 前端将来只需要对接 [`pdfcore::session::ViewerCommand`] 与
//! [`pdfcore::session::SessionEvent`]。

use std::sync::Arc;

use pdfcore::prelude::*;
use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache, MemoryPage};

/// 装配一个会话：等价于 UI 启动时做的那几行。
fn assemble() -> DocumentSession {
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).expect("打开假文档");

    let cache: Arc<dyn RenderCache> = Arc::new(MemoryCache::new());
    let scheduler: Arc<dyn RenderScheduler> =
        Arc::new(ImmediateScheduler::new(opened.raster.clone()));

    DocumentSession::new(
        opened,
        cache,
        scheduler,
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话")
}

/// 当前布局下的最大滚动偏移（y 轴）：断言"跳页/兜底滚动会被夹取"时用。
fn content_max_scroll_y(session: &DocumentSession) -> f32 {
    (session.layout().content_size.height - session.viewport().size.height).max(0.0)
}

/// 第二轮验收：搜索命中跳转要把命中位置放到**视口中心**（与跳页的"顶部对齐"区分开）。
#[test]
fn center_on_page_point_puts_the_point_at_the_view_center() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(700.0, 800.0)), &mut events);

    let target = Point::new(300.0, 400.0);
    session.handle(
        ViewerCommand::CenterOnPagePoint { page: PageIndex::FIRST, point: target },
        &mut events,
    );

    let placement = *session.placement(PageIndex::FIRST).expect("第 1 页布局");
    let content = placement.page_to_content(target);
    let scroll = session.viewport().scroll;
    let view = Point::new(content.x - scroll.x, content.y - scroll.y);
    assert!(
        (view.x - 350.0).abs() < 0.5 && (view.y - 400.0).abs() < 0.5,
        "目标点应位于视口中心，实际 {view:?}"
    );
    assert_eq!(session.state().current_page, PageIndex::FIRST, "当前页应跟随更新");
}

/// 第二轮验收：UI 枚举"要画哪些瓦片"必须与实际提交渲染的范围一致
/// （否则未就绪区域会露出深色画布 —— 800% 时表现为"视野全黑"）。
#[test]
fn visible_tiles_matches_the_requested_range() {
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).unwrap();
    let raster = opened.raster.clone();
    let mut session = DocumentSession::new(
        opened,
        Arc::new(MemoryCache::new()),
        Arc::new(ImmediateScheduler::new(raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话");

    // 未分块时没有瓦片范围（走整页纹理）
    session.set_tiling_policy(TilingPolicy::Never);
    assert!(session.visible_tiles(PageIndex::FIRST).is_none());

    session.set_tiling_policy(TilingPolicy::Always);
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(300.0, 300.0)), &mut events);
    session.handle(ViewerCommand::SetScroll(Vec2::ZERO), &mut events);

    let (first, last) = session.visible_tiles(PageIndex::FIRST).expect("强制分块后应有范围");
    assert_eq!((first.col, first.row), (0, 0));
    assert_eq!((last.col, last.row), (0, 0), "300×300 视口只覆盖左上角一块瓦片");

    // 提交 + 取回后，缓存里的瓦片应与这个范围完全一致
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    let tiles = session.tiles_for(PageIndex::FIRST);
    assert_eq!(tiles.len(), 1, "只应渲染并缓存这一块");
    assert_eq!(tiles[0].0, first);
}

#[test]
fn assemble_a_reader_from_traits() {
    let mut session = assemble();
    let mut events = Vec::new();

    assert_eq!(session.page_count(), 3);
    assert!(session.capabilities().text_layer);

    // 1) 给视口一个真实尺寸（否则没有可见页），然后请求渲染
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    // 2) 首页位图应已进入缓存，且尺寸 = 页尺寸 × 缩放（612x792 @1.0）
    let image = session.image_for(PageIndex::new(0)).expect("首页应已渲染并写入缓存");
    assert_eq!((image.width, image.height), (612, 792));
    assert!(events.iter().any(|event| matches!(event, SessionEvent::PageRendered(_))));
}

#[test]
fn cache_prevents_duplicate_renders() {
    let cache = Arc::new(MemoryCache::new());
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).unwrap();
    let scheduler = Arc::new(ImmediateScheduler::new(opened.raster.clone()));

    let mut session = DocumentSession::new(
        opened,
        cache.clone(),
        scheduler,
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .unwrap();

    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    let after_first = cache.stats().entries;
    assert!(after_first > 0);

    // 再请求两次：不应产生新的渲染（缓存命中）
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    assert_eq!(cache.stats().entries, after_first, "重复请求不应重复渲染");
}

#[test]
fn navigation_updates_current_page_and_scroll() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 600.0)), &mut events);
    assert_eq!(session.state().current_page, PageIndex::FIRST);

    session.handle(ViewerCommand::NextPage, &mut events);
    assert_eq!(session.state().current_page, PageIndex::new(1));
    assert!(session.viewport().scroll.y > 0.0, "跳页应改变滚动位置");
    assert!(events.contains(&SessionEvent::CurrentPageChanged(PageIndex::new(1))));

    session.handle(ViewerCommand::PrevPage, &mut events);
    assert_eq!(session.state().current_page, PageIndex::FIRST);

    // 越界跳页应报错而不是 panic
    events.clear();
    session.handle(ViewerCommand::GoToPage(PageIndex::new(99)), &mut events);
    assert!(matches!(events.last(), Some(SessionEvent::Error(CoreError::PageOutOfRange { .. }))));
}

#[test]
fn zoom_recomputes_layout_and_invalidates_bitmaps() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    let before = session.placement(PageIndex::new(0)).unwrap().rect.size();

    session.handle(ViewerCommand::SetZoom(2.0), &mut events);
    let after = session.placement(PageIndex::new(0)).unwrap().rect.size();
    assert!((after.width - before.width * 2.0).abs() < 0.5);
    assert!(events.contains(&SessionEvent::LayoutChanged));

    // 新缩放下需要重新渲染（旧缓存键失效）
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    let image = session.image_for(PageIndex::new(0)).unwrap();
    assert!((image.width as f32 - after.width).abs() <= 1.0);
}

#[test]
fn per_page_rotation_flows_into_layout_and_render_request() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 600.0)), &mut events);
    session.handle(ViewerCommand::GoToPage(PageIndex::new(2)), &mut events);
    session.pump(&mut events);

    let placement = *session.placement(PageIndex::new(2)).unwrap();
    assert_eq!(placement.rotation, Rotation::Clockwise90, "第 3 页自带 /Rotate 90");
    // 612x792 旋转后应为 792x612
    assert!((placement.rect.width - 792.0).abs() < 0.5);
    assert!((placement.rect.height - 612.0).abs() < 0.5);

    // 渲染结果必须与布局一致（否则会出现画面与高亮错位）
    let image = session.image_for(PageIndex::new(2)).expect("第 3 页应已渲染");
    assert_eq!((image.width, image.height), (792, 612));
}

#[test]
fn text_selection_and_clipboard_events() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);

    // 选中第一页的 "Hello"：先点第一个字符，再拖到第 5 个字符（index 4）的末尾
    session.handle(
        ViewerCommand::SelectBegin(TextPosition::new(PageIndex::new(0), 0)),
        &mut events,
    );
    session.handle(
        ViewerCommand::SelectExtend(TextPosition::new(PageIndex::new(0), 4)),
        &mut events,
    );
    assert_eq!(session.selected_text(), "Hello");
    let highlights = session.highlight_selection();
    assert_eq!(highlights.len(), 1);
    assert_eq!(highlights[0].kind, HighlightKind::Selection);

    // 复制：会话通过事件把文本交回 UI，自身不依赖剪贴板 API
    events.clear();
    session.handle(ViewerCommand::CopySelection, &mut events);
    assert_eq!(events, vec![SessionEvent::CopyToClipboard("Hello".to_string())]);
}

#[test]
fn word_granularity_expands_selection() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);

    // 模拟双击：先把粒度设为 Word，再发送点击位置（第 8 个字符落在 "world" 内部）
    session.handle(
        ViewerCommand::SetSelectionGranularity(SelectionGranularity::Word),
        &mut events,
    );
    session.handle(
        ViewerCommand::SelectBegin(TextPosition::new(PageIndex::new(0), 8)),
        &mut events,
    );
    assert_eq!(session.selected_text(), "world", "单击位置落在词内，应扩展为整词");

    session.handle(ViewerCommand::SelectAll, &mut events);
    let all = session.selected_text();
    assert!(all.starts_with("Hello world"));
    assert!(all.contains("Rotated page"));
    assert!(all.contains('\n'), "跨页选择应以换行连接");
}

#[test]
fn hit_testing_maps_mouse_to_text_position() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);

    // 第一页第一行首字符的中心：页内坐标 (36+3, 792-14+5)
    let placement = *session.placement(PageIndex::new(0)).unwrap();
    let content_point = placement.page_to_content(Point::new(39.0, 783.0));
    let position = session.text_position_at(content_point).expect("应命中字符");
    assert_eq!(position.page, PageIndex::new(0));
    assert!(position.index < 5, "应落在 'Hello' 之内，实际 {}", position.index);

    // 点在没有页面的空白处（内容左上角之外）→ 无命中
    assert_eq!(session.text_position_at(Point::new(-50.0, -50.0)), None);
}

#[test]
fn search_and_outline_come_from_the_backend() {
    let session = assemble();
    let hits = session.search(&SearchQuery::new("page")).expect("假后端支持搜索");
    assert_eq!(hits.len(), 2);

    let highlighted = session.highlights_for_search(&hits, Some(0));
    assert_eq!(highlighted[0].kind, HighlightKind::ActiveSearchMatch);
    assert_eq!(highlighted[1].kind, HighlightKind::SearchMatch);

    let navigation = session.navigation().expect("假后端提供导航索引");
    assert_eq!(navigation.outline.len(), 3);
    assert_eq!(
        navigation.outline[0].destination,
        Some(Destination::Page { page: PageIndex::FIRST, offset: None })
    );

    // D-08：页内链接与命名目标都在打开时解析完毕，点击时 0 次后端调用
    assert_eq!(navigation.links_on(PageIndex::new(0)).len(), 1);
    assert_eq!(
        navigation.links_on(PageIndex::new(0))[0].destination,
        Destination::Page { page: PageIndex::new(1), offset: Some(Point::new(36.0, 20.0)) },
        "演示文档的链接带具体落点（用于验证链接跳转居中）"
    );
    assert!(navigation.links_on(PageIndex::new(2)).is_empty(), "最后一页没有下一页链接");
    assert!(navigation.links_on(PageIndex::new(99)).is_empty(), "越界页应返回空切片而不是 panic");
    assert_eq!(
        navigation.named_destinations.get("chapter-2"),
        Some(&Destination::Page { page: PageIndex::new(1), offset: None })
    );
    assert_eq!(
        navigation.unresolved,
        vec!["chapter-broken".to_string()],
        "解析失败的命名目标进 unresolved，而不是变成点了没反应的链接"
    );
}

#[test]
fn backend_without_text_layer_still_works() {
    // 模拟 hayro 那类"只能渲染"的后端：文本层为 None。
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).unwrap();
    let mut capabilities = opened.capabilities;
    capabilities.text_layer = false;
    capabilities.search = false;
    let raster = opened.raster.clone();
    let document = opened.document.clone();
    let opened = OpenedDocument::new(document, raster.clone(), None, capabilities);

    let mut session = DocumentSession::new(
        opened,
        Arc::new(MemoryCache::new()),
        Arc::new(ImmediateScheduler::new(raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .unwrap();

    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    // 渲染照常工作
    assert!(session.image_for(PageIndex::new(0)).is_some());
    // 但文本能力缺失时应优雅降级：没有文本、没有命中、搜索报 Unsupported
    assert!(session.selected_text().is_empty());
    assert!(session.text_position_at(Point::new(100.0, 100.0)).is_none());
    assert!(session
        .search(&SearchQuery::new("page"))
        .is_err_and(|error| error.is_unsupported()));
}

/// D-01 的守门测试：只实现批量接口，并在单页接口上 `panic!`。
///
/// 如果 `DocumentSession::new` 未来退回"逐页查询 page_size/page_rotation"，
/// 这个测试会立刻 panic 失败 —— 从而锁死"一次批量拿全部页几何"的设计。
struct BulkOnlyDocument {
    info: DocumentInfo,
    geometries: Arc<[PageGeometry]>,
}

impl Document for BulkOnlyDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }

    fn page_geometries(&self) -> Result<Arc<[PageGeometry]>> {
        Ok(self.geometries.clone())
    }

    fn page_size(&self, _page: PageIndex) -> Result<Size> {
        panic!("会话不应逐页查询 page_size（D-01：必须走批量页几何）")
    }

    fn page_rotation(&self, _page: PageIndex) -> Result<Rotation> {
        panic!("会话不应逐页查询 page_rotation（D-01：必须走批量页几何）")
    }
}

#[test]
fn session_consumes_bulk_page_geometries_only() {
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).unwrap();

    let document: Arc<dyn Document> = Arc::new(BulkOnlyDocument {
        info: opened.document.info().clone(),
        geometries: opened.document.page_geometries().unwrap(),
    });
    let rebuilt =
        OpenedDocument::new(document, opened.raster.clone(), None, Capabilities::minimal());

    // 能构造成功本身就证明只用了批量接口（否则 BulkOnlyDocument 会 panic）
    let session = DocumentSession::new(
        rebuilt,
        Arc::new(MemoryCache::new()),
        Arc::new(ImmediateScheduler::new(opened.raster.clone())),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话");

    assert_eq!(session.page_count(), 3);
    // 逐页旋转必须原样流入布局
    assert_eq!(
        session.placement(PageIndex::new(2)).unwrap().rotation,
        Rotation::Clockwise90
    );
    assert!((session.placement(PageIndex::new(2)).unwrap().rect.width - 792.0).abs() < 0.5);
}

/// D-02 端到端：分块策略决定走"整页"还是"瓦片"路径，且瓦片请求只覆盖视口内那几块。
#[test]
fn tiling_routes_to_tiles_and_limits_requests_to_the_viewport() {
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).unwrap();
    let raster = opened.raster.clone();
    let cache = Arc::new(MemoryCache::new());

    let mut session = DocumentSession::new(
        opened,
        cache.clone(),
        Arc::new(ImmediateScheduler::new(raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话");

    assert_eq!(
        session.tiling_policy(),
        TilingPolicy::default(),
        "默认策略是 AbovePixels(16 MP)"
    );

    // 强行分块，并把视口缩到只覆盖页面左上角一小块（网格是 2×2）
    session.set_tiling_policy(TilingPolicy::Always);
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(300.0, 300.0)), &mut events);
    // 明确回到内容原点：视口恰好覆盖页 0 的左上角 198×288 像素（都在第 0 块瓦片内）
    session.handle(ViewerCommand::SetScroll(Vec2::ZERO), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    // 分块路径下整页位图不可用（UI 必须走 tiles_for 拼接）
    assert!(session.image_for(PageIndex::new(0)).is_none());

    let tiles = session.tiles_for(PageIndex::new(0));
    assert_eq!(tiles.len(), 1, "视口 300×300 只覆盖左上角一块瓦片");
    let (tile, image) = &tiles[0];
    assert_eq!((tile.col, tile.row), (0, 0));
    assert_eq!((image.width, image.height), (512, 512));
    assert_eq!(cache.stats().entries, 1, "只应渲染被视口覆盖的那一块");

    // 切回整页策略：瓦片路径关闭，整页位图可用
    session.set_tiling_policy(TilingPolicy::Never);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    assert!(session.tiles_for(PageIndex::new(0)).is_empty());
    assert_eq!(session.image_for(PageIndex::new(0)).unwrap().width, 612);
}

/// D-05 端到端：缩放前后"视野中心对应的页内位置"必须不变。
///
/// 数值取自 data-flow 场景 4：612×792pt 页、zoom 1.5、视口 900×1000、scroll (0,0)
/// → 中心对应页 0 的 PDF (292.00, 466.67)。
/// 注意用**单页**文档：demo 文档第 3 页带 /Rotate 90，会让内容宽度变大并水平居中，
/// 从而改变页 0 的横向位置（那些数字只适用于单页文档）。
#[test]
fn zoom_keeps_the_view_center_anchored() {
    let backend = MemoryBackend::new(vec![MemoryPage::new(
        Size::new(612.0, 792.0),
        Rotation::None,
        vec!["Hello world".to_string(), "Second line".to_string()],
    )]);
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).unwrap();
    let raster = opened.raster.clone();
    let mut session = DocumentSession::new(
        opened,
        Arc::new(MemoryCache::new()),
        Arc::new(ImmediateScheduler::new(raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话");

    let mut events = Vec::new();
    session.handle(ViewerCommand::SetZoom(1.5), &mut events);
    session.handle(ViewerCommand::ResizeViewport(Size::new(900.0, 1000.0)), &mut events);
    session.handle(ViewerCommand::SetScroll(Vec2::ZERO), &mut events);

    let before = session.capture_anchor().expect("视口中心应落在页 0 上");
    assert_eq!(before.page, PageIndex::new(0));
    assert!((before.page_point.x - 292.0).abs() < 0.01, "got {:?}", before.page_point);
    assert!((before.page_point.y - 466.67).abs() < 0.01, "got {:?}", before.page_point);

    // 放大到 2.0：视野中心必须仍是同一个页内位置
    session.handle(ViewerCommand::SetZoom(2.0), &mut events);
    let after = session.capture_anchor().expect("缩放后中心仍应落在页 0 上");
    assert_eq!(after.page, before.page);
    assert!(
        (after.page_point.x - before.page_point.x).abs() < 0.05
            && (after.page_point.y - before.page_point.y).abs() < 0.05,
        "缩放后视野中心漂移了：{before:?} → {after:?}"
    );

    // 对应的滚动偏移与文档推算一致： (146.00, 162.67)
    assert!((session.viewport().scroll.x - 146.0).abs() < 0.5, "got {:?}", session.viewport().scroll);
    assert!((session.viewport().scroll.y - 162.67).abs() < 0.5, "got {:?}", session.viewport().scroll);

    // 再缩回去也必须回到原处（可逆）
    session.handle(ViewerCommand::SetZoom(1.5), &mut events);
    let round_trip = session.capture_anchor().expect("缩回后中心仍应落在页 0 上");
    assert!(
        (round_trip.page_point.x - before.page_point.x).abs() < 0.1
            && (round_trip.page_point.y - before.page_point.y).abs() < 0.1,
        "往复缩放后中心漂移了：{before:?} → {round_trip:?}"
    );
}

/// D-05 退化路径：视野中心落在页间距时捕获失败，应退化为"当前页顶部对齐"而不是 panic。
///
/// 注意第二轮验收统一了跳转语义：`scroll_to_page` 是**顶部对齐**（不再是居中），
/// 因此锚点捕获失败时的兜底行为也随之变为顶部对齐。
#[test]
fn anchor_falls_back_to_page_top_when_center_is_between_pages() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(700.0, 800.0)), &mut events);

    // zoom=1 时页 0 矩形 y 12..804、页 1 起点 y=816；让中心落在 810（页间距内）
    session.handle(ViewerCommand::SetScroll(Vec2::new(0.0, 410.0)), &mut events);
    assert!(session.capture_anchor().is_none(), "中心落在页间距，应捕获失败");

    let current = session.state().current_page;
    session.handle(ViewerCommand::SetZoom(1.5), &mut events); // 不得 panic

    // 退化行为 = 当前页顶部对齐（受 max_scroll 夹取）
    let placement = *session.layout().placement(current).expect("当前页应有布局");
    let expected = placement.rect.y.min(content_max_scroll_y(&session));
    assert!(
        (session.viewport().scroll.y - expected).abs() < 0.5,
        "期望退化到当前页顶部对齐 {expected}，实际 {:?}",
        session.viewport().scroll
    );
}

/// 第三轮验收：缩放之后，**旧比例**的在飞任务必须被取消。
///
/// 不取消的话，PDFium（串行）会先把旧比例画完再画新的 ——
/// 用户感受到的就是"每次缩放都顿一下"。这条测试用同步调度器同样成立：
/// 同步实现没有作业队列，只能把已产生的事件改写成 `Cancelled`，
/// 因此"旧比例不得产出 `PageRendered`"是可断言的。
#[test]
fn zooming_cancels_in_flight_renders_at_the_old_scale() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(700.0, 800.0)), &mut events);

    // 提交但不取回：此刻 scale=1 的任务处于"在飞"状态
    session.handle(ViewerCommand::RequestVisible, &mut events);
    assert!(session.pending_renders() > 0, "应有在飞任务");
    events.clear();

    // 放大 → 旧比例的在飞任务应被取消
    session.handle(ViewerCommand::SetZoom(2.0), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    let old = RenderKey::new(PageIndex::FIRST, Scale::from_zoom(1.0), Rotation::None);
    let new = RenderKey::new(PageIndex::FIRST, Scale::from_zoom(2.0), Rotation::None);
    let rendered: Vec<RenderKey> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::PageRendered(key) => Some(*key),
            _ => None,
        })
        .collect();
    assert!(
        !rendered.contains(&old),
        "旧比例不应产出渲染结果（否则白渲染一次）：{rendered:?}"
    );
    assert!(rendered.contains(&new), "新比例必须渲染：{rendered:?}");
    assert!(session.image_for(PageIndex::FIRST).is_some(), "新比例的位图应已入缓存");
}

/// 链接命中测试（用户需求：链接目的地有具体坐标时，跳转要把该坐标放到视口中心）。
///
/// 核心层负责"点在哪个热区里"，UI 负责把 `offset` 变成居中命令 —— 两边都有测试。
#[test]
fn link_hit_testing_returns_the_destination_with_its_offset() {
    let mut session = assemble();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(700.0, 800.0)), &mut events);

    // 演示文档每页有一条"跳到下一页"的热区：左留白起、页面顶部往下 24pt 处、160×18
    let placement = *session.placement(PageIndex::FIRST).expect("第 1 页布局");
    let page_size = placement.page_size;
    let inside = Point::new(40.0, page_size.height - 12.0);
    let destination = session.link_at(PageIndex::FIRST, inside).expect("应命中链接");
    assert_eq!(
        destination,
        Destination::Page { page: PageIndex::new(1), offset: Some(Point::new(36.0, 20.0)) },
        "链接目标应带具体落点"
    );

    // 热区之外不命中
    assert_eq!(session.link_at(PageIndex::FIRST, Point::new(40.0, 20.0)), None);
    // 最后一页没有链接
    let last = session.page_count() - 1;
    assert_eq!(session.link_at(PageIndex::new(last), inside), None);
}
