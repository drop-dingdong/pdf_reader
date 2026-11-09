use super::*;

fn placement() -> PagePlacement {
    PagePlacement::new(
        PageIndex::new(0),
        Rect::new(12.0, 12.0, 612.0, 792.0),
        Scale::IDENTITY,
        Rotation::None,
        Size::new(612.0, 792.0),
    )
}

fn key(page: u32) -> RenderKey {
    RenderKey::new(PageIndex::new(page), Scale::IDENTITY, Rotation::None)
}

#[test]
fn view_rect_subtracts_scroll() {
    let rect = view_rect_for(&placement(), Vec2::new(100.0, 250.0));
    assert_eq!(rect, Rect::new(-88.0, -238.0, 612.0, 792.0));
}

/// 回归测试：块的位置是 `页.x + 块像素·u`，**不是**"除以显示比例"（D-17/D-94）。
///
/// 数字取自第三轮验收的真实诊断行（748% 下的第 2 页）：
/// `视图矩形=(-1679,-2042 3870×4785)`，网格里的第 (1,1) 块。
/// * `u == 1`（渲染比例 = 显示比例）时退化成"像素偏移直接相加"；
/// * `u < 1`（波段超采样）时块按 `u` 缩小 —— 但绝不会被压到 1/显示比例 那么小。
#[test]
fn tile_view_rect_uses_the_display_to_render_ratio() {
    let page_size = Size::new(612.0, 792.0);
    let display = 7.48f32;
    let page_rect = Rect::new(-1679.0, -2042.0, 3870.0, 4785.0);
    let placement = PagePlacement::new(
        PageIndex::new(1),
        page_rect,
        Scale::from_zoom(display),
        Rotation::None,
        page_size,
    );

    // u == 1：与"像素偏移直接相加"完全一致（D-17 的旧行为）
    let grid = TileGrid::new(page_size, display);
    let (x, y, w, _) = grid.tile_px(1, 1);
    let rect = tile_view_rect(&grid, &placement, 1, 1, display, Vec2::ZERO);
    assert_eq!(
        (rect.x, rect.y, rect.width),
        (page_rect.x + x as f32, page_rect.y + y as f32, w as f32)
    );
    assert_eq!(rect.x, -1679.0 + x as f32, "块位置 = 页原点 + 块像素偏移");

    // u < 1（渲染比例更高）：块按 u 缩小，且仍在页面范围内
    let render = display * 1.6;
    let grid = TileGrid::new(page_size, render);
    let rect = tile_view_rect(&grid, &placement, 1, 1, render, Vec2::ZERO);
    let u = display / render;
    let (x, _, w, _) = grid.tile_px(1, 1);
    assert!((rect.x - (page_rect.x + x as f32 * u)).abs() < 1e-2);
    assert!((rect.width - w as f32 * u).abs() < 1e-2);
    assert!(rect.right() <= page_rect.right() + 1.0, "块不得超出页面");

    // 反面：除以**显示比例**的实现会把块压到页面左上角（第三轮全黑）
    let wrong = Rect::new(
        page_rect.x + x as f32 / display,
        page_rect.y + y as f32 / display,
        0.0,
        0.0,
    );
    assert!(
        rect.x > wrong.x + 100.0,
        "块位置一旦除以显示比例就会被压到左上角"
    );
}

#[test]
fn view_and_content_coordinates_round_trip() {
    let scroll = Vec2::new(33.0, 77.0);
    let content = Point::new(120.0, 340.0);
    let view = Point::new(content.x - scroll.x, content.y - scroll.y);
    assert_eq!(view_to_content(view, scroll), content);
}

#[test]
fn key_mapping_covers_navigation_and_zoom() {
    let none = Modifiers::NONE;
    assert_eq!(
        command_for_key(Key::PageDown, none, 10),
        Some(ViewerCommand::NextPage)
    );
    assert_eq!(
        command_for_key(Key::End, none, 10),
        Some(ViewerCommand::GoToPage(PageIndex::new(9)))
    );
    assert_eq!(command_for_key(Key::End, none, 0), None, "空文档不应越界");
    assert_eq!(
        command_for_key(Key::Num0, none, 3),
        Some(ViewerCommand::SetZoom(1.0))
    );
    assert_eq!(
        command_for_key(Key::ArrowDown, none, 3),
        Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP)))
    );
    assert_eq!(command_for_key(Key::F, none, 3), None, "单独按 F 不映射");
    assert_eq!(
        command_for_key(Key::F, Modifiers::COMMAND, 3),
        Some(ViewerCommand::SetSidebar(SidebarTab::Search))
    );
    assert_eq!(
        command_for_key(Key::C, Modifiers::COMMAND, 3),
        Some(ViewerCommand::CopySelection)
    );
    assert_eq!(command_for_key(Key::Q, none, 3), None);
}

#[test]
fn scroll_mapping_flips_sign_and_supports_zoom_modifier() {
    // 滚轮向上（egui 中 y 为正）→ 滚动偏移减小
    assert_eq!(
        command_for_scroll(EguiVec2::new(0.0, 40.0), false, 1.0),
        Some(ViewerCommand::ScrollBy(Vec2::new(0.0, -40.0)))
    );
    // 横向滚动同理
    assert_eq!(
        command_for_scroll(EguiVec2::new(15.0, 0.0), false, 1.0),
        Some(ViewerCommand::ScrollBy(Vec2::new(-15.0, 0.0)))
    );
    // Ctrl + 滚轮 → 以当前缩放为基准放大
    match command_for_scroll(EguiVec2::new(0.0, 10.0), true, 2.0) {
        Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 2.2).abs() < 1e-4),
        other => panic!("期望 SetZoom，实际 {other:?}"),
    }
    // 无输入 → 无命令
    assert_eq!(command_for_scroll(EguiVec2::ZERO, false, 1.0), None);
    assert_eq!(command_for_scroll(EguiVec2::new(0.0, 0.1), true, 1.0), None);
}

/// Ctrl+滚轮 / 触控板捏合的缩放增量必须被翻译成文档缩放（v1 遗漏了这条路径）。
#[test]
fn zoom_delta_maps_ctrl_wheel_and_pinch() {
    assert_eq!(command_for_zoom_delta(1.0, 2.0), None, "无缩放时不产生命令");
    assert_eq!(
        command_for_zoom_delta(f32::NAN, 2.0),
        None,
        "异常值必须被忽略"
    );

    match command_for_zoom_delta(1.1, 2.0) {
        Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 2.2).abs() < 1e-4),
        other => panic!("期望 SetZoom，实际 {other:?}"),
    }
    match command_for_zoom_delta(0.5, 2.0) {
        Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 1.0).abs() < 1e-4),
        other => panic!("期望 SetZoom，实际 {other:?}"),
    }
}

#[test]
fn keyed_store_evicts_oldest_beyond_capacity() {
    let mut store = KeyedStore::new(2);
    store.insert(key(0), "a");
    store.insert(key(1), "b");
    assert_eq!(store.len(), 2);
    store.insert(key(2), "c");
    assert_eq!(store.len(), 2, "容量上限必须生效");
    assert!(!store.contains(&key(0)), "最旧的应被淘汰");
    assert_eq!(store.get(&key(2)), Some(&"c"));
}

/// D-109：字节账跟着插入/替换/LRU 淘汰/retain/clear 一起动，且**重算**与实际值一致。
/// 构造一条"页事实"（`whole_pages` 由调用方补）。
fn facts(page: PageIndex, rotation: Rotation, render_milli: u32, shape: PageShape) -> PageFacts {
    PageFacts {
        page,
        rotation,
        render_milli,
        shape,
        whole_pages: Vec::new(),
    }
}

#[test]
fn keyed_store_tracks_bytes_of_its_values() {
    // 用"值本身即字节数"的替身，便于手算
    let mut store = KeyedStore::with_bytes(3, |value: &u64| *value);
    store.insert(key(0), 100);
    store.insert(key(1), 200);
    assert_eq!(store.bytes(), 300);
    assert_eq!(store.average_bytes(), 150);

    // 同键替换：只加差值
    store.insert(key(0), 50);
    assert_eq!(store.bytes(), 250);

    // 超过容量（3）→ 淘汰最旧的（key(0) 刚被 touch，被淘汰的是 key(1)）
    store.insert(key(2), 400);
    store.insert(key(3), 1000);
    assert_eq!(store.len(), 3);
    assert_eq!(store.bytes(), 50 + 400 + 1000, "淘汰后账目要对上");

    // retain 摘除后重算
    store.retain(|key| key.page.get() % 2 == 0);
    assert_eq!(
        store.bytes(),
        store.iter().map(|(_, value)| *value).sum::<u64>()
    );

    // 清空归零
    store.clear();
    assert_eq!(store.bytes(), 0);
    assert!(store.is_empty());
}

/// D-111：预算裁剪 —— A 永不淘汰（软上限），C 先让位，B 在额度内保；类内 LRU（最旧先丢）。
#[test]
fn retain_respects_the_byte_budget_by_class() {
    let page = PageIndex::new(0);
    let tile = |scale: f32, col: u32| {
        RenderKey::tiled(
            page,
            Scale::from_zoom(scale),
            Rotation::None,
            TileSpec::new(col, 0),
        )
    };
    let whole = |scale: f32| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);

    // 每张 1 MB（值即字节数）
    const MB: u64 = 1024 * 1024;
    let mut store = KeyedStore::with_bytes(64, |value: &u64| *value);
    // A：当前波段 2 块；B：一张整页底料；C：旧波段的 2 块
    store.insert(tile(4.096, 0), MB);
    store.insert(tile(4.096, 1), MB);
    store.insert(whole(1.6), MB);
    store.insert(tile(6.5536, 0), MB);
    store.insert(tile(6.5536, 1), MB);

    let view = [PageFacts {
        whole_pages: vec![1_600],
        ..facts(page, Rotation::None, 4096, PageShape::Tiles(0, 0, 1, 0))
    }];

    // 预算充足：一张不丢
    store.set_max_bytes(64 * MB);
    let report = store.retain_for_view(&view, |_| MB);
    assert_eq!(store.len(), 5);
    assert_eq!(report.must_bytes, 2 * MB);
    assert_eq!(report.bed_bytes, MB);
    assert_eq!(report.old_bytes, 2 * MB);
    assert!(!report.over_budget);

    // 预算 3 MB：A(2) 之后只剩 1 MB → 只能保一张；B 优先于 C
    store.set_max_bytes(3 * MB);
    let report = store.retain_for_view(&view, |_| MB);
    assert_eq!(store.len(), 3, "2 块 A + 1 张底料");
    assert_eq!(report.bed_bytes, MB, "底料优先保住");
    assert_eq!(report.old_bytes, 0, "旧内容先让位");
    assert!(!report.over_budget);

    // 预算 1 MB：连 A 都放不下 → 软上限：A 全留，B/C 全让位，只报告
    store.set_max_bytes(MB);
    let report = store.retain_for_view(&view, |_| MB);
    assert_eq!(store.len(), 2, "A 必须完整保留");
    assert!(report.over_budget);
    assert_eq!(report.bed_bytes + report.old_bytes, 0);
}

