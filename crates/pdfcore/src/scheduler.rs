//! 后台线程 + 优先级队列的渲染调度器 —— [`RenderScheduler`] 的默认实现。
//!
//! ## 结构
//! ```text
//!   UI 线程                        工作线程（数量 = concurrency）
//!   submit ──▶ [优先级队列] ──▶ 取出 ──▶ 检查取消令牌 ──▶ PageRaster::render
//!   drain_events ◀── [事件队列] ◀──────────── 结果 ─────────────┘
//! ```
//!
//! ## 两条容易写错、但很关键的规则
//! 1. **取消不能"静默丢弃"任务**：必须为每个被取消的任务补一条
//!    [`RenderEvent::Cancelled`]，否则 `DocumentSession` 里"在飞集合"永远清不掉，
//!    该页再也不会被重新请求。因此本实现只给任务**打取消标记**，
//!    由工作线程取出时补发事件（不付出渲染代价）。
//! 2. **已完成的结果不丢弃**：如果任务已进入 `render` 才发现被取消（PDFium 无法中断），
//!    结果照常上报 —— 代价已经付出，进缓存反而对后续滚动有益。
//!
//! ## 并发上限
//! 工作线程数由构造参数决定，调用方应传入 [`Capabilities::max_concurrency`]
//! （PDFium 为 1，即单线程串行）。

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::document::Capabilities;
use crate::ids::{JobId, PageIndex};
use crate::render::{
    CancelToken, PageRaster, RenderEvent, RenderKey, RenderPriority, RenderRequest, RenderScheduler,
};

/// 后台线程 + 优先级队列的调度器。
pub struct WorkerScheduler {
    shared: Arc<Shared>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

struct Shared {
    raster: Arc<dyn PageRaster>,
    state: Mutex<State>,
    wake: Condvar,
    events: Mutex<VecDeque<RenderEvent>>,
    shutting_down: AtomicBool,
    next_job: AtomicU64,
}

#[derive(Default)]
struct State {
    queue: BinaryHeap<QueuedJob>,
    /// 排队中 + 正在渲染的键（用于去重）。
    active: HashSet<RenderKey>,
    /// `JobId` → 取消令牌（用于 `cancel(job)`）。
    tokens: HashMap<JobId, CancelToken>,
    /// 正在渲染的任务数（用于 `pending()`）。
    in_flight: usize,
}


/// 队列中的任务。排序规则：**优先级小的先出**，同优先级按提交顺序 FIFO。
struct QueuedJob {
    priority: RenderPriority,
    sequence: u64,
    job: JobId,
    request: RenderRequest,
}

impl PartialEq for QueuedJob {
    fn eq(&self, other: &Self) -> bool {
        self.priority.rank() == other.priority.rank() && self.sequence == other.sequence
    }
}
impl Eq for QueuedJob {}
impl PartialOrd for QueuedJob {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueuedJob {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap 是最大堆，因此把优先级反过来比较
        other
            .priority
            .rank()
            .cmp(&self.priority.rank())
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl WorkerScheduler {
    /// 以指定的工作线程数构造。
    pub fn new(raster: Arc<dyn PageRaster>, concurrency: usize) -> Self {
        let concurrency = concurrency.max(1);
        let shared = Arc::new(Shared {
            raster,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            events: Mutex::new(VecDeque::new()),
            shutting_down: AtomicBool::new(false),
            next_job: AtomicU64::new(1),
        });

        let workers = (0..concurrency)
            .map(|index| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("pdfcore-render-{index}"))
                    .spawn(move || worker_loop(shared))
                    .expect("创建渲染线程")
            })
            .collect();

        Self { shared, workers: Mutex::new(workers) }
    }

    /// 按后端声明的并发上限构造（**推荐用法**）。
    pub fn with_capabilities(raster: Arc<dyn PageRaster>, capabilities: Capabilities) -> Self {
        Self::new(raster, capabilities.max_concurrency as usize)
    }

    /// 当前事件队列长度（`drain_events` 尚未取走的条数）。
    pub fn queued_events(&self) -> usize {
        self.shared.events.lock().unwrap().len()
    }
}

impl Drop for WorkerScheduler {
    fn drop(&mut self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        let workers = self.workers.get_mut().expect("工作线程句柄");
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        // 取任务（没有则等待唤醒；关闭时退出）
        let job = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if shared.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(job) = state.queue.pop() {
                    state.in_flight += 1;
                    break job;
                }
                state = shared.wake.wait(state).expect("等待渲染任务");
            }
        };

