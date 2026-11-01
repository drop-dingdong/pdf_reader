//! 文本层：字符级文本、选区模型、全文搜索。
//!
//! ## 为什么文本层要单独建模
//! PDF 里的文字**不是**按行存的：它是一堆带变换矩阵的文字绘制指令。渲染成位图很容易，
//! 但"选中、复制、搜索"必须回到字符级信息（每个字符的 Unicode 值与包围盒）。
//! 这部分能力不是所有后端都有（例如 hayro 就没有），因此它是**可选零件**：
//! [`crate::document::OpenedDocument::text`] 为 `None` 时，UI 直接隐藏搜索与复制入口。
//!
//! ## 坐标约定
//! [`PageText`] 里所有包围盒都在 **PDF 页面空间**（左下原点、y 向上、point）。
//! 要画到屏幕上，必须先经 [`crate::layout::PagePlacement`] 换算 —— 这条边界不清会让
//! 高亮框整体上下颠倒。

use std::sync::Arc;

use crate::error::{CoreError, Result};
use crate::geometry::{Point, Rect};
use crate::ids::PageIndex;

/// 页内字符区间，**半开区间** `[start, end)`，下标单位为**字符**（不是字节）。
///
/// 用字符下标而不是字节下标的理由：文本层的包围盒是逐字符给出的，
/// 而 `String` 的字节下标在多字节文本（中文、emoji）里极易切碎字符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextRange {
    /// 起始字符下标（含）。
    pub start: u32,
    /// 结束字符下标（不含）。
    pub end: u32,
}

impl TextRange {
    /// 构造；若传入顺序颠倒会自动交换，保证 `start <= end`。
    pub fn new(start: u32, end: u32) -> Self {
        if start <= end {
            Self { start, end }
        } else {
            Self {
                start: end,
                end: start,
            }
        }
    }

    /// 空区间。
    pub const fn empty() -> Self {
        Self { start: 0, end: 0 }
    }

    /// 长度（字符数）。
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// 是否为空。
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// 是否包含某字符下标。
    pub fn contains(self, index: u32) -> bool {
        index >= self.start && index < self.end
    }

    /// 取并集（用于把两次选择合并）。
    pub fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// 文档中的一个字符位置（页号 + 页内字符下标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    /// 所在页。
    pub page: PageIndex,
    /// 页内字符下标。
    pub index: u32,
}

impl TextPosition {
    /// 构造。
    pub const fn new(page: PageIndex, index: u32) -> Self {
        Self { page, index }
    }
}

/// 选区：由锚点与焦点组成，允许反向拖拽（从后往前选）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// 起始锚点（按下鼠标的位置）。
    pub anchor: TextPosition,
    /// 当前焦点（拖拽到的位置）。
    pub focus: TextPosition,
}

impl Selection {
    /// 构造。
    pub const fn new(anchor: TextPosition, focus: TextPosition) -> Self {
        Self { anchor, focus }
    }

    /// 归一化：返回 `(起点, 终点)`，起点在文档顺序上不晚于终点。
    pub fn normalized(self) -> (TextPosition, TextPosition) {
        if (self.anchor.page, self.anchor.index) <= (self.focus.page, self.focus.index) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 是否为空选区（单击未拖动，或拖动距离为 0）。
    pub fn is_empty(self) -> bool {
        self.anchor == self.focus
    }

    /// 展开成逐页的字符区间，供高亮与复制使用。
    ///
    /// 中间页用 `end = u32::MAX` 表示"到页末"，具体长度由 [`PageText::chars_in`] 裁剪，
    /// 这样调用方不必预先知道每页有多少字符。
    pub fn page_ranges(self) -> Vec<(PageIndex, TextRange)> {
        let (start, end) = self.normalized();
        let mut out = Vec::new();
        if start.page == end.page {
            out.push((start.page, TextRange::new(start.index, end.index)));
            return out;
        }
        out.push((start.page, TextRange::new(start.index, u32::MAX)));
        for page in (start.page.0 + 1)..end.page.0 {
            out.push((PageIndex(page), TextRange::new(0, u32::MAX)));
        }
        out.push((end.page, TextRange::new(0, end.index)));
        out
    }
}

/// 选择粒度：双击选词、三击选行。
///
/// 粒度**不由** [`TextSelection`] 实现计算，而是由调用方用 [`PageText::range_at`]
/// 先把鼠标位置展开成字符区间再交给选区模型 —— 这样选区模型完全不依赖文档内容，可以被单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionGranularity {
    /// 单字符。
    #[default]
    Char,
    /// 单词（双击）。
    Word,
    /// 整行（三击）。
    Line,
}