/// D-112：`desired_textures` 是纯函数 —— 同样的输入永远同一个结果，
/// 且 A/B/C 三类的键可以逐个断言（不依赖纹理层现在有什么）。
#[test]
fn desired_is_pure_and_classified() {
    let page = PageIndex::new(0);
    let view = [PageFacts {
        page,
        rotation: Rotation::None,
        render_milli: 4096,
        shape: PageShape::Tiles(1, 0, 2, 1),
        whole_pages: vec![1600, 2560],
    }];
    // 纹理层现在只有一张 1600 整页 + 一张 6554 的旧块
    let existing = vec![
        RenderKey::new(page, Scale::from_zoom(1.6), Rotation::None),
        RenderKey::tiled(
            page,
            Scale::from_zoom(6.5536),
            Rotation::None,
            TileSpec::new(0, 0),
        ),
    ];
    let size = |key: &RenderKey| if key.tile.is_none() { 1_000 } else { 100 };

    let first = desired_textures(&view, &existing, 0, size);
    let second = desired_textures(&view, &existing, 0, size);
    assert_eq!(first, second, "纯函数：同样输入必须同样输出");

    // A：可见范围 (1,0)-(2,1) 共 2×2 = 4 块
    let must = |col, row| {
        RenderKey::tiled(
            page,
            Scale::from_zoom(4.096),
            Rotation::None,
            TileSpec::new(col, row),
        )
    };
    for col in 1..=2 {
        for row in 0..=1 {
            assert!(first.keep.contains(&must(col, row)), "A 缺 ({col},{row})");
        }
    }
    assert_eq!(first.must_bytes, 4 * 100);
    // B：两张整页底料（其中 1600 已存在 → 不上传；2560 不存在 → 要上传）
    let whole = |scale| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);
    assert!(first.keep.contains(&whole(1.6)));
    assert!(first.keep.contains(&whole(2.56)));
    assert_eq!(first.bed_bytes, 2_000);
    // upload = "缺的 A" + "缺的 B"：4 块瓦片（A 不在 GPU 里）+ 2560 整页（B 缺）
    assert_eq!(first.upload.len(), 5, "{:?}", first.upload);
    assert!(
        first.upload.iter().any(|key| *key == whole(2.56)),
        "缺的底料要上传"
    );
    assert!(
        first.upload.iter().any(|key| *key == must(1, 0)),
        "缺的可见块更要上传（否则画面永远糊着）"
    );
    // C：最近一个旧波段（6554）里的现有键
    let old = RenderKey::tiled(
        page,
        Scale::from_zoom(6.5536),
        Rotation::None,
        TileSpec::new(0, 0),
    );
    assert!(first.keep.contains(&old));
    assert_eq!(first.old_bytes, 100);

    // 同一个函数在"没有任何现有纹理"时也只改变 C 与 upload，不改 A/B
    let empty = desired_textures(&view, &[], 0, size);
    assert_eq!(empty.must_bytes, first.must_bytes);
    assert_eq!(empty.bed_bytes, first.bed_bytes);
    assert_eq!(empty.old_bytes, 0);
    // 4 块 A + 2 张底料都还不在 GPU 里
    assert_eq!(empty.upload.len(), 6, "{:?}", empty.upload);
}

/// 预算在 `desired` 内部一次算完：A 豁免、B 优先于 C。
#[test]
fn desired_trim_by_budget_prefers_must_then_bed() {
    let page = PageIndex::new(0);
    let view = [PageFacts {
        page,
        rotation: Rotation::None,
        render_milli: 4096,
        shape: PageShape::Tiles(0, 0, 1, 0),
        whole_pages: vec![1600],
    }];
    let existing = vec![
        RenderKey::new(page, Scale::from_zoom(1.6), Rotation::None),
        RenderKey::tiled(
            page,
            Scale::from_zoom(6.5536),
            Rotation::None,
            TileSpec::new(0, 0),
        ),
    ];
    let size = |key: &RenderKey| if key.tile.is_none() { 1_000 } else { 100 };

    // 1250 预算：A(200) + B(1000) 之后只剩 50 → C(100) 让位
    let desired = desired_textures(&view, &existing, 1_250, size);
    assert_eq!(desired.must_bytes, 200);
    assert_eq!(desired.bed_bytes, 1_000);
    assert_eq!(desired.old_bytes, 0, "C 先让位");
    assert!(!desired.over_budget);

    // 500 预算：连 B 都放不下 → 只留 A
    let desired = desired_textures(&view, &existing, 500, size);
    assert_eq!(desired.must_bytes, 200);
    assert_eq!(desired.bed_bytes, 0);
    assert!(!desired.over_budget, "A 放得下就不算超预算");

    // 100 预算：A 都放不下 → 软上限：A 全留 + 报告
    let desired = desired_textures(&view, &existing, 100, size);
    assert_eq!(desired.must_bytes, 200);
    assert!(desired.over_budget);
    assert_eq!(desired.bed_bytes + desired.old_bytes, 0);
}

/// D-116：带页内位置的内链是绿色，只有页码的内链与外部 URI 是红色。
#[test]
fn link_box_color_distinguishes_page_only_and_page_with_position() {
    let page = PageIndex::new(3);
    let with_position = Destination::Page {
        page,
        offset: Some(Point::new(10.0, 700.0)),
    };
    let page_only = Destination::Page { page, offset: None };
    assert_eq!(
        link_box_color(&with_position),
        egui::Color32::from_rgb(40, 170, 90),
        "带位置的链接应当是绿色"
    );
    assert_eq!(
        link_box_color(&page_only),
        egui::Color32::from_rgb(220, 40, 40),
        "只有页码的链接应当仍是红色"
    );
    assert_eq!(
        link_box_color(&Destination::Uri("https://example.com".to_string())),
        egui::Color32::from_rgb(220, 40, 40),
        "外部链接保持红色"
    );
}

#[test]
fn keyed_store_reinsert_moves_to_newest() {
    let mut store = KeyedStore::new(2);
    store.insert(key(0), "a");
    store.insert(key(1), "b");
    // 重新插入 0 → 它变成"最新"，被淘汰的应该是 1
    store.insert(key(0), "a2");
    store.insert(key(2), "c");
    assert!(store.contains(&key(0)));
    assert!(!store.contains(&key(1)));
    assert_eq!(store.get(&key(0)), Some(&"a2"));
}

#[test]
fn keyed_store_retain_drops_stale_entries() {
    let mut store = KeyedStore::new(16);
    for page in 0..5 {
        store.insert(key(page), page);
    }
    store.retain(|key| key.page.get() < 2);
    assert_eq!(store.len(), 2);
    assert!(store.contains(&key(0)) && store.contains(&key(1)));

    // retain 之后仍能正常插入（内部顺序表未损坏）
    store.insert(key(9), 9);
    assert_eq!(store.get(&key(9)), Some(&9));
    assert_eq!(store.len(), 3);

    store.clear();
    assert!(store.is_empty());
}

/// D-101：按页面空间矩形 + 指定渲染比例拼图 —— 与当前缩放无关、1:1 拷贝。
#[test]
fn compose_region_snapshot_copies_tiles_one_to_one() {
    let make = |width: u32, height: u32, fill: u8| {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.extend_from_slice(&[fill, fill, fill, 255]);
        }
        RasterImage::new(
            width,
            height,
            PixelFormat::Bgra8,
            std::sync::Arc::from(data),
        )
        .expect("位图")
    };
    // 612×792 @2 倍 → 1224×1584，2×2 网格，每块 612×792
    let page = Size::new(612.0, 792.0);
    let grid = TileGrid::new(page, 2.0);
    assert_eq!(grid.side, 2);
    let a = make(612, 792, 10);
    let b = make(612, 792, 20);
    let tiles = vec![(TileSpec::new(0, 1), &a), (TileSpec::new(1, 1), &b)];
    // 页面下半部分 → 1224×792 的图，左半 10、右半 20
    let rect = Rect::new(0.0, 0.0, 612.0, 396.0);
    let image =
        compose_region_snapshot(rect, page, Rotation::None, 2.0, &grid, &tiles).expect("应有内容");
    assert_eq!(image.size, [1224, 792]);
    assert_eq!(image.pixels[0].r(), 10);
    assert_eq!(image.pixels[611].r(), 10);
    assert_eq!(image.pixels[612].r(), 20);
    assert_eq!(image.pixels[1224 * 791 + 1223].r(), 20);

    // 区域跨越上下两块 → 上半没有块 → 白（而不是 None）
    let rect = Rect::new(0.0, 100.0, 612.0, 700.0);
    let image = compose_region_snapshot(rect, page, Rotation::None, 2.0, &grid, &tiles)
        .expect("部分覆盖也应有图");
    assert_eq!(image.size, [1224, 1400]);
    assert_eq!(image.pixels[0].r(), 255, "未覆盖的页面上部为白");

    // 一个像素都覆盖不到 → None（调用方据此"继续等待渲染"）
    let rect = Rect::new(0.0, 700.0, 612.0, 92.0); // 页面上部
    assert!(compose_region_snapshot(rect, page, Rotation::None, 2.0, &grid, &tiles).is_none());

    // 90° 旋转：块在页面空间是"横"的，映射到设备像素后宽高互换。
    // 旋转后页 792×612pt @2 → 1584×1224 px，网格 2×2，每块 792×612 px。
    let grid = TileGrid::new(page.rotated(Rotation::Clockwise90), 2.0);
    assert_eq!(grid.side, 2);
    let rotated = make(792, 612, 30);
    let rect = grid.tile_page_rect(0, 0, page, Rotation::Clockwise90);
    let image = compose_region_snapshot(
        rect,
        page,
        Rotation::Clockwise90,
        2.0,
        &grid,
        &[(TileSpec::new(0, 0), &rotated)],
    )
    .expect("旋转后应有内容");
    assert_eq!(image.size, [792, 612], "一块在设备像素里的尺寸");
    assert_eq!(image.pixels[0].r(), 30);
}

/// D-108：记录分辨率让预览超过纹理单边上限时，按 1.6 步长降档。
#[test]
fn preview_scale_drops_when_the_texture_side_limit_would_be_exceeded() {
    let page = Size::new(595.28, 841.89);
    let rect = Rect::new(0.0, 0.0, 595.28, 841.89);
    // 整页 + 800% 波段（10.486）→ 6242×8828，边长远小于 16384：不动
    assert_eq!(
        preview_render_scale(rect, page, Rotation::None, 10.486, 16_384),
        10.486
    );
    // 上限 4096：需要降档，降完必须放得下
    let clamped = preview_render_scale(rect, page, Rotation::None, 10.486, 4096);
    assert!(clamped < 10.486, "应降档：{clamped}");
    let (width, height) = region_pixel_size(rect, page, Rotation::None, clamped).expect("区域有效");
    assert!(width.max(height) <= 4096, "降档后 {width}×{height} 仍超限");
    // 0 = 不限制
    assert_eq!(
        preview_render_scale(rect, page, Rotation::None, 10.486, 0),
        10.486
    );
}

// ------------------------------------------------ D-96：整页级就绪门槛

fn draw_input() -> DrawInput {
    DrawInput {
        tiled: true,
        has_whole_current: false,
        ready: 0,
        total: 18,
        preview: Preview::Tiles(65_540),
        waited_ms: 0,
        showing_tiles: false,
    }
}