        let key = job.request.key();
        let event = if job.request.cancel.is_cancelled() {
            // 未付出渲染代价 → 直接补发取消事件（保证调用方的在飞集合能被清理）
            RenderEvent::Cancelled { page: job.request.page }
        } else {
            match shared.raster.render(&job.request) {
                Ok(image) => RenderEvent::Rendered { key, image },
                Err(error) => RenderEvent::Failed { page: job.request.page, error },
            }
        };

        {
            let mut state = shared.state.lock().unwrap();
            state.in_flight = state.in_flight.saturating_sub(1);
            state.active.remove(&key);
            state.tokens.remove(&job.job);
        }
        shared.events.lock().unwrap().push_back(event);
    }
}

impl RenderScheduler for WorkerScheduler {
    fn submit(&self, request: RenderRequest) -> JobId {
        let key = request.key();
        let mut state = self.shared.state.lock().unwrap();

        // 去重：同一键已在排队或正在渲染时不再入队（返回一个"立即可忽略"的任务号）
        if state.active.contains(&key) {
            return JobId(0);
        }

        let job = JobId(self.shared.next_job.fetch_add(1, Ordering::Relaxed));
        let sequence = job.0;
        state.active.insert(key);
        state.tokens.insert(job, request.cancel.clone());
        state.queue.push(QueuedJob { priority: request.priority, sequence, job, request });
        drop(state);

        self.shared.wake.notify_one();
        job
    }

    fn cancel(&self, job: JobId) {
        let state = self.shared.state.lock().unwrap();
        if let Some(token) = state.tokens.get(&job) {
            // 只打标记：由工作线程取出时补发 Cancelled 事件（见模块文档第 1 条）
            token.cancel();
        }
    }

    fn cancel_page(&self, page: PageIndex) {
        let state = self.shared.state.lock().unwrap();
        for (job, token) in &state.tokens {
            let belongs_to_page = state
                .queue
                .iter()
                .any(|queued| queued.job == *job && queued.request.page == page);
            if belongs_to_page {
                token.cancel();
            }
        }
    }

    fn cancel_all(&self) {
        let state = self.shared.state.lock().unwrap();
        for token in state.tokens.values() {
            token.cancel();
        }
    }

    fn drain_events(&self, max: usize) -> Vec<RenderEvent> {
        let mut events = self.shared.events.lock().unwrap();
        let take = max.min(events.len());
        events.drain(..take).collect()
    }

    fn pending(&self) -> usize {
        let state = self.shared.state.lock().unwrap();
        state.queue.len() + state.in_flight
    }
}

