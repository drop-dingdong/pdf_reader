//! 画法决策与真实会话的联调（不需要窗口/GPU）：
//! 用 `DocumentSession` 的真实输出（网格 / 可见范围 / 渲染比例）去喂
//! `view::ready_tiles_in_view` 与 `view::plan_page`，验证"缺一块就不翻页"在真数据上成立，
//! 以及"缩小方向总有整页底料"（D-96 的两条改动）。

use std::sync::Arc;

use app::view::{plan_page, DrawInput, KeyedStore, PageDraw, Preview};
use pdfcore::prelude::*;
use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache, MemoryPage};

/// A4 单页文档 + 800%（必然分块：渲染像素面积远大于 4 MP）。
///
/// 返回位图缓存句柄：下面的"缩小"测试要用它模拟"位图已被 LRU 挤掉"的最坏情形。
fn session_at_800() -> (DocumentSession, PageIndex, Arc<MemoryCache>) {
    let backend = MemoryBackend::new(vec![MemoryPage::new(
        Size::new(595.28, 841.89),
        Rotation::None,
        vec!["Hello".to_string()],
    )]);
    let opened = backend.open(Source::Bytes(b"x"), None).unwrap();
    let raster = opened.raster.clone();
    let cache = Arc::new(MemoryCache::new());
    let mut session = DocumentSession::new(
        opened,
        cache.clone(),
        Arc::new(ImmediateScheduler::new(raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .unwrap();
    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(1200.0, 900.0)), &mut events);
    session.handle(ViewerCommand::SetZoom(8.0), &mut events);
    (session, PageIndex::FIRST, cache)
}

fn input(ready: usize, total: usize, preview: Preview, waited_ms: u64) -> DrawInput {
    DrawInput {
        tiled: true,
        has_whole_current: false,
        ready,
        total,
        preview,
        waited_ms,
        // 集成用例的意图是"首次进入该波段"，粘性由 view.rs 的单测覆盖
        showing_tiles: false,
    }
}

/// 把"当前已缓存的整页图/块"全部上传（模拟 app 的 `upload_page`：一次能把一页传完）。
fn upload_cached(
    session: &DocumentSession,
    store: &mut KeyedStore<&'static str>,
    page: PageIndex,
    render: Scale,
    rotation: Rotation,
) {
    if session.image_for(page).is_some() {
        store.insert(RenderKey::new(page, render, rotation), "whole");
    }
    for (tile, _) in session.tiles_for(page) {
        store.insert(RenderKey::tiled(page, render, rotation, tile), "tile");
    }
}

/// 收尾：把这一帧的渲染结果取回并上传（供下一个缩放档当底料）。
fn settle(session: &mut DocumentSession, store: &mut KeyedStore<&'static str>, page: PageIndex) {
    let mut events = Vec::new();
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    let placement = *session.placement(page).expect("布局");
    let render = session.render_scale(page);
    upload_cached(session, store, page, render, placement.rotation);
    let milli = RenderKey::new(page, render, placement.rotation).scale_milli;
    store.retain_preview_scales(&[(page, placement.rotation)], milli);
}

/// 到达某个缩放的**第一帧**：上传已缓存的 → 按新波段裁剪 → 返回可用的旧内容。
///
/// 顺序与 app 的 `ensure_textures`（先上传再裁剪）+ 绘制决策一致。
fn arrive(session: &mut DocumentSession, store: &mut KeyedStore<&'static str>, zoom: f32) -> Preview {
    let mut events = Vec::new();
    session.handle(ViewerCommand::SetZoom(zoom), &mut events);
    let page = session.state().current_page;
    let placement = *session.placement(page).expect("布局");
    let render = session.render_scale(page);
    let milli = RenderKey::new(page, render, placement.rotation).scale_milli;
    upload_cached(session, store, page, render, placement.rotation);
    store.retain_preview_scales(&[(page, placement.rotation)], milli);
    store.preview_source(page, placement.rotation, milli)
}

/// 可见范围的块数必须与"绘制循环会枚举的块数"一致，且键与 session 的渲染比例一致。
#[test]
fn ready_count_uses_the_same_keys_and_range_as_the_session() {
    let (session, page, _cache) = session_at_800();
    let grid = session.tile_grid(page).expect("800% 下必须分块");
    let range = session.visible_tiles(page).expect("应覆盖到块");
    let render = session.render_scale(page);
    // 容量要装得下整页的块：`KeyedStore` 是 LRU，容量小会把刚插入的块挤掉
    // （真实 app 的容量是 128，且只上传视口附近的块）。
    let mut store: KeyedStore<&'static str> = KeyedStore::new(512);

    let (ready, total) = store.ready_tiles_in_view(page, Rotation::None, render, &grid, range);
    assert!(total > 0, "800% 下视口应覆盖若干块");
    assert_eq!(ready, 0);

    // 只放一块：必须仍判定"未就绪"，且计划不翻页（这是白块修复的核心）
    let first = RenderKey::tiled(page, render, Rotation::None, TileSpec::new(range.0, range.1));
    store.insert(first, "one");
    let (ready, total_after) =
        store.ready_tiles_in_view(page, Rotation::None, render, &grid, range);
    assert_eq!((ready, total_after), (1, total));
    let plan = plan_page(&input(1, total, Preview::Tiles(6_554), 0));
    assert!(
        matches!(plan, PageDraw::TilePreview(6_554)),
        "缺一块时不得翻页（否则就是几块清晰 + 大片白）：实际 {plan:?}"
    );

    // 全部放进去：翻页
    for (col, row) in grid.all_tiles() {
        store.insert(
            RenderKey::tiled(page, render, Rotation::None, TileSpec::new(col, row)),
            "t",
        );
    }
    let (ready, total_after) =
        store.ready_tiles_in_view(page, Rotation::None, render, &grid, range);
    assert_eq!(ready, total_after, "整页的块都已就绪");
    let plan = plan_page(&input(ready, total_after, Preview::Tiles(6_554), 0));
    assert_eq!(plan, PageDraw::Tiles);
}

/// **缩小方向必须有整页底料**（D-96 的改动二）：从 800% 逐步缩回 100%，
/// 每一步都必须在**位图缓存已被挤掉**的前提下仍有整页图可顶。
///
/// 为什么先把位图缓存清掉：不清的话，缩小帧可以靠"重新上传 CPU 里还留着的位图"过关，
/// 这条测试就测不到"保留策略是否真的留住了整页底料"。
#[test]
fn zooming_out_keeps_a_whole_page_preview_even_after_bitmaps_are_evicted() {
    let (mut session, page, cache) = session_at_800();
    let mut store: KeyedStore<&'static str> = KeyedStore::new(512);

    // 先按 100% → 800% 走一遍，把各波段的位图/纹理都"渲染"出来
    for zoom in [1.0f32, 1.6, 2.6, 4.1, 6.6, 8.0] {
        let _ = arrive(&mut session, &mut store, zoom);
        settle(&mut session, &mut store, page);
    }

    // 模拟"位图缓存被 LRU 挤掉"：此后只能靠纹理层留下的底料
    cache.clear();

    // 再逐步缩回：每一次"到达"都必须有**整页**底料
    // （A4 在 100%–255% 是整页图，因此保留策略一定留得住一张；
    //   只剩"上次可见那一片块"是不够的 —— 其余部分会露白）
    for zoom in [6.5f32, 5.0, 4.0, 3.0, 2.5, 2.0, 1.5, 1.0] {
        let preview = arrive(&mut session, &mut store, zoom);
        assert!(
            matches!(preview, Preview::Whole(_)),
            "缩到 {zoom} 倍时必须有整页图可顶（否则只剩上次可见那一片，其余是白）：实际 {preview:?}"
        );
        settle(&mut session, &mut store, page);
    }
}

/// 同一波段内改缩放：渲染比例、整页像素尺寸都不应变化（D-94 的行为不能退化）。
#[test]
fn band_and_grid_are_stable_inside_a_band() {
    let (mut session, page, _cache) = session_at_800();
    let mut events = Vec::new();
    let grid = session.tile_grid(page).expect("分块");
    let render = session.render_scale(page);
    let range = session.visible_tiles(page).expect("范围");

    session.handle(ViewerCommand::SetZoom(7.0), &mut events);
    assert_eq!(session.render_scale(page), render, "7.0 与 8.0 仍在同一波段");
    assert_eq!(
        session.tile_grid(page).expect("分块").render_px(),
        grid.render_px(),
        "波段内整页像素尺寸不变"
    );
    // 波段内放大 → 视口覆盖的页面面积变小，可见块范围只会收缩
    let other = session.visible_tiles(page).expect("范围");
    assert!(
        other.0 <= range.0 && other.1 <= range.1 && other.2 <= range.2 && other.3 <= range.3,
        "波段内可见范围只应收缩：{range:?} → {other:?}"
    );
}

/// D-101：预览区域渲染 —— 按"记录的分辨率"请求块，就绪后能拼出**与当前缩放无关**的图。
#[test]
fn region_render_feeds_the_preview_at_the_recorded_scale() {
    let backend = MemoryBackend::new(vec![MemoryPage::new(
        Size::new(595.28, 841.89),
        Rotation::None,
        vec!["Hello".to_string()],
    )]);
    let opened = backend.open(Source::Bytes(b"x"), None).unwrap();
    let raster = opened.raster.clone();
    let mut session = DocumentSession::new(
        opened,
        Arc::new(MemoryCache::new()),
        Arc::new(ImmediateScheduler::new(raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .unwrap();
    let mut events = Vec::new();
    // 视口只显示页面顶部一小块：页面下半部分在缓存里没有像素
    session.handle(ViewerCommand::ResizeViewport(Size::new(600.0, 400.0)), &mut events);
    session.handle(ViewerCommand::SetZoom(8.0), &mut events);
    session.handle(ViewerCommand::SetScroll(Vec2::new(0.0, 0.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    let page = PageIndex::FIRST;
    // 目标区域在页面**下半部**（视口没去过）：当前波段一定没有它的块
    let target = Rect::new(100.0, 100.0, 120.0, 60.0);
    // 记录分辨率 200% → 吸到波段 2.56
    let render = Scale::from_zoom(render_scale_for(2.0, DEFAULT_OVERSCAN));
    assert!(session.region_tiles(page, target, render).is_none(), "还没请求，必然缺块");

    let count = session.request_region(page, target, render);
    assert!(count > 0, "应提交覆盖该区域的块");
    session.pump(&mut events);

    let tiles = session.region_tiles(page, target, render).expect("渲染完应全部就绪");
    assert!(!tiles.is_empty());
    // 该区域的像素尺寸 = 区域 × 渲染比例（与当前 800% 的缩放无关）
    let grid = TileGrid::new(Size::new(595.28, 841.89), render.0);
    let borrowed: Vec<(TileSpec, &RasterImage)> =
        tiles.iter().map(|(tile, image)| (*tile, image.as_ref())).collect();
    let image = app::view::compose_region_snapshot(
        target,
        Size::new(595.28, 841.89),
        Rotation::None,
        render.0,
        &grid,
        &borrowed,
    )
    .expect("应能拼出预览图");
    // 尺寸只由「区域 × 记录分辨率」决定（与当前 800% 无关）。
    // 允许 ±1：实现按"两端各取整一次"计算（与网格共享边界同一套规则）。
    let expected_w = (target.width * render.0).round() as usize;
    let expected_h = (target.height * render.0).round() as usize;
    assert!(
        image.size[0].abs_diff(expected_w) <= 1 && image.size[1].abs_diff(expected_h) <= 1,
        "输出 {}x{} 应约等于区域 × 渲染比例 {}x{}",
        image.size[0],
        image.size[1],
        expected_w,
        expected_h
    );
}