/// **关键回归**：只要还缺一块，就不许翻页 —— 否则就是"几块清晰 + 大片白"。
#[test]
fn plan_page_waits_for_every_visible_tile() {
    let input = DrawInput {
        ready: 17,
        waited_ms: 100,
        ..draw_input()
    };
    assert_eq!(
        plan_page(&input),
        PageDraw::TilePreview(65_540),
        "缺一块仍用旧内容顶住"
    );
    let input = DrawInput {
        ready: 2,
        waited_ms: 0,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));

    let input = DrawInput {
        ready: 18,
        waited_ms: 0,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Tiles, "全齐才切到当前波段");
}

/// D-104：同一波段内**已经切到瓦片**后，滚出新块也不退回预览。
///
/// 退回预览 = 整页变糊（实测真机 40~100ms）；正确做法是保留已清晰的块，
/// 缺的那块由底料补（D-98）。
#[test]
fn tiles_are_sticky_inside_a_band() {
    let input = DrawInput {
        ready: 4,
        total: 6,
        showing_tiles: true,
        ..draw_input()
    };
    assert_eq!(
        plan_page(&input),
        PageDraw::Tiles,
        "已进入瓦片模式：留在瓦片 + 底料"
    );

    // 一块就绪都没有也一样粘住（底料在，页面不会露白）
    let input = DrawInput {
        ready: 0,
        total: 6,
        showing_tiles: true,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Tiles);
}

/// 粘性只在**同一波段**内成立：新波段第一帧仍要等可见范围全齐（D-97 的门槛不许被绕过）。
#[test]
fn a_new_band_still_waits_for_the_whole_range() {
    let input = DrawInput {
        ready: 4,
        total: 6,
        showing_tiles: false,
        ..draw_input()
    };
    assert_eq!(
        plan_page(&input),
        PageDraw::TilePreview(65_540),
        "新波段（上一帧属于旧波段）仍先显示旧内容"
    );
}

/// 粘性不能盖过"不分块"和"当前波段整页在"这两条更早的判据。
#[test]
fn stickiness_does_not_override_earlier_rules() {
    let tiled_input = DrawInput {
        tiled: false,
        showing_tiles: true,
        ..draw_input()
    };
    assert_eq!(
        plan_page(&tiled_input),
        PageDraw::TilePreview(65_540),
        "不分块时按旧内容拉伸，与粘性无关"
    );
    let whole_input = DrawInput {
        has_whole_current: true,
        showing_tiles: true,
        ..draw_input()
    };
    assert_eq!(plan_page(&whole_input), PageDraw::Whole);
}

/// 超时兜底：某块渲染失败/块特别多时，不许永远停在糊图。
#[test]
fn plan_page_falls_back_to_partial_tiles_after_the_timeout() {
    let input = DrawInput {
        ready: 5,
        waited_ms: READY_TIMEOUT_MS - 1,
        ..draw_input()
    };
    assert_eq!(
        plan_page(&input),
        PageDraw::TilePreview(65_540),
        "超时前继续顶住"
    );
    let input = DrawInput {
        ready: 5,
        waited_ms: READY_TIMEOUT_MS,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Tiles, "超时后画已就绪部分");
    // 一块都没有时超时也没有意义（没有内容可画）
    let input = DrawInput {
        ready: 0,
        waited_ms: 9_999,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));
}

/// 旧内容优先级：整页图 > 旧块；都没有时才允许补白。
#[test]
fn plan_page_prefers_whole_page_preview_then_tiles_then_blank() {
    let input = DrawInput {
        ready: 3,
        preview: Preview::Whole(25_600),
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Stretch(25_600));
    let input = DrawInput {
        ready: 3,
        preview: Preview::Tiles(65_540),
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));
    let input = DrawInput {
        ready: 3,
        preview: Preview::None,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Tiles, "没有旧内容时只能补白");
}

/// 不分块（整页路径）不受就绪门槛影响：有整页图看整页，只有旧内容就顶住。
#[test]
fn plan_page_keeps_the_whole_page_path_unchanged() {
    let input = DrawInput {
        tiled: false,
        has_whole_current: true,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Whole);
    let input = DrawInput {
        tiled: false,
        preview: Preview::Whole(16_000),
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Stretch(16_000));
    let input = DrawInput {
        tiled: false,
        preview: Preview::Tiles(16_000),
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::TilePreview(16_000));
    let input = DrawInput {
        tiled: false,
        preview: Preview::None,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Blank);
}

/// 退化输入（可见范围为空的极端情形）不得 panic，也不得永远停在预览。
#[test]
fn plan_page_handles_an_empty_range() {
    let input = DrawInput {
        ready: 0,
        total: 0,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::TilePreview(65_540));
    let input = DrawInput {
        ready: 0,
        total: 0,
        preview: Preview::None,
        ..draw_input()
    };
    assert_eq!(plan_page(&input), PageDraw::Tiles);
}

/// `ready/total` 必须与绘制用同一段枚举（`grid.contains` 过滤），否则会"永远差一块"。
#[test]
fn ready_tiles_in_view_matches_the_draw_range() {
    let page = PageIndex::new(0);
    let grid = TileGrid::new(Size::new(612.0, 792.0), 8.0);
    assert_eq!(grid.side, 8);
    let render = Scale::from_zoom(8.0);
    let range = (0, 0, 1, 1);
    let mut store = KeyedStore::new(64);

    assert_eq!(
        store.ready_tiles_in_view(page, Rotation::None, render, &grid, range),
        (0, 4),
        "空存储：四块就绪"
    );
    for (col, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        store.insert(
            RenderKey::tiled(page, render, Rotation::None, TileSpec::new(col, row)),
            (col, row),
        );
    }
    assert_eq!(
        store.ready_tiles_in_view(page, Rotation::None, render, &grid, range),
        (4, 4)
    );

    // 越界块不计入总数（否则永远 ready < total）
    let out_of_grid = (0, 0, grid.side, grid.side);
    let (ready, total) =
        store.ready_tiles_in_view(page, Rotation::None, render, &grid, out_of_grid);
    assert!(total > 4 && ready >= 4);
    assert_eq!(
        total,
        grid.all_tiles().len(),
        "总数必须与网格的有效块数一致（越界的行列被过滤）"
    );

    // 别的波段 / 别的旋转不算
    let other = Scale::from_zoom(4.0);
    assert_eq!(
        store.ready_tiles_in_view(page, Rotation::None, other, &grid, range),
        (0, 4)
    );
    assert_eq!(
        store.ready_tiles_in_view(page, Rotation::Clockwise90, render, &grid, range),
        (0, 4)
    );
}

/// 预览源取舍：缩小方向优先整页图（覆盖），放大方向优先更接近的（清晰）。
#[test]
fn preview_source_prefers_coverage_when_zooming_out() {
    let page = PageIndex::new(0);
    let whole = |scale: f32| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);
    let tile = |scale: f32, col: u32| {
        RenderKey::tiled(
            page,
            Scale::from_zoom(scale),
            Rotation::None,
            TileSpec::new(col, 0),
        )
    };
    // 只有整页图 / 只有块 / 都没有
    let mut store = KeyedStore::new(16);
    store.insert(whole(1.6), "whole");
    assert_eq!(
        store.preview_source(page, Rotation::None, 10_486),
        Preview::Whole(1_600)
    );
    let mut store = KeyedStore::new(16);
    store.insert(tile(6.5536, 0), "tile");
    assert_eq!(
        store.preview_source(page, Rotation::None, 10_486),
        Preview::Tiles(6_554)
    );
    let store: KeyedStore<&str> = KeyedStore::new(16);
    assert_eq!(
        store.preview_source(page, Rotation::None, 10_486),
        Preview::None
    );

    // 缩小方向：整页图来自更低波段、块来自更高波段 → 用整页图（覆盖整个视口）
    let mut store = KeyedStore::new(16);
    store.insert(whole(2.56), "whole");
    store.insert(tile(10.48576, 0), "tile");
    assert_eq!(
        store.preview_source(page, Rotation::None, 6_554),
        Preview::Whole(2_560),
        "缩小时旧块只覆盖缩之前那一片，必须用整页图顶住"
    );

    // 放大方向：块来自更低波段 → 取更接近的（更清晰），且它覆盖是新视口的超集
    let mut store = KeyedStore::new(16);
    store.insert(whole(1.6), "whole");
    store.insert(tile(6.5536, 0), "tile");
    assert_eq!(
        store.preview_source(page, Rotation::None, 10_486),
        Preview::Tiles(6_554),
        "放大时优先更接近目标的预览"
    );

    // 距离相等 → 整页图（覆盖更全）
    let mut store = KeyedStore::new(16);
    store.insert(whole(2.0), "whole");
    store.insert(tile(4.0, 0), "tile");
    assert_eq!(
        store.preview_source(page, Rotation::None, 3_000),
        Preview::Whole(2_000)
    );
}

/// **回归（D-99）**：预览层也必须叠在底料之上 —— 连续放大时旧波段可能只有一两块，
/// 光靠"旧瓦片预览"会露白。
#[test]
fn preview_layers_always_need_an_underlay() {
    assert!(
        needs_underlay(PageDraw::TilePreview(6_554), 0, 0),
        "旧块预览可能只覆盖一小块，必须有底料"
    );
    assert!(needs_underlay(PageDraw::TilePreview(6_554), 18, 18));
    assert!(needs_underlay(PageDraw::Stretch(2_560), 0, 0));
    assert!(needs_underlay(PageDraw::Tiles, 7, 8), "缺块时需要底料");
    assert!(
        !needs_underlay(PageDraw::Tiles, 8, 8),
        "块全齐时块已把底料盖满，不必再画"
    );
    assert!(
        !needs_underlay(PageDraw::Whole, 0, 0),
        "当前波段整页图无需底料"
    );
    assert!(!needs_underlay(PageDraw::Blank, 0, 0));
}

/// 打底要"覆盖优先"：即使旧块更接近目标，也优先用整页图（能盖满整页）。
#[test]
fn underlay_source_prefers_coverage_over_sharpness() {
    let page = PageIndex::new(0);
    let whole = RenderKey::new(page, Scale::from_zoom(2.56), Rotation::None);
    let tile = RenderKey::tiled(
        page,
        Scale::from_zoom(6.5536),
        Rotation::None,
        TileSpec::new(5, 7),
    );

    let mut store = KeyedStore::new(16);
    store.insert(whole, "whole");
    store.insert(tile, "tile");
    // 目标 800%：旧块 6554 比整页图 2560 更接近，但打底必须用整页图
    assert_eq!(
        store.underlay_source(page, Rotation::None, 10_486),
        Preview::Whole(2_560)
    );
    // 同时也说明"整屏预览"仍可选更清晰的那个（两条规则各司其职）
    assert_eq!(
        store.preview_source(page, Rotation::None, 10_486),
        Preview::Tiles(6_554)
    );

    // 没有整页图时退回旧块；都没有则 None
    let mut store = KeyedStore::new(16);
    store.insert(tile, "tile");
    assert_eq!(
        store.underlay_source(page, Rotation::None, 10_486),
        Preview::Tiles(6_554)
    );
    let store: KeyedStore<&str> = KeyedStore::new(16);
    assert_eq!(
        store.underlay_source(page, Rotation::None, 10_486),
        Preview::None
    );
}

