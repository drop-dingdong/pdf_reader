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
use crate::geometry::{Rect, Rotation, Size};
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

/// 瓦片坐标（列、行）：该页在**当前渲染波段网格**里的位置。
///
/// 网格本身由 [`crate::tiling::TileGrid`] 给出（在渲染比例下取一组共享整数边界，
/// 让每块边长接近 512px）。这里只保留下标作**身份**：它进缓存键，
/// 并与渲染比例一起唯一确定一块区域 —— 同一个渲染比例下网格是固定的，
/// 因此 `(scale_milli, rotation, tile)` 不会指向别处的像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileSpec {
    /// 列（自左向右，0 起）。
    pub col: u32,
    /// 行（自上向下，0 起）。
    pub row: u32,
}

impl TileSpec {
    /// 构造。
    pub const fn new(col: u32, row: u32) -> Self {
        Self { col, row }
    }
}

/// 何时分块：按"页的**渲染**像素面积"阈值决定，避免小页被碎片化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilingPolicy {
    /// 从不分块（整页一张位图）。
    Never,
    /// 页的渲染像素面积超过阈值时才分块。
    ///
    /// 判据用**渲染**尺寸而不是显示尺寸：真正分配位图的是渲染尺寸，而波段渲染比例
    /// 最多是显示比例的 1.6 倍（面积 2.56 倍），用显示尺寸判会让"整页位图"的实际
    /// 内存与耗时远超阈值（D-94）。
    ///
    /// 默认 4 MP：实测（真实 PDFium）一块 512×512 瓦片约 9ms，而整页渲染在 4 MP 以上
    /// 就要几十毫秒到几百毫秒、内存随面积线性增长（31 MP = 124 MB）。
    /// 阈值取 4 MP 是"整页一次画完更省"与"分块更省内存/更跟手"的平衡点（D-15）。
    AbovePixels(u32),
    /// 总是分块。
    Always,
}

impl Default for TilingPolicy {
    fn default() -> Self {
        Self::AbovePixels(4_000_000)
    }
}

impl TilingPolicy {
    /// 给定页的像素尺寸，是否需要分块（只看面积，忽略纹理单边上限）。
    pub fn requires_tiling(self, pixel_size: Size) -> bool {
        self.requires_tiling_bounded(pixel_size, u32::MAX)
    }

    /// 同 [`TilingPolicy::requires_tiling`]，但**先看硬约束**：单边超过 `max_side_px` 必须分块。
    ///
    /// 即使策略是 [`TilingPolicy::Never`]，超限也必须分块 —— 那不是"取舍"，
    /// 而是"这张纹理根本创建不出来"（D-108）。
    pub fn requires_tiling_bounded(self, pixel_size: Size, max_side_px: u32) -> bool {
        let width = pixel_size.width.max(0.0);
        let height = pixel_size.height.max(0.0);
        let longest = width.max(height);
        // 0 = 不限制（与 `DocumentSession::max_texture_side` 的约定一致）
        if max_side_px > 0 && longest > max_side_px as f32 {
            return true;
        }
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::AbovePixels(limit) => width * height > limit as f32,
        }
    }
}

/// 缓存键。
///
/// 由"页 + **渲染比例** + 旋转 + 瓦片坐标"构成：这些量变化了就必须重新渲染；其余参数
/// （优先级、取消令牌）不影响像素结果，因此不进键。
/// **瓦片坐标必须进键**，否则不同瓦片会互相覆盖（表现为"页面上出现了别处的图"）。
///
/// ## `scale_milli` 是**渲染比例**，不是显示比例（D-94）
/// 渲染比例来自波段表（[`crate::tiling::render_scale_for`]），于是**同一波段内的任意缩放
/// 得到同一个键** —— 这正是验收判据"同波段内改缩放不应产生新的 `[瓦片]` 行"的依据。
/// 反过来，这里**不再**对比例做 1/8 量化：量化会让键里的比例与实际用于渲染的比例不一致，
/// 预览拼合时按它重建网格就会差几像素（D-94 之前的做法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    /// 页号。
    pub page: PageIndex,
    /// 渲染比例，以千分之一为单位存储（`f32` 不能做 `Hash`/`Eq`）。
    pub scale_milli: u32,
    /// 旋转。
    pub rotation: Rotation,
    /// 瓦片坐标；`None` 表示整页。
    pub tile: Option<TileSpec>,
}

impl RenderKey {
    /// 整页渲染的键。
    pub fn new(page: PageIndex, scale: Scale, rotation: Rotation) -> Self {
        Self { page, scale_milli: milli(scale), rotation, tile: None }
    }

    /// 单个瓦片的键。
    pub fn tiled(page: PageIndex, scale: Scale, rotation: Rotation, tile: TileSpec) -> Self {
        Self { page, scale_milli: milli(scale), rotation, tile: Some(tile) }
    }

    /// 还原比例（= 渲染比例）。
    pub fn scale(self) -> Scale {
        Scale(self.scale_milli as f32 / 1000.0)
    }
}

