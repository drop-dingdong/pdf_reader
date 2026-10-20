//! 等比分块的**纯几何**：拆分、探测、排列。
//!
//! 这里不涉及渲染、缓存或 UI，只负责回答三个问题：
//! 1. 一页在当前比例下应当拆成几块、每块的整数像素边界在哪；
//! 2. 给定视口，需要哪些块；
//! 3. 每块应当摆在哪里、显示多大。
//!
//! ## 为什么用整数边界
//! PDFium 只能按**整数像素**裁剪，而布局是浮点，二者必然差 ≤1px。
//! 若按"理想网格"直接摆放，每块内容比例会各自差一点，接缝处就会错位。
//! 因此这里：
//! * 在**渲染比例**下用 `round` 生成一组**共享**的整数边界 `edges`；
//! * 每块的显示位置/尺寸都由同一组边界导出：`x = page.x + edges[i] * u`，
//!   `w = (edges[i+1] - edges[i]) * u`，其中 `u = 显示比例 / 渲染比例`。
//!
//! 相邻块共用 `edges[i]`，所以 `x_i + w_i == x_{i+1}` **严格成立**（同一个浮点表达式）。

use crate::geometry::{Point, Rect, Rotation, Size};

/// 目标块边长（像素）：层级选择让每块尽量接近这个大小。
pub const TARGET_TILE_PX: f32 = 512.0;

/// 一页的分块网格。
#[derive(Debug, Clone, PartialEq)]
pub struct TileGrid {
    /// 每边分块数（`2^level`，4、9、16… 由 `level` 决定）。
    pub side: u32,
    /// 建立网格时用的**渲染比例**（块矩形换算回页面空间要用它）。
    pub render_scale: f32,
    /// 渲染比例下的页像素尺寸（未取整，仅用于诊断）。
    pub render_size: (f32, f32),
    /// X 方向边界（长度 `side + 1`，单调递增，首元素 0，末元素 = `round(宽)`）。
    pub edges_x: Vec<u32>,
    /// Y 方向边界（同上）。
    pub edges_y: Vec<u32>,
}

impl TileGrid {
    /// 依据页尺寸与两个比例构建网格。
    ///
    /// * `render_scale` 是**实际渲染**使用的比例（通常 = 显示比例 × 超采样倍数）；
    /// * 层级按"渲染像素 / [`TARGET_TILE_PX`]"取最接近的 2 的幂。
    pub fn new(page_size: Size, render_scale: f32) -> Self {
        Self::new_bounded(page_size, render_scale, u32::MAX)
    }

    /// 同 [`TileGrid::new`]，但保证**块边长不超过** `max_side_px`（纹理单边上限）。
    ///
    /// 为什么需要：显卡对单张纹理的边长有硬限制（常见 16384）。一页即使总像素不多，
    /// 只要**单边**超限就画不出来 —— 例如很长的条幅页。这时必须继续细分，
    /// 与"面积超过 4 MP 才分块"是**两件独立**的事（D-108）。
    pub fn new_bounded(page_size: Size, render_scale: f32, max_side_px: u32) -> Self {
        let width = (page_size.width * render_scale).max(1.0);
        let height = (page_size.height * render_scale).max(1.0);
        let longest = width.max(height);
        let mut level = Self::level_for(longest);
        // 单边硬上限：继续细分到"块边长 ≤ 上限"，但仍受 32×32 的层级上限约束
        // 0 = 不限制（与 `DocumentSession::max_texture_side` 的约定一致）
        let limit = if max_side_px == 0 { f32::INFINITY } else { max_side_px as f32 };
        while level < 5 && longest / ((1u32 << level) as f32) > limit {
            level += 1;
        }
        let side = 1u32 << level;
        TileGrid {
            side,
            render_scale,
            render_size: (width, height),
            edges_x: edges(width, side),
            edges_y: edges(height, side),
        }
    }

    /// 让块边长最接近 [`TARGET_TILE_PX`] 的层级（`4` 块 = level 1 起，最大 level 5 = 32×32）。
    pub fn level_for(longest_px: f32) -> u32 {
        let longest = longest_px.max(1.0);
        let mut level = 0u32;
        while level < 5 {
            let side: f32 = (1u32 << (level + 1)) as f32;
            // 升到下一级后，块边长是否仍然 ≥ 目标？是则继续细分。
            let tile_side: f32 = longest / side;
            // 只有当"再细分一级后块边长仍不小于目标"时才细分：
            // 这样 842px 的 A4 页在 100% 下不分块（842/2 = 421 < 512）。
            if tile_side < TARGET_TILE_PX {
                break;
            }
            level += 1;
        }
        level
    }