/// 保留规则：当前 + 最近的其它波段 + 当前以下最近的"整页图"所在波段。
#[test]
fn retain_keeps_whole_pages_declared_by_the_facts() {
    let page = PageIndex::new(0);
    let whole = |scale: f32| RenderKey::new(page, Scale::from_zoom(scale), Rotation::None);
    let tile = |scale: f32, col: u32| {
        RenderKey::tiled(
            page,
            Scale::from_zoom(scale),
            Rotation::None,
            TileSpec::new(col, 0),
        )
    };

    let mut store = KeyedStore::new(64);
    // 100% / 200% 是整页图，300% 起分块
    store.insert(whole(1.6), "w1600");
    store.insert(whole(2.56), "w2560");
    store.insert(tile(4.096, 0), "t4096");
    store.insert(tile(6.5536, 0), "t6554");
    store.insert(tile(10.48576, 0), "t10486");

    // B 类现在是**声明式**的：由 facts.whole_pages 指定（app 用波段代数算出），
    // 不再"扫描现有键猜哪些是底料"（D-112）
    let view = [PageFacts {
        whole_pages: vec![1_600, 2_560],
        ..facts(page, Rotation::None, 10_486, PageShape::Tiles(0, 0, 1, 1))
    }];
    store.retain_for_view(&view, |_| 0);

    assert!(store.contains(&tile(10.48576, 0)), "A 类：当前波段的可见块");
    assert!(store.contains(&tile(6.5536, 0)), "C 类：最近一个旧波段");
    assert!(store.contains(&whole(2.56)), "B 类：声明的整页底料");
    assert!(store.contains(&whole(1.6)), "B 类：更小的整页底料也留");
    assert!(!store.contains(&tile(4.096, 0)), "更旧的块波段应丢弃");
}

/// 没有整页图时退回"当前 + 最近"（与旧行为一致）。
#[test]
fn retain_without_whole_pages_only_keeps_current_and_nearest() {
    let page = PageIndex::new(0);
    let tile = |scale: f32, col: u32| {
        RenderKey::tiled(
            page,
            Scale::from_zoom(scale),
            Rotation::None,
            TileSpec::new(col, 0),
        )
    };
    let mut store = KeyedStore::new(64);
    store.insert(tile(4.096, 0), "a");
    store.insert(tile(6.5536, 0), "b");
    store.insert(tile(10.48576, 0), "c");
    store.retain_for_view(
        &[facts(
            page,
            Rotation::None,
            10_486,
            PageShape::Tiles(0, 0, 1, 1),
        )],
        |_| 0,
    );
    assert!(store.contains(&tile(10.48576, 0)));
    assert!(store.contains(&tile(6.5536, 0)));
    assert!(!store.contains(&tile(4.096, 0)));
}

/// 缩放期间用"上一档缩放"的位图拉伸占位（第二轮验收：避免大片白色填充）。
#[test]
fn best_page_texture_picks_the_nearest_scale() {
    let scaled = |page: u32, scale: f32, rotation: Rotation| {
        RenderKey::new(PageIndex::new(page), Scale::from_zoom(scale), rotation)
    };

    let mut store = KeyedStore::new(16);
    store.insert(scaled(0, 1.0, Rotation::None), "one");
    store.insert(scaled(0, 2.0, Rotation::None), "two");
    store.insert(scaled(1, 2.0, Rotation::None), "other-page");

    // 目标 2.1 → 最接近的整页图是 2.0
    assert_eq!(
        store.best_page_texture(PageIndex::new(0), Rotation::None, 2100),
        Some(&"two")
    );
    // 目标 1.1 → 1.0
    assert_eq!(
        store.best_page_texture(PageIndex::new(0), Rotation::None, 1100),
        Some(&"one")
    );
    // 其它页不参与
    assert_eq!(
        store.best_page_texture(PageIndex::new(2), Rotation::None, 2000),
        None
    );
    // 旋转不同不可顶替（否则画面会横躺）
    assert_eq!(
        store.best_page_texture(PageIndex::new(0), Rotation::Clockwise90, 2000),
        None
    );
}

/// 分块渲染时，判断"能不能立刻画真内容"：同页同缩放的瓦片只要有一块就绪就算能。
#[test]
fn has_tiles_detects_ready_tiles_for_the_same_key() {
    let mut store = KeyedStore::new(8);
    let whole = RenderKey::new(PageIndex::new(0), Scale::from_zoom(2.0), Rotation::None);
    assert!(!store.has_tiles(&whole), "空存储不应报告有瓦片");

    store.insert(
        RenderKey::tiled(
            PageIndex::new(0),
            Scale::from_zoom(2.0),
            Rotation::None,
            TileSpec::new(0, 0),
        ),
        "tile",
    );
    assert!(store.has_tiles(&whole), "同页同缩放的瓦片应被识别");
    assert!(
        !store.has_tiles(&RenderKey::new(
            PageIndex::new(1),
            Scale::from_zoom(2.0),
            Rotation::None
        )),
        "其它页的瓦片不算"
    );
    assert!(
        !store.has_tiles(&RenderKey::new(
            PageIndex::new(0),
            Scale::from_zoom(1.5),
            Rotation::None
        )),
        "其它缩放的瓦片不算"
    );
}

/// 高倍缩放的过场：旧瓦片要按"新页像素 / 旧页像素"等比放大回整页。
#[test]
fn tile_view_rect_scaled_maps_old_tiles_onto_the_new_page() {
    let page_rect = Rect::new(-100.0, -200.0, 2000.0, 4000.0);
    // 旧比例下页位图 1000×2000，瓦片 (0,0) 占 500×500 像素
    let old_page = Size::new(1000.0, 2000.0);
    let new_page = Size::new(2000.0, 4000.0);
    let rect = tile_view_rect_scaled(
        page_rect,
        Rect::new(500.0, 500.0, 500.0, 500.0),
        old_page,
        new_page,
    );
    // 放大 2 倍：偏移与原尺寸都翻倍
    assert_eq!(rect, Rect::new(900.0, 800.0, 1000.0, 1000.0));

    // 尺寸为 0 时不得产生 NaN/Inf
    let degenerate = tile_view_rect_scaled(
        page_rect,
        Rect::new(0.0, 0.0, 10.0, 10.0),
        Size::new(0.0, 0.0),
        new_page,
    );
    assert!(degenerate.x.is_finite() && degenerate.width.is_finite());
}

/// 预览用的旧瓦片查询：只认同页同旋转、且不等于当前缩放档的那些。
#[test]
fn best_tile_scale_and_tiles_at_pick_the_previous_scale() {
    let mut store = KeyedStore::new(16);
    let tile = |scale: f32, col: u32, row: u32| {
        RenderKey::tiled(
            PageIndex::new(0),
            Scale::from_zoom(scale),
            Rotation::None,
            TileSpec::new(col, row),
        )
    };
    store.insert(tile(1.0, 0, 0), "old-a");
    store.insert(tile(1.0, 1, 0), "old-b");
    store.insert(tile(2.0, 0, 0), "current");

    // 当前档 2.0 → 最近的"有瓦片的其它档"是 1.0
    assert_eq!(
        store.best_tile_scale(PageIndex::new(0), Rotation::None, 2000),
        Some(1000)
    );
    // 当前档 1.0 → 2.0
    assert_eq!(
        store.best_tile_scale(PageIndex::new(0), Rotation::None, 1000),
        Some(2000)
    );
    // 其它页 / 其它旋转都没有
    assert_eq!(
        store.best_tile_scale(PageIndex::new(1), Rotation::None, 2000),
        None
    );
    assert_eq!(
        store.best_tile_scale(PageIndex::new(0), Rotation::Clockwise90, 2000),
        None
    );

    let old = store.tiles_at(PageIndex::new(0), Rotation::None, 1000);
    assert_eq!(old.len(), 2, "旧档应有 2 块");
    assert!(old
        .iter()
        .any(|(tile, value)| *tile == TileSpec::new(0, 0) && **value == "old-a"));
    assert_eq!(
        store
            .tiles_at(PageIndex::new(0), Rotation::None, 2000)
            .len(),
        1
    );
}

/// 诊断助手：判断热区是否越界（越界即坐标空间不一致的证据）。
#[test]
fn link_rect_in_page_bounds_detects_coordinate_space_mismatch() {
    let page = Size::new(612.0, 792.0);
    assert!(link_rect_in_page_bounds(
        Rect::new(72.0, 700.0, 200.0, 20.0),
        page
    ));
    // 浮点余量：贴边不算越界
    assert!(link_rect_in_page_bounds(
        Rect::new(0.0, 0.0, 612.0, 792.0),
        page
    ));
    // 典型的"整体偏移"：MediaBox 原点不为 0 时，热区会跑到页外
    assert!(!link_rect_in_page_bounds(
        Rect::new(72.0, 800.0, 200.0, 20.0),
        page
    ));
    assert!(!link_rect_in_page_bounds(
        Rect::new(-40.0, 100.0, 20.0, 20.0),
        page
    ));
    assert!(!link_rect_in_page_bounds(
        Rect::new(600.0, 100.0, 100.0, 20.0),
        page
    ));
}

/// 链接跳转语义：有落点 → 先跳页再居中；无落点 → 只跳页（顶部对齐）。
#[test]
fn commands_for_destination_centers_when_an_offset_exists() {
    let with_offset = Destination::Page {
        page: PageIndex::new(3),
        offset: Some(Point::new(120.0, 700.0)),
    };
    assert_eq!(
        commands_for_destination(&with_offset),
        vec![
            ViewerCommand::GoToPage(PageIndex::new(3)),
            ViewerCommand::CenterOnPagePoint {
                page: PageIndex::new(3),
                point: Point::new(120.0, 700.0),
            },
        ]
    );

    let page_only = Destination::Page {
        page: PageIndex::new(1),
        offset: None,
    };
    assert_eq!(
        commands_for_destination(&page_only),
        vec![ViewerCommand::GoToPage(PageIndex::new(1))],
        "没有落点就只跳页，不做居中"
    );

    assert!(commands_for_destination(&Destination::Uri("https://x".into())).is_empty());
}

/// 链接框的显示条件：开关 + 缩放阈值。
#[test]
fn link_overlay_visibility_respects_the_switch_and_zoom_threshold() {
    assert!(!link_overlay_visible(false, 2.0, 0.75), "开关关闭时不画");
    assert!(!link_overlay_visible(true, 0.5, 0.75), "缩放低于阈值时不画");
    assert!(link_overlay_visible(true, 0.75, 0.75), "正好达到阈值就画");
    assert!(link_overlay_visible(true, 3.0, 0.75));
    assert!(
        !link_overlay_visible(true, f32::NAN, 0.75),
        "异常缩放值不画"
    );
}

