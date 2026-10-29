//! 页面标注（注释）的数据模型 —— 见 D-118。
//!
//! 这一层只放**纯数据与不变量**，不涉及绘制、持久化与交互：
//!
//! * 几何一律是**未旋转的页面空间**（PDF 点，左下原点），与 [`crate::document::Link`]
//!   和 [`crate::text::TextChar::bbox`] 一致。旋转由布局在绘制时统一换算，
//!   所以缩放 / 旋转 / 换排布都不会让标注漂移。
//! * 正文不在这里：正文存在 app 的 `custom_texts` 里，这里只留一个**引用**
//!   （哨兵标题，S2 落盘时才真正用到）。
//! * 标注**不进渲染管线**：它不产生 `RenderKey`，也不影响瓦片网格。

use crate::geometry::{Point, Rect};
use crate::ids::PageIndex;

/// 文本类标注的默认颜色（半透明黄，像荧光笔）。
pub const TEXT_MARK_COLOR: [u8; 3] = [255, 214, 0];

/// 高亮的默认不透明度（下划线/删除线默认不透明）。
pub const TEXT_MARK_OPACITY: f32 = 0.35;

/// 下划线/删除线的页面空间线宽。
pub const TEXT_MARK_LINE_WIDTH: f32 = 1.5;

/// 下划线相对文字底部的下移量（页面空间）。
pub const UNDERLINE_GAP: f32 = 2.4;

/// 删除线在文字包围盒内的垂直位置（0=底，1=顶）。
///
/// 用 0.4 而不是 0.5：删除线应接近 x-height 中线，避免大写字母把线抬得过高。
pub const STRIKE_FRACTION: f32 = 0.4;

/// 墨迹的默认颜色（不透明深色，像笔）。
///
/// 墨迹不能沿用高亮的半透明黄：35% 的黄在页面上几乎看不见，
/// 用户会以为"画了但没有反应"。默认样式只在**新建**时使用，存档里的值优先。
pub const INK_COLOR: [u8; 3] = [25, 25, 25];

/// 墨迹的默认不透明度。
pub const INK_OPACITY: f32 = 1.0;

/// 标注 id。
///
/// **删除后不复用**：撤销栈（S3）会把旧 id 放回，复用会与新标注撞号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnnotationId(pub u64);

impl AnnotationId {
    /// 原始值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 标注种类。
///
/// 文本类（高亮 / 下划线 / 删除线）的几何是"若干四边形"——天然按行分成多个矩形；
/// 墨迹的几何是**页面空间折线**（S4）；选框在 S5 之后再加，模型不预先为它留空壳。
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationKind {
    /// 高亮：半透明填充。
    Highlight {
        /// 每行的矩形（页面空间）。
        quads: Vec<Rect>,
    },
    /// 下划线：底部一条线。
    Underline {
        /// 每行的矩形（页面空间）。
        quads: Vec<Rect>,
    },
    /// 删除线：中部一条线。
    StrikeOut {
        /// 每行的矩形（页面空间）。
        quads: Vec<Rect>,
    },
    /// 墨迹：一笔绘制出的折线。
    ///
    /// 顶点与线宽**都是页面空间单位**：缩放时按布局的 `scale` 换算，粗细才与缩放无关
    /// （存屏幕像素会让"画完再放大"变成一条细线）。
    Ink {
        /// 折线顶点（页面空间，顺序即笔顺；至少两个点）。
        points: Vec<Point>,
        /// 线宽（页面空间）。
        width: f32,
    },
}

impl AnnotationKind {
    /// 该种类的行矩形（墨迹没有矩形，返回空切片）。
    ///
    /// 文本类标注的绘制、命中与"按页取行"都走它；墨迹为空是**刻意**的：
    /// 调用方如果只处理 `quads`，自然会跳过墨迹，而不是把它当零面积矩形误判。
    pub fn quads(&self) -> &[Rect] {
        match self {
            Self::Highlight { quads } | Self::Underline { quads } | Self::StrikeOut { quads } => {
                quads
            }
            Self::Ink { .. } => &[],
        }
    }

    /// 墨迹的折线与线宽；其它种类为 `None`。
    pub fn ink(&self) -> Option<(&[Point], f32)> {
        match self {
            Self::Ink { points, width } => Some((points, *width)),
            _ => None,
        }
    }