/// 一个字符及其位置。
#[derive(Debug, Clone, PartialEq)]
pub struct TextChar {
    /// 字符本身。
    pub ch: char,
    /// 包围盒（PDF 页面空间）。后端生成的合成字符（如自动连字符）可能为零矩形。
    pub bbox: Rect,
    /// 基线起点。
    pub origin: Point,
    /// 所属 [`TextSpan`] 的下标。
    pub span: u32,
}

/// 一段样式一致的文本（同一字体、同一字号）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpan {
    /// 该段覆盖的字符区间。
    pub range: TextRange,
    /// 字号（point）。
    pub font_size: f32,
}

/// 一页的文本层。
///
/// **不变量**：`text.chars().count() == chars.len()`，即 [`TextChar`] 与 `text` 的字符一一对应。
/// 后端实现必须保证这一点（缺少位置的字符也要带一个零包围盒占位），否则选区会整体错位。
#[derive(Debug, Clone, PartialEq)]
pub struct PageText {
    /// 页号。
    pub page: PageIndex,
    /// 该页的纯文本（含后端给出的换行）。
    pub text: String,
    /// 逐字符信息。
    pub chars: Vec<TextChar>,
    /// 样式分段。
    pub spans: Vec<TextSpan>,
}

impl PageText {
    /// 字符数。
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// 是否没有文本（扫描件、纯图片页）。
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// 取区间内的字符（越界自动裁剪）。
    pub fn chars_in(&self, range: TextRange) -> &[TextChar] {
        let len = self.chars.len() as u32;
        let start = range.start.min(len) as usize;
        let end = range.end.min(len) as usize;
        if start >= end {
            &[]
        } else {
            &self.chars[start..end]
        }
    }

    /// 取区间文本。
    pub fn text_for_range(&self, range: TextRange) -> String {
        self.chars_in(range).iter().map(|c| c.ch).collect()
    }

    /// 与给定页面矩形相交的字符范围（取最小..最大下标）。
    ///
    /// 用途：在页面上框选一块区域 → 转成文本范围 → 复用 [`Self::rects_for_range`] 的
    /// 按行合并。标注与选字因此共用同一套几何算法，不会在旋转页上分叉。
    pub fn range_for_page_rect(&self, rect: Rect) -> Option<TextRange> {
        let mut start = None;
        let mut end = None;
        for (index, char) in self.chars.iter().enumerate() {
            if !char.bbox.intersects(rect) {
                continue;
            }
            start.get_or_insert(index);
            end = Some(index);
        }
        match (start, end) {
            (Some(start), Some(end)) => Some(TextRange::new(start as u32, end as u32 + 1)),
            _ => None,
        }
    }

    /// 取区间的高亮矩形：**按行合并**，避免一个字符一个矩形导致 UI 抖动与绘制开销。
    pub fn rects_for_range(&self, range: TextRange) -> Vec<Rect> {
        let mut out: Vec<Rect> = Vec::new();
        for ch in self.chars_in(range) {
            if ch.bbox.is_empty() {
                continue; // 换行符与合成字符没有可见范围
            }
            // 先尝试并入上一行；失败则在 match 之外 push，避免与 last_mut 的借用冲突。
            let merged = match out.last_mut() {
                Some(last) if vertical_overlap(*last, ch.bbox) => {
                    *last = last.union(ch.bbox);
                    true
                }
                _ => false,
            };
            if !merged {
                out.push(ch.bbox);
            }
        }
        out
    }