/// 自定义图片 preview：跨页框选时只保留**重叠面积最大**的那一页及其交叠矩形。
#[test]
fn largest_overlap_page_picks_the_dominant_page() {
    let page_a = Rect::new(0.0, 0.0, 100.0, 100.0);
    let page_b = Rect::new(0.0, 110.0, 100.0, 100.0);
    let pages = [(0usize, page_a), (1usize, page_b)];

    // 选区内 A 占 60 高、B 占 20 高 → 取 A
    let selection = Rect::new(0.0, 40.0, 100.0, 90.0);
    let (index, overlap) = largest_overlap_page(pages.iter().copied(), selection).expect("应命中");
    assert_eq!(index, 0, "取重叠更大的 A 页");
    assert_eq!(overlap, Rect::new(0.0, 40.0, 100.0, 60.0), "只保留交叠部分");

    // 反过来：B 占得多 → 取 B
    let selection = Rect::new(0.0, 90.0, 100.0, 60.0);
    let (index, overlap) = largest_overlap_page(pages.iter().copied(), selection).expect("应命中");
    assert_eq!(index, 1);
    assert_eq!(overlap, Rect::new(0.0, 110.0, 100.0, 40.0));

    // 完全落在页间距 → 没有页
    assert!(
        largest_overlap_page(pages.iter().copied(), Rect::new(0.0, 104.0, 100.0, 4.0)).is_none()
    );

    // 面积并列时取先出现者（页面顺序），结果确定
    let tie = vec![
        (0usize, Rect::new(0.0, 0.0, 100.0, 50.0)),
        (1usize, Rect::new(0.0, 50.0, 100.0, 50.0)),
    ];
    let (index, _) = largest_overlap_page(tie, Rect::new(0.0, 0.0, 100.0, 100.0)).expect("应命中");
    assert_eq!(index, 0);
}

/// 跨页截图：所有与选区相交的页都要参与（含页间距留白，不能只留最大重叠那一页）。
#[test]
fn pages_intersecting_keeps_every_overlapping_page() {
    let page_a = Rect::new(0.0, 0.0, 100.0, 100.0);
    let page_b = Rect::new(0.0, 110.0, 100.0, 100.0); // 与 A 之间有 10pt 页间距
    let page_c = Rect::new(0.0, 400.0, 100.0, 100.0);
    let pages = [(0usize, page_a), (1usize, page_b), (2usize, page_c)];

    // 选区跨越 A 与 B（含间距）
    let selection = Rect::new(0.0, 80.0, 100.0, 60.0);
    let hit = pages_intersecting(pages.iter().copied(), selection);
    assert_eq!(hit.len(), 2, "A 与 B 都要在");
    assert_eq!(hit[0].0, 0);
    assert_eq!(hit[1].0, 1);

    // 选区只碰 C
    let hit = pages_intersecting(pages.iter().copied(), Rect::new(0.0, 420.0, 50.0, 20.0));
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].0, 2);

    // 只落在页间距里 → 没有任何页（结果是一张全白图）
    let hit = pages_intersecting(pages.iter().copied(), Rect::new(0.0, 104.0, 100.0, 4.0));
    assert!(hit.is_empty());

    // 空输入不 panic
    assert!(pages_intersecting(std::iter::empty(), selection).is_empty());
}

/// 正文尺寸 ↔ 浮窗尺寸必须互逆（否则每帧写回会让尺寸漂移）。
#[test]
fn body_size_and_float_size_are_inverse() {
    let title = 22.0;
    let body = egui::vec2(300.0, 170.0);
    let size = float_size_for_body(body, title);
    assert_eq!(size, egui::vec2(316.0, 208.0), "两侧 8px + 标题 22px");

    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), size);
    assert_eq!(body_size_of(rect, title), body, "往返必须回到原值");

    let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2.0, 2.0));
    let back = body_size_of(tiny, title);
    assert!(back.x >= 1.0 && back.y >= 1.0, "退化尺寸不给 0/负值");
}

/// 侧栏条目触发的浮窗：贴左缘、与该行居中、夹取在视图内。
#[test]
fn float_rect_at_row_hugs_the_sidebar_and_aligns_with_the_row() {
    let canvas = egui::Rect::from_min_size(egui::pos2(270.0, 40.0), egui::vec2(800.0, 600.0));
    let size = egui::vec2(320.0, 200.0);
    let min = egui::vec2(160.0, 90.0);

    // 中间某行 → 贴左缘（画布左 + 16），垂直与行居中
    let row = egui::Rect::from_min_size(egui::pos2(0.0, 300.0), egui::vec2(260.0, 18.0));
    let rect = float_rect_at_row(canvas, row, size, min);
    assert!(
        (rect.min.x - (canvas.min.x + 16.0)).abs() < 1e-3,
        "紧贴侧边栏"
    );
    assert!(
        (rect.center().y - row.center().y).abs() < 1e-3,
        "与条目行垂直居中"
    );

    // 顶部行 → 会被夹回视图内（不会超出上边）
    let top = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(260.0, 18.0));
    let rect = float_rect_at_row(canvas, top, size, min);
    assert!(rect.min.y >= canvas.min.y - 1e-3);

    // 画布未知 → 退回固定位置，不 panic
    let rect = float_rect_at_row(egui::Rect::NOTHING, row, size, min);
    assert_eq!(rect.min, egui::pos2(48.0, 48.0));
}

/// 顶栏要用的几个纯换算：缩放百分比、适应高度、页码校验、菜单行文本。
#[test]
fn toolbar_conversions() {
    // 缩放：显示为整数百分比，解析时取整（153.2 与 153 等价）
    assert_eq!(zoom_percent_text(1.532), "153%");
    assert_eq!(zoom_percent_text(1.0), "100%");
    assert_eq!(parse_zoom_percent("153.2"), Some(1.53));
    assert_eq!(parse_zoom_percent("153"), Some(1.53));
    assert_eq!(parse_zoom_percent(" 153% "), Some(1.53));
    assert_eq!(parse_zoom_percent("abc"), None);
    assert_eq!(parse_zoom_percent("0"), None, "0% 无意义");
    assert_eq!(parse_zoom_percent("-5"), None);

    // 适应高度：视口高 ÷ 页高；退化输入返回 None
    assert_eq!(fit_height_zoom(600.0, 300.0), Some(2.0));
    assert_eq!(fit_height_zoom(0.0, 300.0), None);
    assert_eq!(fit_height_zoom(600.0, 0.0), None);

    // 页码：1..=page_count 合法
    assert_eq!(page_index_from_text("3", 10), Some(PageIndex::new(2)));
    assert_eq!(page_index_from_text("1", 10), Some(PageIndex::FIRST));
    assert_eq!(page_index_from_text("0", 10), None);
    assert_eq!(page_index_from_text("11", 10), None);
    assert_eq!(page_index_from_text("x", 10), None);

    // 菜单行：勾选时行首为 ✓，否则为空格
    assert_eq!(checkbox_row("链接框", true), "✓ 链接框");
    assert_eq!(checkbox_row("链接框", false), "  链接框");
}

/// 尺寸比较要带容差：亚像素误差不能算"修改"。
#[test]
fn body_size_changed_ignores_subpixel_noise() {
    let stored = egui::vec2(320.0, 200.0);
    assert!(!body_size_changed(stored, egui::vec2(320.0, 200.0)));
    assert!(
        !body_size_changed(stored, egui::vec2(320.3, 199.8)),
        "亚像素误差不算修改"
    );
    assert!(
        body_size_changed(stored, egui::vec2(330.0, 200.0)),
        "明显变宽算修改"
    );
    assert!(
        body_size_changed(stored, egui::vec2(320.0, 190.0)),
        "明显变矮算修改"
    );
}

/// 时间行：相同只显示创建；不同显示两条；都没有就不显示。
#[test]
fn annotation_time_line_omits_duplicates_and_empty() {
    assert!(annotation_time_line(None, None).is_none());
    let same = annotation_time_line(Some(1_761_903_600), Some(1_761_903_600)).unwrap();
    assert!(
        same.starts_with("创建 ") && !same.contains("修改"),
        "{same}"
    );
    let different = annotation_time_line(Some(1_761_903_600), Some(1_761_907_200)).unwrap();
    assert!(
        different.contains("创建 ") && different.contains(" · 修改 "),
        "{different}"
    );
}

/// 「整理副本」在注释未写回时的提示。
#[test]
fn compact_unsaved_hint_only_when_annotations_are_dirty() {
    assert!(compact_unsaved_hint(false).is_none());
    let text = compact_unsaved_hint(true).expect("脏时应有提示");
    assert!(text.contains("不会被整理进副本"), "实际：{text}");
}

/// 保存按钮文案随"是否有未保存修改"变化。
#[test]
fn save_button_label_marks_unsaved_changes() {
    assert_eq!(save_button_label(false), "保存");
    assert_eq!(save_button_label(true), "*保存");
    assert!(save_button_label(true).starts_with('*'));
}

/// 侧栏 Pin 列表 → 正式浮窗的查找。
#[test]
fn find_float_for_custom_matches_only_the_official_float() {
    let customs = vec![None, Some(0), None, Some(1)];
    assert_eq!(find_float_for_custom(&customs, 0), Some(1));
    assert_eq!(find_float_for_custom(&customs, 1), Some(3));
    assert_eq!(find_float_for_custom(&customs, 2), None, "没有正式浮窗");
    assert_eq!(find_float_for_custom(&[], 0), None);
}

/// 浮窗文本排版：自动换行、行数上限、溢出省略号。
#[test]
fn overflow_layout_wraps_limits_rows_and_ellipsizes() {
    let font = egui::FontId::proportional(13.0);
    let color = egui::Color32::WHITE;

    // 正文：宽度受限（自动换行）+ 行数上限
    let job = overflow_layout("很长的单行文本", font.clone(), color, 200.0, 6);
    assert_eq!(job.wrap.max_width, 200.0, "宽度受限 → 自动换行");
    assert_eq!(job.wrap.max_rows, 6);
    assert_eq!(job.wrap.overflow_character, Some('…'), "超出要显示省略号");
    assert_eq!(job.text, "很长的单行文本");
    assert_eq!(job.sections.len(), 1);
    assert_eq!(job.sections[0].format.font_id, font);

    // 标题：只允许一行
    let job = overflow_layout("一个非常非常长的标题", font.clone(), color, 120.0, 1);
    assert_eq!(job.wrap.max_rows, 1, "标题只保留一行（超出截断）");

    // 负数宽度不应 panic，按 0 处理
    let job = overflow_layout("x", font, color, -10.0, 1);
    assert_eq!(job.wrap.max_width, 0.0);
}

/// 自定义 preview 的配套小工具：零尺寸矩形 + 退出 Pin 的唯一条件。
#[test]
fn zero_rect_and_pinning_command_whitelist() {
    let point = Point::new(120.0, 340.0);
    let rect = zero_rect_at(point);
    assert_eq!(rect, Rect::new(120.0, 340.0, 0.0, 0.0));
    assert_eq!(rect.width, 0.0, "大小为零（用户要求）");
    assert_eq!(rect.center(), point, "以该点为中心");

    // 只有切到"选字"或"截图"模式才退出 Pin
    assert!(pin_exits_on_mode_change(true, false), "切到选字 → 退出");
    assert!(pin_exits_on_mode_change(false, true), "切到截图 → 退出");
    assert!(pin_exits_on_mode_change(true, true));
    assert!(
        !pin_exits_on_mode_change(false, false),
        "缩放/翻页/侧栏/checkbox 等不改预览模式 → 保持 Pin"
    );
}

/// 拖动浮窗：平移后仍被夹在视图区域内（三个拖动入口共用同一规则）。
#[test]
fn drag_float_rect_translates_and_clamps() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let min = egui::vec2(160.0, 90.0);
    let rect = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0));

    // 正常平移
    let moved = drag_float_rect(rect, egui::vec2(20.0, -30.0), canvas, min);
    assert_eq!(moved.min, egui::pos2(120.0, 70.0));
    assert_eq!(moved.size(), rect.size(), "拖动只改位置，不改尺寸");

    // 拖出右下 → 拉回区域内
    let moved = drag_float_rect(rect, egui::vec2(2000.0, 2000.0), canvas, min);
    assert!(moved.max.x <= canvas.max.x + 1e-3 && moved.max.y <= canvas.max.y + 1e-3);

    // 拖出左上 → 贴住左上角
    let moved = drag_float_rect(rect, egui::vec2(-2000.0, -2000.0), canvas, min);
    assert_eq!(moved.min, canvas.min);
}