    /// 几何是否满足该类的不变量（`Annotation::new` 的唯一判据）。
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Highlight { quads } | Self::Underline { quads } | Self::StrikeOut { quads } => {
                !quads.is_empty()
                    && quads
                        .iter()
                        .all(|quad| quad.width > 0.0 && quad.height > 0.0)
            }
            Self::Ink { points, width } => {
                points.len() >= 2
                    && width.is_finite()
                    && *width > 0.0
                    && points
                        .iter()
                        .all(|point| point.x.is_finite() && point.y.is_finite())
            }
        }
    }
}

/// 点到线段 `a─b` 的最短距离（墨迹命中用；退化成点时距离到该点）。
fn distance_to_segment(point: Point, a: Point, b: Point) -> f32 {
    let ab = b - a;
    let length_squared = ab.x * ab.x + ab.y * ab.y;
    if length_squared <= f32::EPSILON {
        return point.distance(a);
    }
    let ap = point - a;
    let t = ((ap.x * ab.x + ap.y * ab.y) / length_squared).clamp(0.0, 1.0);
    let closest = Point::new(a.x + ab.x * t, a.y + ab.y * t);
    point.distance(closest)
}

/// PDF 里注释对象的身份（只用于运行时定位；不写进 overlay）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PdfObjectRef {
    /// 对象号。
    pub num: u32,
    /// generation。
    pub gen: u16,
}

/// 一个标注。
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// id（由 [`AnnotationStore`] 分配）。
    pub id: AnnotationId,
    /// 所在页。
    pub page: PageIndex,
    /// 种类与几何（页面空间）。
    pub kind: AnnotationKind,
    /// 颜色（RGB）。
    pub color: [u8; 3],
    /// 不透明度（`0.0..=1.0`）。
    pub opacity: f32,
    /// 正文（直接属于注释；`None` = 没有正文）。
    pub body: Option<String>,
    /// 来自 PDF 的对象号；新建未写入 / 内联注释为 `None`。
    pub pdf_ref: Option<PdfObjectRef>,
}

impl Annotation {
    /// 构造一个标注并做不变量检查（几何为空或退化时返回 `None`）。
    pub fn new(page: PageIndex, kind: AnnotationKind) -> Option<Self> {
        if !kind.is_valid() {
            return None;
        }
        let (color, opacity) = match &kind {
            AnnotationKind::Ink { .. } => (INK_COLOR, INK_OPACITY),
            AnnotationKind::Highlight { .. } => (TEXT_MARK_COLOR, TEXT_MARK_OPACITY),
            AnnotationKind::Underline { .. } | AnnotationKind::StrikeOut { .. } => {
                (TEXT_MARK_COLOR, 1.0)
            }
        };
        Some(Self {
            id: AnnotationId(0),
            page,
            kind,
            color,
            opacity,
            body: None,
            pdf_ref: None,
        })
    }

    /// 覆盖全部几何的包围盒（页面空间）。
    ///
    /// 墨迹的包围盒按折线点集外扩**半个线宽**——否则一条横线的高度为 0，
    /// 依赖包围盒的浮窗位置、截图与命中预备检查都会退化成一条线。
    pub fn bounds(&self) -> Option<Rect> {
        if let Some((points, width)) = self.kind.ink() {
            let first = *points.first()?;
            let mut min = first;
            let mut max = first;
            for point in points {
                min.x = min.x.min(point.x);
                min.y = min.y.min(point.y);
                max.x = max.x.max(point.x);
                max.y = max.y.max(point.y);
            }
            let half = width * 0.5;
            return Some(Rect::new(
                min.x - half,
                min.y - half,
                (max.x - min.x) + width,
                (max.y - min.y) + width,
            ));
        }
        let mut quads = self.kind.quads().iter().copied();
        let first = quads.next()?;
        Some(quads.fold(first, |union, quad| union.union(quad)))
    }

