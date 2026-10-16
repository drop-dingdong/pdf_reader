//! 布局引擎：把"每页多大"算成"每页摆在内容空间的哪里、显示多大"。
//!
//! ## 为什么布局要独立成 trait
//! 布局是**纯函数**：输入页几何与参数，输出每页的矩形。它不碰 PDF、不碰 GPU、不碰 UI 状态，
//! 因此可以被完整单测（见本文件末尾）—— 而"滚动错位""跳页偏了""高亮上下颠倒"这类 bug
//! 绝大多数都出在这一层。
//!
//! 同时，阅读器的几种排布方式（连续滚动、单页、未来的双页对开）差异巨大，
//! 用 trait 隔离后，MVP 只需要实现 [`VerticalLayout`] 一个，将来加对开模式也不影响调用方。

use crate::geometry::{Insets, Point, Rect, Rotation, Size, Vec2};
use crate::ids::PageIndex;
use crate::render::Scale;

/// 一页的几何信息：原始尺寸 + 页面自带的 `/Rotate`。
///
/// 之所以不把"旋转"合并进 [`Size`]：PDF 允许**每一页有不同的 `/Rotate`**，
/// 布局必须按页处理；把它显式建模出来，可以避免"整篇文档用一个全局旋转"这种常见错误。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeometry {
    /// 未旋转的页面尺寸（PDF point）。
    pub size: Size,
    /// 页面自带的 `/Rotate`。
    pub rotation: Rotation,
}

impl PageGeometry {
    /// 构造。
    pub const fn new(size: Size, rotation: Rotation) -> Self {
        Self { size, rotation }
    }

    /// 叠加用户旋转后的**实际**旋转。
    pub fn effective_rotation(self, user_rotation: Rotation) -> Rotation {
        self.rotation.compose(user_rotation)
    }

    /// 叠加用户旋转后的**显示尺寸**。
    pub fn displayed_size(self, user_rotation: Rotation) -> Size {
        self.size.rotated(self.effective_rotation(user_rotation))
    }
}

/// 页面排布模式。
///
/// MVP 中两种模式共用同一套纵向布局（[`VerticalLayout`]），差别只在滚动行为：
/// `Continuous` 自由滚动，`SinglePage` 通过翻页命令按页对齐。
/// "只渲染当前页"的裁剪属于优化项，留待 M4。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageLayoutMode {
    /// 连续纵向滚动（MVP 默认，最接近浏览器的阅读体验）。
    #[default]
    Continuous,
    /// 一次一页（翻页式）。
    SinglePage,
}

/// 阅读方向。
///
/// MVP 的连续/单页模式都是纵向排布，因此**该字段暂不产生影响**；它被保留在
/// [`LayoutParams`] 里，是为了让未来的双页对开（漫画 RTL、阿拉伯语 RTL）与横向排布
/// 不必再改一次接口。这样做的代价（一个当前无用的枚举）小于将来破坏接口的代价。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadingDirection {
    /// 从左到右。
    #[default]
    LeftToRight,
    /// 从右到左（漫画、阿拉伯语）。
    RightToLeft,
}

/// 布局参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    /// 排布模式。
    pub mode: PageLayoutMode,
    /// 阅读方向。
    pub direction: ReadingDirection,
    /// 缩放。
    pub scale: Scale,
    /// 相邻页之间的间隙（内容空间像素）。
    pub page_gap: f32,
    /// 内容四周留白。
    pub margin: Insets,
    /// **用户**施加的旋转；与每页自带的 `/Rotate` 叠加后得到最终显示旋转。
    pub user_rotation: Rotation,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            mode: PageLayoutMode::Continuous,
            direction: ReadingDirection::LeftToRight,
            scale: Scale::IDENTITY,
            page_gap: 12.0,
            margin: Insets::all(12.0),
            user_rotation: Rotation::None,
        }
    }
}

/// 视口：可见区域的大小，以及它相对内容空间原点的滚动偏移。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Viewport {
    /// 视口尺寸（UI 给的可用区域，已扣掉工具栏/侧边栏）。
    pub size: Size,
    /// 滚动偏移：视口左上角在内容空间中的坐标。
    pub scroll: Vec2,
}

impl Viewport {
    /// 构造。
    pub const fn new(size: Size, scroll: Vec2) -> Self {
        Self { size, scroll }
    }

    /// 视口在内容空间中覆盖的矩形。
    pub fn content_rect(&self) -> Rect {
        Rect::new(self.scroll.x, self.scroll.y, self.size.width, self.size.height)
    }
}