/// 置顶：移到末尾，且不移乱其它项的相对顺序。
#[test]
fn move_to_last_keeps_the_relative_order_of_the_rest() {
    let mut items = vec!["a", "b", "c", "d"];
    move_to_last(&mut items, 1); // 把 b 置顶
    assert_eq!(items, vec!["a", "c", "d", "b"], "其余顺序不变（a, c, d）");

    move_to_last(&mut items, 0); // 把 a 置顶
    assert_eq!(items, vec!["c", "d", "b", "a"]);

    // 越界不 panic、不改变内容
    let before = items.clone();
    move_to_last(&mut items, 99);
    assert_eq!(items, before);
}

/// 同一目的地的固定浮窗查找：只认同来源的浮窗，普通浮窗（None）不参与。
#[test]
fn find_float_for_target_matches_only_pinned_previews() {
    let page = PageIndex::new(2);
    let origins = vec![None, Some((page, 0)), Some((page, 1))];

    assert_eq!(find_float_for_target(&origins, (page, 1)), Some(2));
    assert_eq!(find_float_for_target(&origins, (page, 0)), Some(1));
    assert_eq!(
        find_float_for_target(&origins, (page, 9)),
        None,
        "没固定过就没有"
    );
    assert_eq!(
        find_float_for_target(&origins, (PageIndex::new(3), 0)),
        None,
        "页不同不算"
    );
    assert_eq!(find_float_for_target(&[], (page, 0)), None);
}

/// 悬停预览的两个约束：滚动期间不出现；浮窗之下不出现。
#[test]
fn hover_preview_is_blocked_while_scrolling_and_under_floats() {
    assert!(hover_preview_allowed(false, false, false), "正常情况应出现");
    assert!(
        !hover_preview_allowed(true, false, false),
        "滚动/缩放静默窗口内不出现"
    );
    assert!(
        !hover_preview_allowed(false, true, false),
        "指针在浮窗上不出现（浮窗屏蔽主视图）"
    );
    assert!(
        !hover_preview_allowed(false, false, true),
        "画墨迹期间不出现（笔画可能压住链接）"
    );
    assert!(!hover_preview_allowed(true, true, false));
    assert!(!hover_preview_allowed(false, true, true));

    let rects = [
        egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 50.0)),
        egui::Rect::from_min_size(egui::pos2(200.0, 200.0), egui::vec2(100.0, 50.0)),
    ];
    assert!(pointer_over_any(egui::pos2(50.0, 30.0), &rects));
    assert!(pointer_over_any(egui::pos2(250.0, 220.0), &rects));
    assert!(
        !pointer_over_any(egui::pos2(150.0, 150.0), &rects),
        "浮窗之间不算遮挡"
    );
    assert!(
        !pointer_over_any(egui::pos2(50.0, 30.0), &[]),
        "没有浮窗时不遮挡"
    );
}

/// 临时预览浮窗：左上角放在链接框中心（保证与链接框重叠，鼠标移动路径连续）。
#[test]
fn tip_rect_starts_at_the_link_center_so_it_overlaps() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
    let link = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(180.0, 24.0));
    let size = egui::vec2(360.0, 260.0);

    let tip = tip_rect_for(link, size, canvas);
    assert_eq!(tip.min, link.center(), "左上角 = 链接框中心");
    assert!(
        tip.intersects(link),
        "必须与链接框重叠（否则鼠标一移开预览就消失）"
    );
    assert!(link.contains(tip.min), "锚点落在链接框内");

    // 靠近右下边界时会夹取回区域内
    let edge_link = egui::Rect::from_min_size(egui::pos2(900.0, 760.0), egui::vec2(80.0, 24.0));
    let tip = tip_rect_for(edge_link, size, canvas);
    assert!(tip.max.x <= canvas.max.x + 1e-3 && tip.max.y <= canvas.max.y + 1e-3);
}

/// 建表与查询必须走同一个量化：链接 → 被引用点 的查找才不会落空。
#[test]
fn dest_lookup_uses_the_same_quantization_as_the_table() {
    let link = |page: u32, point: Point| Link {
        rect: Rect::new(0.0, 0.0, 10.0, 10.0),
        destination: Destination::Page {
            page: PageIndex::new(page),
            offset: Some(point),
        },
    };
    let links = vec![
        vec![
            link(1, Point::new(10.0, 20.0)),
            link(1, Point::new(300.0, 400.0)),
        ],
        Vec::new(),
    ];
    let points = destination_points(&links, 2);
    let first_page = &points[1];
    assert_eq!(first_page.len(), 2);

    // 用"略偏一点"的同一点去查，仍应命中（量化到同一格）
    assert_eq!(find_dest_index(first_page, Point::new(10.2, 20.2)), Some(0));
    assert_eq!(find_dest_index(first_page, Point::new(10.0, 20.0)), Some(0));
    assert_eq!(
        find_dest_index(first_page, Point::new(300.0, 400.0)),
        Some(1)
    );
    // 另一个点 → 未命中
    assert_eq!(find_dest_index(first_page, Point::new(50.0, 60.0)), None);
    // 量化函数本身：0.4 → 0.5，0.2 → 0.0
    assert_eq!(
        quantize_dest_point(Point::new(10.4, 20.2)),
        Point::new(10.5, 20.0)
    );
}

/// 被引用点的默认矩形：以该点为中心、尺寸固定；页面矩形→屏幕矩形要能来回对上。
#[test]
fn default_dest_rect_and_page_rect_to_screen() {
    let rect = default_dest_rect(Point::new(100.0, 200.0));
    assert_eq!(rect, Rect::new(40.0, 170.0, 120.0, 60.0));
    assert_eq!(rect.center(), Point::new(100.0, 200.0), "以该点为中心");

    // 页 200×100 放在内容空间 (10,20)，缩放 1、无旋转；滚动 (5,7)，画布原点 (300,40)
    let placement = PagePlacement::new(
        PageIndex::FIRST,
        Rect::new(10.0, 20.0, 200.0, 100.0),
        Scale::IDENTITY,
        Rotation::None,
        Size::new(200.0, 100.0),
    );
    let screen = page_rect_to_screen(
        &placement,
        Rect::new(30.0, 40.0, 50.0, 20.0),
        Vec2::new(5.0, 7.0),
        egui::pos2(300.0, 40.0),
    );
    // 内容空间 = 页内 + 页原点 = (40,60,50,20)；屏幕 = 内容 − 滚动 + 画布原点
    assert_eq!(
        screen,
        egui::Rect::from_min_size(egui::pos2(335.0, 93.0), egui::vec2(50.0, 20.0))
    );
}

/// 回归：屏幕坐标 → 内容空间必须减去画布原点；且框选区域截到的内容必然**落在当前可见范围内**。
#[test]
fn screen_rect_to_content_subtracts_the_canvas_origin() {
    // 画布左上角在屏幕 (265, 40)（左侧有 260px 侧栏 + 5px 拖拽条）
    let canvas_min = egui::pos2(265.0, 40.0);
    let selection = egui::Rect::from_min_size(egui::pos2(365.0, 140.0), egui::vec2(100.0, 50.0));
    let scroll = Vec2::new(1000.0, 2000.0);

    let content = screen_rect_to_content(canvas_min, selection, scroll);
    assert_eq!(content, Rect::new(1100.0, 2100.0, 100.0, 50.0));

    // 反例：漏掉 canvas.min 会得到这个（正是缺陷时的取值）
    let buggy = Rect::new(
        selection.min.x + scroll.x,
        selection.min.y + scroll.y,
        selection.width(),
        selection.height(),
    );
    assert_ne!(content, buggy, "漏减画布原点会整体偏移 canvas.min");

    // 关键性质：**框选区域截到的内容必然落在当前可见范围内**（用户实测的判据）
    let viewport = Size::new(800.0, 600.0);
    let visible = Rect::new(scroll.x, scroll.y, viewport.width, viewport.height);
    let inside = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(100.0, 50.0));
    let content = screen_rect_to_content(canvas_min, inside, scroll);
    assert!(
        visible.intersection(content).is_some(),
        "截图内容 {content:?} 必须落在可见范围 {visible:?} 内"
    );
}

/// 快照合成：白底、按页裁剪、支持整页与分块两种来源。
#[test]
fn compose_snapshot_blits_pages_tiles_and_leaves_white_outside() {
    let make = |width: u32, height: u32, fill: u8| {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.extend_from_slice(&[fill, fill, fill, 255]); // BGRA：灰
        }
        RasterImage::new(
            width,
            height,
            PixelFormat::Bgra8,
            std::sync::Arc::from(data),
        )
        .expect("位图")
    };

    // 整页来源：页 4×4 放在内容空间 (10,10)，整页填 100
    let whole = make(4, 4, 100);
    let pages = vec![SnapshotPage {
        rect: Rect::new(10.0, 10.0, 4.0, 4.0),
        whole: Some(&whole),
        tiles: Vec::new(),
    }];

    // 区域完全落在页内 → 全部为 100
    let snapshot = compose_snapshot(&pages, Rect::new(11.0, 11.0, 2.0, 2.0)).expect("应有内容");
    assert_eq!(snapshot.size, [2, 2]);
    assert!(snapshot.pixels.iter().all(|pixel| pixel.r() == 100));

    // 区域跨出页面 → 页外部分留白（255）
    let snapshot = compose_snapshot(&pages, Rect::new(12.0, 12.0, 4.0, 4.0)).expect("应有内容");
    assert_eq!(snapshot.size, [4, 4]);
    assert_eq!(snapshot.pixels[0].r(), 100, "页内像素应拷贝过来");
    assert_eq!(snapshot.pixels[3 * 4 + 3].r(), 255, "页外像素应为白");

    // 完全在页外 → 无可用像素
    assert!(compose_snapshot(&pages, Rect::new(100.0, 100.0, 4.0, 4.0)).is_none());

    // 分块来源：两块各占页宽的一半（块的位置由 TileGrid 给出，这里直接给内容空间矩形）
    let left = make(512, 512, 50);
    let right = make(512, 512, 200);
    let tiled = vec![SnapshotPage {
        rect: Rect::new(0.0, 0.0, 1024.0, 512.0),
        whole: None,
        tiles: vec![
            (Rect::new(0.0, 0.0, 512.0, 512.0), &left),
            (Rect::new(512.0, 0.0, 512.0, 512.0), &right),
        ],
    }];
    let snapshot = compose_snapshot(&tiled, Rect::new(0.0, 0.0, 1024.0, 512.0)).expect("应有内容");
    assert_eq!(snapshot.size, [1024, 512]);
    assert_eq!(snapshot.pixels[0].r(), 50, "左块覆盖 x<512");
    assert_eq!(snapshot.pixels[511].r(), 50);
    assert_eq!(snapshot.pixels[512].r(), 200, "右块覆盖 x≥512");
    assert_eq!(snapshot.pixels[1024 * 10].r(), 50, "第二行左侧仍是左块");
    assert_eq!(
        snapshot.pixels[1024 * 10 + 512].r(),
        200,
        "第二行右侧仍是右块"
    );

    // 多页 + 页间距：中间留白
    let top = make(2, 2, 10);
    let bottom = make(2, 2, 20);
    let stacked = vec![
        SnapshotPage {
            rect: Rect::new(0.0, 0.0, 2.0, 2.0),
            whole: Some(&top),
            tiles: Vec::new(),
        },
        SnapshotPage {
            rect: Rect::new(0.0, 3.0, 2.0, 2.0),
            whole: Some(&bottom),
            tiles: Vec::new(),
        },
    ];
    let snapshot = compose_snapshot(&stacked, Rect::new(0.0, 0.0, 2.0, 5.0)).expect("应有内容");
    assert_eq!(snapshot.size, [2, 5]);
    assert_eq!(snapshot.pixels[0].r(), 10, "第一页");
    assert_eq!(snapshot.pixels[2 * 2].r(), 255, "页间距应留白");
    assert_eq!(snapshot.pixels[3 * 2].r(), 20, "第二页");
}

