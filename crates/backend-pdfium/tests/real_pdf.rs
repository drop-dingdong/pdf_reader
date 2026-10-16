//! 真实 PDF 的端到端测试：`PdfiumBackend` → `pdfcore` 各 trait → `DocumentSession`。
//!
//! 找不到 PDFium 动态库时**跳过并给出指引**（先跑 `scripts/fetch-pdfium.sh`）。
//! 测试用的 PDF 由 `tests/common/mod.rs` 现场生成，因此仓库里没有二进制夹具。

mod common;

use std::sync::Arc;

use backend_pdfium::PdfiumBackend;
use pdfcore::prelude::*;
use pdfcore::testkit::{ImmediateScheduler, MemoryCache};

/// 取后端；动态库缺失时返回 `None`（调用方据此跳过）。
fn backend() -> Option<PdfiumBackend> {
    match backend_pdfium::pdfium() {
        Ok(_) => Some(PdfiumBackend::new()),
        Err(_) => {
            eprintln!("跳过：未找到 PDFium 动态库。请先运行 scripts/fetch-pdfium.sh 或设置 PDFIUM_PATH。");
            None
        }
    }
}

fn assert_close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.5, "期望 {expected}，实际 {actual}");
}

#[test]
fn opens_from_path_and_reports_unrotated_geometry() {
    let Some(backend) = backend() else { return };
    let path = common::two_page_pdf_file("geometry");
    let opened = backend.open(Source::Path(&path), None).expect("打开临时 PDF");

    // D-01：一次拿到批量页几何（不再逐页跨 FFI）
    let geometries = opened.document.page_geometries().expect("批量页几何");
    assert_eq!(geometries.len(), 2);

    assert_close(geometries[0].size.width, 612.0);
    assert_close(geometries[0].size.height, 792.0);
    assert_eq!(geometries[0].rotation, Rotation::None);

    // 第 2 页 MediaBox 是 200×400 且带 /Rotate 90。
    // 注意：PDFium 的 page.width()/height() 返回的是**旋转后**的显示尺寸（400×200），
    // 适配层必须还原成 MediaBox（200×400），否则布局会把它再旋转一次。
    assert_close(geometries[1].size.width, 200.0);
    assert_close(geometries[1].size.height, 400.0);
    assert_eq!(geometries[1].rotation, Rotation::Clockwise90);

    // 单页查询走 trait 默认实现（查表），越界必须报错
    assert_eq!(
        opened.document.page_rotation(PageIndex::new(1)).expect("第 2 页旋转"),
        Rotation::Clockwise90
    );
    assert!(matches!(
        opened.document.page_size(PageIndex::new(9)),
        Err(CoreError::PageOutOfRange { .. })
    ));

    assert_eq!(opened.capabilities.max_concurrency, 1, "PDFium 必须声明并发上限 1");
    assert!(opened.capabilities.text_layer, "PDFium 提供文本层");
    assert!(opened.has_text_layer());

    let _ = std::fs::remove_file(path);
}

#[test]
fn opens_from_bytes_and_pre_resolves_navigation() {
    let Some(backend) = backend() else { return };
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("从字节流打开");

    // D-08：大纲与链接在打开时已解析完毕
    let navigation = opened.document.navigation().expect("导航索引");
    assert_eq!(navigation.outline.len(), 1);
    assert_eq!(navigation.outline[0].title, "Chapter One");
    assert_eq!(
        navigation.outline[0].destination,
        Some(Destination::Page { page: PageIndex::FIRST, offset: None })
    );

    let links = navigation.links_on(PageIndex::FIRST);
    assert_eq!(links.len(), 1, "第 1 页有一个跳转到第 2 页的链接");
    assert_eq!(links[0].destination, Destination::Page { page: PageIndex::new(1), offset: None });
    // 链接热区在 PDF 页面空间且落在页面内
    assert!(links[0].rect.x >= 0.0 && links[0].rect.right() <= 612.5);
    assert!(links[0].rect.y >= 0.0 && links[0].rect.bottom() <= 792.5);

    assert!(navigation.links_on(PageIndex::new(1)).is_empty(), "第 2 页没有链接");
    assert!(navigation.links_on(PageIndex::new(99)).is_empty(), "越界返回空切片");
}

#[test]
fn renders_whole_page_and_detects_ink() {
    let Some(backend) = backend() else { return };
    let opened = backend.open(Source::Bytes(&common::two_page_pdf()), None).expect("打开");

    let image = opened
        .raster
        .render(&RenderRequest::new(PageIndex::FIRST, Scale::IDENTITY, Rotation::None))
        .expect("整页渲染");

    assert_eq!((image.width, image.height), (612, 792));
    assert_eq!(image.format, PixelFormat::Bgra8, "PDFium 原生输出 BGRA");
    // 白底黑字：必须存在明显深色像素 —— 证明真的渲染出了内容而不是空白位图
    let ink = image.data().iter().filter(|&&byte| byte < 100).count();
    assert!(ink > 100, "应渲染出文字像素，实际深色字节数 {ink}");
}

#[test]
fn rotated_page_is_rendered_in_displayed_orientation() {
    let Some(backend) = backend() else { return };
    let opened = backend.open(Source::Bytes(&common::two_page_pdf()), None).expect("打开");

    // MediaBox 200×400 + /Rotate 90 → 显示 400×200
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::new(1),
            Scale::IDENTITY,
            Rotation::Clockwise90,
        ))
        .expect("旋转页渲染");
    assert_eq!((image.width, image.height), (400, 200), "位图必须已是显示朝向");
}