    /// 框选：返回与矩形相交的字符区间（按字符中心是否落在矩形内判断，符合直觉）。
    pub fn range_for_rect(&self, rect: Rect) -> Option<TextRange> {
        let mut first: Option<u32> = None;
        let mut last: Option<u32> = None;
        for (index, ch) in self.chars.iter().enumerate() {
            if ch.bbox.is_empty() {
                continue;
            }
            if rect.contains(ch.bbox.center()) {
                let index = index as u32;
                first.get_or_insert(index);
                last = Some(index);
            }
        }
        match (first, last) {
            (Some(start), Some(end)) => Some(TextRange::new(start, end + 1)),
            _ => None,
        }
    }

    /// 命中测试：把页面坐标转成字符下标。
    ///
    /// 先找包围盒真正包含该点的字符；若没有（点在字距或行距之间），退化为
    /// **附近**最近字符的中心 —— "附近"的容差是字符高度的一半。
    ///
    /// 容差是必需的：没有它，点在页面空白/页边距上也会命中"全页离得最近的那个字"，
    /// 于是"拖拽选字"会在任何地方都生效、无法平移画面（第二轮验收反馈的"自动模式不正常"）。
    pub fn hit_test(&self, point: Point) -> Option<u32> {
        let mut nearest: Option<(f32, u32)> = None;
        for (index, ch) in self.chars.iter().enumerate() {
            if ch.bbox.is_empty() {
                continue;
            }
            if ch.bbox.contains(point) {
                return Some(index as u32);
            }
            // 容差随字号缩放（小字号页面上也不至于把远处的字算进来）
            let tolerance = (ch.bbox.height * 0.5).max(2.0);
            let expanded = Rect::new(
                ch.bbox.x - tolerance,
                ch.bbox.y - tolerance,
                ch.bbox.width + tolerance * 2.0,
                ch.bbox.height + tolerance * 2.0,
            );
            if !expanded.contains(point) {
                continue;
            }
            let distance = ch.bbox.center().distance(point);
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, index as u32));
            }
        }
        nearest.map(|(_, index)| index)
    }

    /// 单词区间（双击选词）。
    ///
    /// 词的定义是"连续的字母/数字/下划线"（CJK 汉字属于字母类，因此一整串汉字会被视为一个词，
    /// 这与主流阅读器一致）。
    pub fn word_range_at(&self, index: u32) -> TextRange {
        if self.chars.is_empty() {
            return TextRange::empty();
        }
        let index = (index as usize).min(self.chars.len() - 1);
        if !is_word_char(self.chars[index].ch) {
            return TextRange::new(index as u32, index as u32 + 1);
        }
        let mut start = index;
        while start > 0 && is_word_char(self.chars[start - 1].ch) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < self.chars.len() && is_word_char(self.chars[end].ch) {
            end += 1;
        }
        TextRange::new(start as u32, end as u32)
    }

    /// 整行区间（三击选行）。
    ///
    /// 实现是**启发式**的：以目标字符的包围盒为基准，向左右扩展到"纵向不再重叠"为止。
    /// 之所以不引入完整的行结构，是因为 PDF 本身没有行概念，不同的行判定算法各有取舍；
    /// 这里的简化对"同一水平线上的文本"已经足够，且零额外内存。
    pub fn line_range_at(&self, index: u32) -> TextRange {
        if self.chars.is_empty() {
            return TextRange::empty();
        }
        let index = (index as usize).min(self.chars.len() - 1);
        let reference = self.chars[index].bbox;
        if reference.is_empty() {
            return TextRange::new(index as u32, index as u32 + 1);
        }
        let mut start = index;
        while start > 0 {
            let previous = self.chars[start - 1].bbox;
            if previous.is_empty() || !vertical_overlap(previous, reference) {
                break;
            }
            start -= 1;
        }
        let mut end = index + 1;
        while end < self.chars.len() {
            let next = self.chars[end].bbox;
            if next.is_empty() || !vertical_overlap(next, reference) {
                break;
            }
            end += 1;
        }
        TextRange::new(start as u32, end as u32)
    }

    /// 按粒度取区间：调用方（会话层）在 `TextSelection::begin/extend` 之前调用它，
    /// 于是选区模型只需做区间记账。
    pub fn range_at(&self, index: u32, granularity: SelectionGranularity) -> TextRange {
        match granularity {
            SelectionGranularity::Char => TextRange::new(index, index.saturating_add(1)),
            SelectionGranularity::Word => self.word_range_at(index),
            SelectionGranularity::Line => self.line_range_at(index),
        }
    }
}