    /// X 方向边界切片。
    pub fn x_edges(&self) -> &[u32] {
        &self.edges_x
    }

    /// Y 方向边界切片。
    pub fn y_edges(&self) -> &[u32] {
        &self.edges_y
    }

    /// 某块的**渲染像素**矩形 `(x, y, w, h)`；空块返回宽度/高度 0。
    pub fn tile_px(&self, col: u32, row: u32) -> (u32, u32, u32, u32) {
        let (x0, x1) = self.edge_span(&self.edges_x, col);
        let (y0, y1) = self.edge_span(&self.edges_y, row);
        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    /// 整页在**渲染空间**中的像素尺寸（= 最后一条边界的值）。
    ///
    /// 后端渲染时必须用**同一个**尺寸作为整页位图的尺寸（PDFium 的 `set_fixed_size`），
    /// 否则块与整页的缩放比会差一点点，接缝处就错位 —— 这个方法的唯一目的就是让两侧
    /// 拿到同一个整数，而不是各自 `round` 一次。
    pub fn render_px(&self) -> (u32, u32) {
        (
            self.edges_x.last().copied().unwrap_or(0),
            self.edges_y.last().copied().unwrap_or(0),
        )
    }

    /// 某块是否在网格内（有正面积）。
    pub fn contains(&self, col: u32, row: u32) -> bool {
        let (_, _, width, height) = self.tile_px(col, row);
        width > 0 && height > 0
    }

    fn edge_span(&self, edges: &[u32], index: u32) -> (u32, u32) {
        let lo = edges.get(index as usize).copied().unwrap_or(0);
        let hi = edges.get(index as usize + 1).copied().unwrap_or(lo);
        (lo, hi)
    }

    /// 某块在**内容空间**的显示矩形。
    ///
    /// `page_rect` 是该页在内容空间的矩形（左上角 + 显示尺寸）；
    /// `u = display_scale / render_scale` 把"渲染像素"换算成"内容单位"。
    pub fn display_rect(
        &self,
        page_rect: Rect,
        col: u32,
        row: u32,
        display_scale: f32,
        render_scale: f32,
    ) -> Rect {
        let u = ratio(display_scale, render_scale);
        let (x, y, w, h) = self.tile_px(col, row);
        Rect::new(
            page_rect.x + x as f32 * u,
            page_rect.y + y as f32 * u,
            w as f32 * u,
            h as f32 * u,
        )
    }

    /// 某块在 **PDF 页面空间**中的矩形（未旋转、左下原点）。
    ///
    /// 这是渲染请求要携带的"块矩形"：后端按它渲染该区域即可，**不必知道网格怎么切**
    /// （网格是渲染无关的几何，块怎么切由上层决定）。越界返回 [`Rect::ZERO`]。
    ///
    /// 换算路径：渲染像素 → 渲染比例下的显示坐标（÷`render_scale`）→ 页面空间
    /// （[`Rotation::unmap_point`]）→ 四角外接矩形（90°/270° 会交换宽高）。
    pub fn tile_page_rect(
        &self,
        col: u32,
        row: u32,
        page_size: Size,
        rotation: Rotation,
    ) -> Rect {
        let (x, y, width, height) = self.tile_px(col, row);
        if width == 0 || height == 0 {
            return Rect::ZERO;
        }
        let scale = if self.render_scale > 0.0 { self.render_scale } else { 1.0 };
        let pixels = Rect::new(
            x as f32 / scale,
            y as f32 / scale,
            width as f32 / scale,
            height as f32 / scale,
        );
        let corners = [
            Point::new(pixels.x, pixels.y),
            Point::new(pixels.right(), pixels.y),
            Point::new(pixels.x, pixels.bottom()),
            Point::new(pixels.right(), pixels.bottom()),
        ];
        let mut iter = corners.iter().map(|point| rotation.unmap_point(*point, page_size));
        let first = iter.next().unwrap_or(Point::ORIGIN);
        iter.fold(Rect::from_min_max(first, first), |acc, point| {
            acc.union(Rect::from_min_max(point, point))
        })
    }

    /// 覆盖某个**页面空间矩形**所需的块范围 `(col0, row0, col1, row1)`（含端点）。
    ///
    /// 用途：为"预览截图"这类**非视口**用途按需渲染一块区域（D-101）。它与
    /// [`Self::tiles_in_view`] 的区别只是输入坐标空间不同（页面空间 vs 内容空间），
    /// 边界判定规则完全一样：右/下边界正好落在块起点时**不算**下一块。
    pub fn tiles_for_page_rect(
        &self,
        rect: Rect,
        page_size: Size,
        rotation: Rotation,
    ) -> Option<(u32, u32, u32, u32)> {
        if !(rect.width > 0.0) || !(rect.height > 0.0) {
            return None;
        }
        let scale = if self.render_scale > 0.0 { self.render_scale } else { 1.0 };
        let corners = [
            Point::new(rect.x, rect.y),
            Point::new(rect.right(), rect.y),
            Point::new(rect.x, rect.bottom()),
            Point::new(rect.right(), rect.bottom()),
        ];
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);
        for corner in corners {
            let display = rotation.map_point(corner, page_size);
            let device = Point::new(display.x * scale, display.y * scale);
            min = Point::new(min.x.min(device.x), min.y.min(device.y));
            max = Point::new(max.x.max(device.x), max.y.max(device.y));
        }
        let (page_width, page_height) = self.render_px();
        let (page_width, page_height) = (page_width as f32, page_height as f32);
        // 完全在页外
        if max.x <= 0.0 || max.y <= 0.0 || min.x >= page_width || min.y >= page_height {
            return None;
        }
        let col0 = index_at(&self.edges_x, min.x.max(0.0), false);
        let row0 = index_at(&self.edges_y, min.y.max(0.0), false);
        let col1 = index_at(&self.edges_x, max.x.min(page_width), true);
        let row1 = index_at(&self.edges_y, max.y.min(page_height), true);
        if col0 > col1 || row0 > row1 {
            return None;
        }
        Some((col0, row0, col1, row1))
    }