    /// 点是否落在标注内（`tolerance` 是页面空间单位，由调用方按缩放换算）。
    ///
    /// 文本类看"是否落在某行矩形里"；墨迹看"到折线的距离是否小于半线宽（或容差）"，
    /// **不能**用包围盒——一笔 L 形墨迹的包围盒里有大片空白，用包围盒会出现
    /// "点在空白处却选中了墨迹"。
    pub fn contains(&self, point: Point, tolerance: f32) -> bool {
        let tolerance = tolerance.max(0.0);
        if let Some((points, width)) = self.kind.ink() {
            let radius = (width * 0.5).max(tolerance);
            return points
                .windows(2)
                .any(|pair| distance_to_segment(point, pair[0], pair[1]) <= radius);
        }
        self.kind.quads().iter().any(|quad| {
            let expanded = Rect::new(
                quad.x - tolerance,
                quad.y - tolerance,
                quad.width + tolerance * 2.0,
                quad.height + tolerance * 2.0,
            );
            expanded.contains(point)
        })
    }

    /// 不透明度（夹到 `0.0..=1.0`）。
    pub fn clamped_opacity(&self) -> f32 {
        if self.opacity.is_finite() {
            self.opacity.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

/// 一份文档的全部标注（按页过滤是绘制与命中的常用操作）。
#[derive(Debug, Clone, Default)]
pub struct AnnotationStore {
    items: Vec<Annotation>,
    next_id: u64,
}

impl AnnotationStore {
    /// 空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 全部标注（按添加顺序）。
    pub fn iter(&self) -> impl Iterator<Item = &Annotation> {
        self.items.iter()
    }

    /// 某页的标注（按添加顺序）。
    pub fn on_page(&self, page: PageIndex) -> impl Iterator<Item = &Annotation> {
        self.items.iter().filter(move |item| item.page == page)
    }

    /// 数量。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 加一个标注，返回分配到的 id；几何无效时返回 `None`（不插入）。
    pub fn add(&mut self, page: PageIndex, kind: AnnotationKind) -> Option<AnnotationId> {
        let mut annotation = Annotation::new(page, kind)?;
        self.next_id += 1;
        annotation.id = AnnotationId(self.next_id);
        let id = annotation.id;
        self.items.push(annotation);
        Some(id)
    }

    /// 删除一个标注；返回是否真的删掉了。
    pub fn remove(&mut self, id: AnnotationId) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != before
    }

    /// 按 id 取（命中测试用）。
    /// 按 id 取可变引用（加载时补 PDF 对象号用）。
    pub fn get_mut(&mut self, id: AnnotationId) -> Option<&mut Annotation> {
        self.items.iter_mut().find(|annotation| annotation.id == id)
    }

    /// 按 id 取不可变引用。
    pub fn get(&self, id: AnnotationId) -> Option<&Annotation> {
        self.items.iter().find(|item| item.id == id)
    }

    /// 改正文引用（提交时用；S3 的 `SetBody` 命令最终落到这里）。
    pub fn set_body(&mut self, id: AnnotationId, body: Option<String>) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.body = body;
                true
            }
            None => false,
        }
    }

    /// 清空（关闭文档时）。
    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 0;
    }

    /// 插入一个**已有 id** 的标注（撤销/重做恢复删除项时用）。
    ///
    /// id 必须大于当前 `next_id` 时才会推进计数器，保证"删除后不复用"。
    pub fn insert(&mut self, annotation: Annotation) {
        self.next_id = self.next_id.max(annotation.id.get());
        self.items.push(annotation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect::new(x, y, width, height)
    }

    #[test]
    fn rejects_degenerate_geometry() {
        let page = PageIndex::FIRST;
        assert!(Annotation::new(page, AnnotationKind::Highlight { quads: vec![] }).is_none());
        assert!(Annotation::new(
            page,
            AnnotationKind::Highlight {
                quads: vec![quad(10.0, 20.0, 0.0, 5.0)]
            }
        )
        .is_none());
        assert!(Annotation::new(
            page,
            AnnotationKind::Highlight {
                quads: vec![quad(10.0, 20.0, 50.0, 8.0)]
            }
        )
        .is_some());
    }

    #[test]
    fn bounds_union_and_contains_with_tolerance() {
        let page = PageIndex::FIRST;
        let annotation = Annotation::new(
            page,
            AnnotationKind::Highlight {
                quads: vec![quad(10.0, 20.0, 50.0, 8.0), quad(10.0, 30.0, 40.0, 8.0)],
            },
        )
        .expect("有效几何");
        let bounds = annotation.bounds().expect("包围盒");
        assert_eq!((bounds.x, bounds.y), (10.0, 20.0));
        assert_eq!((bounds.width, bounds.height), (50.0, 18.0));

        assert!(annotation.contains(Point::new(15.0, 25.0), 0.0));
        assert!(
            !annotation.contains(Point::new(15.0, 40.0), 0.0),
            "第二行下方不应命中"
        );
        assert!(
            annotation.contains(Point::new(15.0, 39.0), 2.0),
            "容差应当把边缘外的点算进来"
        );
    }

    #[test]
    fn ink_needs_two_finite_points_and_a_positive_width() {
        let page = PageIndex::FIRST;
        let two = || vec![Point::new(10.0, 20.0), Point::new(60.0, 28.0)];
        assert!(
            Annotation::new(
                page,
                AnnotationKind::Ink {
                    points: two(),
                    width: 4.0
                }
            )
            .is_some(),
            "两个点 + 正线宽是合法墨迹"
        );
        assert!(
            Annotation::new(
                page,
                AnnotationKind::Ink {
                    points: vec![Point::new(10.0, 20.0)],
                    width: 4.0
                }
            )
            .is_none(),
            "单点画不出折线"
        );
        assert!(
            Annotation::new(
                page,
                AnnotationKind::Ink {
                    points: two(),
                    width: 0.0
                }
            )
            .is_none(),
            "零线宽看不见，也不该入库"
        );
        assert!(
            Annotation::new(
                page,
                AnnotationKind::Ink {
                    points: two(),
                    width: f32::NAN
                }
            )
            .is_none(),
            "NaN 线宽会污染包围盒"
        );
        assert!(
            Annotation::new(
                page,
                AnnotationKind::Ink {
                    points: vec![Point::new(f32::NAN, 0.0), Point::new(1.0, 1.0)],
                    width: 2.0
                }
            )
            .is_none(),
            "NaN 顶点会污染包围盒与命中"
        );
    }

    #[test]
    fn ink_has_no_quads_but_has_an_ink_accessor() {
        let annotation = Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)],
                width: 2.0,
            },
        )
        .expect("有效墨迹");
        assert!(
            annotation.kind.quads().is_empty(),
            "墨迹没有行矩形；只处理 quads 的旧代码应当自然跳过它"
        );
        let (points, width) = annotation.kind.ink().expect("墨迹访问器");
        assert_eq!(points.len(), 2);
        assert_eq!(width, 2.0);
    }

    #[test]
    fn ink_bounds_include_half_the_stroke_width() {
        let annotation = Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![
                    Point::new(10.0, 20.0),
                    Point::new(60.0, 28.0),
                    Point::new(40.0, 50.0),
                ],
                width: 4.0,
            },
        )
        .expect("有效墨迹");
        let bounds = annotation.bounds().expect("包围盒");
        // 点集 x:10..60 y:20..50，外扩半个线宽（2）
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (8.0, 18.0, 54.0, 34.0)
        );
    }

    #[test]
    fn ink_hit_tests_the_polyline_not_the_bounding_box() {
        // L 形：横向 (0,0)→(100,0)，再纵向 (100,0)→(100,80)
        let annotation = Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![
                    Point::new(0.0, 0.0),
                    Point::new(100.0, 0.0),
                    Point::new(100.0, 80.0),
                ],
                width: 4.0,
            },
        )
        .expect("有效墨迹");

        assert!(
            !annotation.contains(Point::new(50.0, 8.0), 0.0),
            "半线宽是 2，离折线 8 的点不该命中"
        );
        assert!(
            annotation.contains(Point::new(50.0, 1.5), 0.0),
            "贴线应当命中"
        );
        assert!(
            annotation.contains(Point::new(50.0, 8.0), 9.0),
            "容差大于半线宽时按容差判定（命中逻辑是距离比较）"
        );

        // 包围盒内部、但离两条边都很远：绝不能命中（这是"包围盒当几何"的经典 bug）
        assert!(
            !annotation.contains(Point::new(50.0, 40.0), 0.0),
            "包围盒内的空白不算命中"
        );
        // 线段端点之外的外延区域
        assert!(
            !annotation.contains(Point::new(-10.0, 0.0), 0.0),
            "折线端点之外不命中"
        );
        assert!(
            annotation.contains(Point::new(-1.0, 0.0), 0.0),
            "端点处的半线宽内仍应命中"
        );
    }

    #[test]
    fn ink_defaults_are_an_opaque_pen_not_a_highlighter() {
        let page = PageIndex::FIRST;
        let ink = Annotation::new(
            page,
            AnnotationKind::Ink {
                points: vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)],
                width: 2.0,
            },
        )
        .expect("有效墨迹");
        assert_eq!((ink.color, ink.opacity), (INK_COLOR, INK_OPACITY));
        assert!(
            ink.clamped_opacity() >= 0.99,
            "墨迹必须不透明（半透明的黄看着像没画上）"
        );

        let highlight = Annotation::new(
            page,
            AnnotationKind::Highlight {
                quads: vec![quad(0.0, 0.0, 5.0, 5.0)],
            },
        )
        .expect("有效高亮");
        assert_eq!(
            (highlight.color, highlight.opacity),
            (TEXT_MARK_COLOR, TEXT_MARK_OPACITY),
            "高亮仍是半透明黄"
        );
        let underline = Annotation::new(
            page,
            AnnotationKind::Underline {
                quads: vec![quad(0.0, 0.0, 5.0, 5.0)],
            },
        )
        .expect("有效下划线");
        assert_eq!(underline.opacity, 1.0, "下划线/删除线默认不透明");
    }

    #[test]
    fn store_assigns_distinct_ids_and_never_reuses_them() {
        let mut store = AnnotationStore::new();
        let page = PageIndex::FIRST;
        let first = store
            .add(
                page,
                AnnotationKind::Highlight {
                    quads: vec![quad(0.0, 0.0, 10.0, 10.0)],
                },
            )
            .expect("第一条");
        let second = store
            .add(
                page,
                AnnotationKind::Underline {
                    quads: vec![quad(0.0, 0.0, 10.0, 2.0)],
                },
            )
            .expect("第二条");
        assert_ne!(first, second);
        assert_eq!(store.len(), 2);

        assert!(store.remove(first));
        assert!(!store.remove(first), "重复删除应当返回 false");
        let third = store
            .add(
                page,
                AnnotationKind::StrikeOut {
                    quads: vec![quad(0.0, 0.0, 10.0, 2.0)],
                },
            )
            .expect("第三条");
        assert!(
            third.get() > second.get(),
            "id 不得复用（撤销会放回旧 id，复用会撞号）"
        );
    }

    #[test]
    fn insert_keeps_next_id_ahead_of_restored_items() {
        let page = PageIndex::FIRST;
        let mut store = AnnotationStore::new();
        let mut restored = Annotation::new(
            page,
            AnnotationKind::Highlight {
                quads: vec![quad(0.0, 0.0, 5.0, 5.0)],
            },
        )
        .expect("有效几何");
        restored.id = AnnotationId(7);
        store.insert(restored);
        let next = store
            .add(
                page,
                AnnotationKind::Highlight {
                    quads: vec![quad(0.0, 0.0, 5.0, 5.0)],
                },
            )
            .expect("新标注");
        assert_eq!(
            next.get(),
            8,
            "恢复的 id 必须推进计数器，避免再分配同一个号"
        );
    }

    #[test]
    fn on_page_filters_by_page_and_body_can_be_set() {
        let mut store = AnnotationStore::new();
        let first = store
            .add(
                PageIndex::FIRST,
                AnnotationKind::Highlight {
                    quads: vec![quad(0.0, 0.0, 5.0, 5.0)],
                },
            )
            .expect("页 0");
        store
            .add(
                PageIndex::new(1),
                AnnotationKind::Highlight {
                    quads: vec![quad(0.0, 0.0, 5.0, 5.0)],
                },
            )
            .expect("页 1");

        assert_eq!(store.on_page(PageIndex::FIRST).count(), 1);
        assert_eq!(store.on_page(PageIndex::new(1)).count(), 1);
        assert_eq!(store.on_page(PageIndex::new(9)).count(), 0);

        assert!(store.set_body(first, Some("#annot:1".to_string())));
        assert_eq!(
            store.get(first).and_then(|item| item.body.as_deref()),
            Some("#annot:1")
        );
        assert!(
            !store.set_body(AnnotationId(99), None),
            "不存在的 id 返回 false"
        );
    }
}