/// 高亮类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// 当前选区。
    Selection,
    /// 搜索命中。
    SearchMatch,
    /// 当前跳转到的搜索命中（用更强的颜色）。
    ActiveSearchMatch,
}

/// 一块高亮：某一页上的若干矩形。
///
/// 选区与搜索命中共用此类型，UI 只需写一次绘制逻辑。
#[derive(Debug, Clone, PartialEq)]
pub struct Highlight {
    /// 页号。
    pub page: PageIndex,
    /// 矩形（PDF 页面空间，需经布局换算后绘制）。
    pub rects: Vec<Rect>,
    /// 类型，决定配色。
    pub kind: HighlightKind,
}

/// 全文搜索条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// 关键词。
    pub needle: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 是否全词匹配。
    pub whole_word: bool,
    /// 从哪一页开始搜（`None` 表示从头）。
    pub start_page: Option<PageIndex>,
    /// 最多返回多少条命中（`None` 表示不限，但后端应有内部上限）。
    pub limit: Option<usize>,
}

impl SearchQuery {
    /// 构造一个默认条件（不区分大小写、非全词、不限页）。
    pub fn new(needle: impl Into<String>) -> Self {
        Self {
            needle: needle.into(),
            case_sensitive: false,
            whole_word: false,
            start_page: None,
            limit: None,
        }
    }
}

/// 一条搜索命中。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// 命中所在页。
    pub page: PageIndex,
    /// 命中的字符区间。
    pub range: TextRange,
    /// 命中矩形（可多行）。
    pub rects: Vec<Rect>,
}

/// **文本层能力**：字符级文本提取与全文搜索。
///
/// ## 这个 trait 存在的目的
/// * 让"选中/复制/搜索"这三件事与具体 PDF 库解耦：PDFium 用它现成的 `FPDFText_*`，
///   未来自研后端可以从头实现而不影响 UI；
/// * 让能力可缺省：后端只做渲染时，[`crate::document::OpenedDocument::text`] 直接为 `None`，
///   UI 据此隐藏搜索框 —— 不需要任何 `if backend == "hayro"` 之类的分支。
///
/// ## 实现者义务
/// * 必须保证 [`PageText`] 的"字符一一对应"不变量；
/// * 返回的包围盒必须在 **PDF 页面空间**（左下原点），否则高亮会整体错位；
/// * 文本提取可能很慢（大页、复杂字体），实现应自带缓存，或由会话层缓存
///   （[`crate::session::DocumentSession`] 默认在会话内缓存页文本）。
pub trait TextLayer: Send + Sync {
    /// 取某页文本层。实现应保证重复调用不会重复付出解析代价（内部缓存或依赖会话层缓存）。
    fn page_text(&self, page: PageIndex) -> Result<Arc<PageText>>;

    /// 全文搜索。
    ///
    /// 默认实现返回"不支持"：这使只做渲染、或只做文本提取的后端**只需实现一个方法**，
    /// 而 UI 通过 [`crate::document::Capabilities::search`] 提前知道不要展示搜索框。
    fn search(&self, _query: &SearchQuery) -> Result<Vec<SearchHit>> {
        Err(CoreError::unsupported("全文搜索"))
    }
}

/// **选区模型**：鼠标交互 → 选中区间 → 高亮矩形 / 可复制文本。
///
/// ## 这个 trait 存在的目的
/// 把"选中状态机"从 UI 事件处理与文档内容中剥离出来：
/// * 它**不含文档内容**，只记两个字符位置，因此可以被 100% 单测覆盖（含反向拖拽、跨页选择）；
/// * UI 只负责把鼠标坐标交给 [`PageText::hit_test`]，再把结果交给本 trait；
/// * 高亮与复制有默认实现，基于 `selection()` + 调用方提供的"取页文本"闭包，
///   因此实现者最少只需写 5 个方法。
///
/// ## 实现者义务
/// * `begin` 重置选择、`extend` 更新焦点；两者接收的 `position` 应已由调用方按
///   [`SelectionGranularity`] 展开（见 [`PageText::range_at`]）。
/// * 必须是 `Send`（会话可能被移动到其它线程），但不需要 `Sync`（选区只属于 UI 线程）。
pub trait TextSelection: Send {
    /// 当前粒度。
    fn granularity(&self) -> SelectionGranularity;