    /// 视口需要的块范围 `(col0, row0, col1, row1)`（含端点）；完全不相交返回 `None`。
    ///
    /// 做法：视口先与该页矩形求交，再换算到"渲染像素"，最后在边界表里定位下标 ——
    /// 与"四角探测"等价（矩形网格无缝铺满平面），但对边界正好落点的情形更精确。
    pub fn tiles_in_view(
        &self,
        page_rect: Rect,
        view: Rect,
        display_scale: f32,
        render_scale: f32,
    ) -> Option<(u32, u32, u32, u32)> {
        let overlap = page_rect.intersection(view)?;
        if overlap.width <= 0.0 || overlap.height <= 0.0 {
            return None;
        }
        let u = ratio(display_scale, render_scale);
        if u <= 0.0 {
            return None;
        }
        // 内容空间 → 该页渲染像素坐标
        let px0 = (overlap.x - page_rect.x) / u;
        let py0 = (overlap.y - page_rect.y) / u;
        let px1 = px0 + overlap.width / u;
        let py1 = py0 + overlap.height / u;
        let col0 = index_at(&self.edges_x, px0, false);
        let row0 = index_at(&self.edges_y, py0, false);
        let col1 = index_at(&self.edges_x, px1, true);
        let row1 = index_at(&self.edges_y, py1, true);
        if col0 > col1 || row0 > row1 {
            return None;
        }
        Some((col0, row0, col1, row1))
    }

    /// 网格里所有非空块。
    pub fn all_tiles(&self) -> Vec<(u32, u32)> {
        let mut tiles = Vec::new();
        for row in 0..self.side {
            for col in 0..self.side {
                let (_, _, w, h) = self.tile_px(col, row);
                if w > 0 && h > 0 {
                    tiles.push((col, row));
                }
            }
        }
        tiles
    }
}

/// 生成共享边界：`edges[i] = round(i * total / side)`。
fn edges(total: f32, side: u32) -> Vec<u32> {
    (0..=side)
        .map(|i| {
            let value = total * i as f32 / side as f32;
            value.round().max(0.0) as u32
        })
        .collect()
}

/// `display / render`；非法输入退回 1.0。
fn ratio(display_scale: f32, render_scale: f32) -> f32 {
    if display_scale > 0.0 && render_scale > 0.0 {
        display_scale / render_scale
    } else {
        1.0
    }
}

