//! 预渲染阶梯（preview ladder）的端到端测试。
//!
//! 设计见 `docs/preview-ladder-design.md`；这里覆盖设计文档 §7 的逻辑层项：
//! 预览档计算与量化、切档时的取消、`render_scale`/`image_at` 一致性、预取仍用完整档。

use std::sync::Arc;

use pdfcore::prelude::*;
use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache};

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

fn ready(session: &mut DocumentSession, events: &mut Vec<SessionEvent>) {
    session.handle(ViewerCommand::ResizeViewport(Size::new(840.0, 800.0)), events);
    session.handle(ViewerCommand::RequestVisible, events);
    session.pump(events);
}

/// 预览档 = 目标 ÷ 4 后向下量化到 2 的幂；完整档本来便宜时不启用（设计 §3.3 / §9-Q3）。
#[test]
fn preview_scale_is_quantized_and_only_engages_when_needed() {
    let mut session = assemble();
    let mut events = Vec::new();
    ready(&mut session, &mut events);
    let page = PageIndex::FIRST;

    // 100%：612×792 = 0.48 MP，远低于分块阈值 → 不启用（降质没意义）
    session.handle(ViewerCommand::SetZoom(1.0), &mut events);
    assert_eq!(session.preview_scale(page), None);
    assert_eq!(session.render_scale(page), Some(Scale::from_zoom(1.0)));

    // 200%：1.9 MP < 4 MP → 仍不启用
    session.handle(ViewerCommand::SetZoom(2.0), &mut events);
    assert_eq!(session.preview_scale(page), None);

    // 300%：4.36 MP > 4 MP → 启用；3.0/4 = 0.75 → 向下量化到 0.5
    session.handle(ViewerCommand::SetZoom(3.0), &mut events);
    assert_eq!(session.preview_scale(page), Some(Scale::from_zoom(0.5)));

    // 400%：4.0/4 = 1.0 → 1.0
    session.handle(ViewerCommand::SetZoom(4.0), &mut events);
    assert_eq!(session.preview_scale(page), Some(Scale::from_zoom(1.0)));

    // 800%：8.0/4 = 2.0 → 2.0
    session.handle(ViewerCommand::SetZoom(8.0), &mut events);
    assert_eq!(session.preview_scale(page), Some(Scale::from_zoom(2.0)));
}

/// 连续缩放跨 10 档时，预览档只应命中少数几个键（量化 + 去重才不会每档都渲染）。
#[test]
fn continuous_zoom_hits_only_a_few_preview_keys() {
    let mut session = assemble();
    let mut events = Vec::new();
    ready(&mut session, &mut events);
    let page = PageIndex::FIRST;

    let mut levels = std::collections::BTreeSet::new();
    for step in 0..12 {
        let zoom = 1.0 * 1.25f32.powi(step);
        session.handle(ViewerCommand::SetZoom(zoom), &mut events);
        if let Some(scale) = session.preview_scale(page) {
            levels.insert((scale.0 * 1000.0) as u32);
        }
    }
    assert!(
        (2..=4).contains(&levels.len()),
        "12 档连续缩放应只落在 2–4 个预览档上，实际 {levels:?}"
    );
}

/// 预览模式：请求的是预览档；切回完整档后请求的是布局比例。
#[test]
fn preview_quality_renders_the_preview_scale_and_full_restores_it() {
    let mut session = assemble();
    let mut events = Vec::new();
    ready(&mut session, &mut events);
    let page = PageIndex::FIRST;
    session.handle(ViewerCommand::SetZoom(8.0), &mut events);

    let preview = session.preview_scale(page).expect("800% 应启用预览档");
    assert_eq!(preview, Scale::from_zoom(2.0));

    // 切到预览档 → 渲染出来的是预览档的整页图（1.9 MP < 4 MP → 整页）
    session.handle(ViewerCommand::SetRenderQuality(RenderQuality::Preview), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    assert_eq!(session.render_scale(page), Some(preview));
    assert!(
        session.image_at(page, preview).is_some(),
        "预览档的位图应已入缓存"
    );
    assert!(
        session.image_for(page).is_none(),
        "完整档（分块）此时不应有整页位图"
    );

    // 切回完整档 → 渲染的是布局比例（800% 走分块，因此整页图仍为 None，但瓦片应就绪）
    session.handle(ViewerCommand::SetRenderQuality(RenderQuality::Full), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);
    assert_eq!(session.render_scale(page), Some(Scale::from_zoom(8.0)));
    assert!(
        !session.tiles_for(page).is_empty(),
        "完整档在 800% 下应就绪若干瓦片"
    );
}

/// 切到预览档时必须**取消**完整档的在飞任务（否则 PDFium 先把昂贵那张画完，预览反而迟到）。
#[test]
fn switching_to_preview_cancels_full_quality_in_flight() {
    let mut session = assemble();
    let mut events = Vec::new();
    ready(&mut session, &mut events);
    session.handle(ViewerCommand::SetZoom(8.0), &mut events);

    // 提交完整档但不取回 → 处于"在飞"
    session.handle(ViewerCommand::RequestVisible, &mut events);
    assert!(session.pending_renders() > 0, "应有在飞任务");
    events.clear();

    session.handle(ViewerCommand::SetRenderQuality(RenderQuality::Preview), &mut events);
    session.pump(&mut events);

    let full_milli = RenderKey::new(PageIndex::FIRST, Scale::from_zoom(8.0), Rotation::None)
        .scale_milli;
    let rendered_full: Vec<RenderKey> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::PageRendered(key) if key.scale_milli == full_milli => Some(*key),
            _ => None,
        })
        .collect();
    assert!(
        rendered_full.is_empty(),
        "切到预览后，完整档不应再产出渲染结果：{rendered_full:?}"
    );
}

/// 预取固定使用**完整档**：用户停手后马上要看的就是它，预取糊图没有意义。
#[test]
fn prefetch_uses_the_full_scale() {
    let mut session = assemble();
    let mut events = Vec::new();
    ready(&mut session, &mut events);
    session.handle(ViewerCommand::SetZoom(8.0), &mut events);
    session.handle(ViewerCommand::SetRenderQuality(RenderQuality::Preview), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    let preview = session.preview_scale(PageIndex::FIRST).expect("预览档");
    // 第 2 页是预取页（可见页 +1）：它**不应**有预览档的位图
    assert!(
        session.image_at(PageIndex::new(1), preview).is_none(),
        "预取不应使用预览档"
    );
}

/// `RenderQuality` 切换幂等；完整档下 `render_scale`/`image_at` 与 `image_for` 一致。
#[test]
fn full_quality_keeps_the_existing_behavior() {
    let mut session = assemble();
    let mut events = Vec::new();
    ready(&mut session, &mut events);
    let page = PageIndex::FIRST;

    // 幂等：重复设置同一档不改变任何东西
    session.handle(ViewerCommand::SetRenderQuality(RenderQuality::Full), &mut events);
    session.handle(ViewerCommand::SetRenderQuality(RenderQuality::Full), &mut events);
    assert_eq!(session.state().render_quality, RenderQuality::Full);

    let whole = session.image_for(page).expect("100% 下应有整页位图");
    assert!(Arc::ptr_eq(
        &whole,
        &session
            .image_at(page, Scale::from_zoom(1.0))
            .expect("image_at 应与 image_for 一致")
    ));
}