impl std::fmt::Debug for WorkerScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.state.lock().unwrap();
        f.debug_struct("WorkerScheduler")
            .field("workers", &self.workers.lock().unwrap().len())
            .field("queued", &state.queue.len())
            .field("in_flight", &state.in_flight)
            .field("events", &self.shared.events.lock().unwrap().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::geometry::Rotation;
    use crate::image::{PixelFormat, RasterImage};
    use crate::render::Scale;
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    /// 可以把渲染"卡住"的闸门，用于构造确定性的并发场景。
    #[derive(Default)]
    struct Gate {
        open: StdMutex<bool>,
        condvar: Condvar,
    }

    impl Gate {
        fn closed() -> Self {
            Self { open: StdMutex::new(false), condvar: Condvar::new() }
        }
        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.condvar.wait(open).unwrap();
            }
        }
        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.condvar.notify_all();
        }
    }

    /// 受控光栅器：记录开始/完成顺序，可阻塞在闸门上，可指定某些页失败。
    struct TestRaster {
        gate: Arc<Gate>,
        started: StdMutex<Vec<PageIndex>>,
        finished: StdMutex<Vec<PageIndex>>,
        failing: StdMutex<Vec<PageIndex>>,
        bytes: usize,
    }

    impl TestRaster {
        fn new(gate: Arc<Gate>) -> Self {
            Self {
                gate,
                started: StdMutex::new(Vec::new()),
                finished: StdMutex::new(Vec::new()),
                failing: StdMutex::new(Vec::new()),
                bytes: 16,
            }
        }
        fn started_count(&self) -> usize {
            self.started.lock().unwrap().len()
        }
        fn finished(&self) -> Vec<PageIndex> {
            self.finished.lock().unwrap().clone()
        }
    }

    impl PageRaster for TestRaster {
        fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>> {
            if request.cancel.is_cancelled() {
                return Err(crate::error::CoreError::Cancelled);
            }
            self.started.lock().unwrap().push(request.page);
            self.gate.wait();
            if self.failing.lock().unwrap().contains(&request.page) {
                return Err(crate::error::CoreError::Backend("测试用失败".to_string()));
            }
            self.finished.lock().unwrap().push(request.page);
            let data: Arc<[u8]> = Arc::from(vec![0u8; self.bytes].into_boxed_slice());
            Ok(Arc::new(
                RasterImage::new(4, 1, PixelFormat::Rgba8, data).expect("测试位图"),
            ))
        }
    }

    fn request(page: u32, priority: RenderPriority) -> RenderRequest {
        RenderRequest::new(PageIndex::new(page), Scale::IDENTITY, Rotation::None)
            .with_priority(priority)
    }

    /// 轮询等待条件成立（避免依赖固定 sleep）。
    fn wait_until(label: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("超时：{label}");
    }

    fn drain_all(scheduler: &WorkerScheduler) -> Vec<RenderEvent> {
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut idle_streak = 0;
        while Instant::now() < deadline {
            let batch = scheduler.drain_events(16);
            if batch.is_empty() {
                // 工作线程是"先减 in_flight、再推事件"，因此需要连续观察到空闲才认为结束
                if scheduler.pending() == 0 {
                    idle_streak += 1;
                    if idle_streak >= 3 {
                        break;
                    }
                }
            } else {
                idle_streak = 0;
                all.extend(batch);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        all
    }

    #[test]
    fn renders_in_background_and_reports_event() {
        let gate = Arc::new(Gate::default());
        gate.open();
        let raster = Arc::new(TestRaster::new(gate));
        let scheduler = WorkerScheduler::new(raster, 1);

        let job = scheduler.submit(request(3, RenderPriority::Visible));
        assert_ne!(job, JobId(0));

        wait_until("任务完成", Duration::from_secs(2), || scheduler.pending() == 0);
        let events = scheduler.drain_events(8);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RenderEvent::Rendered { key, image } => {
                assert_eq!(key.page, PageIndex::new(3));
                assert_eq!(image.byte_len(), 16);
            }
            other => panic!("期望 Rendered，实际 {other:?}"),
        }
    }

    #[test]
    fn deduplicates_identical_keys() {
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));
        let scheduler = WorkerScheduler::new(raster.clone(), 1);

        let first = scheduler.submit(request(1, RenderPriority::Visible));
        wait_until("第一个任务已开始", Duration::from_secs(2), || raster.started_count() == 1);
        // 同一页同一缩放同一旋转 → 应被去重，不入队
        let second = scheduler.submit(request(1, RenderPriority::Interactive));
        assert_eq!(second, JobId(0), "重复提交应返回可忽略的任务号");
        assert_eq!(scheduler.pending(), 1, "去重后队列里仍只有一个任务");

        gate.open();
        let events = drain_all(&scheduler);
        assert_eq!(raster.started_count(), 1, "同一键只渲染一次");
        assert_eq!(events.len(), 1);
        assert_ne!(first, second);
    }

    #[test]
    fn honours_priority_order() {
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));
        let scheduler = WorkerScheduler::new(raster.clone(), 1);

        // 先占住唯一的工作线程
        scheduler.submit(request(9, RenderPriority::Visible));
        wait_until("占位任务已开始", Duration::from_secs(2), || raster.started_count() == 1);

        // 依次提交：低优先级先提交，高优先级后提交
        scheduler.submit(request(3, RenderPriority::Background));
        scheduler.submit(request(2, RenderPriority::Prefetch));
        scheduler.submit(request(1, RenderPriority::Interactive));

        gate.open();
        drain_all(&scheduler);

        assert_eq!(
            raster.finished(),
            vec![PageIndex::new(9), PageIndex::new(1), PageIndex::new(2), PageIndex::new(3)],
            "同优先级 FIFO，跨优先级按 rank 升序"
        );
    }

    #[test]
    fn cancelling_a_queued_job_emits_cancelled_without_rendering() {
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));
        let scheduler = WorkerScheduler::new(raster.clone(), 1);

        scheduler.submit(request(9, RenderPriority::Visible)); // 占位
        wait_until("占位任务已开始", Duration::from_secs(2), || raster.started_count() == 1);
        let doomed = scheduler.submit(request(5, RenderPriority::Visible));

        scheduler.cancel(doomed);
        gate.open();
        let events = drain_all(&scheduler);

        assert!(
            !raster.finished().contains(&PageIndex::new(5)),
            "被取消的任务不应进入渲染"
        );
        let cancelled = events.iter().find_map(|event| match event {
            RenderEvent::Cancelled { page } if page.get() == 5 => Some(()),
            _ => None,
        });
        assert!(cancelled.is_some(), "必须补发 Cancelled 事件（否则调用方在飞集合泄漏）：{events:?}");
    }

    #[test]
    fn cancelling_a_page_clears_all_its_scales_and_tiles() {
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));
        let scheduler = WorkerScheduler::new(raster.clone(), 1);

        scheduler.submit(request(9, RenderPriority::Visible)); // 占位
        wait_until("占位任务已开始", Duration::from_secs(2), || raster.started_count() == 1);

        scheduler.submit(request(4, RenderPriority::Visible));
        scheduler.submit(request(5, RenderPriority::Visible));
        scheduler.cancel_page(PageIndex::new(4));
        gate.open();
        drain_all(&scheduler);

        let finished = raster.finished();
        assert!(!finished.contains(&PageIndex::new(4)), "页 4 的任务应全部取消");
        assert!(finished.contains(&PageIndex::new(5)), "页 5 不受影响");
    }

    #[test]
    fn respects_concurrency_limit() {
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));

        // 并发 2：两个任务应同时在渲染中
        let scheduler = WorkerScheduler::new(raster.clone(), 2);
        scheduler.submit(request(1, RenderPriority::Visible));
        scheduler.submit(request(2, RenderPriority::Visible));
        wait_until("两个任务应并发开始", Duration::from_secs(2), || raster.started_count() == 2);
        assert_eq!(scheduler.pending(), 2);
        gate.open();
        drain_all(&scheduler);
        assert_eq!(raster.finished().len(), 2);
    }

    #[test]
    fn capabilities_decide_the_worker_count() {
        // PDFium 声明 max_concurrency = 1：即便提交多个任务，同一时刻也只有一个在渲染
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));
        let scheduler =
            WorkerScheduler::with_capabilities(raster.clone(), crate::document::Capabilities::full());

        for page in 0..4 {
            scheduler.submit(request(page, RenderPriority::Visible));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(raster.started_count(), 1, "并发上限为 1 时不得并行渲染");

        gate.open();
        drain_all(&scheduler);
        assert_eq!(raster.finished().len(), 4);
    }

    #[test]
    fn failed_render_is_reported_as_event() {
        let gate = Arc::new(Gate::default());
        gate.open();
        let raster = Arc::new(TestRaster::new(gate));
        raster.failing.lock().unwrap().push(PageIndex::new(6));
        let scheduler = WorkerScheduler::new(raster, 1);

        scheduler.submit(request(6, RenderPriority::Visible));
        let events = drain_all(&scheduler);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RenderEvent::Failed { page, error } => {
                assert_eq!(*page, PageIndex::new(6));
                assert!(matches!(error, crate::error::CoreError::Backend(_)));
            }
            other => panic!("期望 Failed，实际 {other:?}"),
        }
    }

    #[test]
    fn drop_shuts_down_workers_without_hanging() {
        let gate = Arc::new(Gate::default());
        gate.open();
        let raster = Arc::new(TestRaster::new(gate));
        let scheduler = WorkerScheduler::new(raster.clone(), 2);
        scheduler.submit(request(1, RenderPriority::Visible));
        wait_until("任务完成", Duration::from_secs(2), || scheduler.pending() == 0);

        let start = Instant::now();
        drop(scheduler);
        assert!(start.elapsed() < Duration::from_secs(2), "Drop 应join 工作线程而不是挂住");
    }

    #[test]
    fn cancel_all_stops_pending_work() {
        let gate = Arc::new(Gate::closed());
        let raster = Arc::new(TestRaster::new(gate.clone()));
        let scheduler = WorkerScheduler::new(raster.clone(), 1);

        scheduler.submit(request(9, RenderPriority::Visible)); // 占位（已进入渲染，无法中断）
        wait_until("占位任务已开始", Duration::from_secs(2), || raster.started_count() == 1);
        for page in 0..3 {
            scheduler.submit(request(page, RenderPriority::Visible));
        }

        scheduler.cancel_all();
        gate.open();
        drain_all(&scheduler);

        // 占位任务已在渲染中（PDFium 无法中断），它仍会完成；
        // 但排队中的 3 个任务必须一个都不渲染。
        let finished = raster.finished();
        for page in 0..3 {
            assert!(
                !finished.contains(&PageIndex::new(page)),
                "排队中的任务被取消后不应渲染，实际完成：{finished:?}"
            );
        }
    }
}