/// 在边界表中定位坐标所在的块下标。
///
/// * `is_high == false`：取"包含该坐标"的块（右/下边界视为属于前一块）；
/// * `is_high == true`：取"严格在该坐标左侧"的最后一块（正好落在边界上时不算下一块）。
fn index_at(edges: &[u32], value: f32, is_high: bool) -> u32 {
    let last = edges.len().saturating_sub(2) as u32;
    let mut result = 0u32;
    for (index, edge) in edges.iter().enumerate().take(edges.len().saturating_sub(1)) {
        let start = *edge as f32;
        let inside = if is_high { start < value } else { start <= value };
        if inside {
            result = index as u32;
        } else {
            break;
        }
    }
    result.min(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常用页尺寸（A4 点）与一些"不能被整除"的尺寸。
    fn sizes() -> Vec<Size> {
        vec![
            Size::new(595.0, 842.0),
            Size::new(2263.7, 2800.6),
            Size::new(1001.3, 501.7),
            Size::new(37.5, 37.5),
        ]
    }

    /// 边界必须单调、首尾正确，且每块像素之和等于整页像素。
    #[test]
    fn edges_are_shared_integer_boundaries() {
        for size in sizes() {
            for render in [1.0f32, 2.64, 7.38] {
                let grid = TileGrid::new(size, render);
                for edges in [grid.x_edges(), grid.y_edges()] {
                    assert_eq!(edges[0], 0, "首元素必须是 0");
                    assert!(
                        edges.windows(2).all(|pair| pair[0] <= pair[1]),
                        "边界必须单调：{edges:?}"
                    );
                    // 每块像素之和 == 末元素
                    let sum: u32 = edges.windows(2).map(|pair| pair[1] - pair[0]).sum();
                    assert_eq!(sum, *edges.last().unwrap());
                }
                // 末元素 = round(渲染像素)
                let expected = (size.width * render).round().max(1.0) as u32;
                assert_eq!(*grid.x_edges().last().unwrap(), expected, "宽边界应覆盖整页");
            }
        }
    }

    /// 相邻块的显示矩形必须**严格相接**（不是"近似"）。
    #[test]
    fn adjacent_tiles_abut_exactly() {
        let size = Size::new(2263.7, 2800.6);
        let grid = TileGrid::new(size, 2.64);
        let display = 1.32;
        let render = 2.64;
        let page = Rect::new(100.0, -50.0, size.width * display, size.height * display);

        for row in 0..grid.side {
            for col in 1..grid.side {
                let left = grid.display_rect(page, col - 1, row, display, render);
                let right = grid.display_rect(page, col, row, display, render);
                assert_eq!(
                    left.x + left.width,
                    right.x,
                    "第 {col} 列与左邻必须严格相接"
                );
            }
        }
        for col in 0..grid.side {
            for row in 1..grid.side {
                let top = grid.display_rect(page, col, row - 1, display, render);
                let bottom = grid.display_rect(page, col, row, display, render);
                assert_eq!(top.y + top.height, bottom.y, "第 {row} 行与上邻必须严格相接");
            }
        }
    }

    /// 视口探测：与全量扫描的结果必须一致。
    #[test]
    fn view_probe_matches_full_scan() {
        let size = Size::new(2263.7, 2800.6);
        let display = 1.0;
        let render = 2.0;
        let grid = TileGrid::new(size, render);
        let page = Rect::new(0.0, 0.0, size.width, size.height);

        // 伪随机视口（固定种子，测试可复现）
        let mut seed = 0x2545_F491u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0
        };
        let mut checked = 0;
        for _ in 0..200 {
            let x = next() * size.width * 1.4 - size.width * 0.2;
            let y = next() * size.height * 1.4 - size.height * 0.2;
            let w = 40.0 + next() * size.width * 0.8;
            let h = 40.0 + next() * size.height * 0.8;
            let view = Rect::new(x, y, w, h);

            let Some((col0, row0, col1, row1)) = grid.tiles_in_view(page, view, display, render)
            else {
                // 无交叠时全量扫描也应当没有任何块
                let any = grid.all_tiles().iter().any(|(col, row)| {
                    let rect = grid.display_rect(page, *col, *row, display, render);
                    rect.intersection(view).is_some()
                });
                assert!(!any, "视口无交叠却扫到了块：{view:?}");
                continue;
            };

            // 范围内的每一块都应与视口相交
            for row in row0..=row1 {
                for col in col0..=col1 {
                    let rect = grid.display_rect(page, col, row, display, render);
                    let hit = rect
                        .intersection(view)
                        .is_some_and(|overlap| overlap.width > 0.0 && overlap.height > 0.0);
                    assert!(hit, "探测给出 ({col},{row}) 却与视口不相交");
                }
            }
            // 范围外的块都不能与视口相交
            for (col, row) in grid.all_tiles() {
                if col >= col0 && col <= col1 && row >= row0 && row <= row1 {
                    continue;
                }
                let rect = grid.display_rect(page, col, row, display, render);
                let hit = rect
                    .intersection(view)
                    .is_some_and(|overlap| overlap.width > 0.0 && overlap.height > 0.0);
                assert!(!hit, "范围外的 ({col},{row}) 却与视口相交：{view:?}");
            }
            checked += 1;
        }
        assert!(checked > 100, "有效样本太少（{checked}）");
    }

    /// 波段复用：同一个网格在两种显示比例下，各块只做**统一**缩放，相对位置不变。
    #[test]
    fn display_scale_change_only_scales_uniformly() {
        let size = Size::new(1200.0, 1600.0);
        let render = 2.0;
        let grid = TileGrid::new(size, render);
        let page_a = Rect::new(0.0, 0.0, size.width * 0.9, size.height * 0.9);
        let page_b = Rect::new(0.0, 0.0, size.width * 1.1, size.height * 1.1);

        for row in 0..grid.side {
            for col in 0..grid.side {
                let a = grid.display_rect(page_a, col, row, 0.9, render);
                let b = grid.display_rect(page_b, col, row, 1.1, render);
                // 相对页原点的偏移比例应一致：a.x / 页宽 == b.x / 页宽
                let ratio_a = a.x / page_a.width;
                let ratio_b = b.x / page_b.width;
                assert!((ratio_a - ratio_b).abs() < 1e-6, "相对位置必须与显示比例无关");
            }
        }
    }

    /// 块矩形必须在页面空间里：不越界、拼起来正好覆盖整页、且与像素矩形同面积。
    #[test]
    fn tile_page_rects_cover_the_page_and_stay_inside() {
        let page = Size::new(612.0, 792.0);
        for rotation in [
            Rotation::None,
            Rotation::Clockwise90,
            Rotation::Clockwise180,
            Rotation::Clockwise270,
        ] {
            let render = 2.0f32;
            let grid = TileGrid::new(page.rotated(rotation), render);
            let mut area = 0.0;
            for (col, row) in grid.all_tiles() {
                let rect = grid.tile_page_rect(col, row, page, rotation);
                assert!(
                    rect.x >= -1e-3
                        && rect.y >= -1e-3
                        && rect.right() <= page.width + 1e-3
                        && rect.bottom() <= page.height + 1e-3,
                    "块 ({col},{row}) 的页面矩形越界：{rect:?}（rotation={rotation:?}）"
                );
                // 面积守恒：块的页面面积 × 比例² 应等于它的像素面积
                let (_, _, width, height) = grid.tile_px(col, row);
                let expected = (width as f32 * height as f32) / (render * render);
                let actual = rect.width * rect.height;
                assert!(
                    (actual - expected).abs() < 1.0,
                    "块 ({col},{row}) 面积不符：{actual} vs {expected}"
                );
                area += actual;
            }
            let page_area = page.width * page.height;
            assert!(
                (area - page_area).abs() < 1.0,
                "所有块的页面面积之和必须等于整页（rotation={rotation:?}）：{area}"
            );
        }
    }

    /// 不旋转时块矩形的数值必须严格可推：底部那块对应页面下半部分（PDF y 向上）。
    #[test]
    fn tile_page_rect_values_are_exact_without_rotation() {
        let page = Size::new(612.0, 792.0);
        let grid = TileGrid::new(page, 2.0);
        assert_eq!(grid.side, 2);
        // 位图第 0 行（顶部）→ 页面 y 396..792
        let top_left = grid.tile_page_rect(0, 0, page, Rotation::None);
        assert_eq!(top_left, Rect::new(0.0, 396.0, 306.0, 396.0));
        // 位图第 1 行（底部）→ 页面 y 0..396
        let bottom_left = grid.tile_page_rect(0, 1, page, Rotation::None);
        assert_eq!(bottom_left, Rect::new(0.0, 0.0, 306.0, 396.0));
        // 越界块没有矩形
        assert_eq!(grid.tile_page_rect(2, 0, page, Rotation::None), Rect::ZERO);
    }

    /// 覆盖页面空间矩形的块范围，必须与"逐块用 `tile_page_rect` 求交"的结果一致。
    #[test]
    fn page_rect_probe_matches_a_full_scan() {
        let page = Size::new(595.28, 841.89);
        for rotation in [Rotation::None, Rotation::Clockwise90, Rotation::Clockwise270] {
            for render in [1.6f32, 4.096, 10.48576] {
                let grid = TileGrid::new(page.rotated(rotation), render);
                // 若干页面空间矩形：整页、四角、跨块、以及一个极小区域
                let rects = [
                    Rect::new(0.0, 0.0, page.width, page.height),
                    Rect::new(0.0, 0.0, 40.0, 40.0),
                    Rect::new(page.width - 40.0, page.height - 40.0, 40.0, 40.0),
                    Rect::new(120.0, 200.0, 300.0, 260.0),
                    Rect::new(page.width * 0.5, page.height * 0.5, 5.0, 5.0),
                ];
                for rect in rects {
                    let Some((c0, r0, c1, r1)) =
                        grid.tiles_for_page_rect(rect, page, rotation)
                    else {
                        panic!("页面内矩形必须覆盖到块：{rect:?}");
                    };
                    assert!(c1 >= c0 && r1 >= r0);
                    // 范围内的每一块都必须与矩形相交
                    for row in r0..=r1 {
                        for col in c0..=c1 {
                            let tile_rect = grid.tile_page_rect(col, row, page, rotation);
                            let overlap = tile_rect
                                .intersection(rect)
                                .is_some_and(|hit| hit.width > 0.0 && hit.height > 0.0);
                            assert!(
                                overlap,
                                "探测给出 ({col},{row}) 却与 {rect:?} 不相交（rotation={rotation:?}）"
                            );
                        }
                    }
                    // 范围外的每一块都不得与矩形相交
                    for (col, row) in grid.all_tiles() {
                        if col >= c0 && col <= c1 && row >= r0 && row <= r1 {
                            continue;
                        }
                        let tile_rect = grid.tile_page_rect(col, row, page, rotation);
                        let overlap = tile_rect
                            .intersection(rect)
                            .is_some_and(|hit| hit.width > 0.0 && hit.height > 0.0);
                        assert!(
                            !overlap,
                            "范围外的 ({col},{row}) 却与 {rect:?} 相交（rotation={rotation:?}）"
                        );
                    }
                }
                // 页外 → None
                assert!(grid
                    .tiles_for_page_rect(
                        Rect::new(-50.0, -50.0, 10.0, 10.0),
                        page,
                        rotation
                    )
                    .is_none());
            }
        }
    }

    /// 层级选择：块边长应接近目标值（512），且页越大层级越高。
    /// D-108：单边超过纹理上限时继续细分，直到每块都放得下。
    #[test]
    fn bounded_grid_keeps_tiles_within_the_texture_side_limit() {
        // 3000×4000 的页：按 512 目标应得 side=4（块 1000×1000）
        let plain = TileGrid::new(Size::new(3000.0, 4000.0), 1.0);
        assert_eq!(plain.side, 4);
        // 上限 900 → 必须再细分一级（块 500 ≤ 900）
        let bounded = TileGrid::new_bounded(Size::new(3000.0, 4000.0), 1.0, 900);
        assert_eq!(bounded.side, 8);
        assert!(bounded.render_size.1 / bounded.side as f32 <= 900.0);
        // 上限很宽松时不改变原有层级
        let loose = TileGrid::new_bounded(Size::new(3000.0, 4000.0), 1.0, 16_384);
        assert_eq!(loose.side, plain.side);
        // 层级上限 32：极端长页也只能到 32，块边长由后端上限兜底
        let extreme = TileGrid::new_bounded(Size::new(100.0, 600_000.0), 1.0, 512);
        assert_eq!(extreme.side, 32);
    }

    #[test]
    fn level_targets_about_512_pixels() {
        let small = TileGrid::new(Size::new(595.0, 842.0), 1.0);
        assert_eq!(small.side, 1, "595×842 在 100% 下不需要分块");

        let big = TileGrid::new(Size::new(2000.0, 3000.0), 1.0);
        assert!(big.side >= 2, "2000px 宽应至少分成 2 份");

        let huge = TileGrid::new(Size::new(2000.0, 3000.0), 8.0);
        let tile_px = huge.render_size.0 / huge.side as f32;
        assert!(
            (256.0..=1024.0).contains(&tile_px),
            "块边长 {tile_px} 应落在 256–1024 之间"
        );
    }

    /// 视口完全在页外 → `None`；页的边角块也应有正尺寸。
    #[test]
    fn degenerate_inputs_are_safe() {
        let size = Size::new(500.0, 700.0);
        let grid = TileGrid::new(size, 4.0);
        let page = Rect::new(0.0, 0.0, size.width, size.height);
        assert!(grid.tiles_in_view(page, Rect::new(5000.0, 5000.0, 10.0, 10.0), 1.0, 4.0).is_none());
        for (col, row) in grid.all_tiles() {
            let (_, _, w, h) = grid.tile_px(col, row);
            assert!(w > 0 && h > 0);
        }
    }

    /// `render_px()` 必须等于最后一条边界：后端拿它当整页位图尺寸，
    /// 两侧各算一次 `round` 就可能差 1px，接缝随之错位。
    #[test]
    fn render_px_matches_the_last_edge() {
        for size in sizes() {
            for render in [1.0f32, 1.6, 2.56, 7.38] {
                let grid = TileGrid::new(size, render);
                assert_eq!(
                    grid.render_px(),
                    (*grid.x_edges().last().unwrap(), *grid.y_edges().last().unwrap())
                );
            }
        }
    }

    /// 越界块必须被判为空（后端据此报错，而不是渲染一张 0×0 的图）。
    #[test]
    fn tiles_outside_the_grid_are_empty() {
        let grid = TileGrid::new(Size::new(612.0, 792.0), 8.0);
        assert_eq!(grid.side, 8, "612×792 @8x 应分成 8×8");
        assert!(grid.contains(0, 0) && grid.contains(7, 7));
        assert!(!grid.contains(8, 0), "列越界");
        assert!(!grid.contains(0, 8), "行越界");
        assert_eq!(grid.tile_px(8, 0).2, 0);
        assert_eq!(grid.tile_px(0, 8).3, 0);
    }
}