    /// 设置粒度（双击/三击时由 UI 调用）。
    fn set_granularity(&mut self, granularity: SelectionGranularity);

    /// 开始新选择。
    fn begin(&mut self, position: TextPosition);

    /// 扩展到新位置（拖拽中反复调用）。
    fn extend(&mut self, position: TextPosition);

    /// 清空选择（单击空白、Esc）。
    fn clear(&mut self);

    /// 当前选择；无选择时为 `None`。
    fn selection(&self) -> Option<Selection>;

    /// 是否没有有效选择。
    fn is_empty(&self) -> bool {
        self.selection()
            .is_none_or(|selection| selection.is_empty())
    }

    /// 计算高亮矩形。
    ///
    /// 默认实现：把选择展开成逐页区间，逐页取 [`PageText::rects_for_range`]。
    /// 尚未加载文本的页会被跳过（UI 可先请求该页文本再重算）。
    fn highlight(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> Vec<Highlight> {
        let Some(selection) = self.selection() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (page, range) in selection.page_ranges() {
            let Some(text) = text_of(page) else {
                continue;
            };
            let rects = text.rects_for_range(range);
            if !rects.is_empty() {
                out.push(Highlight {
                    page,
                    rects,
                    kind: HighlightKind::Selection,
                });
            }
        }
        out
    }

    /// 取选中文本，用于复制到剪贴板。跨页处以换行连接。
    fn selected_text(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> String {
        let Some(selection) = self.selection() else {
            return String::new();
        };
        let mut out = String::new();
        for (page, range) in selection.page_ranges() {
            let Some(text) = text_of(page) else {
                continue;
            };
            let piece = text.text_for_range(range);
            if piece.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&piece);
        }
        out
    }
}

/// [`TextSelection`] 的默认实现：只记两个位置的极简状态机。
///
/// MVP 直接用它即可；将来若需要"以词为单位的增量拖拽""选择记忆"等行为，
/// 可以再写一个实现替换，而不需要改动 UI。
#[derive(Debug, Clone, Default)]
pub struct RangeSelection {
    granularity: SelectionGranularity,
    anchor: Option<TextPosition>,
    focus: Option<TextPosition>,
}

impl RangeSelection {
    /// 新建（粒度为 [`SelectionGranularity::Char`]）。
    pub fn new() -> Self {
        Self::default()
    }
}

impl TextSelection for RangeSelection {
    fn granularity(&self) -> SelectionGranularity {
        self.granularity
    }

    fn set_granularity(&mut self, granularity: SelectionGranularity) {
        self.granularity = granularity;
    }

    fn begin(&mut self, position: TextPosition) {
        self.anchor = Some(position);
        self.focus = Some(position);
    }

    fn extend(&mut self, position: TextPosition) {
        if self.anchor.is_none() {
            self.anchor = Some(position);
        }
        self.focus = Some(position);
    }

    fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
    }

    fn selection(&self) -> Option<Selection> {
        match (self.anchor, self.focus) {
            (Some(anchor), Some(focus)) => Some(Selection::new(anchor, focus)),
            _ => None,
        }
    }
}

/// 两个矩形在纵向上是否重叠（用于判定"同一行"）。
fn vertical_overlap(a: Rect, b: Rect) -> bool {
    a.y < b.bottom() && b.y < a.bottom()
}

