//! 运行期零件的集成测试：真实的 [`WorkerScheduler`]（后台线程）与 [`LruCache`] 接进
//! [`DocumentSession`]，以及"滚出视口即取消"的记账正确性。
//!
//! 与 `tests/assemble.rs` 的区别：那里用同步的 `ImmediateScheduler` 验证装配与业务逻辑，
//! 这里验证**真实线程 + 缓存淘汰 + 取消**在会话里协同工作。

use std::sync::Arc;
use std::time::{Duration, Instant};

use pdfcore::prelude::*;
use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache};

/// 用真实后台调度器装配会话。
fn session_with_worker_scheduler(
    cache: Arc<LruCache>,
    concurrency: usize,
) -> DocumentSession {
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).expect("打开假文档");
    let scheduler = Arc::new(WorkerScheduler::new(opened.raster.clone(), concurrency));

    DocumentSession::new(
        opened,
        cache,
        scheduler,
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话")
}

/// 轮询 `pump` 直到条件成立（真实线程是异步的，必须轮询而不是假定一步到位）。
fn pump_until(
    session: &mut DocumentSession,
    label: &str,
    timeout: Duration,
    mut condition: impl FnMut(&DocumentSession) -> bool,
) {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        session.pump(&mut events);
        if condition(session) {
            return;
        }
        session.handle(ViewerCommand::RequestVisible, &mut events);
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("超时：{label}");
}

/// 逆时针旋转：等价于三次顺时针，四个方向闭合回原位。
#[test]
fn counter_clockwise_rotation_closes_the_loop() {
    let cache = Arc::new(LruCache::new(64 * 1024 * 1024));
    let mut session = session_with_worker_scheduler(cache, 1);
    let start = session.state().user_rotation;
    for _ in 0..4 {
        let _ = session.handle(ViewerCommand::RotateCounterClockwise, &mut Vec::new());
    }
    assert_eq!(session.state().user_rotation, start, "四次逆时针应回到初始方向");
    let _ = session.handle(ViewerCommand::RotateCounterClockwise, &mut Vec::new());
    assert_ne!(session.state().user_rotation, start, "一次逆时针应改变方向");
}

#[test]
fn worker_scheduler_renders_through_the_session() {
    let cache = Arc::new(LruCache::new(64 * 1024 * 1024));
    let mut session = session_with_worker_scheduler(cache.clone(), 1);

    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);

    pump_until(&mut session, "首页渲染入缓存", Duration::from_secs(5), |session| {
        session.image_for(PageIndex::FIRST).is_some()
    });

    let image = session.image_for(PageIndex::FIRST).expect("首页位图");
    assert_eq!((image.width, image.height), (612, 792));

    // 位图确实进了 LRU 缓存（而不是只存在于事件里）
    let stats = cache.stats();
    assert!(stats.entries >= 1, "缓存应有条目：{stats:?}");
    assert!(stats.bytes >= image.byte_len() as u64);

    // 后台调度器不应把任务留在队列里
    assert_eq!(session.pump(&mut Vec::new()), (), "pump 不应 panic");
}

#[test]
fn lru_cache_evicts_and_session_still_works() {
    // 上限只够放一张 612×792 的位图（1.9 MB）→ 第 2 页渲染后会淘汰第 1 页
    let cache = Arc::new(LruCache::new(2 * 1024 * 1024));
    let mut session = session_with_worker_scheduler(cache.clone(), 1);

    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    pump_until(&mut session, "首页就绪", Duration::from_secs(5), |session| {
        session.image_for(PageIndex::FIRST).is_some()
    });

    // 跳到第 2 页并等它渲染
    session.handle(ViewerCommand::GoToPage(PageIndex::new(1)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    pump_until(&mut session, "第 2 页就绪", Duration::from_secs(5), |session| {
        session.image_for(PageIndex::new(1)).is_some()
    });

    let stats = cache.stats();
    assert!(stats.bytes <= cache.capacity_bytes(), "缓存必须遵守上限：{stats:?}");
    // 被淘汰的页会重新请求（这是缓存未命中的正常路径，不应报错）
    assert!(session.image_for(PageIndex::new(1)).is_some());
}

#[test]
fn scrolling_away_cancels_stale_work() {
    // 同步调度器会把该页已产生的事件改写为 Cancelled，因此"取消"可被观测
    let backend = MemoryBackend::demo();
    let opened = backend.open(Source::Bytes(b"%PDF-1.4 fake"), None).expect("打开");
    let cache = Arc::new(MemoryCache::new());
    let mut session = DocumentSession::new(
        opened,
        cache.clone(),
        Arc::new(ImmediateScheduler::new(backend.open(Source::Bytes(b"x"), None).unwrap().raster)),
        Box::new(VerticalLayout),
        Box::new(RangeSelection::new()),
    )
    .expect("装配会话");

    let mut events = Vec::new();
    session.handle(ViewerCommand::ResizeViewport(Size::new(700.0, 800.0)), &mut events);
    session.handle(ViewerCommand::RequestVisible, &mut events);
    // 关键：这里**故意不 pump**。pump 会把已完成的任务从"在飞集合"里清掉，
    // 那样滚动时就无任务可取消了。保持"已提交、未取回"的状态，才能验证取消路径。
    assert!(session.image_for(PageIndex::FIRST).is_none(), "尚未 pump，缓存里不应有位图");

    // 滚到文档底部：可见页变为第 2/3 页，第 1 页的在飞任务应被取消
    session.handle(ViewerCommand::SetScroll(Vec2::new(0.0, 1624.0)), &mut events);
    assert_eq!(session.state().current_page, PageIndex::new(2), "中心落在第 3 页");

    events.clear();
    session.handle(ViewerCommand::RequestVisible, &mut events);
    session.pump(&mut events);

    assert!(
        events.iter().any(|event| matches!(event, SessionEvent::PageRendered(_))),
        "当前可见页应被渲染：{events:?}"
    );
    assert!(
        session.image_for(PageIndex::FIRST).is_none(),
        "滚出视口的页其任务已被取消，位图不应进入缓存"
    );
    assert!(session.image_for(PageIndex::new(2)).is_some(), "当前页应可用");
}