// ---------------------------------------------------------------------------
// 波段（超采样复用）
// ---------------------------------------------------------------------------

/// 默认超采样倍数：按 1.6 倍渲染，则在 1/1.6–1 的区间内只需下采样，不必重渲染。
pub const DEFAULT_OVERSCAN: f32 = 1.6;

/// 缩放比 → 波段号。同一波段内的缩放**共用一次渲染**。
///
/// 波段 `b` 覆盖 `[overscan^b, overscan^(b+1))`；渲染时取波段**上界**，
/// 因此波段内始终满足 `u = 显示/渲染 ≤ 1`（只下采样，不会放大变糊）。
pub fn band_for(scale: f32, overscan: f32) -> i32 {
    let ratio = if overscan > 1.0 { overscan } else { DEFAULT_OVERSCAN };
    let value = if scale > 0.0 { scale } else { 1.0 };
    (value.ln() / ratio.ln()).floor() as i32
}

/// 波段号 → 该波段应采用的**渲染比例**（取波段上界）。
pub fn render_scale_for_band(band: i32, overscan: f32) -> f32 {
    let ratio = if overscan > 1.0 { overscan } else { DEFAULT_OVERSCAN };
    ratio.powi(band + 1)
}

/// 已缓存 `render_scale` 的纹理，在显示比例为 `display_scale` 时是否仍可用。
///
/// 规则：**只允许下采样**（`display_scale <= render_scale`）——
/// 缩小时永远不必重渲染；放大到超过缓存比例才需要重渲染。
pub fn reuse_is_enough(display_scale: f32, render_scale: f32) -> bool {
    if display_scale <= 0.0 || render_scale <= 0.0 {
        return false;
    }
    display_scale <= render_scale * 1.0001
}

