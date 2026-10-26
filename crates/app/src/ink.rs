//! 墨迹的手势状态机（D-118 S4b）。
//!
//! 这里只有**纯数据**：页面空间的采样点、抽稀、点数上限与"收笔是否成立"。
//! 它不碰 egui、不碰标注集合，因此"一笔一标注 / 太短不成立 / Esc 丢弃"这些规则
//! 都能用单测钉住；真正决定"何时起笔、何时取消"的是 [`crate::PdfReaderApp`]。

use pdfcore::prelude::{PageIndex, Point};

/// 相邻采样点的最小距离（**页面空间**单位）。
///
/// 用页面空间而不是屏幕像素：否则同一笔画在 100% 与 800% 下会得到不同密度的顶点，
/// "几何只与页面空间有关"的约定就破了。
pub(crate) const MIN_SAMPLE_DISTANCE: f32 = 0.5;

/// 一笔最多保留的顶点数（防止一次长拖拽把撤销栈与存档撑爆）。
pub(crate) const MAX_POINTS: usize = 2000;

/// 正在画的一笔墨迹。
///
/// **抬笔之前它不是标注**：不在 `AnnotationStore` 里、不产生命令、不进撤销栈；
/// 取消（Esc / 切工具）只要丢掉这个结构即可，不会留下半条。
#[derive(Debug, Clone)]
pub struct InkStroke {
    page: PageIndex,
    width: f32,
    points: Vec<Point>,
}

impl InkStroke {
    /// 起笔：记下所在页、线宽与第一个点。
    pub fn start(page: PageIndex, width: f32, point: Point) -> Self {
        Self {
            page,
            width,
            points: vec![point],
        }
    }

    /// 所在页（拖动跨到别的页也不改：整笔属于起笔那一页）。
    pub const fn page(&self) -> PageIndex {
        self.page
    }

    /// 线宽（页面空间）。
    pub const fn width(&self) -> f32 {
        self.width
    }

    /// 已采样的顶点（绘制中用来跟手）。
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// 追加一个采样点；返回是否真的加进去了。
    ///
    /// 丢弃三种点：非有限值（NaN/∞ 会污染包围盒）、离上一个点太近
    /// （< [`MIN_SAMPLE_DISTANCE`]，抽稀）、以及超过 [`MAX_POINTS`] 的溢出点。
    pub fn push(&mut self, point: Point) -> bool {
        if !point.x.is_finite() || !point.y.is_finite() {
            return false;
        }
        let near = match self.points.last() {
            Some(last) => last.distance(point) < MIN_SAMPLE_DISTANCE,
            None => false,
        };
        if near || self.points.len() >= MAX_POINTS {
            return false;
        }
        self.points.push(point);
        true
    }

    /// 收笔：至少两个点才算一笔有效的墨迹。
    ///
    /// 单点（只点了一下没拖）不产生标注 —— 模型的不变量也不接受一个点的折线。
    pub fn finish(self) -> Option<(PageIndex, Vec<Point>, f32)> {
        if self.points.len() < 2 {
            return None;
        }
        Some((self.page, self.points, self.width))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageIndex {
        PageIndex::FIRST
    }

    #[test]
    fn decimates_points_closer_than_half_a_page_unit() {
        let mut stroke = InkStroke::start(page(), 2.0, Point::new(100.0, 200.0));
        assert!(
            !stroke.push(Point::new(100.2, 200.2)),
            "太近的采样点应当丢弃"
        );
        assert!(stroke.push(Point::new(100.6, 200.0)), "超过阈值应当保留");
        assert_eq!(stroke.points().len(), 2);
        assert_eq!(stroke.points()[0], Point::new(100.0, 200.0));
    }

    #[test]
    fn rejects_non_finite_samples() {
        let mut stroke = InkStroke::start(page(), 2.0, Point::new(0.0, 0.0));
        assert!(!stroke.push(Point::new(f32::NAN, 1.0)));
        assert!(!stroke.push(Point::new(1.0, f32::INFINITY)));
        assert_eq!(stroke.points().len(), 1, "坏点不能进折线");
    }

    #[test]
    fn caps_the_number_of_points_per_stroke() {
        let mut stroke = InkStroke::start(page(), 2.0, Point::new(0.0, 0.0));
        for index in 1..=MAX_POINTS + 50 {
            stroke.push(Point::new(index as f32, 0.0));
        }
        assert_eq!(stroke.points().len(), MAX_POINTS, "必须在上限处停住");
    }

    #[test]
    fn a_single_point_is_not_a_stroke() {
        let stroke = InkStroke::start(page(), 2.0, Point::new(10.0, 10.0));
        assert!(
            stroke.clone().finish().is_none(),
            "只点一下（没有第二个点）不产生标注"
        );
        let mut stroke = stroke;
        stroke.push(Point::new(11.0, 10.0));
        let (restored_page, points, width) = stroke.finish().expect("两个点应当成立");
        assert_eq!(restored_page, page());
        assert_eq!(points.len(), 2);
        assert_eq!(width, 2.0);
    }
}
