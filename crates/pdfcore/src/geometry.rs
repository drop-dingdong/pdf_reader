//! 几何类型、单位与**坐标约定**。
//!
//! ## 全项目统一的坐标系（务必遵守，否则会出现"文字与高亮错位"这类顽疾）
//!
//! | 空间 | 原点 | y 轴 | 单位 | 谁负责换算 |
//! | --- | --- | --- | --- | --- |
//! | PDF 页面空间 | 左下角 | **向上** | point（1/72 英寸） | 后端（PDFium）给出原始值 |
//! | 内容空间 | 整篇文档左上角 | 向下 | 设备无关像素 | [`crate::layout::PageLayout`] |
//! | 视图空间 | 视口左上角 | 向下 | 设备无关像素 | UI（egui 的 `Rect` 语义一致） |
//!
//! 换算链：`PDF 页面坐标 --Rotation::map_point--> 旋转后页内坐标 --×Scale + 页在内容空间的偏移--> 内容坐标 --−滚动偏移--> 视图坐标`。
//!
//! [`Rotation::map_point`] / [`Rotation::unmap_point`] 是整条链里最容易写错的部分，因此被抽成
//! 纯函数并配有往返测试（见本文件末尾）。

use std::ops::{Add, AddAssign, Mul, Sub};

/// 二维点（含义由所在坐标系决定）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// 横坐标。
    pub x: f32,
    /// 纵坐标。
    pub y: f32,
}

impl Point {
    /// 原点。
    pub const ORIGIN: Self = Self { x: 0.0, y: 0.0 };

    /// 构造。
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// 平移。
    pub fn translate(self, delta: Vec2) -> Self {
        Self::new(self.x + delta.x, self.y + delta.y)
    }

    /// 到另一点的欧氏距离。
    pub fn distance(self, other: Self) -> f32 {
        (self - other).length()
    }
}

impl Add<Vec2> for Point {
    type Output = Self;
    fn add(self, rhs: Vec2) -> Self {
        self.translate(rhs)
    }
}

impl Sub for Point {
    type Output = Vec2;
    fn sub(self, rhs: Self) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// 二维向量 / 偏移量。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    /// 横向分量。
    pub x: f32,
    /// 纵向分量。
    pub y: f32,
}

impl Vec2 {
    /// 零向量。
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// 构造。
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// 长度。
    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// 逐分量乘法（用于缩放）。
    pub fn scale(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

impl Add for Vec2 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl AddAssign for Vec2 {
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        self.scale(rhs)
    }
}

/// 尺寸（宽高均为正数；不做负值校验，由调用方保证）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    /// 宽。
    pub width: f32,
    /// 高。
    pub height: f32,
}

impl Size {
    /// 零尺寸。
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };

    /// 构造。
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// 面积。
    pub fn area(self) -> f32 {
        self.width * self.height
    }

    /// 是否退化为空。
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// 等比缩放。
    pub fn scale(self, factor: f32) -> Self {
        Self::new(self.width * factor, self.height * factor)
    }

    /// 应用页面旋转后的**显示尺寸**（旋转 90°/270° 时宽高互换）。
    pub fn rotated(self, rotation: Rotation) -> Self {
        if rotation.swaps_dimensions() {
            Self::new(self.height, self.width)
        } else {
            self
        }
    }
}

/// 轴对齐矩形。坐标系含义见[模块说明](self)。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// 左上角横坐标。
    pub x: f32,
    /// 左上角纵坐标。
    pub y: f32,
    /// 宽。
    pub width: f32,
    /// 高。
    pub height: f32,
}

impl Rect {
    /// 空矩形。
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    /// 由左上角与宽高构造。
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 由左上角与右下角构造。
    pub fn from_min_max(min: Point, max: Point) -> Self {
        Self::new(min.x, min.y, max.x - min.x, max.y - min.y)
    }

    /// 左上角。
    pub fn min(self) -> Point {
        Point::new(self.x, self.y)
    }

    /// 右下角。
    pub fn max(self) -> Point {
        Point::new(self.right(), self.bottom())
    }

