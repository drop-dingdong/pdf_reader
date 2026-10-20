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
            eprintln!(
                "跳过：未找到 PDFium 动态库。请先运行 scripts/fetch-pdfium.sh 或设置 PDFIUM_PATH。"
            );
            None
        }
    }
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.5,
        "期望 {expected}，实际 {actual}"
    );
}

/// 按网格构造一个块请求（D-95 的形状：块的**页面空间矩形** + **期望像素尺寸**）。
///
/// 与 `DocumentSession` 内部走的是同一个 `TileGrid`，因此这些测试同时验证
/// "块矩形 → 设备像素"的换算与上位层一致。
fn tile_request(
    page: PageIndex,
    page_size: Size,
    rotation: Rotation,
    zoom: f32,
    tile: TileSpec,
) -> RenderRequest {
    let grid = TileGrid::new(page_size.rotated(rotation), zoom);
    let (_, _, width, height) = grid.tile_px(tile.col, tile.row);
    let region = RenderRegion {
        page_rect: grid.tile_page_rect(tile.col, tile.row, page_size, rotation),
        pixel_size: (width, height),
    };
    RenderRequest::tiled(page, Scale::from_zoom(zoom), rotation, tile, region)
}

#[test]
fn opens_from_path_and_reports_unrotated_geometry() {
    let Some(backend) = backend() else { return };
    let path = common::two_page_pdf_file("geometry");
    let opened = backend
        .open(Source::Path(&path), None)
        .expect("打开临时 PDF");

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
        opened
            .document
            .page_rotation(PageIndex::new(1))
            .expect("第 2 页旋转"),
        Rotation::Clockwise90
    );
    assert!(matches!(
        opened.document.page_size(PageIndex::new(9)),
        Err(CoreError::PageOutOfRange { .. })
    ));

    assert_eq!(
        opened.capabilities.max_concurrency, 1,
        "PDFium 必须声明并发上限 1"
    );
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
        Some(Destination::Page {
            page: PageIndex::FIRST,
            offset: None
        })
    );

    let links = navigation.links_on(PageIndex::FIRST);
    assert_eq!(links.len(), 1, "第 1 页有一个跳转到第 2 页的链接");
    assert_eq!(
        links[0].destination,
        Destination::Page {
            page: PageIndex::new(1),
            offset: None
        }
    );
    // 链接热区在 PDF 页面空间且落在页面内
    assert!(links[0].rect.x >= 0.0 && links[0].rect.right() <= 612.5);
    assert!(links[0].rect.y >= 0.0 && links[0].rect.bottom() <= 792.5);

    assert!(
        navigation.links_on(PageIndex::new(1)).is_empty(),
        "第 2 页没有链接"
    );
    assert!(
        navigation.links_on(PageIndex::new(99)).is_empty(),
        "越界返回空切片"
    );
}

#[test]
fn renders_whole_page_and_detects_ink() {
    let Some(backend) = backend() else { return };
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");

    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale::IDENTITY,
            Rotation::None,
        ))
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
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");

    // MediaBox 200×400 + /Rotate 90 → 显示 400×200
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::new(1),
            Scale::IDENTITY,
            Rotation::Clockwise90,
        ))
        .expect("旋转页渲染");
    assert_eq!(
        (image.width, image.height),
        (400, 200),
        "位图必须已是显示朝向"
    );
}