/// 某一页在内容空间中的摆放结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePlacement {
    /// 页号。
    pub page: PageIndex,
    /// 该页在内容空间的矩形（已包含旋转后的显示尺寸与缩放）。
    pub rect: Rect,
    /// 渲染该页时应使用的缩放。
    pub scale: Scale,
    /// 该页的**最终**显示旋转（页面 `/Rotate` ⊕ 用户旋转）。
    ///
    /// 渲染请求必须使用这个值，否则会出现"布局转了、位图没转"的错位。
    pub rotation: Rotation,
    /// 该页**未旋转**的原始尺寸（PDF point）。坐标换算需要它。
    pub page_size: Size,
}

impl PagePlacement {
    /// 构造。
    pub const fn new(
        page: PageIndex,
        rect: Rect,
        scale: Scale,
        rotation: Rotation,
        page_size: Size,
    ) -> Self {
        Self { page, rect, scale, rotation, page_size }
    }

    /// 旋转后的显示尺寸（内容空间像素）。
    pub fn displayed_size(&self) -> Size {
        self.rect.size()
    }

    /// 该页在当前缩放下的**像素尺寸**（分块网格用它）。
    ///
    /// 在此处一次性取整：瓦片拼接要求整数像素边界，取整只做一次，下游不再各算一遍。
    pub fn pixel_size(&self) -> Size {
        Size::new(self.rect.width.round().max(0.0), self.rect.height.round().max(0.0))
    }

    /// **PDF 页面坐标 → 内容空间坐标**（文字高亮、链接热区都要用它）。
    pub fn page_to_content(&self, page_point: Point) -> Point {
        if !self.scale.is_valid() {
            return self.rect.min();
        }
        let displayed = self.rotation.map_point(page_point, self.page_size);
        Point::new(
            self.rect.x + displayed.x * self.scale.0,
            self.rect.y + displayed.y * self.scale.0,
        )
    }

    /// **内容空间坐标 → PDF 页面坐标**（鼠标点击 → 命中字符要用它）。
    pub fn content_to_page(&self, content_point: Point) -> Point {
        if !self.scale.is_valid() {
            return Point::ORIGIN;
        }
        let displayed = Point::new(
            (content_point.x - self.rect.x) / self.scale.0,
            (content_point.y - self.rect.y) / self.scale.0,
        );
        self.rotation.unmap_point(displayed, self.page_size)
    }

    /// 把 PDF 页面空间的一个矩形换算到内容空间。
    ///
    /// 对 90°/270° 旋转，矩形的宽高会互换，因此不能只换算两个角点就完事，
    /// 这里用"四个角点取外接矩形"的稳妥做法。
    pub fn rect_to_content(&self, page_rect: Rect) -> Rect {
        let corners = [
            page_rect.min(),
            Point::new(page_rect.right(), page_rect.y),
            Point::new(page_rect.x, page_rect.bottom()),
            page_rect.max(),
        ];
        let mut iter = corners.iter().map(|point| self.page_to_content(*point));
        let first = iter.next().unwrap_or(Point::ORIGIN);
        iter.fold(Rect::from_min_max(first, first), |acc, point| {
            acc.union(Rect::from_min_max(point, point))
        })
    }
}

/// 整篇文档的布局结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocumentLayout {
    /// 内容总尺寸（用于计算滚动条范围与最大滚动量）。
    pub content_size: Size,
    /// 每页的位置，顺序与页序一致。
    pub placements: Vec<PagePlacement>,
}

impl DocumentLayout {
    /// 取某页的摆放结果。
    ///
    /// 目前是线性查找：每帧只需对"可见的几页 + 命中测试"调用，数千页规模下开销可忽略。
    /// 若将来支持十万页文档，可在此处换成按 `rect.y` 排序后的二分查找，调用方不受影响。
    pub fn placement(&self, page: PageIndex) -> Option<&PagePlacement> {
        self.placements.iter().find(|placement| placement.page == page)
    }

    /// 布局中的页数。
    pub fn page_count(&self) -> usize {
        self.placements.len()
    }
}

