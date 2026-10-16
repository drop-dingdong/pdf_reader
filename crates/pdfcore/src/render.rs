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
use crate::geometry::{Point, Rect, Rotation, Size};
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

/// 瓦片边长（设备像素）。
///
/// 做成常量而不是配置：它是缓存键的一部分，变动会导致全部瓦片失效，
/// 没有"运行期可调"的必要。
pub const TILE_SIZE: u16 = 512;

/// 瓦片坐标（列、行）。网格由该页的**像素尺寸**决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileSpec {
    /// 列（自左向右，0 起）。
    pub col: u16,
    /// 行（自上向下，0 起）。
    pub row: u16,
}

impl TileSpec {
    /// 构造。
    pub const fn new(col: u16, row: u16) -> Self {
        Self { col, row }
    }

    /// 页在给定像素尺寸下的瓦片网格（列数、行数）。尺寸为 0 时返回 `(0, 0)`。
    pub fn grid_for(displayed_size: Size) -> (u16, u16) {
        fn count(extent: f32) -> u16 {
            if extent <= 0.0 {
                0
            } else {
                (extent / TILE_SIZE as f32).ceil().min(u16::MAX as f32) as u16
            }
        }
        (count(displayed_size.width), count(displayed_size.height))
    }

    /// 该瓦片在"旋转后页位图"中的像素矩形（整数边界）。
    ///
    /// 拼接时以它为准 —— 既保证不重叠，也不会因取整产生 1px 缝隙。
    pub fn pixel_rect(self, displayed_size: Size) -> Rect {
        let x = self.col as f32 * TILE_SIZE as f32;
        let y = self.row as f32 * TILE_SIZE as f32;
        Rect::new(
            x,
            y,
            (displayed_size.width - x).clamp(0.0, TILE_SIZE as f32),
            (displayed_size.height - y).clamp(0.0, TILE_SIZE as f32),
        )
    }

    /// 该瓦片在 **PDF 页面空间**中的裁剪矩形。
    ///
    /// 渲染请求携带它：后端应"按这个矩形直接渲染该区域"，**不得先整页渲染再裁剪**，
    /// 否则分块就失去了省内存的意义。
    pub fn page_rect(self, page_size: Size, rotation: Rotation, scale: Scale) -> Rect {
        let displayed = page_size.rotated(rotation).scale(scale.0);
        let pixels = self.pixel_rect(displayed);
        if pixels.is_empty() || !scale.is_valid() {
            return Rect::ZERO;
        }
        // 像素矩形四角 → 显示坐标（÷scale）→ PDF 空间（unmap）→ 外接矩形
        let corners = [
            Point::new(pixels.x / scale.0, pixels.y / scale.0),
            Point::new(pixels.right() / scale.0, pixels.y / scale.0),
            Point::new(pixels.x / scale.0, pixels.bottom() / scale.0),
            Point::new(pixels.right() / scale.0, pixels.bottom() / scale.0),
        ];
        let mut iter = corners.iter().map(|point| rotation.unmap_point(*point, page_size));
        let first = iter.next().unwrap_or(Point::ORIGIN);
        iter.fold(Rect::from_min_max(first, first), |acc, point| {
            acc.union(Rect::from_min_max(point, point))
        })
    }
}

/// 视口（以"该页位图中的像素矩形"表示）覆盖到的瓦片范围。
///
/// 返回左上与右下两个瓦片（含端点），已按 `grid` 裁剪；完全不覆盖时返回 `None`。
/// 会话用它把"渲染哪些瓦片"限制在视口内，而不是整页 130 块。
pub fn tiles_covering(visible_px: Rect, grid: (u16, u16)) -> Option<(TileSpec, TileSpec)> {
    if grid.0 == 0 || grid.1 == 0 || visible_px.is_empty() {
        return None;
    }
    // 右/下边界恰好落在下一块起点时，不应把那一块算进来
    const EPSILON: f32 = 0.001;
    let tile = TILE_SIZE as f32;
    let max_col = grid.0 as u32 - 1;
    let max_row = grid.1 as u32 - 1;

    let col0 = (visible_px.x / tile).floor().max(0.0) as u32;
    let row0 = (visible_px.y / tile).floor().max(0.0) as u32;
    if col0 > max_col || row0 > max_row {
        return None;
    }
    let col1 = (((visible_px.right() - EPSILON) / tile).floor().max(0.0) as u32).min(max_col);
    let row1 = (((visible_px.bottom() - EPSILON) / tile).floor().max(0.0) as u32).min(max_row);
    if col0 > col1 || row0 > row1 {
        return None;
    }
    Some((TileSpec::new(col0 as u16, row0 as u16), TileSpec::new(col1 as u16, row1 as u16)))
}