#[cfg(test)]
mod band_tests {
    use super::*;

    /// 同一波段内的缩放必须得到同一个波段号；跨过边界才换号。
    #[test]
    fn band_is_stable_inside_its_range() {
        let overscan = 2.0;
        // 波段 0 覆盖 [1, 2)
        assert_eq!(band_for(1.0, overscan), 0);
        assert_eq!(band_for(1.5, overscan), 0);
        assert_eq!(band_for(1.99, overscan), 0);
        assert_eq!(band_for(2.0, overscan), 1, "到达上界即换波段");
        assert_eq!(band_for(3.9, overscan), 1);
        assert_eq!(band_for(4.0, overscan), 2);
        // 缩小方向：波段为负
        assert_eq!(band_for(0.5, overscan), -1);
        assert_eq!(band_for(0.25, overscan), -2);
        // 非法输入退回 0
        assert_eq!(band_for(0.0, overscan), 0);
        assert_eq!(band_for(-1.0, overscan), 0);
    }

    /// 波段上界作为渲染比例：波段内一律只下采样（u ≤ 1），且不会超过一个超采样倍数。
    #[test]
    fn render_scale_is_the_band_ceiling() {
        for overscan in [1.6f32, 2.0, 3.0] {
            for band in -2..4 {
                let render = render_scale_for_band(band, overscan);
                let low = render / overscan;
                for display in [low, (low + render) * 0.5, render] {
                    let u = display / render;
                    assert!(u <= 1.0 + 1e-4, "波段内必须只下采样：u={u}");
                    assert!(u > 1.0 / overscan - 1e-4, "u 不应超过一个超采样倍数：u={u}");
                }
            }
        }
    }