#[test]
fn tiles_render_only_the_requested_region() {
    let Some(backend) = backend() else { return };
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");

    // 612×792 @2x → 1224×1584 → 2×2 网格，每块 612×792（D-94：网格由 TileGrid 给出）
    let scale = Scale::from_zoom(2.0);
    let grid = TileGrid::new(Size::new(612.0, 792.0), 2.0);
    assert_eq!(grid.side, 2);
    assert_eq!(grid.render_px(), (1224, 1584));

    let whole = opened
        .raster
        .render(&RenderRequest::new(PageIndex::FIRST, scale, Rotation::None))
        .expect("整页 2x");
    assert_eq!((whole.width, whole.height), grid.render_px());

    // 左上角块：尺寸 = 网格给出的像素矩形，且必须包含文字（文字位于页面顶部）
    let (x, y, width, height) = grid.tile_px(0, 0);
    assert_eq!((x, y, width, height), (0, 0, 612, 792));
    let tile = opened
        .raster
        .render(&tile_request(
            PageIndex::FIRST,
            Size::new(612.0, 792.0),
            Rotation::None,
            2.0,
            TileSpec::new(0, 0),
        ))
        .expect("块 (0,0)");
    assert_eq!((tile.width, tile.height), (width, height));
    let ink = tile.data().iter().filter(|&&byte| byte < 100).count();
    assert!(ink > 100, "第 0 块应包含文字像素，实际 {ink}");

    // 右下角块（页面底部空白）：尺寸同样来自网格，且不应有文字
    let (_, _, width, height) = grid.tile_px(1, 1);
    let empty = opened
        .raster
        .render(&tile_request(
            PageIndex::FIRST,
            Size::new(612.0, 792.0),
            Rotation::None,
            2.0,
            TileSpec::new(1, 1),
        ))
        .expect("块 (1,1)");
    assert_eq!((empty.width, empty.height), (width, height));
    let ink = empty.data().iter().filter(|&&byte| byte < 100).count();
    assert_eq!(ink, 0, "页面底部空白区域不应有文字像素");

    // 越界块必须报错（而不是渲染一张 0×0 的图）
    assert!(opened
        .raster
        .render(&tile_request(
            PageIndex::FIRST,
            Size::new(612.0, 792.0),
            Rotation::None,
            2.0,
            TileSpec::new(2, 0),
        ))
        .is_err());
}

#[test]
fn extracts_text_with_boxes_and_searches() {
    let Some(backend) = backend() else { return };
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");
    let text_layer = opened.text.as_ref().expect("PDFium 提供文本层");

    let page = text_layer.page_text(PageIndex::FIRST).expect("第 1 页文本");
    assert!(
        page.text.contains("Hello World"),
        "实际提取：{:?}",
        page.text
    );
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
    assert!(
        first.bbox.height > 0.0,
        "字符包围盒不应退化：{:?}",
        first.bbox
    );

    // 搜索（不区分大小写）
    let hits = text_layer.search(&SearchQuery::new("world")).expect("搜索");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].page, PageIndex::FIRST);
    assert!(!hits[0].rects.is_empty(), "命中应有高亮矩形");

    // 第 2 页也可提取
    let second = text_layer
        .page_text(PageIndex::new(1))
        .expect("第 2 页文本");
    assert!(
        second.text.contains("Second"),
        "实际提取：{:?}",
        second.text
    );

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
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");
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
    session.handle(
        ViewerCommand::ResizeViewport(Size::new(800.0, 1000.0)),
        &mut events,
    );
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    // 位图经"调度 → 缓存"就绪。尺寸按**波段渲染比例**给（D-94：超采样，比显示尺寸大）
    let render = session.render_scale(PageIndex::FIRST);
    let image = session
        .image_for(PageIndex::FIRST)
        .expect("首页应渲染并写入缓存");
    assert_eq!(
        (image.width, image.height),
        (
            (612.0 * render.0).round() as u32,
            (792.0 * render.0).round() as u32
        )
    );
    assert!(render.0 >= 1.0, "渲染比例不得小于显示比例");
    assert!(events
        .iter()
        .any(|event| matches!(event, SessionEvent::PageRendered(_))));

    // 文本层驱动选择与高亮（真实字符包围盒）
    session.handle(ViewerCommand::SelectAll, &mut events);
    let selected = session.selected_text();
    assert!(selected.contains("Hello World"), "实际选中：{selected:?}");
    let highlights = session.highlight_selection();
    assert!(!highlights.is_empty(), "应有高亮矩形");
    assert!(highlights
        .iter()
        .all(|highlight| !highlight.rects.is_empty()));

    // 大纲来自真实后端
    let navigation = session.navigation().expect("导航索引");
    assert_eq!(navigation.outline[0].title, "Chapter One");

    // 翻页后第 2 页（带 /Rotate 90）的布局尺寸应为旋转后的 400×200（zoom=1）
    session.handle(ViewerCommand::GoToPage(PageIndex::new(1)), &mut events);
    let placement = session.placement(PageIndex::new(1)).expect("第 2 页布局");
    assert_close(placement.rect.width, 400.0);
    assert_close(placement.rect.height, 200.0);
}