/// 是否为"词字符"。
///
/// 用 `is_alphanumeric` 而非 ASCII 判断，才能让中文、日文、俄文都被正确当作词字符。
/// 代价是"一整串连续汉字"会被视为一个词，这与主流阅读器的双击行为一致。
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一页合成文本：每行字符宽 6、高 10、行距 12；行间插入带空包围盒的 `\n`。
    fn make_page(page: u32, lines: &[&str]) -> PageText {
        let mut text = String::new();
        let mut chars = Vec::new();
        for (line_index, line) in lines.iter().enumerate() {
            if line_index > 0 {
                text.push('\n');
                chars.push(TextChar {
                    ch: '\n',
                    bbox: Rect::ZERO,
                    origin: Point::ORIGIN,
                    span: 0,
                });
            }
            for (char_index, ch) in line.chars().enumerate() {
                text.push(ch);
                let x = char_index as f32 * 6.0;
                let y = line_index as f32 * 12.0;
                chars.push(TextChar {
                    ch,
                    bbox: Rect::new(x, y, 6.0, 10.0),
                    origin: Point::new(x, y + 10.0),
                    span: 0,
                });
            }
        }
        let len = chars.len() as u32;
        PageText {
            page: PageIndex(page),
            text,
            chars,
            spans: vec![TextSpan {
                range: TextRange::new(0, len),
                font_size: 10.0,
            }],
        }
    }

    #[test]
    fn char_index_matches_text_invariant() {
        let page = make_page(0, &["Hello world", "Second line"]);
        assert_eq!(page.text.chars().count(), page.chars.len());
        assert_eq!(page.len(), 23);
        assert_eq!(page.text_for_range(TextRange::new(0, 5)), "Hello");
        assert_eq!(page.text_for_range(TextRange::new(12, 18)), "Second");
    }

    #[test]
    fn word_and_line_ranges() {
        let page = make_page(0, &["Hello world", "Second line"]);
        assert_eq!(page.word_range_at(1), TextRange::new(0, 5));
        assert_eq!(page.word_range_at(7), TextRange::new(6, 11));
        assert_eq!(
            page.word_range_at(5),
            TextRange::new(5, 6),
            "空格自身成区间"
        );
        assert_eq!(
            page.line_range_at(1),
            TextRange::new(0, 11),
            "换行符终止整行"
        );
        assert_eq!(page.line_range_at(13), TextRange::new(12, 23));
        assert_eq!(
            page.range_at(7, SelectionGranularity::Word),
            TextRange::new(6, 11)
        );
        assert_eq!(
            page.range_at(7, SelectionGranularity::Char),
            TextRange::new(7, 8)
        );
    }

    #[test]
    fn highlight_merges_per_line() {
        let page = make_page(0, &["Hello world", "Second line"]);
        let rects = page.rects_for_range(TextRange::new(0, 11));
        assert_eq!(rects.len(), 1, "同一行应合并为一个矩形");
        assert_eq!(rects[0], Rect::new(0.0, 0.0, 66.0, 10.0));

        let rects = page.rects_for_range(TextRange::new(0, 23));
        assert_eq!(rects.len(), 2, "跨行应产生两个矩形");
        assert_eq!(rects[1], Rect::new(0.0, 12.0, 66.0, 10.0));
    }

    #[test]
    fn hit_test_and_rect_selection() {
        let page = make_page(0, &["Hello world", "Second line"]);
        assert_eq!(page.hit_test(Point::new(3.0, 5.0)), Some(0));
        assert_eq!(page.hit_test(Point::new(9.0, 5.0)), Some(1));
        // 点在字距间的空白处，退化为最近的字符（容差 = 字高一半）
        assert!(page.hit_test(Point::new(5.9, 5.0)).is_some());
        // 点在页面空白处（第二行下方很远处）必须返回 None：
        // 否则"拖拽选字"会在任何地方都生效，无法平移画面
        assert_eq!(
            page.hit_test(Point::new(3.0, 200.0)),
            None,
            "远离文字的空白点不应命中"
        );
        assert_eq!(
            page.hit_test(Point::new(-50.0, 5.0)),
            None,
            "页外点不应命中"
        );
        // 框选前两个字符
        assert_eq!(
            page.range_for_rect(Rect::new(0.0, 0.0, 11.0, 10.0)),
            Some(TextRange::new(0, 2))
        );
        // 框住整页
        assert_eq!(
            page.range_for_rect(Rect::new(-1.0, -1.0, 100.0, 100.0)),
            Some(TextRange::new(0, 23))
        );
    }

    #[test]
    fn selection_across_pages_is_ordered_and_joined() {
        let page0 = Arc::new(make_page(0, &["Hello world"]));
        let page1 = Arc::new(make_page(1, &["Second line"]));
        let lookup = |page: PageIndex| -> Option<Arc<PageText>> {
            match page.get() {
                0 => Some(page0.clone()),
                1 => Some(page1.clone()),
                _ => None,
            }
        };

        let mut selection = RangeSelection::new();
        // 反向拖拽：先点第二页，再拖回第一页 —— 顺序必须被归一化
        selection.begin(TextPosition::new(PageIndex::new(1), 6));
        selection.extend(TextPosition::new(PageIndex::new(0), 0));

        let ranges = selection.selection().unwrap().page_ranges();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], (PageIndex::new(0), TextRange::new(0, u32::MAX)));
        assert_eq!(ranges[1], (PageIndex::new(1), TextRange::new(0, 6)));

        let highlights = selection.highlight(&lookup);
        assert_eq!(highlights.len(), 2);
        assert_eq!(highlights[0].kind, HighlightKind::Selection);
        assert_eq!(selection.selected_text(&lookup), "Hello world\nSecond");
    }

    #[test]
    fn selection_within_one_page() {
        let page0 = Arc::new(make_page(0, &["Hello world"]));
        let lookup = |page: PageIndex| -> Option<Arc<PageText>> {
            if page == PageIndex::new(0) {
                Some(page0.clone())
            } else {
                None
            }
        };
        let mut selection = RangeSelection::new();
        selection.set_granularity(SelectionGranularity::Word);
        let range = page0.range_at(7, selection.granularity());
        selection.begin(TextPosition::new(PageIndex::new(0), range.start));
        selection.extend(TextPosition::new(PageIndex::new(0), range.end));
        assert_eq!(selection.selected_text(&lookup), "world");
        assert_eq!(selection.highlight(&lookup).len(), 1);
        selection.clear();
        assert!(selection.is_empty());
        assert!(selection.selected_text(&lookup).is_empty());
    }

    #[test]
    fn range_helpers() {
        assert_eq!(TextRange::new(5, 2), TextRange::new(2, 5), "自动归一化");
        assert_eq!(TextRange::new(2, 5).len(), 3);
        assert!(TextRange::new(2, 5).contains(4));
        assert!(!TextRange::new(2, 5).contains(5));
        assert_eq!(
            TextRange::new(0, 2).union(TextRange::new(5, 9)),
            TextRange::new(0, 9)
        );
    }
}