    /// 缩小不需要重渲染；放大超过缓存比例才需要。
    #[test]
    fn reuse_covers_zoom_out_but_not_zoom_in() {
        assert!(reuse_is_enough(1.0, 2.0), "缩小可用");
        assert!(reuse_is_enough(2.0, 2.0), "恰好等于可用");
        assert!(!reuse_is_enough(2.01, 2.0), "放大超出即需重渲染");
        assert!(!reuse_is_enough(0.0, 2.0), "非法输入不可用");
        assert!(!reuse_is_enough(1.0, 0.0));
    }
}

/// 显示比例 → 该波段实际采用的**渲染比例**（= 波段上界）。
///
/// 渲染与显示因此分离：请求按此比例渲染，贴图时用 `u = display / 该值` 换算尺寸。
/// 波段内多次缩放只会改变 `u`，**不会**产生新的渲染请求。
///
/// `overscan <= 1.0` 表示**关闭超采样**：渲染比例 = 显示比例。
/// 这只用于测试与"新旧行为对比"（代价是失去波段复用 —— 显示比例是连续值，
/// 每变一点就是一个新键）；正常运行请用 [`DEFAULT_OVERSCAN`]。
pub fn render_scale_for(display_scale: f32, overscan: f32) -> f32 {
    let display = if display_scale > 0.0 { display_scale } else { 1.0 };
    if !(overscan > 1.0) {
        return display;
    }
    render_scale_for_band(band_for(display, overscan), overscan)
}

