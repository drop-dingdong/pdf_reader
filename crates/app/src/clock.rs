//! 可注入的时钟。
//!
//! ## 为什么需要它
//! UI 里有若干**行为性**的截止时间（缩放预览窗口 160ms、滚动静默 250ms、
//! 就绪兜底 400ms、高亮消退 0.7s）。它们若直接读 `Instant::now()`，回放器就无法确定性地
//! 验证"窗口期内不请求渲染""超时兜底"这类行为 —— 帧跑得再快，真实时间也不一定过去。
//!
//! 因此：
//! * **行为性判定**一律读 [`Clock::now`]（可注入 `manual` 时钟，由回放器按帧推进）；
//! * **耗时测量**（转换/上传/帧耗时）仍用 `Instant::now()` 的真实时间 —— 那是"测多久"，
//!   不能是假的。
//!
//! 默认时钟是 [`Clock::system`]，行为与直接读 `Instant::now()` 完全一致。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 时钟：真实时间，或可手动推进的测试时钟。
#[derive(Clone)]
pub struct Clock {
    start: Instant,
    offset: Arc<Mutex<Duration>>,
    manual: bool,
}

impl Clock {
    /// 真实时钟（默认）。
    pub fn system() -> Self {
        Self { start: Instant::now(), offset: Arc::new(Mutex::new(Duration::ZERO)), manual: false }
    }

    /// 手动时钟：`now()` 只在 [`Clock::advance`] 之后前进（从创建时刻起算）。
    pub fn manual() -> Self {
        Self { start: Instant::now(), offset: Arc::new(Mutex::new(Duration::ZERO)), manual: true }
    }

    /// 当前时刻。
    pub fn now(&self) -> Instant {
        if !self.manual {
            return Instant::now();
        }
        self.start + *self.offset.lock().expect("clock poisoned")
    }

    /// 推进手动时钟（对真实时钟是空操作）。
    pub fn advance(&self, delta: Duration) {
        if !self.manual {
            return;
        }
        let mut offset = self.offset.lock().expect("clock poisoned");
        *offset += delta;
    }

    /// 是否为手动时钟（诊断用）。
    pub fn is_manual(&self) -> bool {
        self.manual
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clock")
            .field("manual", &self.manual)
            .field("offset", &*self.offset.lock().expect("clock poisoned"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_only_moves_when_advanced() {
        let clock = Clock::manual();
        let start = clock.now();
        assert_eq!(clock.now(), start, "不推进就不动");
        clock.advance(Duration::from_millis(160));
        assert_eq!(clock.now(), start + Duration::from_millis(160));
        clock.advance(Duration::from_millis(240));
        assert_eq!(clock.now(), start + Duration::from_millis(400), "推进是累加的");
        assert!(clock.is_manual());
    }

    #[test]
    fn system_clock_tracks_real_time() {
        let clock = Clock::system();
        let before = clock.now();
        std::thread::sleep(Duration::from_millis(5));
        assert!(clock.now() > before, "真实时钟必须前进");
        clock.advance(Duration::from_secs(10));
        assert!(!clock.is_manual(), "真实时钟忽略 advance");
    }

    #[test]
    fn clones_share_the_same_offset() {
        let clock = Clock::manual();
        let clone = clock.clone();
        clock.advance(Duration::from_millis(50));
        assert_eq!(clone.now(), clock.now());
    }
}