/// 波段超采样（`u = 显示/渲染 < 1`）：位图比它铺满的内容矩形**大**。
///
/// 这是 D-94 之后必须成立的一条：快照按"铺满内容矩形"重采样，
/// 因此整页图与块都不会因为渲染比例更高而只截到左上角一小块。
#[test]
fn compose_snapshot_scales_supersampled_bitmaps_to_fit() {
    let make = |width: u32, height: u32, fill: u8| {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.extend_from_slice(&[fill, fill, fill, 255]);
        }
        RasterImage::new(
            width,
            height,
            PixelFormat::Bgra8,
            std::sync::Arc::from(data),
        )
        .expect("位图")
    };

    // 页内容 100×100，整页位图按 1.6 倍渲染成 160×160
    let whole = make(160, 160, 90);
    let pages = vec![SnapshotPage {
        rect: Rect::new(0.0, 0.0, 100.0, 100.0),
        whole: Some(&whole),
        tiles: Vec::new(),
    }];
    let snapshot = compose_snapshot(&pages, Rect::new(0.0, 0.0, 100.0, 100.0)).expect("应有内容");
    assert_eq!(snapshot.size, [100, 100]);
    assert!(
        snapshot.pixels.iter().all(|pixel| pixel.r() == 90),
        "整页必须铺满（右下角也不能留白）"
    );

    // 块：内容空间各占一半，位图同样按 1.6 倍更大 → 仍然各占一半
    let left = make(160, 160, 10);
    let right = make(160, 160, 20);
    let tiled = vec![SnapshotPage {
        rect: Rect::new(0.0, 0.0, 100.0, 100.0),
        whole: None,
        tiles: vec![
            (Rect::new(0.0, 0.0, 50.0, 100.0), &left),
            (Rect::new(50.0, 0.0, 50.0, 100.0), &right),
        ],
    }];
    let snapshot = compose_snapshot(&tiled, Rect::new(0.0, 0.0, 100.0, 100.0)).expect("应有内容");
    assert_eq!(snapshot.pixels[0].r(), 10, "左块覆盖 x<50");
    assert_eq!(snapshot.pixels[49].r(), 10);
    assert_eq!(snapshot.pixels[50].r(), 20, "右块覆盖 x≥50");
    assert_eq!(snapshot.pixels[99 * 100 + 99].r(), 20, "右下角属于右块");
}

/// 图像按保持长宽比放进窗口区域（居中、不拉伸）。
#[test]
fn fit_inside_preserves_aspect_ratio() {
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));

    // 宽图 → 受宽度限制，高度按比例
    let fitted = fit_inside(egui::vec2(400.0, 100.0), area);
    assert_eq!(fitted.width(), 200.0);
    assert_eq!(fitted.height(), 50.0);
    assert_eq!(fitted.center(), area.center());

    // 高图 → 受高度限制
    let fitted = fit_inside(egui::vec2(100.0, 400.0), area);
    assert_eq!(fitted.height(), 100.0);
    assert_eq!(fitted.width(), 25.0);

    // 退化输入不 panic
    assert_eq!(fit_inside(egui::vec2(0.0, 0.0), area), area);
}

/// 截图遮罩：选区内部必须**不被**蒙层覆盖（即只有四块绕开选区的矩形）。
#[test]
fn veil_bands_leave_the_selection_transparent() {
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 80.0));

    // 无选区 → 整块蒙层
    assert_eq!(veil_bands(area, None), vec![area]);

    // 中间选区 → 四块，且没有任何一块覆盖选区中心
    let hole = egui::Rect::from_min_size(egui::pos2(30.0, 20.0), egui::vec2(40.0, 30.0));
    let bands = veil_bands(area, Some(hole));
    assert_eq!(bands.len(), 4);
    for band in &bands {
        assert!(!band.contains(hole.center()), "蒙层不得覆盖选区中心");
    }
    // 四块面积之和 = 总面积 − 选区面积
    let covered: f32 = bands.iter().map(|band| band.width() * band.height()).sum();
    let expected = area.width() * area.height() - hole.width() * hole.height();
    assert!(
        (covered - expected).abs() < 1e-3,
        "覆盖面积 {covered} vs {expected}"
    );

    // 选区贴住左边 → 只剩 3 块（左块宽度为 0，不产生）
    let edge = egui::Rect::from_min_size(egui::pos2(0.0, 20.0), egui::vec2(40.0, 30.0));
    assert_eq!(veil_bands(area, Some(edge)).len(), 3);

    // 选区完全在区域外 → 退回整块蒙层
    let outside = egui::Rect::from_min_size(egui::pos2(500.0, 500.0), egui::vec2(10.0, 10.0));
    assert_eq!(veil_bands(area, Some(outside)), vec![area]);
}

/// 截图区域 → PDF 页面空间：只取与该页的交集，且坐标归一化。
#[test]
fn view_rect_to_page_rect_maps_and_clips() {
    // 页 200×100 放在内容空间 (10, 20)，缩放 1、无旋转
    let placement = PagePlacement::new(
        PageIndex::FIRST,
        Rect::new(10.0, 20.0, 200.0, 100.0),
        Scale::IDENTITY,
        Rotation::None,
        Size::new(200.0, 100.0),
    );

    // 滚动为 0 时，视图矩形 (30,40,50,20) → 内容 (30,40,50,20) → 页内 (20,20)
    let page_rect =
        view_rect_to_page_rect(&placement, Rect::new(30.0, 40.0, 50.0, 20.0), Vec2::ZERO)
            .expect("应命中页面");
    assert!((page_rect.x - 20.0).abs() < 1e-3);
    assert!((page_rect.width - 50.0).abs() < 1e-3);

    // 滚动会平移内容：scroll=(10,10) 时同一视图矩形对应的页内区域右移下移
    let scrolled = view_rect_to_page_rect(
        &placement,
        Rect::new(30.0, 40.0, 50.0, 20.0),
        Vec2::new(10.0, 10.0),
    )
    .expect("应命中页面");
    assert!((scrolled.x - 30.0).abs() < 1e-3, "实际 {}", scrolled.x);

    // 完全在页外 → None
    assert!(
        view_rect_to_page_rect(&placement, Rect::new(500.0, 500.0, 10.0, 10.0), Vec2::ZERO)
            .is_none()
    );
}

/// 引用点：按目标页分组、按位置去重、无形坐标的目标不产生标记。
#[test]
fn destination_points_group_by_page_and_dedupe_by_position() {
    let link = |destination: Destination| Link {
        rect: Rect::new(0.0, 0.0, 10.0, 10.0),
        destination,
    };
    let links_by_page = vec![
        vec![
            // 两个链接指向**同一点** → 只记一次
            link(Destination::Page {
                page: PageIndex::new(1),
                offset: Some(Point::new(10.0, 20.0)),
            }),
            link(Destination::Page {
                page: PageIndex::new(1),
                offset: Some(Point::new(10.2, 20.2)),
            }),
            // 另一个点 → 另记一次
            link(Destination::Page {
                page: PageIndex::new(1),
                offset: Some(Point::new(300.0, 400.0)),
            }),
            // 只有页、没有坐标 → 不产生标记
            link(Destination::Page {
                page: PageIndex::new(2),
                offset: None,
            }),
            // 外部 URI → 忽略
            link(Destination::Uri("https://example.com".into())),
        ],
        // 第 2 页上的链接也可以指向第 1 页（同样落在同一个 0.5pt 网格上 → 仍视为同一点）
        vec![link(Destination::Page {
            page: PageIndex::new(1),
            offset: Some(Point::new(10.2, 20.2)),
        })],
    ];

    let points = destination_points(&links_by_page, 3);
    assert_eq!(points.len(), 3, "按目标页分桶");
    assert_eq!(
        points[1],
        vec![Point::new(10.0, 20.0), Point::new(300.0, 400.0)],
        "同一点去重（10.2/20.2 量化到 0.5pt 网格后仍是 10.0/20.0），不同点保留"
    );
    assert!(points[2].is_empty(), "无坐标的目标不产生标记");
    assert!(points[0].is_empty());

    // 越界目标丢弃，不 panic
    let points = destination_points(&links_by_page, 1);
    assert_eq!(points.len(), 1);
}

/// 浮窗：夹取到视图区域内、不小于最小尺寸、区域过小时不产生负尺寸。
#[test]
fn clamp_float_rect_keeps_the_window_inside_the_view() {
    let rect = |x: f32, y: f32, w: f32, h: f32| {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    };
    let area = rect(100.0, 50.0, 800.0, 600.0);

    // 拖到右下角外 → 拉回区域内部
    let clamped = clamp_float_rect(rect(2000.0, 2000.0, 300.0, 200.0), area, FLOAT_MIN_SIZE);
    assert!(clamped.max.x <= area.max.x + 1e-3 && clamped.max.y <= area.max.y + 1e-3);
    assert!(clamped.min.x >= area.min.x && clamped.min.y >= area.min.y);

    // 拖到左上角外 → 拉回区域内
    let clamped = clamp_float_rect(rect(-500.0, -500.0, 300.0, 200.0), area, FLOAT_MIN_SIZE);
    assert_eq!((clamped.min.x, clamped.min.y), (area.min.x, area.min.y));

    // 太小 → 抬到最小尺寸
    let clamped = clamp_float_rect(rect(200.0, 100.0, 10.0, 10.0), area, FLOAT_MIN_SIZE);
    assert_eq!(
        (clamped.width(), clamped.height()),
        (FLOAT_MIN_SIZE.x, FLOAT_MIN_SIZE.y)
    );

    // 比区域还大 → 收缩到区域尺寸
    let clamped = clamp_float_rect(rect(100.0, 50.0, 5000.0, 5000.0), area, FLOAT_MIN_SIZE);
    assert_eq!(
        (clamped.width(), clamped.height()),
        (area.width(), area.height())
    );

    // 区域本身比最小尺寸还小 → 不得出现负尺寸
    let tiny = rect(0.0, 0.0, 40.0, 30.0);
    let clamped = clamp_float_rect(rect(0.0, 0.0, 200.0, 200.0), tiny, FLOAT_MIN_SIZE);
    assert!(clamped.width() >= 0.0 && clamped.height() >= 0.0);
    assert_eq!(clamped.width(), tiny.width());
}