    /// 中心点。
    pub fn center(self) -> Point {
        Point::new(self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    /// 右边界。
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    /// 下边界。
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    /// 尺寸。
    pub fn size(self) -> Size {
        Size::new(self.width, self.height)
    }

    /// 是否为零面积或负面积。
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// 是否包含某点（左闭右开，避免相邻页边界重复命中）。
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// 是否与另一矩形相交（边界接触不算相交）。
    pub fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// 交集；不相交时为 `None`。
    pub fn intersection(self, other: Self) -> Option<Self> {
        if !self.intersects(other) {
            return None;
        }
        let min = Point::new(self.x.max(other.x), self.y.max(other.y));
        let max = Point::new(
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        );
        Some(Self::from_min_max(min, max))
    }

    /// 并集（外接矩形）。
    pub fn union(self, other: Self) -> Self {
        let min = Point::new(self.x.min(other.x), self.y.min(other.y));
        let max = Point::new(
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        );
        Self::from_min_max(min, max)
    }

    /// 平移。
    pub fn translate(self, delta: Vec2) -> Self {
        Self::new(self.x + delta.x, self.y + delta.y, self.width, self.height)
    }

    /// 内缩（页边距、视口留白）。
    pub fn inset(self, insets: Insets) -> Self {
        Self::new(
            self.x + insets.left,
            self.y + insets.top,
            (self.width - insets.left - insets.right).max(0.0),
            (self.height - insets.top - insets.bottom).max(0.0),
        )
    }
}

/// 四边留白。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Insets {
    /// 上。
    pub top: f32,
    /// 右。
    pub right: f32,
    /// 下。
    pub bottom: f32,
    /// 左。
    pub left: f32,
}

impl Insets {
    /// 全零。
    pub const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    /// 四边同值。
    pub const fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    /// 分别指定。
    pub const fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// 左右之和。
    pub const fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// 上下之和。
    pub const fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

/// 页面显示旋转（对应 PDF 的 `/Rotate`，以及用户手动旋转视图）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Rotation {
    /// 不旋转。
    #[default]
    None,
    /// 顺时针 90°。
    Clockwise90,
    /// 顺时针 180°。
    Clockwise180,
    /// 顺时针 270°。
    Clockwise270,
}

impl Rotation {
    /// 旋转角度（度）。
    pub const fn degrees(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Clockwise90 => 90,
            Self::Clockwise180 => 180,
            Self::Clockwise270 => 270,
        }
    }

    /// 由 PDF `/Rotate` 值构造（自动取模 360；非法值返回 `None`）。
    // 不使用 `rem_euclid`：它在 const 上下文中的稳定性依版本而异，这里手工取模更保险。
    pub const fn from_degrees(degrees: i32) -> Option<Self> {
        let normalized = degrees.rem_euclid(360);
        match normalized {
            0 => Some(Self::None),
            90 => Some(Self::Clockwise90),
            180 => Some(Self::Clockwise180),
            270 => Some(Self::Clockwise270),
            _ => None,
        }
    }

    /// 是否交换宽高。
    pub const fn swaps_dimensions(self) -> bool {
        matches!(self, Self::Clockwise90 | Self::Clockwise270)
    }

    /// 叠加两个旋转（用户旋转 + 页面自带 `/Rotate`）。
    pub const fn compose(self, other: Self) -> Self {
        match Self::from_degrees(self.degrees() as i32 + other.degrees() as i32) {
            Some(r) => r,
            None => Self::None,
        }
    }

    /// 逆向旋转（用于把视图坐标反算回页面坐标）。
    pub const fn inverse(self) -> Self {
        match self {
            Self::None => Self::None,
            Self::Clockwise90 => Self::Clockwise270,
            Self::Clockwise180 => Self::Clockwise180,
            Self::Clockwise270 => Self::Clockwise90,
        }
    }

    /// 旋转后的显示尺寸。
    pub fn apply_to_size(self, size: Size) -> Size {
        size.rotated(self)
    }

    /// PDF 页面坐标 → **旋转后**的页内显示坐标（左上原点、y 向下）。
    ///
    /// `page_size` 必须是**未旋转**的页面尺寸（即 `FPDF_GetPageWidth/Height` 的原始值）。
    ///
    /// 推导方式：把页面想象成一张纸，按顺时针旋转 `θ`，再观察四个角落到显示框的哪个位置。
    /// 注意 `None` 与 `Clockwise180` 都**包含 y 轴翻转**（PDF 的 y 向上，显示坐标 y 向下），
    /// 而 90°/270° 的翻转已经隐含在坐标交换里 —— 这是本模块最容易写错的地方。
    pub fn map_point(self, point: Point, page_size: Size) -> Point {
        match self {
            // 不旋转：仅翻转 y 轴。PDF 左上角 (0, h) → 显示 (0, 0)。
            Self::None => Point::new(point.x, page_size.height - point.y),
            // 顺时针 90°：左下角 → 显示左上角。
            Self::Clockwise90 => Point::new(point.y, point.x),
            // 顺时针 180°：左下角 → 显示右上角，y 同样需要翻转。
            Self::Clockwise180 => Point::new(page_size.width - point.x, point.y),
            // 顺时针 270°：右下角 → 显示左上角。
            Self::Clockwise270 => Point::new(page_size.height - point.y, page_size.width - point.x),
        }
    }

