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
/// 三类的几何都是"若干四边形"（文本类标注天然按行分成多个矩形）；
/// 墨迹与选框在 S4/S5 再加，模型不预先为它们留空壳。
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
}

impl AnnotationKind {
    /// 该种类的行矩形。
    pub fn quads(&self) -> &[Rect] {
        match self {
            Self::Highlight { quads } | Self::Underline { quads } | Self::StrikeOut { quads } => {
                quads
            }
        }
    }
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
    /// 正文的引用（`custom_texts` 里的哨兵标题）；没有正文时为 `None`。
    pub body_title: Option<String>,
}

impl Annotation {
    /// 构造一个标注并做不变量检查（几何为空或退化时返回 `None`）。
    pub fn new(page: PageIndex, kind: AnnotationKind) -> Option<Self> {
        let quads = kind.quads();
        if quads.is_empty()
            || !quads
                .iter()
                .all(|quad| quad.width > 0.0 && quad.height > 0.0)
        {
            return None;
        }
        Some(Self {
            id: AnnotationId(0),
            page,
            kind,
            color: [255, 214, 0],
            opacity: 0.35,
            body_title: None,
        })
    }

    /// 覆盖全部行矩形的包围盒（页面空间）。
    pub fn bounds(&self) -> Option<Rect> {
        let mut quads = self.kind.quads().iter().copied();
        let first = quads.next()?;
        Some(quads.fold(first, |union, quad| union.union(quad)))
    }

    /// 点是否落在标注内（`tolerance` 是页面空间单位，由调用方按缩放换算）。
    pub fn contains(&self, point: Point, tolerance: f32) -> bool {
        let tolerance = tolerance.max(0.0);
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
    pub fn get(&self, id: AnnotationId) -> Option<&Annotation> {
        self.items.iter().find(|item| item.id == id)
    }

    /// 改正文引用（提交时用；S3 的 `SetBody` 命令最终落到这里）。
    pub fn set_body(&mut self, id: AnnotationId, body_title: Option<String>) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.body_title = body_title;
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
            store.get(first).and_then(|item| item.body_title.as_deref()),
            Some("#annot:1")
        );
        assert!(
            !store.set_body(AnnotationId(99), None),
            "不存在的 id 返回 false"
        );
    }
}