/// 比例 → 千分之一整数。
///
/// 只做 `round`，不做"量化到 1/8 档"（D-94）：键里的比例要能被用来**重建渲染网格**
/// （预览拼合），必须与真正渲染时用的比例一致。同波段复用由波段表本身保证
/// （`render_scale_for` 在波段内返回同一个值），不依赖量化。
fn milli(scale: Scale) -> u32 {
    (scale.0 * 1000.0).round().clamp(1.0, u32::MAX as f32) as u32
}

/// 一次渲染要覆盖的**页面区域**：块矩形 + 期望像素尺寸。
///
/// ## 为什么是"页面空间矩形 + 期望像素尺寸"
/// 块怎么切是**渲染无关**的几何问题（由 [`crate::tiling::TileGrid`] 决定），后端不该知道
/// 我们的网格长什么样；它只需要"把这块页面区域画成这么大"。
///
/// ## 期望尺寸是**契约**，不是建议
/// 后端必须精确产出 `pixel_size` 的位图。若它的几何推导（旋转 / 比例 / 取整）与上层不一致，
/// 必须返回 [`CoreError::Internal`] 而**不得静默拉伸** —— 静默拉伸正是
/// "同一行文字在块接缝处错位"那类缺陷的温床（D-84/D-86）：错就错在"看起来有图，其实比例不对"。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderRegion {
    /// 区域在 PDF **页面空间**中的矩形（左下原点、未旋转 —— 后端自己按 `rotation` 旋转）。
    pub page_rect: Rect,
    /// 期望输出像素尺寸 `(宽, 高)`。
    pub pixel_size: (u32, u32),
}

/// 一次渲染请求。
#[derive(Debug, Clone)]
pub struct RenderRequest {
    /// 要渲染的页。
    pub page: PageIndex,
    /// **渲染比例**：`display_scale` 所在波段的渲染比例（≥ 显示比例）。
    ///
    /// 注意它不是 UI 上的显示比例：后者由布局给出，波段内可以连续变化；
    /// 渲染只认这个离散值，因此波段内反复缩放不会产生新请求。贴图时用
    /// `u = 显示比例 / 本比例` 换算每块的显示尺寸（见 `TileGrid::display_rect`）。
    pub scale: Scale,
    /// 旋转（真机上是"页面自带 `/Rotate` ⊕ 用户旋转"的结果）。
    pub rotation: Rotation,
    /// 瓦片坐标；`None` 表示整页。
    pub tile: Option<TileSpec>,
    /// 要渲染的页面区域；**瓦片请求必须携带**（整页请求为 `None`，尺寸由后端推导）。
    pub region: Option<RenderRegion>,
    /// 优先级。
    pub priority: RenderPriority,
    /// 取消令牌。
    pub cancel: CancelToken,
}

impl RenderRequest {
    /// 整页渲染请求（`Visible` 优先级）。
    pub fn new(page: PageIndex, scale: Scale, rotation: Rotation) -> Self {
        Self {
            page,
            scale,
            rotation,
            tile: None,
            region: None,
            priority: RenderPriority::Visible,
            cancel: CancelToken::new(),
        }
    }

    /// 单瓦片渲染请求（`Visible` 优先级）。
    ///
    /// `region` 是该块的页面空间矩形与期望像素尺寸（由
    /// [`crate::tiling::TileGrid::tile_page_rect`] 与 [`crate::tiling::TileGrid::tile_px`]
    /// 给出）。**必填**：没有它后端无从知道该渲染哪块区域。
    pub fn tiled(
        page: PageIndex,
        scale: Scale,
        rotation: Rotation,
        tile: TileSpec,
        region: RenderRegion,
    ) -> Self {
        Self { tile: Some(tile), region: Some(region), ..Self::new(page, scale, rotation) }
    }

    /// 设置优先级。
    pub fn with_priority(mut self, priority: RenderPriority) -> Self {
        self.priority = priority;
        self
    }

