//! 渲染管线：缩放、取消、缓存、调度。
//!
//! 这一层解决的核心矛盾是：**PDFium 不是线程安全的，而 UI 每帧都要拿到位图**。
//! 解法是把"渲染"切成三段，各自可替换：
//!
//! ```text
//!  UI 每帧 ──submit(RenderRequest)──▶ RenderScheduler ──▶ PageRaster（PDFium，单线程串行）
//!     ▲                                     │
//!     └────drain_events(RenderEvent)────────┘
//!                     │
//!                RenderCache（命中即不渲染）
//! ```
//!
//! * [`PageRaster`] 只负责"给定页与缩放，产出位图"，实现者无需关心并发与缓存；
//! * [`RenderScheduler`] 负责排队、优先级、取消、把结果以事件形式交回 UI；
//! * [`RenderCache`] 负责按 [`RenderKey`] 缓存位图，可换成 LRU / 磁盘 / 内存受限版本。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{CoreError, Result};
use crate::geometry::{Rotation, Size};
use crate::ids::{JobId, PageIndex};
use crate::image::RasterImage;

/// 渲染缩放：**设备像素 / point**。
///
/// 之所以不用"百分比"或"DPI"直接当主类型：PDF 页面坐标本来就是 point，用 point 做基准
/// 可以让 `像素 = point × scale` 一步到位，避免 96/72 之类的杂散换算因子散落各处。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Scale(pub f32);

impl Scale {
    /// 1:1（72 DPI）。
    pub const IDENTITY: Self = Self(1.0);

    /// 由缩放倍数构造（`zoom = 1.0` 即 100%）。
    pub fn from_zoom(zoom: f32) -> Self {
        Self(zoom)
    }

    /// 由 DPI 构造（96 DPI 屏幕上的 100% 约为 1.333）。
    pub fn from_dpi(dpi: f32) -> Self {
        Self(dpi / 72.0)
    }

    /// 缩放倍数。
    pub fn zoom(self) -> f32 {
        self.0
    }

    /// 等效 DPI。
    pub fn dpi(self) -> f32 {
        self.0 * 72.0
    }

    /// 是否为正的有限值。
    pub fn is_valid(self) -> bool {
        self.0.is_finite() && self.0 > 0.0
    }

    /// 量化到 1/8 步长，用作缓存键。
    ///
    /// 必要性：缩放是连续值，若不量化，用户每滚动一次滚轮就会产生一批全新的缓存条目，
    /// 缓存命中率会跌到接近 0；量化后 12.5% 以内的缩放共用同一张位图，再由 GPU 拉伸补足。
    pub fn quantize(self) -> Self {
        const STEP: f32 = 0.125;
        Self((self.0 / STEP).round() * STEP)
    }
}

/// 渲染优先级。调度器按 [`RenderPriority::rank`] 从小到大出队。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RenderPriority {
    /// 用户当前操作直接需要（跳页后当前页）。
    Interactive,
    /// 视口内可见页。
    Visible,
    /// 视口外预取（滚一屏之内）。
    Prefetch,
    /// 后台任务（缩略图、全文搜索用的低精度渲染）。
    Background,
}

impl RenderPriority {
    /// 排序权重（越小越优先）。
    pub const fn rank(self) -> u8 {
        match self {
            Self::Interactive => 0,
            Self::Visible => 1,
            Self::Prefetch => 2,
            Self::Background => 3,
        }
    }
}

/// 取消令牌。
///
/// 重要契约：**PDFium 无法中断已经在执行的渲染**，因此取消只在任务出队前生效；
/// 实现者应在开始渲染前与渲染结束后各检查一次。真正让"快速滚动不卡"的手段是
/// 优先级队列 + 缩放量化 + 位图缓存，而不是指望能打断 PDFium。
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// 新建未取消的令牌。
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// 标记取消。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// 是否已取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// 缓存键。
///
/// 由"页 + 量化缩放 + 旋转"三元组构成：这三个量变化了就必须重新渲染，其余参数（优先级、
/// 取消令牌）不影响像素结果，因此不进键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    /// 页号。
    pub page: PageIndex,
    /// 量化后的缩放，以千分之一为单位存储（`f32` 不能做 `Hash`/`Eq`）。
    pub scale_milli: u32,
    /// 旋转。
    pub rotation: Rotation,
}

impl RenderKey {
    /// 构造（内部会量化缩放）。
    pub fn new(page: PageIndex, scale: Scale, rotation: Rotation) -> Self {
        let quantized = scale.quantize().0;
        Self { page, scale_milli: (quantized * 1000.0).round().max(1.0) as u32, rotation }
    }