/// 何时分块：按"页的像素面积"阈值决定，避免小页被碎片化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilingPolicy {
    /// 从不分块（整页一张位图）。
    Never,
    /// 页的像素面积超过阈值时才分块。
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
    /// 给定页的像素尺寸，是否需要分块。
    pub fn requires_tiling(self, pixel_size: Size) -> bool {
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::AbovePixels(limit) => {
                pixel_size.width.max(0.0) * pixel_size.height.max(0.0) > limit as f32
            }
        }
    }
}

/// 缓存键。
///
/// 由"页 + 量化缩放 + 旋转 + 瓦片坐标"构成：这些量变化了就必须重新渲染；其余参数
/// （优先级、取消令牌）不影响像素结果，因此不进键。
/// **瓦片坐标必须进键**，否则不同瓦片会互相覆盖（表现为"页面上出现了别处的图"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    /// 页号。
    pub page: PageIndex,
    /// 量化后的缩放，以千分之一为单位存储（`f32` 不能做 `Hash`/`Eq`）。
    pub scale_milli: u32,
    /// 旋转。
    pub rotation: Rotation,
    /// 瓦片坐标；`None` 表示整页。
    pub tile: Option<TileSpec>,
}

impl RenderKey {
    /// 整页渲染的键（内部会量化缩放）。
    pub fn new(page: PageIndex, scale: Scale, rotation: Rotation) -> Self {
        Self { page, scale_milli: quantize_milli(scale), rotation, tile: None }
    }

    /// 单个瓦片的键（内部会量化缩放）。
    pub fn tiled(page: PageIndex, scale: Scale, rotation: Rotation, tile: TileSpec) -> Self {
        Self { page, scale_milli: quantize_milli(scale), rotation, tile: Some(tile) }
    }

    /// 还原量化后的缩放值。
    pub fn scale(self) -> Scale {
        Scale(self.scale_milli as f32 / 1000.0)
    }
}