#[test]
fn tiles_render_only_the_requested_region() {
    let Some(backend) = backend() else { return };
    let opened = backend.open(Source::Bytes(&common::two_page_pdf()), None).expect("打开");

    // 612×792 @2x → 1224×1584 → 网格 3×4
    let scale = Scale::from_zoom(2.0);
    assert_eq!(TileSpec::grid_for(Size::new(1224.0, 1584.0)), (3, 4));

    let whole = opened
        .raster
        .render(&RenderRequest::new(PageIndex::FIRST, scale, Rotation::None))
        .expect("整页 2x");
    assert_eq!((whole.width, whole.height), (1224, 1584));

    // 左上角瓦片：512×512，且必须包含文字（文字位于页面顶部）
    let tile = opened
        .raster
        .render(&RenderRequest::tiled(
            PageIndex::FIRST,
            scale,
            Rotation::None,
            TileSpec::new(0, 0),
        ))
        .expect("瓦片 (0,0)");
    assert_eq!((tile.width, tile.height), (512, 512));
    let ink = tile.data().iter().filter(|&&byte| byte < 100).count();
    assert!(ink > 100, "第 0 块瓦片应包含文字像素，实际 {ink}");

    // 右下角瓦片（页面底部空白）：尺寸为余数，且不应有文字
    let empty = opened
        .raster
        .render(&RenderRequest::tiled(
            PageIndex::FIRST,
            scale,
            Rotation::None,
            TileSpec::new(2, 3),
        ))
        .expect("瓦片 (2,3)");
    assert_eq!((empty.width, empty.height), (1224 - 1024, 1584 - 1536));
    let ink = empty.data().iter().filter(|&&byte| byte < 100).count();
    assert_eq!(ink, 0, "页面底部空白区域不应有文字像素");

    // 越界瓦片必须报错
    assert!(opened
        .raster
        .render(&RenderRequest::tiled(
            PageIndex::FIRST,
            scale,
            Rotation::None,
            TileSpec::new(3, 0),
        ))
        .is_err());
}

#[test]
fn extracts_text_with_boxes_and_searches() {
    let Some(backend) = backend() else { return };
    let opened = backend.open(Source::Bytes(&common::two_page_pdf()), None).expect("打开");
    let text_layer = opened.text.as_ref().expect("PDFium 提供文本层");

    let page = text_layer.page_text(PageIndex::FIRST).expect("第 1 页文本");
    assert!(page.text.contains("Hello World"), "实际提取：{:?}", page.text);
    // pdfcore 的不变量：字符一一对应
    assert_eq!(page.text.chars().count(), page.chars.len());

    // 包围盒在 PDF 页面空间（左下原点）：文字画在页面上部 → y 应大于半页高度。
    // 若适配层误翻了 y，这里会立刻失败。
    let first = page
        .chars
        .iter()
        .find(|character| character.ch.is_alphanumeric())
        .expect("应至少有一个可见字符");
    assert!(
        first.bbox.y > 400.0,
        "首字符包围盒应位于页面上半部（PDF 空间），实际 {:?}",
        first.bbox
    );
    assert!(first.bbox.height > 0.0, "字符包围盒不应退化：{:?}", first.bbox);

    // 搜索（不区分大小写）
    let hits = text_layer.search(&SearchQuery::new("world")).expect("搜索");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].page, PageIndex::FIRST);
    assert!(!hits[0].rects.is_empty(), "命中应有高亮矩形");

    // 第 2 页也可提取
    let second = text_layer.page_text(PageIndex::new(1)).expect("第 2 页文本");
    assert!(second.text.contains("Second"), "实际提取：{:?}", second.text);

    // 越界页
    assert!(matches!(
        text_layer.page_text(PageIndex::new(9)),
        Err(CoreError::PageOutOfRange { .. })
    ));
}

/// 最关键的一条：**用真实后端把阅读器整体装起来**（渲染 + 文本选择都走真实 PDF）。
#[test]
fn assembles_a_session_with_the_real_backend() {
    let Some(backend) = backend() else { return };
    let opened = backend.open(Source::Bytes(&common::two_page_pdf()), None).expect("打开");
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
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 1000.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    // 位图经"调度 → 缓存"就绪
    let image = session.image_for(PageIndex::FIRST).expect("首页应渲染并写入缓存");
    assert_eq!((image.width, image.height), (612, 792));
    assert!(events.iter().any(|event| matches!(event, SessionEvent::PageRendered(_))));

    // 文本层驱动选择与高亮（真实字符包围盒）
    session.handle(ViewerCommand::SelectAll, &mut events);
    let selected = session.selected_text();
    assert!(selected.contains("Hello World"), "实际选中：{selected:?}");
    let highlights = session.highlight_selection();
    assert!(!highlights.is_empty(), "应有高亮矩形");
    assert!(highlights.iter().all(|highlight| !highlight.rects.is_empty()));

    // 大纲来自真实后端
    let navigation = session.navigation().expect("导航索引");
    assert_eq!(navigation.outline[0].title, "Chapter One");

    // 翻页后第 2 页（带 /Rotate 90）的布局尺寸应为旋转后的 400×200（zoom=1）
    session.handle(ViewerCommand::GoToPage(PageIndex::new(1)), &mut events);
    let placement = session.placement(PageIndex::new(1)).expect("第 2 页布局");
    assert_close(placement.rect.width, 400.0);
    assert_close(placement.rect.height, 200.0);
}