#[cfg(test)]
mod annotation_range_tests {
    use super::*;
    use crate::geometry::Rect;

    fn page_with(chars: &[Rect]) -> PageText {
        PageText {
            page: PageIndex::FIRST,
            text: "abc".to_string(),
            chars: chars
                .iter()
                .map(|bbox| TextChar {
                    ch: 'a',
                    bbox: *bbox,
                    origin: Point::new(bbox.x, bbox.y),
                    span: 0,
                })
                .collect(),
            spans: Vec::new(),
        }
    }

    #[test]
    fn rect_selects_intersecting_characters_and_merges_per_line() {
        let page = page_with(&[
            Rect::new(0.0, 80.0, 5.0, 10.0),
            Rect::new(6.0, 80.0, 5.0, 10.0),
            Rect::new(0.0, 60.0, 5.0, 10.0),
        ]);
        // 只框住第一行两个字符
        let range = page
            .range_for_page_rect(Rect::new(-1.0, 78.0, 20.0, 14.0))
            .expect("应当命中第一行");
        assert_eq!((range.start, range.end), (0, 2));
        assert_eq!(page.rects_for_range(range).len(), 1);

        // 框住两行 → 两个矩形
        let range = page
            .range_for_page_rect(Rect::new(-1.0, 58.0, 20.0, 40.0))
            .expect("应当命中两行");
        assert_eq!((range.start, range.end), (0, 3));
        assert_eq!(page.rects_for_range(range).len(), 2);

        // 空区域
        assert!(page
            .range_for_page_rect(Rect::new(200.0, 200.0, 10.0, 10.0))
            .is_none());
    }
}