    /// 还原缩放值。
    pub fn scale(self) -> Scale {
        Scale(self.scale_milli as f32 / 1000.0)
    }
}

/// 一次渲染请求。
#[derive(Debug, Clone)]
pub struct RenderRequest {
    /// 要渲染的页。
    pub page: PageIndex,
    /// 缩放。
    pub scale: Scale,
    /// 旋转（真机上是"页面自带 `/Rotate` ⊕ 用户旋转"的结果）。
    pub rotation: Rotation,
    /// 优先级。
    pub priority: RenderPriority,
    /// 取消令牌。
    pub cancel: CancelToken,
}

impl RenderRequest {
    /// 构造一个 `Visible` 优先级的请求。
    pub fn new(page: PageIndex, scale: Scale, rotation: Rotation) -> Self {
        Self {
            page,
            scale,
            rotation,
            priority: RenderPriority::Visible,
            cancel: CancelToken::new(),
        }
    }

    /// 设置优先级。
    pub fn with_priority(mut self, priority: RenderPriority) -> Self {
        self.priority = priority;
        self
    }

    /// 对应的缓存键。
    pub fn key(&self) -> RenderKey {
        RenderKey::new(self.page, self.scale, self.rotation)
    }
}

/// **光栅化能力**：把一页按请求渲染成位图。
///
/// ## 这个 trait 存在的目的
/// 把"PDF 后端"压缩成**一个方法**，从而：
/// * PDFium 后端只需实现它，不必关心缓存、优先级、线程（那些在上层）；
/// * 测试可以塞一个"画纯色方块"的假实现，完全不碰真实 PDF；
/// * 未来换 hayro / 自研渲染器时，改动范围仅限于此 trait 的一个实现。
///
/// ## 实现者义务
/// * 不得修改自身可见状态以外的全局状态；`&self` 意味着实现必须自己处理内部可变性
///   （PDFium 后端典型做法是用 `Mutex` 包住文档句柄）。
/// * 必须支持并发调用（`Send + Sync`），并发上限由
///   [`crate::document::Capabilities::max_concurrency`] 声明，调度器会遵守；
///   **PDFium 后端应声明 1**。
/// * 渲染前若 [`CancelToken::is_cancelled`] 为真，应立刻返回 [`CoreError::Cancelled`]。
pub trait PageRaster: Send + Sync {
    /// 渲染一页。返回的位图尺寸应等于 `页尺寸(旋转后) × scale`（允许 ±1 像素取整误差）。
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>>;
}

/// 缓存统计（用于状态栏展示与调优）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 当前缓存条目数。
    pub entries: usize,
    /// 估算占用字节数。
    pub bytes: u64,
    /// 命中次数。
    pub hits: u64,
    /// 未命中次数。
    pub misses: u64,
}

/// **位图缓存**：`RenderKey` → 位图。
///
/// ## 这个 trait 存在的目的
/// 把"缓存策略"从渲染与 UI 中独立出来，使得：
/// * MVP 可以用一个几十行的 `HashMap` 实现先跑通；
/// * 之后换成 LRU / 按内存上限淘汰 / 落磁盘的实现，**不改动任何调用方**；
/// * 单测里可以用一个永远不命中的缓存来验证调度逻辑。
///
/// ## 实现者义务
/// * `get` 必须是 O(1) 级别且不阻塞渲染线程太久（会被 UI 线程每帧调用）。
/// * 允许返回 `None` 表示未命中；不允许返回尺寸/格式与键不匹配的位图。
pub trait RenderCache: Send + Sync {
    /// 查询；未命中返回 `None`。
    fn get(&self, key: &RenderKey) -> Option<Arc<RasterImage>>;

    /// 写入（同键覆盖）。
    fn insert(&self, key: RenderKey, image: Arc<RasterImage>);

    /// 丢弃某一页的所有缩放版本（页内容变化、文档重载时调用）。
    fn remove_page(&self, page: PageIndex);

    /// 清空。
    fn clear(&self);

    /// 统计信息；默认返回空统计，实现可按需覆盖。
    fn stats(&self) -> CacheStats {
        CacheStats::default()
    }
}