/// 分块渲染的**几何正确性**：整页布满边框与横线的文档里，每一块瓦片都必须有墨。
///
/// 这条测试是为了钉住第二轮验收的"800% 视野全黑"：
/// 当时的适配层把瓦片区域用 `set_origin(-x, -y)` 传成**负起点**，
/// 而 PDFium 在负起点下不绘制任何内容 —— 于是除左上角外所有瓦片都是空白透明的，
/// 画布露出深色背景，看起来就是"全黑"。
#[test]
fn every_tile_contains_ink_for_a_full_page_document() {
    let Some(backend) = backend() else { return };
    let path = common::many_page_pdf_file("tile-ink", 2, 612.0, 792.0);
    let opened = backend
        .open(Source::Path(&path), None)
        .expect("打开多页 PDF");

    // 612×792 @2x → 1224×1584 → 2×2 网格
    let zoom = 2.0;
    let grid = TileGrid::new(Size::new(612.0, 792.0), zoom);
    assert_eq!(grid.side, 2);

    for (col, row) in grid.all_tiles() {
        let tile = opened
            .raster
            .render(&tile_request(
                PageIndex::FIRST,
                Size::new(612.0, 792.0),
                Rotation::None,
                zoom,
                TileSpec::new(col, row),
            ))
            .expect("块");
        let ink = tile.data().iter().filter(|&&byte| byte < 100).count();
        assert!(
            ink > 50,
            "块 ({col},{row}) 必须包含墨迹（该页整页都有边框与横线），实际深色字节 {ink}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// 块内容必须等于"整页渲染后裁出的同一区域"：几何、朝向、缩放全都对得上。
///
/// 这是分块渲染最强的一条不变量 —— 只要块与整页裁剪一致，
/// 拼接出来的画面就必然与不分块时逐像素等价。
///
/// 用两种页宽验证：612×792（每块正好 612×792，边界整数）与 610×790（边界必须取整，
/// 每块宽度在 457/458 之间交替 —— 这正是"共享整数边界"要覆盖的情形）。
#[test]
fn tiles_match_the_crop_of_the_whole_page_render() {
    let Some(backend) = backend() else { return };

    let check = |name: &str, width: f32, height: f32, zoom: f32, rotation: Rotation| {
        let path = common::many_page_pdf_file(name, 1, width, height);
        let opened = backend.open(Source::Path(&path), None).expect("打开");

        let scale = Scale::from_zoom(zoom);
        let page_size = Size::new(width, height);
        let grid = TileGrid::new(page_size.rotated(rotation), zoom);
        let whole = opened
            .raster
            .render(&RenderRequest::new(PageIndex::FIRST, scale, rotation))
            .expect("整页");
        assert_eq!((whole.width, whole.height), grid.render_px());

        for (col, row) in grid.all_tiles() {
            let tile = opened
                .raster
                .render(&tile_request(
                    PageIndex::FIRST,
                    page_size,
                    rotation,
                    zoom,
                    TileSpec::new(col, row),
                ))
                .expect("块");
            let (tile_x, tile_y, tile_width, tile_height) = grid.tile_px(col, row);
            assert_eq!((tile.width, tile.height), (tile_width, tile_height));

            let mut worst = 0u8;
            for y in 0..tile.height as usize {
                for x in 0..tile.width as usize {
                    let tile_offset = (y * tile.width as usize + x) * 4;
                    let whole_offset =
                        ((tile_y as usize + y) * whole.width as usize + (tile_x as usize + x)) * 4;
                    for channel in 0..4 {
                        let diff = tile.data()[tile_offset + channel]
                            .abs_diff(whole.data()[whole_offset + channel]);
                        worst = worst.max(diff);
                    }
                }
            }
            assert!(
                worst <= 16,
                "块 ({col},{row}) 与整页裁剪不一致：最大通道差 {worst}（rotation={rotation:?}）"
            );
        }
        let _ = std::fs::remove_file(&path);
    };

    check("tile-crop-even", 612.0, 792.0, 2.0, Rotation::None);
    check("tile-crop-odd", 610.0, 790.0, 3.0, Rotation::None);
    // 旋转 90°：块矩形在页面空间里是"横"的，映射到设备后要交换宽高 —— 最容易算错的一种
    check(
        "tile-crop-rotated",
        610.0,
        790.0,
        3.0,
        Rotation::Clockwise90,
    );
}

/// 800% 现场诊断：高倍缩放下瓦片是否仍然有内容。
#[test]
fn tiles_at_800_percent_still_contain_ink() {
    let Some(backend) = backend() else { return };
    let path = common::many_page_pdf_file("tile-800", 2, 612.0, 792.0);
    let opened = backend.open(Source::Path(&path), None).expect("打开");

    // 612×792 @8x → 4896×6336 → 8×8 网格，每块 612×792
    let zoom = 8.0;
    let grid = TileGrid::new(Size::new(612.0, 792.0), zoom);
    assert_eq!(grid.side, 8);
    assert_eq!(grid.render_px(), (4896, 6336));

    for (col, row) in [(0u32, 0u32), (1, 0), (0, 1), (2, 1), (5, 6), (7, 7)] {
        let started = std::time::Instant::now();
        let tile = opened
            .raster
            .render(&tile_request(
                PageIndex::FIRST,
                Size::new(612.0, 792.0),
                Rotation::None,
                zoom,
                TileSpec::new(col, row),
            ))
            .expect("块");
        let ink = tile.data().iter().filter(|&&byte| byte < 100).count();
        println!(
            "块 ({col},{row}) {}×{} 用时 {:?} 深色字节 {ink}",
            tile.width,
            tile.height,
            started.elapsed()
        );
        assert!(ink > 50, "800% 下块 ({col},{row}) 必须包含墨迹，实际 {ink}");
    }
    let _ = std::fs::remove_file(&path);
}

/// 契约（D-95）：期望像素尺寸与几何推导不一致时**必须报错**，不得静默拉伸。
///
/// 静默拉伸的后果是"看起来有图，但同一行文字在块接缝处错位"（D-84/D-86）——
/// 比直接失败难查得多，因此这里把它钉成硬失败。
#[test]
fn wrong_expected_tile_size_is_rejected_not_stretched() {
    let Some(backend) = backend() else { return };
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");
    let page_size = Size::new(612.0, 792.0);

    let mut wrong_size = tile_request(
        PageIndex::FIRST,
        page_size,
        Rotation::None,
        2.0,
        TileSpec::new(0, 0),
    );
    if let Some(region) = wrong_size.region.as_mut() {
        region.pixel_size.0 += 7;
    }
    match opened.raster.render(&wrong_size) {
        Err(CoreError::Internal(message)) => {
            assert!(
                message.contains("尺寸"),
                "错误信息应指出尺寸不符，实际：{message}"
            )
        }
        Ok(image) => panic!(
            "尺寸不符必须报错，实际渲染出 {}×{}",
            image.width, image.height
        ),
        Err(other) => panic!("应返回 Internal，实际 {other:?}"),
    }

    // 块矩形落在半像素上同样必须报错（否则整块内容会被平移/缩放）
    let mut half_pixel = tile_request(
        PageIndex::FIRST,
        page_size,
        Rotation::None,
        2.0,
        TileSpec::new(0, 0),
    );
    if let Some(region) = half_pixel.region.as_mut() {
        // 2 倍下 0.25pt = 0.5px：落在半像素上
        region.page_rect.x += 0.25;
    }
    assert!(
        matches!(
            opened.raster.render(&half_pixel),
            Err(CoreError::Internal(_))
        ),
        "半像素的块矩形必须被拒绝"
    );

    // 缺区域 = 无法知道该渲染哪块，必须报错
    let mut missing = tile_request(
        PageIndex::FIRST,
        page_size,
        Rotation::None,
        2.0,
        TileSpec::new(0, 0),
    );
    missing.region = None;
    assert!(
        matches!(opened.raster.render(&missing), Err(CoreError::Internal(_))),
        "瓦片请求缺区域必须报错"
    );
}

/// **验收判据 5**（真实后端版）：同一波段内改缩放不产生新的渲染请求。
///
/// 与 `pdfcore/tests/assemble.rs` 里那条同名测试互补：这里走真实 PDFium 渲染，
/// 因此能同时验证"键里的比例与真正送给 PDFium 的比例一致"。
#[test]
fn zooming_inside_a_band_does_not_render_again_with_the_real_backend() {
    let Some(backend) = backend() else { return };
    let opened = backend
        .open(Source::Bytes(&common::two_page_pdf()), None)
        .expect("打开");
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
    session.handle(
        ViewerCommand::ResizeViewport(Size::new(600.0, 700.0)),
        &mut events,
    );
    // 100% 与 150% 都落在波段 0（[1, 1.6)），渲染比例恒为 1.6
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    let render = session.render_scale(PageIndex::FIRST);
    let image_at_100 = session.image_for(PageIndex::FIRST).expect("100% 应已渲染");
    assert_eq!(
        (image_at_100.width, image_at_100.height),
        (
            (612.0 * render.0).round() as u32,
            (792.0 * render.0).round() as u32
        )
    );

    events.clear();
    session.handle(ViewerCommand::SetZoom(1.5), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    assert_eq!(
        session.render_scale(PageIndex::FIRST),
        render,
        "波段内渲染比例不变"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, SessionEvent::PageRendered(_))),
        "同一波段内不应重新渲染：{events:?}"
    );
    assert!(
        session.image_for(PageIndex::FIRST).is_some(),
        "复用同一张位图，整页图仍应可用"
    );
}

/// 坐标归一化：`MediaBox` 原点非 0 时，**文本与链接热区**都必须归一化到渲染盒坐标系。
///
/// 这是"某些 PDF 的链接框/选取高亮与文字整体错位"的修复验证：
/// PDFium 的文本与注释坐标按 `MediaBox`，而渲染按 `MediaBox ∩ CropBox`，
/// 两者原点不同就会整体偏移。
#[test]
fn geometry_text_and_links_are_normalized_to_the_rendered_box() {
    let Some(backend) = backend() else { return };
    let path = common::offset_media_box_pdf_file("offset-media-box");
    let opened = backend.open(Source::Path(&path), None).expect("打开");

    let geometries = opened.document.page_geometries().expect("批量页几何");
    assert_eq!(geometries.len(), 1);
    // 尺寸不受原点影响（仍是 612×792），原点被记录下来
    assert_close(geometries[0].size.width, 612.0);
    assert_close(geometries[0].size.height, 792.0);
    assert_close(geometries[0].origin.x, 20.0);
    assert_close(geometries[0].origin.y, 30.0);

    // 链接热区：原始 /Rect [70 500 300 520] → 归一化后应整体减去 (20, 30)
    let navigation = opened.document.navigation().expect("导航索引");
    let links = navigation.links_on(PageIndex::FIRST);
    assert_eq!(links.len(), 1);
    assert_close(links[0].rect.x, 50.0);
    assert_close(links[0].rect.y, 470.0);
    assert_close(links[0].rect.width, 230.0);
    assert_close(links[0].rect.height, 20.0);
    // 归一化后必须落在页面范围内（这正是"错位"时不会满足的条件）
    assert!(links[0].rect.x >= 0.0 && links[0].rect.right() <= 612.5);
    assert!(links[0].rect.y >= 0.0 && links[0].rect.bottom() <= 792.5);

    // 文本字符框同样归一化：文字基点 x=72 → 归一化后 ≈ 52
    let text = opened
        .text
        .as_ref()
        .expect("文本层")
        .page_text(PageIndex::FIRST)
        .expect("文本层");
    let first = text
        .chars_in(TextRange::new(0, 1))
        .first()
        .expect("至少一个字符");
    assert!(
        (first.bbox.x - 52.0).abs() < 4.0,
        "字符 x 应约为 52（72−20），实际 {}",
        first.bbox.x
    );
    // **像素级对齐检验**（不依赖字体度量猜测）：把这一页渲染出来，取墨迹范围，
    // 换算到 PDF 页面空间后必须与归一化后的字符框重叠 ——
    // 这正是用户反馈"选取区域/搜索高亮与实际文字不符"的直接判据。
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale::IDENTITY,
            Rotation::None,
        ))
        .expect("整页渲染");
    assert_eq!((image.width, image.height), (612, 792));
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..image.height {
        for x in 0..image.width {
            let offset = ((y * image.width + x) * 4) as usize;
            if image.data()[offset..offset + 3]
                .iter()
                .all(|byte| *byte < 128)
            {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    assert!(min_x != u32::MAX, "测试页应当有文字墨迹");
    // 图像 y 向下、PDF 页面空间 y 向上
    let ink = pdfcore::prelude::Rect::new(
        min_x as f32,
        792.0 - max_y as f32 - 1.0,
        (max_x - min_x) as f32,
        (max_y - min_y) as f32,
    );
    let overlaps = |a: pdfcore::prelude::Rect, b: pdfcore::prelude::Rect| {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    };
    assert!(
        overlaps(ink, first.bbox),
        "归一化后的字符框 {:?} 必须与实际墨迹 {ink:?} 重叠",
        first.bbox
    );

    let _ = std::fs::remove_file(&path);
}