#[cfg(test)]
mod render_scale_tests {
    use super::*;

    /// 渲染比例必须 ≥ 显示比例（只下采样），且不超过一个超采样倍数。
    #[test]
    fn render_scale_dominates_display_scale() {
        let overscan = DEFAULT_OVERSCAN;
        for display in [0.1f32, 0.5, 0.9, 1.0, 1.3, 1.59, 1.6, 2.4, 4.0, 7.38] {
            let render = render_scale_for(display, overscan);
            assert!(
                render >= display - 1e-3,
                "渲染比例 {render} 必须 ≥ 显示比例 {display}"
            );
            assert!(
                render <= display * overscan + 1e-3,
                "渲染比例 {render} 不应超过显示比例的 {overscan} 倍（display={display}）"
            );
        }
    }

    /// 同一波段内，多次缩放得到**同一个**渲染比例（这正是"不重渲染"的依据）。
    #[test]
    fn render_scale_is_constant_within_a_band() {
        let overscan = 2.0;
        let render = render_scale_for(2.2, overscan);
        for display in [2.2f32, 2.5, 3.0, 3.9] {
            assert_eq!(
                render_scale_for(display, overscan),
                render,
                "波段内 display={display} 应复用同一渲染比例"
            );
        }
        assert_ne!(
            render_scale_for(4.1, overscan),
            render,
            "跨过波段边界应换渲染比例"
        );
    }

    /// `overscan <= 1.0`：关闭超采样，渲染比例就是显示比例（测试/对比用）。
    #[test]
    fn overscan_of_one_disables_supersampling() {
        for display in [0.2f32, 0.5, 1.0, 1.7, 4.0] {
            assert_eq!(render_scale_for(display, 1.0), display);
            assert_eq!(render_scale_for(display, 0.5), display, "小于 1 同样视为关闭");
        }
        assert_eq!(render_scale_for(0.0, 1.0), 1.0, "非法显示比例退回 1.0");
    }
}