    /// 对应的缓存键。
    pub fn key(&self) -> RenderKey {
        RenderKey {
            page: self.page,
            scale_milli: milli(self.scale),
            rotation: self.rotation,
            tile: self.tile,
        }
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
/// * **瓦片请求**（`request.tile = Some`）只渲染 [`RenderRequest::region`] 描述的页面区域，
///   **不得先整页渲染再裁剪**（否则分块就失去了省内存/省时间的意义）。
/// * 输出位图尺寸**必须等于** `region.pixel_size`；推导不一致时返回
///   [`CoreError::Internal`]，**不得静默拉伸**（D-95）。
pub trait PageRaster: Send + Sync {
    /// 渲染一页。整页请求的位图尺寸应等于 `页尺寸(旋转后) × scale`（允许 ±1 像素取整误差）；
    /// 瓦片请求则是该块的像素矩形尺寸。
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

    /// D-108：面积没超，但单边超限 → 仍然必须分块（与策略无关）。
    #[test]
    fn a_long_page_tiles_even_below_the_area_threshold() {
        let strip = Size::new(100.0, 30_000.0); // 3 MP，面积远低于 4 MP
        assert!(!TilingPolicy::AbovePixels(4_000_000).requires_tiling(strip));
        assert!(TilingPolicy::AbovePixels(4_000_000).requires_tiling_bounded(strip, 16_384));
        // 即使策略说"永不分块"，超过纹理上限也得服从硬约束
        assert!(TilingPolicy::Never.requires_tiling_bounded(strip, 16_384));
        // 不限制时保持原行为
        assert!(!TilingPolicy::Never.requires_tiling_bounded(strip, 0));
        assert!(!TilingPolicy::AbovePixels(4_000_000).requires_tiling_bounded(strip, 0));
    }

    use super::*;

    #[test]
    fn render_key_tracks_scale_and_rotation_exactly() {
        let page = PageIndex::new(3);
        let k1 = RenderKey::new(page, Scale(1.01), Rotation::None);
        let k2 = RenderKey::new(page, Scale(1.02), Rotation::None);
        let k4 = RenderKey::new(page, Scale(1.01), Rotation::Clockwise90);
        assert_ne!(
            k1, k2,
            "键里的比例不再量化（D-94）：它必须等于真正用于渲染的比例，否则重建网格做预览拼接时会差几像素"
        );
        assert_eq!(k1, RenderKey::new(page, Scale(1.01), Rotation::None));
        assert_ne!(k1, k4, "旋转不同必须换键，否则会出现画面不旋转的 bug");
        // 同一波段比例 → 同一个键（"同波段改缩放不重渲染"的依据）
        let band = crate::tiling::render_scale_for(1.3, crate::tiling::DEFAULT_OVERSCAN);
        assert_eq!(
            RenderKey::new(page, Scale(band), Rotation::None),
            RenderKey::new(page, Scale(band), Rotation::None)
        );
    }

    #[test]
    fn key_milli_round_trips_the_render_scale() {
        // 预览拼合要用 `scale_milli` 反推渲染比例再重建网格：必须能还原
        for render in [1.0f32, 1.6, 2.56, 4.096, 0.625] {
            let key = RenderKey::new(PageIndex::FIRST, Scale(render), Rotation::None);
            let restored = key.scale().0;
            assert!(
                (restored - render).abs() < 1e-3,
                "反推比例 {restored} 应接近 {render}"
            );
        }
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

    #[test]
    fn tiling_policy_thresholds() {
        let policy = TilingPolicy::default();
        assert_eq!(policy, TilingPolicy::AbovePixels(4_000_000));
        // zoom=2 的 612×792 → 1224×1584 ≈ 1.94 MP < 4 MP → 整页（一次渲染更划算）
        assert!(!policy.requires_tiling(Size::new(1224.0, 1584.0)));
        // zoom=3 → 1836×2376 ≈ 4.36 MP > 4 MP → 分块（实测每块 ~9ms，比整页 17MB 快得多）
        assert!(policy.requires_tiling(Size::new(1836.0, 2376.0)));
        // zoom=8 → 4896×6336 ≈ 31 MP → 分块（整页要 124 MB）
        assert!(policy.requires_tiling(Size::new(4896.0, 6336.0)));
        assert!(!TilingPolicy::Never.requires_tiling(Size::new(9999.0, 9999.0)));
        assert!(TilingPolicy::Always.requires_tiling(Size::new(1.0, 1.0)));
    }

    /// 契约（D-95）：块请求必须带区域，整页请求不得带区域。
    #[test]
    fn tiled_request_carries_a_region_and_whole_page_does_not() {
        let page = PageIndex::new(0);
        let scale = Scale::from_zoom(2.0);
        let region = RenderRegion {
            page_rect: Rect::new(0.0, 396.0, 306.0, 396.0),
            pixel_size: (612, 792),
        };
        let request =
            RenderRequest::tiled(page, scale, Rotation::None, TileSpec::new(1, 0), region);
        assert_eq!(request.tile, Some(TileSpec::new(1, 0)));
        assert_eq!(request.region, Some(region));
        assert_eq!(request.key().tile, Some(TileSpec::new(1, 0)));

        let whole = RenderRequest::new(page, scale, Rotation::None);
        assert_eq!(whole.tile, None);
        assert_eq!(whole.region, None, "整页请求由后端推导尺寸，不带块区域");
    }

    #[test]
    fn tile_keys_do_not_collide_with_whole_page_keys() {
        let page = PageIndex::new(0);
        let scale = Scale::from_zoom(2.0);
        let whole = RenderKey::new(page, scale, Rotation::None);
        let tile_a = RenderKey::tiled(page, scale, Rotation::None, TileSpec::new(0, 0));
        let tile_b = RenderKey::tiled(page, scale, Rotation::None, TileSpec::new(1, 0));

        assert_ne!(whole, tile_a, "整页键与瓦片键必须不同");
        assert_ne!(tile_a, tile_b, "不同瓦片必须有不同的键");
        let region = RenderRegion {
            page_rect: Rect::new(0.0, 0.0, 612.0, 792.0),
            pixel_size: (612, 792),
        };
        assert_eq!(
            RenderRequest::tiled(page, scale, Rotation::None, TileSpec::new(1, 0), region).key(),
            tile_b
        );
        assert_eq!(RenderRequest::new(page, scale, Rotation::None).key(), whole);
    }
}