/// 回归：指针不在画布上（例如在侧栏里滚动）时，滚轮**不得**作用于文档视图。
#[test]
fn wheel_targets_canvas_only_when_the_pointer_is_over_it() {
    assert!(wheel_targets_canvas(true), "指针在画布上 → 滚动文档");
    assert!(
        !wheel_targets_canvas(false),
        "指针在侧栏/工具栏上 → 不滚动文档"
    );
    assert!(
        !wheel_targets_canvas_with_float(true, true),
        "指针在浮窗上 → 只滚浮窗，不滚文档"
    );
    assert!(
        wheel_targets_canvas_with_float(true, false),
        "指针在画布空白处 → 仍滚文档"
    );
}

/// 位图 → ColorImage：一遍完成、通道顺序正确、尺寸匹配。
#[test]
fn color_image_converts_bgra_in_one_pass() {
    // 2×2 BGRA：红 / 绿 / 蓝 / 半透明白
    let data: Vec<u8> = vec![
        0, 0, 255, 255, // BGRA 红
        0, 255, 0, 255, // BGRA 绿
        255, 0, 0, 255, // BGRA 蓝
        255, 255, 255, 128,
    ];
    let image = color_image(PixelFormat::Bgra8, &data, 2, 2);
    assert_eq!(image.size, [2, 2]);
    assert_eq!(image.pixels.len(), 4);
    assert_eq!(image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
    assert_eq!(image.pixels[1], egui::Color32::from_rgb(0, 255, 0));
    assert_eq!(image.pixels[2], egui::Color32::from_rgb(0, 0, 255));
    assert_eq!(image.pixels[3].a(), 128, "alpha 必须保留");

    // RGBA 走另一条分支
    let rgba: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0];
    let image = color_image(PixelFormat::Rgba8, &rgba, 2, 2);
    assert_eq!(image.pixels[0], egui::Color32::from_rgb(255, 0, 0));

    // 灰度：R=G=B
    let gray: Vec<u8> = vec![10, 200, 0, 255];
    let image = color_image(PixelFormat::Gray8, &gray, 2, 2);
    assert_eq!(image.pixels[1], egui::Color32::from_rgb(200, 200, 200));

    // 数据不足时不得越界（按可用像素截断）
    let short = color_image(PixelFormat::Bgra8, &data[..4], 2, 2);
    assert_eq!(short.pixels.len(), 1, "不足的像素不应被凭空补齐");
}

/// 大纲摊平：顺序、层级、目标都要原样保留（虚拟化行渲染依赖它）。
#[test]
fn flatten_outline_keeps_order_depth_and_destination() {
    let leaf = |title: &str, page: u32| OutlineNode {
        title: title.to_string(),
        destination: Some(Destination::Page {
            page: PageIndex::new(page),
            offset: None,
        }),
        children: Vec::new(),
    };
    let mut root = leaf("章 1", 0);
    root.children = vec![leaf("节 1.1", 1), leaf("节 1.2", 2)];
    let nodes = vec![root, leaf("章 2", 3)];

    let rows = flatten_outline(&nodes);
    assert_eq!(rows.len(), outline_node_count(&nodes), "行数应等于节点总数");
    assert_eq!(
        rows.iter()
            .map(|(depth, title, _)| (*depth, title.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "章 1"), (1, "节 1.1"), (1, "节 1.2"), (0, "章 2"),],
        "深度优先、父在前、层级正确"
    );
    assert_eq!(
        rows[1].2,
        Some(Destination::Page {
            page: PageIndex::new(1),
            offset: None
        }),
        "目标必须保留（点击跳转要用）"
    );
    assert!(flatten_outline(&[]).is_empty());
}

/// 大纲节点计数（诊断用：侧栏一次布局上百行会稳定吃掉几十毫秒/帧）。
#[test]
fn outline_node_count_counts_nested_nodes() {
    let leaf = |title: &str| OutlineNode {
        title: title.to_string(),
        destination: None,
        children: Vec::new(),
    };
    let mut root = leaf("章 1");
    root.children = vec![leaf("节 1.1"), leaf("节 1.2")];
    let nodes = vec![root, leaf("章 2")];
    assert_eq!(
        outline_node_count(&nodes),
        4,
        "根 1 + 它的 2 个子节点 + 根 2 = 4"
    );
    assert_eq!(outline_node_count(&[]), 0);
}

/// 上传与裁剪必须共用同一集合，否则相邻页纹理会被"上传→裁掉→再上传"地抖动。
#[test]
fn texture_pages_covers_visible_plus_neighbours() {
    let visible = [PageIndex::new(4)];
    let pages = texture_pages(&visible, PageIndex::new(4), 10);
    assert_eq!(
        pages,
        vec![PageIndex::new(3), PageIndex::new(4), PageIndex::new(5)],
        "可见页 ±1 都要（上传与裁剪共用）"
    );

    // 首尾越界自动裁剪
    assert_eq!(
        texture_pages(&[PageIndex::FIRST], PageIndex::FIRST, 10),
        vec![PageIndex::FIRST, PageIndex::new(1)]
    );
    let last = PageIndex::new(9);
    assert_eq!(
        texture_pages(&[last], last, 10),
        vec![PageIndex::new(8), last]
    );

    // 当前页不在可见集合里时也要保留
    let pages = texture_pages(&[PageIndex::new(4)], PageIndex::new(7), 10);
    assert!(pages.contains(&PageIndex::new(7)), "当前页必须保留");

    // 多页可见时不重复
    let pages = texture_pages(
        &[PageIndex::new(4), PageIndex::new(5)],
        PageIndex::new(4),
        10,
    );
    assert_eq!(
        pages,
        vec![
            PageIndex::new(3),
            PageIndex::new(4),
            PageIndex::new(5),
            PageIndex::new(6)
        ]
    );
    assert_eq!(pages.len(), 4, "不得有重复项");
}

/// 回归：画布必须保留**全部剩余高度**。
///
/// 第五轮缺陷：拖拽条用 `allocate_painter(5px × 整高)` 画，纵向布局把整高吃光，
/// 画布高度变成 0 → "分割线能拖，但没有任何图像"。
#[test]
fn canvas_rect_keeps_full_height() {
    let full = egui::Rect::from_min_size(egui::pos2(260.0, 40.0), egui::vec2(840.0, 800.0));

    let canvas = canvas_rect_for(full, 5.0);
    assert_eq!(canvas.min.x, 265.0, "画布应从拖拽条右侧开始");
    assert_eq!(canvas.min.y, 40.0);
    assert_eq!(canvas.width(), 835.0, "只让出拖拽条的宽度");
    assert_eq!(
        canvas.height(),
        800.0,
        "高度必须保持满高（这正是当时的缺陷）"
    );

    // 侧栏隐藏时不让位
    assert_eq!(canvas_rect_for(full, 0.0), full);

    // 区域比拖拽条还窄时不得出现负尺寸
    let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(3.0, 10.0));
    let canvas = canvas_rect_for(tiny, 5.0);
    assert!(
        canvas.width() >= 0.0 && canvas.height() >= 0.0,
        "实际 {canvas:?}"
    );
    assert_eq!(canvas.height(), 10.0, "仍要保持满高");
}

/// 缩放过场要用的"上一档"必须保留，但比例不能无限堆积（第五轮验收：相邻页闪白）。
#[test]
fn retain_keeps_current_and_one_old_scale() {
    let page = PageIndex::new(0);
    let other = PageIndex::new(1);
    // 用**瓦片**表达"旧档"：整页图另有"全留"的规则（D-110），不能混在一起测
    let key = |page: PageIndex, scale: f32, rotation: Rotation| {
        RenderKey::tiled(page, Scale::from_zoom(scale), rotation, TileSpec::new(0, 0))
    };

    let mut store = KeyedStore::new(64);
    for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
        store.insert(key(page, scale, Rotation::None), scale);
    }
    store.insert(key(other, 1.5, Rotation::None), 1.5);
    store.insert(key(other, 2.0, Rotation::None), 2.0);
    store.insert(key(PageIndex::new(2), 2.0, Rotation::None), 2.0);
    // 旧旋转的图留着只会画成横躺
    store.insert(key(page, 2.0, Rotation::Clockwise90), 2.0);

    store.retain_for_view(
        &[
            facts(page, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
            facts(other, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
        ],
        |_| 0,
    );

    assert!(
        store.contains(&key(page, 2.0, Rotation::None)),
        "当前比例必须保留"
    );
    assert!(
        store.contains(&key(page, 1.5, Rotation::None)),
        "最近的旧比例必须保留（缩放过场要用它顶替，否则闪白）"
    );
    assert!(
        !store.contains(&key(page, 1.25, Rotation::None)),
        "更旧的档应丢弃"
    );
    assert!(!store.contains(&key(page, 1.0, Rotation::None)));
    assert!(!store.contains(&key(page, 3.0, Rotation::None)));
    // 高于当前比例的**整页图**没有用（缩小用不到），丢掉
    store.insert(
        RenderKey::new(page, Scale::from_zoom(3.0), Rotation::None),
        3.0,
    );
    store.retain_for_view(
        &[
            facts(page, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
            facts(other, Rotation::None, 2000, PageShape::Tiles(0, 0, 0, 0)),
        ],
        |_| 0,
    );
    assert!(
        !store.contains(&RenderKey::new(page, Scale::from_zoom(3.0), Rotation::None)),
        "高于当前比例的整页图应丢弃"
    );
    assert!(
        !store.contains(&key(page, 2.0, Rotation::Clockwise90)),
        "旧旋转必须丢弃"
    );
    assert!(
        !store.contains(&key(PageIndex::new(2), 2.0, Rotation::None)),
        "不在保留列表里的页应丢弃"
    );
    // 其它可见页同样"当前 + 最近旧档"
    assert!(store.contains(&key(other, 2.0, Rotation::None)));
    assert!(store.contains(&key(other, 1.5, Rotation::None)));
}

#[test]
fn float_link_parses_annotation_and_preview_forms() {
    assert_eq!(
        parse_float_link("pd-float://annotation/42"),
        Some(FloatLinkTarget::Annotation { page: None, id: 42 })
    );
    assert_eq!(
        parse_float_link("pd-float://annotation/3/42"),
        Some(FloatLinkTarget::Annotation {
            page: Some(PageIndex::new(3)),
            id: 42
        })
    );
    assert_eq!(
        parse_float_link("pd-float://preview/3/7"),
        Some(FloatLinkTarget::PreviewLegacy {
            page: PageIndex::new(3),
            id: 7
        })
    );
    assert_eq!(
        parse_float_link("pd-float://preview/preview-abc-1"),
        Some(FloatLinkTarget::PreviewRecord("preview-abc-1".to_string()))
    );
    // 查询串/锚点不影响基础解析
    assert_eq!(
        parse_float_link("pd-float://annotation/42?x=1"),
        Some(FloatLinkTarget::Annotation { page: None, id: 42 })
    );
}

#[test]
fn float_link_rejects_other_schemes_and_bad_segments() {
    assert_eq!(parse_float_link("https://example.com"), None);
    assert_eq!(parse_float_link("pd-float://"), None);
    assert_eq!(parse_float_link("pd-float://annotation/"), None);
    assert_eq!(parse_float_link("pd-float://annotation/abc"), None);
    assert_eq!(parse_float_link("pd-float://preview/1/2/3"), None);
    assert_eq!(parse_float_link("pd-float://document/1"), None);
}