    /// [`Rotation::map_point`] 的逆运算：旋转后的页内显示坐标 → PDF 页面坐标。
    pub fn unmap_point(self, point: Point, page_size: Size) -> Point {
        match self {
            Self::None => Point::new(point.x, page_size.height - point.y),
            Self::Clockwise90 => Point::new(point.y, point.x),
            Self::Clockwise180 => Point::new(page_size.width - point.x, point.y),
            Self::Clockwise270 => Point::new(page_size.width - point.y, page_size.height - point.x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [Size; 3] = [
        Size::new(612.0, 792.0),   // US Letter 纵向
        Size::new(792.0, 612.0),   // 横向
        Size::new(595.28, 841.89), // A4
    ];

    const ROTATIONS: [Rotation; 4] = [
        Rotation::None,
        Rotation::Clockwise90,
        Rotation::Clockwise180,
        Rotation::Clockwise270,
    ];

    #[test]
    fn rotation_swaps_dimensions_only_for_odd_quarter_turns() {
        assert!(!Rotation::None.swaps_dimensions());
        assert!(Rotation::Clockwise90.swaps_dimensions());
        assert!(!Rotation::Clockwise180.swaps_dimensions());
        assert!(Rotation::Clockwise270.swaps_dimensions());

        let s = Size::new(10.0, 20.0);
        assert_eq!(
            Rotation::Clockwise90.apply_to_size(s),
            Size::new(20.0, 10.0)
        );
        assert_eq!(Rotation::Clockwise180.apply_to_size(s), s);
    }

    /// 关键不变量：map / unmap 必须互为逆运算，否则文字选中与高亮会错位。
    #[test]
    fn map_and_unmap_are_inverse() {
        for size in SIZES {
            for rotation in ROTATIONS {
                for point in [
                    Point::new(0.0, 0.0),
                    Point::new(size.width, size.height),
                    Point::new(size.width * 0.25, size.height * 0.75),
                ] {
                    let displayed = rotation.map_point(point, size);
                    let restored = rotation.unmap_point(displayed, size);
                    assert!(
                        (restored.x - point.x).abs() < 1e-3 && (restored.y - point.y).abs() < 1e-3,
                        "rotation={rotation:?} size={size:?} point={point:?} -> {displayed:?} -> {restored:?}"
                    );
                }
            }
        }
    }

    /// 映射结果必须落在旋转后的显示框内（左下角映射到左上角，是一个易错点）。
    #[test]
    fn mapped_points_stay_inside_displayed_box() {
        let size = Size::new(200.0, 100.0);
        for rotation in ROTATIONS {
            let displayed_size = rotation.apply_to_size(size);
            for corner in [
                Point::new(0.0, 0.0),
                Point::new(size.width, 0.0),
                Point::new(0.0, size.height),
                Point::new(size.width, size.height),
            ] {
                let p = rotation.map_point(corner, size);
                assert!(
                    (-1e-3..=displayed_size.width + 1e-3).contains(&p.x)
                        && (-1e-3..=displayed_size.height + 1e-3).contains(&p.y),
                    "rotation={rotation:?} corner={corner:?} mapped={p:?} box={displayed_size:?}"
                );
            }
        }
    }

    /// PDF 页面空间原点在左下角，因此 90° 顺时针旋转后它应落在显示页的左上角。
    #[test]
    fn pdf_bottom_left_becomes_display_top_left_for_90_degrees() {
        let size = Size::new(200.0, 100.0);
        let mapped = Rotation::Clockwise90.map_point(Point::new(0.0, 0.0), size);
        assert_eq!(mapped, Point::new(0.0, 0.0));
    }

    /// 不旋转时也必须有 y 轴翻转：PDF 的左上角 (0, h) 对应显示的 (0, 0)。
    #[test]
    fn no_rotation_still_flips_y_axis() {
        let size = Size::new(200.0, 100.0);
        assert_eq!(
            Rotation::None.map_point(Point::new(0.0, 100.0), size),
            Point::new(0.0, 0.0)
        );
        assert_eq!(
            Rotation::None.map_point(Point::new(0.0, 0.0), size),
            Point::new(0.0, 100.0)
        );
        // 180°：PDF 左下角落到显示右上角
        assert_eq!(
            Rotation::Clockwise180.map_point(Point::new(0.0, 0.0), size),
            Point::new(200.0, 0.0)
        );
    }

    #[test]
    fn compose_and_inverse_are_consistent() {
        assert_eq!(Rotation::Clockwise90.inverse(), Rotation::Clockwise270);
        assert_eq!(Rotation::Clockwise180.inverse(), Rotation::Clockwise180);
        assert_eq!(
            Rotation::Clockwise90.compose(Rotation::Clockwise270),
            Rotation::None
        );
        assert_eq!(
            Rotation::Clockwise270.compose(Rotation::Clockwise90),
            Rotation::None
        );
        assert_eq!(Rotation::from_degrees(450), Some(Rotation::Clockwise90));
        assert_eq!(Rotation::from_degrees(-90), Some(Rotation::Clockwise270));
    }

    #[test]
    fn rect_algebra() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert!(a.intersects(b));
        assert_eq!(a.intersection(b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(a.union(b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert!(
            !a.intersects(Rect::new(10.0, 0.0, 5.0, 5.0)),
            "边界接触不算相交"
        );
        assert!(a.contains(Point::new(0.0, 0.0)));
        assert!(!a.contains(Point::new(10.0, 10.0)), "右/下边界为开区间");
        assert_eq!(a.inset(Insets::all(1.0)), Rect::new(1.0, 1.0, 8.0, 8.0));
        assert_eq!(a.inset(Insets::all(20.0)).size(), Size::ZERO);
    }
}