/// 渲染完成后回传给 UI 的事件。
#[derive(Debug, Clone)]
pub enum RenderEvent {
    /// 渲染成功。UI 通常只需记录（位图已由调度器写入缓存），必要时直接上传纹理。
    Rendered {
        /// 缓存键。
        key: RenderKey,
        /// 位图。
        image: Arc<RasterImage>,
    },
    /// 渲染失败（损坏页、内存不足等）。
    Failed {
        /// 页号。
        page: PageIndex,
        /// 原因。
        error: CoreError,
    },
    /// 任务被取消（UI 通常可忽略）。
    Cancelled {
        /// 页号。
        page: PageIndex,
    },
}

/// **渲染调度**：UI 面向的唯一渲染入口。
///
/// ## 这个 trait 存在的目的
/// 让 UI 完全不必知道"谁在渲染、有几个线程、PDFium 是否线程安全"：
/// * `submit` 是**非阻塞**的，UI 每帧都可以放心调用；
/// * 结果通过 `drain_events` 轮询取回，与 egui 的"每帧拉取"模型天然契合
///   （不需要把 UI 状态塞进回调里，也不会出现回调跨帧持有 UI 引用的问题）；
/// * 真实实现用后台线程 + 优先级队列，测试实现可以**直接同步渲染**，
///   于是「UI → 会话 → 调度 → 后端 → 缓存 → UI」整条链路可以在单测里跑通。
///
/// ## 实现者义务
/// * `submit` 必须立即返回，不得同步等待渲染完成（同步实现仅用于测试）。
/// * 必须自行去重：同一 [`RenderKey`] 已在队列或已在飞行中时，重复提交应被忽略或合并。
/// * 必须遵守 [`crate::document::Capabilities::max_concurrency`]，尤其对 PDFium 只能是 1。
pub trait RenderScheduler: Send + Sync {
    /// 提交任务；返回任务号用于取消。若因去重而未真正入队，返回的任务号应立即可被忽略。
    fn submit(&self, request: RenderRequest) -> JobId;

    /// 取消指定任务。
    fn cancel(&self, job: JobId);

    /// 取消某一页的所有任务（该页滚出视口时调用）。
    fn cancel_page(&self, page: PageIndex);

    /// 取消全部任务（关闭文档、切换文档时调用）。
    fn cancel_all(&self);

    /// 取出至多 `max` 条已完成事件（非阻塞）。UI 每帧调用一次。
    fn drain_events(&self, max: usize) -> Vec<RenderEvent>;

    /// 排队中 + 正在执行的任务数（用于状态栏与背压判断）。
    fn pending(&self) -> usize;
}

/// 便捷函数：按目标像素宽度求缩放。
///
/// 缩略图不需要单独的 trait：页尺寸来自 [`crate::document::Document`]，
/// 于是"目标宽 160px"可以换算成一个普通 [`Scale`] 交给 [`PageRaster::render`]，
/// 复用同一条渲染与缓存路径（这也意味着缩略图与主视图共享缓存，不会重复渲染同一张图）。
pub fn scale_for_target_width(page_size: Size, rotation: Rotation, target_width: f32) -> Scale {
    let displayed = page_size.rotated(rotation);
    if displayed.width <= 0.0 {
        return Scale::IDENTITY;
    }
    Scale(target_width / displayed.width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_absorbs_small_zoom_jitter() {
        let a = Scale(1.00).quantize();
        let b = Scale(1.05).quantize();
        let c = Scale(1.30).quantize();
        assert_eq!(a, b, "5% 的抖动应落在同一档");
        assert_ne!(a, c, "30% 的差异应换档");
    }

    #[test]
    fn render_key_ignores_priority_but_not_scale_bucket() {
        let page = PageIndex::new(3);
        let k1 = RenderKey::new(page, Scale(1.01), Rotation::None);
        let k2 = RenderKey::new(page, Scale(1.02), Rotation::None);
        let k3 = RenderKey::new(page, Scale(1.30), Rotation::None);
        let k4 = RenderKey::new(page, Scale(1.01), Rotation::Clockwise90);
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k1, k4, "旋转不同必须换键，否则会出现画面不旋转的 bug");
    }

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn scale_helpers() {
        assert!((Scale::from_dpi(96.0).zoom() - 1.3333).abs() < 1e-3);
        assert!((Scale::from_zoom(1.5).dpi() - 108.0).abs() < 1e-3);
        assert!(!Scale(0.0).is_valid());
        assert_eq!(
            scale_for_target_width(Size::new(600.0, 800.0), Rotation::None, 120.0),
            Scale(0.2)
        );
        assert_eq!(
            scale_for_target_width(Size::new(600.0, 800.0), Rotation::Clockwise90, 120.0),
            Scale(0.15),
            "旋转 90° 后宽高互换，分母应为原高度"
        );
    }
}