/// **布局引擎**：页几何 → 内容空间排布。
///
/// ## 这个 trait 存在的目的
/// * 把"排布算法"与"滚动状态、UI 绘制、文档加载"彻底解耦，使它可以被纯单测覆盖；
/// * 允许用不同实现替换排布方式（纵向连续 / 单页翻页 / 双页对开 / 横向），
///   而 [`crate::session::DocumentSession`] 与 UI 完全不需要改动；
/// * 通过**默认方法**提供滚动相关的高频操作（可见页、命中、跳页、夹取滚动），
///   实现者最少只需实现 [`PageLayout::compute`] 一个方法。
///
/// ## 实现者义务
/// * `compute` 必须是纯函数：相同输入必须得到相同输出。
/// * `placements` 的顺序必须与页序一致，且覆盖所有页（缺页会导致该页永远渲染不出来）。
/// * 必须原样使用 `params.user_rotation` 与每页的 `rotation` 叠加，并把结果写进
///   [`PagePlacement::rotation`] —— 渲染请求会直接采用它。
pub trait PageLayout: Send + Sync {
    /// 计算整篇文档的布局。**唯一必须实现的方法。**
    fn compute(&self, pages: &[PageGeometry], params: &LayoutParams) -> DocumentLayout;

    /// 取视口内可见的页。
    ///
    /// 用 `out: &mut Vec` 而不是返回 `Vec` 的原因：UI 每帧都要调用它，
    /// 复用同一个缓冲区可以避免每帧分配（立即模式 GUI 里这点开销会被放大到 60 次/秒）。
    fn visible_pages(&self, layout: &DocumentLayout, viewport: &Viewport, out: &mut Vec<PageIndex>) {
        out.clear();
        let window = viewport.content_rect();
        for placement in &layout.placements {
            if placement.rect.intersects(window) {
                out.push(placement.page);
            }
        }
    }

    /// 命中测试：内容空间的点落在哪一页上。
    fn page_at(&self, layout: &DocumentLayout, content_point: Point) -> Option<PageIndex> {
        layout
            .placements
            .iter()
            .find(|placement| placement.rect.contains(content_point))
            .map(|placement| placement.page)
    }

    /// 跳转到某页时应该设置的滚动偏移（默认把该页居中）。
    fn scroll_to_page(
        &self,
        layout: &DocumentLayout,
        page: PageIndex,
        viewport_size: Size,
    ) -> Vec2 {
        let Some(placement) = layout.placement(page) else {
            return Vec2::ZERO;
        };
        let target = Vec2::new(
            placement.rect.x + placement.rect.width * 0.5 - viewport_size.width * 0.5,
            placement.rect.y + placement.rect.height * 0.5 - viewport_size.height * 0.5,
        );
        self.clamp_scroll(layout, viewport_size, target)
    }

    /// 最大滚动偏移。
    fn max_scroll(&self, layout: &DocumentLayout, viewport_size: Size) -> Vec2 {
        Vec2::new(
            (layout.content_size.width - viewport_size.width).max(0.0),
            (layout.content_size.height - viewport_size.height).max(0.0),
        )
    }

    /// 把滚动偏移夹取到合法范围内（避免滚出内容区、出现空白页）。
    fn clamp_scroll(&self, layout: &DocumentLayout, viewport_size: Size, scroll: Vec2) -> Vec2 {
        let max = self.max_scroll(layout, viewport_size);
        Vec2::new(scroll.x.clamp(0.0, max.x), scroll.y.clamp(0.0, max.y))
    }
}

/// MVP 的默认布局：所有页纵向排列、水平居中，页间距与留白由 [`LayoutParams`] 决定。
///
/// 选它作为默认实现，是因为"连续滚动 + 视口虚拟化"是长文档阅读体验最好、也最容易做对的形式。
#[derive(Debug, Clone, Copy, Default)]
pub struct VerticalLayout;