/// 把缩放量化并转成千分之一的整数。
fn quantize_milli(scale: Scale) -> u32 {
    (scale.quantize().0 * 1000.0).round().max(1.0) as u32
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
    /// 瓦片坐标；`None` 表示整页。
    pub tile: Option<TileSpec>,
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
            priority: RenderPriority::Visible,
            cancel: CancelToken::new(),
        }
    }

    /// 单瓦片渲染请求（`Visible` 优先级）。
    pub fn tiled(page: PageIndex, scale: Scale, rotation: Rotation, tile: TileSpec) -> Self {
        Self { tile: Some(tile), ..Self::new(page, scale, rotation) }
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
            scale_milli: quantize_milli(self.scale),
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

    // ---------------------------------------------------------------- D-02 分块

    #[test]
    fn tile_grid_matches_documented_examples() {
        assert_eq!(TileSpec::grid_for(Size::new(612.0, 792.0)), (2, 2));
        // data-flow 场景 2b：612×792pt 在 zoom=8 时为 4896×6336
        assert_eq!(TileSpec::grid_for(Size::new(4896.0, 6336.0)), (10, 13));
        assert_eq!(TileSpec::grid_for(Size::ZERO), (0, 0));
        assert_eq!(TileSpec::grid_for(Size::new(512.0, 512.0)), (1, 1));
        assert_eq!(TileSpec::grid_for(Size::new(513.0, 1.0)), (2, 1));
    }

    #[test]
    fn tile_pixel_rects_tile_the_page_without_gap_or_overlap() {
        let displayed = Size::new(1200.0, 1100.0);
        let (cols, rows) = TileSpec::grid_for(displayed);
        assert_eq!((cols, rows), (3, 3));

        let mut area = 0.0;
        for row in 0..rows {
            for col in 0..cols {
                let rect = TileSpec::new(col, row).pixel_rect(displayed);
                assert!(rect.width > 0.0 && rect.height > 0.0);
                assert!(rect.width <= TILE_SIZE as f32 && rect.height <= TILE_SIZE as f32);
                assert!(rect.right() <= displayed.width + 1e-3);
                assert!(rect.bottom() <= displayed.height + 1e-3);
                area += rect.width * rect.height;
            }
        }
        assert!(
            (area - displayed.area()).abs() < 1.0,
            "瓦片面积之和必须等于页面面积（既不重叠也不留缝），实际 {area}"
        );

        // 最后一列/最后一行不足一整块
        assert_eq!(TileSpec::new(2, 0).pixel_rect(displayed).width, 1200.0 - 1024.0);
        assert_eq!(TileSpec::new(0, 2).pixel_rect(displayed).height, 1100.0 - 1024.0);
    }

    #[test]
    fn tile_page_rect_is_in_pdf_space_and_rotation_aware() {
        let page_size = Size::new(612.0, 792.0);

        // 未旋转：位图左上角那块瓦片对应 PDF 空间的**上部**（PDF y 向上）
        let rect = TileSpec::new(0, 0).page_rect(page_size, Rotation::None, Scale::IDENTITY);
        assert!((rect.x - 0.0).abs() < 1e-3);
        assert!((rect.y - (792.0 - 512.0)).abs() < 1e-3);
        assert!((rect.width - 512.0).abs() < 1e-3);
        assert!((rect.height - 512.0).abs() < 1e-3);

        // scale = 2 时，同一瓦片只覆盖页面上四分之一大小的区域
        let half = TileSpec::new(0, 0).page_rect(page_size, Rotation::None, Scale::from_zoom(2.0));
        assert!((half.width - 256.0).abs() < 1e-3);
        assert!((half.height - 256.0).abs() < 1e-3);

        // 旋转 90°：显示尺寸变为 792×612，网格随之变化，且结果必须落在页面内
        assert_eq!(TileSpec::grid_for(page_size.rotated(Rotation::Clockwise90)), (2, 2));
        let rotated = TileSpec::new(0, 0).page_rect(page_size, Rotation::Clockwise90, Scale::IDENTITY);
        assert!(rotated.right() <= page_size.width + 1e-3);
        assert!(rotated.bottom() <= page_size.height + 1e-3);
        assert!((rotated.width - 512.0).abs() < 1e-3);
    }

    #[test]
    fn tiles_covering_restricts_to_the_viewport() {
        let grid = (10u16, 13u16);

        // 视口只覆盖左上 900×1000 px → 2×2 块，而不是整页 130 块
        let (first, last) = tiles_covering(Rect::new(0.0, 0.0, 900.0, 1000.0), grid).unwrap();
        assert_eq!((first.col, first.row), (0, 0));
        assert_eq!((last.col, last.row), (1, 1));

        // 边界恰好落在瓦片起点时，不应多算一块
        let (first, last) = tiles_covering(Rect::new(512.0, 512.0, 512.0, 512.0), grid).unwrap();
        assert_eq!((first.col, first.row), (1, 1));
        assert_eq!((last.col, last.row), (1, 1));

        // 完全在页外 / 零尺寸 / 零网格 → None
        assert!(tiles_covering(Rect::new(9000.0, 9000.0, 10.0, 10.0), grid).is_none());
        assert!(tiles_covering(Rect::ZERO, grid).is_none());
        assert!(tiles_covering(Rect::new(0.0, 0.0, 10.0, 10.0), (0, 0)).is_none());
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

    #[test]
    fn tile_keys_do_not_collide_with_whole_page_keys() {
        let page = PageIndex::new(0);
        let scale = Scale::from_zoom(2.0);
        let whole = RenderKey::new(page, scale, Rotation::None);
        let tile_a = RenderKey::tiled(page, scale, Rotation::None, TileSpec::new(0, 0));
        let tile_b = RenderKey::tiled(page, scale, Rotation::None, TileSpec::new(1, 0));

        assert_ne!(whole, tile_a, "整页键与瓦片键必须不同");
        assert_ne!(tile_a, tile_b, "不同瓦片必须有不同的键");
        assert_eq!(
            RenderRequest::tiled(page, scale, Rotation::None, TileSpec::new(1, 0)).key(),
            tile_b
        );
        assert_eq!(RenderRequest::new(page, scale, Rotation::None).key(), whole);
        assert!(RenderRequest::tiled(page, scale, Rotation::None, tile_a.tile.unwrap()).tile.is_some());
    }
}
