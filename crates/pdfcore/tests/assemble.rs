//! 端到端装配测试：**不依赖 PDFium、不依赖 PDF 文件、不依赖 GPU**，
//! 用内存假后端把整条链路跑通。
//!
//! 这个测试是本次 trait 设计的核心验收项：如果它能通过，就证明
//! "后端 / 缓存 / 调度 / 布局 / 选区" 五个零件确实可以由外部拼装起来，
//! 并且 egui 前端将来只需要对接 [`pdfcore::session::ViewerCommand`] 与
//! [`pdfcore::session::SessionEvent`]。

use std::sync::Arc;

use pdfcore::prelude::*;
use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache};

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

    let outline = session.outline().expect("假后端提供大纲");
    assert_eq!(outline.len(), 3);
    assert_eq!(outline[0].destination, Some(Destination::Page { page: PageIndex::FIRST, offset: None }));
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