impl PageLayout for VerticalLayout {
    fn compute(&self, pages: &[PageGeometry], params: &LayoutParams) -> DocumentLayout {
        let scale = if params.scale.is_valid() { params.scale } else { Scale::IDENTITY };
        let margin = params.margin;
        let gap = params.page_gap.max(0.0);

        // 第一遍：算出旋转+缩放后的显示尺寸，并取最大宽度用于水平居中。
        let displayed: Vec<(Size, Rotation)> = pages
            .iter()
            .map(|page| {
                let rotation = page.effective_rotation(params.user_rotation);
                (page.size.rotated(rotation).scale(scale.0), rotation)
            })
            .collect();
        let inner_width = displayed.iter().map(|(size, _)| size.width).fold(0.0_f32, f32::max);

        // 第二遍：逐页摆放。
        let mut y = margin.top;
        let mut placements = Vec::with_capacity(pages.len());
        for (index, page) in pages.iter().enumerate() {
            let (size, rotation) = displayed[index];
            let x = margin.left + (inner_width - size.width) * 0.5;
            placements.push(PagePlacement::new(
                PageIndex::new(index as u32),
                Rect::new(x, y, size.width, size.height),
                scale,
                rotation,
                page.size,
            ));
            y += size.height;
            if index + 1 < pages.len() {
                y += gap;
            }
        }

        let content_height = if pages.is_empty() { margin.vertical() } else { y + margin.bottom };
        DocumentLayout {
            content_size: Size::new(margin.left + inner_width + margin.right, content_height),
            placements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4() -> Size {
        Size::new(595.0, 842.0)
    }

    /// 若干同样大小、无自带旋转的页。
    fn small_pages(count: usize) -> Vec<PageGeometry> {
        (0..count)
            .map(|_| PageGeometry::new(Size::new(100.0, 200.0), Rotation::None))
            .collect()
    }

    fn params(scale: f32) -> LayoutParams {
        LayoutParams {
            scale: Scale::from_zoom(scale),
            page_gap: 10.0,
            margin: Insets::all(5.0),
            ..LayoutParams::default()
        }
    }

    #[test]
    fn stacks_pages_vertically_and_centers_them() {
        let layout = VerticalLayout.compute(&small_pages(3), &params(2.0));
        assert_eq!(layout.page_count(), 3);

        // 100x200 @2x → 200x400；页间距 10；留白 5
        assert_eq!(layout.placements[0].rect, Rect::new(5.0, 5.0, 200.0, 400.0));
        assert_eq!(layout.placements[1].rect, Rect::new(5.0, 415.0, 200.0, 400.0));
        assert_eq!(layout.placements[2].rect, Rect::new(5.0, 825.0, 200.0, 400.0));
        assert_eq!(layout.content_size, Size::new(210.0, 1230.0));
    }

    #[test]
    fn wider_pages_are_centered_and_drive_content_width() {
        let pages = vec![
            PageGeometry::new(Size::new(100.0, 200.0), Rotation::None),
            PageGeometry::new(Size::new(300.0, 100.0), Rotation::None),
        ];
        let layout = VerticalLayout.compute(&pages, &params(1.0));
        // 最宽 300 → 内容宽 5 + 300 + 5
        assert_eq!(layout.content_size.width, 310.0);
        assert_eq!(layout.placements[0].rect.x, 5.0 + (300.0 - 100.0) * 0.5);
        assert_eq!(layout.placements[1].rect.x, 5.0);
    }

    #[test]
    fn empty_document_keeps_margins_only() {
        let layout = VerticalLayout.compute(&[], &params(1.0));
        assert_eq!(layout.page_count(), 0);
        assert_eq!(layout.content_size, Size::new(10.0, 10.0));
    }

    #[test]
    fn visible_pages_and_hit_testing() {
        let layout = VerticalLayout.compute(&small_pages(3), &params(1.0));
        // 每页 100x200，间距 10，留白 5 → 页矩形 y = 5, 215, 425
        let mut visible = Vec::new();
        let viewport = Viewport::new(Size::new(120.0, 150.0), Vec2::new(0.0, 0.0));
        VerticalLayout.visible_pages(&layout, &viewport, &mut visible);
        assert_eq!(visible, vec![PageIndex::new(0)]);

        let viewport = Viewport::new(Size::new(120.0, 200.0), Vec2::new(0.0, 180.0));
        VerticalLayout.visible_pages(&layout, &viewport, &mut visible);
        assert_eq!(visible, vec![PageIndex::new(0), PageIndex::new(1)]);

        // 命中测试：内容坐标 (50, 250) 落在第 2 页（215..415）
        assert_eq!(
            VerticalLayout.page_at(&layout, Point::new(50.0, 250.0)),
            Some(PageIndex::new(1))
        );
        // 落在页间距里 → 无命中
        assert_eq!(VerticalLayout.page_at(&layout, Point::new(50.0, 210.0)), None);
    }

    #[test]
    fn scrolling_is_clamped_and_page_jump_centers() {
        let layout = VerticalLayout.compute(&small_pages(3), &params(1.0));
        let viewport_size = Size::new(120.0, 150.0);
        let max = VerticalLayout.max_scroll(&layout, viewport_size);
        // 内容高 = 5(上留白) + 200×3 + 10×2(页间距) + 5(下留白) = 630
        assert_eq!(layout.content_size.height, 630.0);
        assert_eq!(max, Vec2::new(0.0, 630.0 - 150.0));

        let clamped = VerticalLayout.clamp_scroll(&layout, viewport_size, Vec2::new(999.0, -20.0));
        assert_eq!(clamped, Vec2::new(max.x, 0.0));

        // 跳页后该页应居中（再被夹取到合法范围）
        let scroll = VerticalLayout.scroll_to_page(&layout, PageIndex::new(2), viewport_size);
        let placement = layout.placement(PageIndex::new(2)).unwrap();
        let center = placement.rect.y + placement.rect.height * 0.5 - viewport_size.height * 0.5;
        assert!((scroll.y - center.min(max.y)).abs() < 1e-3);
    }

    #[test]
    fn per_page_rotation_is_respected() {
        let pages = vec![
            PageGeometry::new(Size::new(100.0, 200.0), Rotation::Clockwise90),
            PageGeometry::new(Size::new(100.0, 200.0), Rotation::None),
        ];
        let layout = VerticalLayout.compute(&pages, &params(1.0));
        assert_eq!(layout.placements[0].rect.size(), Size::new(200.0, 100.0));
        assert_eq!(layout.placements[0].rotation, Rotation::Clockwise90);
        assert_eq!(layout.placements[1].rect.size(), Size::new(100.0, 200.0));
        assert_eq!(layout.placements[1].rotation, Rotation::None);
    }

    #[test]
    fn user_rotation_stacks_on_page_rotation() {
        let pages = vec![PageGeometry::new(Size::new(100.0, 200.0), Rotation::Clockwise90)];
        let mut p = params(1.0);
        p.user_rotation = Rotation::Clockwise90;
        let layout = VerticalLayout.compute(&pages, &p);
        assert_eq!(layout.placements[0].rotation, Rotation::Clockwise180);
        assert_eq!(layout.placements[0].rect.size(), Size::new(100.0, 200.0));
    }

    #[test]
    fn page_and_content_coordinates_round_trip() {
        let placement = PagePlacement::new(
            PageIndex::new(0),
            Rect::new(10.0, 20.0, 200.0, 400.0),
            Scale::from_zoom(2.0),
            Rotation::None,
            Size::new(100.0, 200.0),
        );
        // PDF 左上角 (0, 200) → 内容 (10, 20)
        let top_left = placement.page_to_content(Point::new(0.0, 200.0));
        assert!((top_left.x - 10.0).abs() < 1e-3 && (top_left.y - 20.0).abs() < 1e-3);
        // PDF 左下角 (0, 0) → 内容 (10, 420)
        let bottom_left = placement.page_to_content(Point::new(0.0, 0.0));
        assert!((bottom_left.x - 10.0).abs() < 1e-3 && (bottom_left.y - 420.0).abs() < 1e-3);

        let restored = placement.content_to_page(top_left);
        assert!((restored.x - 0.0).abs() < 1e-3 && (restored.y - 200.0).abs() < 1e-3);
    }

    #[test]
    fn rect_to_content_handles_rotated_pages() {
        let placement = PagePlacement::new(
            PageIndex::new(0),
            Rect::new(0.0, 0.0, 200.0, 100.0),
            Scale::IDENTITY,
            Rotation::Clockwise90,
            Size::new(100.0, 200.0),
        );
        // 页面空间里一个"高瘦"的矩形，旋转 90° 后应变成"矮胖"
        let converted = placement.rect_to_content(Rect::new(0.0, 0.0, 20.0, 100.0));
        assert!(
            (converted.width - 100.0).abs() < 1e-3 && (converted.height - 20.0).abs() < 1e-3,
            "got {converted:?}"
        );
    }

    #[test]
    fn a4_page_at_100_percent_is_72_dpi() {
        let pages = vec![PageGeometry::new(a4(), Rotation::None)];
        let layout = VerticalLayout.compute(&pages, &params(1.0));
        assert_eq!(layout.placements[0].rect.size(), a4());
        assert!((layout.placements[0].scale.dpi() - 72.0).abs() < 1e-3);
    }

    /// D-02：分块网格用的是"取整后的像素尺寸"，取整必须只做一次。
    #[test]
    fn pixel_size_rounds_placement_dimensions() {
        let placement = PagePlacement::new(
            PageIndex::new(0),
            Rect::new(12.0, 12.0, 918.4, 1188.6),
            Scale::from_zoom(1.5),
            Rotation::None,
            Size::new(612.0, 792.0),
        );
        assert_eq!(placement.pixel_size(), Size::new(918.0, 1189.0));
        // 与显示尺寸区分：displayed_size 保留小数
        assert_eq!(placement.displayed_size(), Size::new(918.4, 1188.6));
    }
}
