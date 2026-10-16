//! 文本层适配：字符级提取与搜索。
//!
//! ## 关于不变量
//! pdfcore 要求 `PageText::text.chars().count() == PageText::chars.len()`。
//! PDFium 的 `PdfPageTextChar::unicode_char()` 对无法映射到单个 `char` 的字符返回 `None`，
//! 这里用 `U+FFFD` 占位以保证"一一对应"—— 宁可替换字符，也不能让选区整体错位。
//!
//! ## 关于搜索
//! M1 采用"逐字符扫描"实现，与 `pdfcore::testkit` 的假后端**同一算法**：
//! 这样两个后端在测试里行为一致，便于交叉验证。后续可换成 PDFium 的
//! `FPDFText_FindStart`（支持连字、全词等更精细语义）。

use std::sync::Arc;

use pdfcore::prelude::*;

use crate::document::{to_rect, SharedDocument};
use crate::error::to_core_error;

/// [`TextLayer`] 的 PDFium 实现。
pub struct PdfiumTextLayer(pub(crate) Arc<SharedDocument>);

impl TextLayer for PdfiumTextLayer {
    fn page_text(&self, page: PageIndex) -> Result<Arc<PageText>> {
        let pdf_page = self.0.page(page)?;
        let text = pdf_page.text().map_err(to_core_error)?;

        let mut buffer = String::new();
        let mut chars = Vec::with_capacity(text.len().max(0) as usize);
        for character in text.chars().iter() {
            let ch = character.unicode_char().unwrap_or('\u{FFFD}');
            // 包围盒在 PDF 页面空间（左下原点）—— 与 pdfcore 的约定一致，不做任何翻转
            let bbox = character.tight_bounds().map(to_rect).unwrap_or(Rect::ZERO);
            buffer.push(ch);
            chars.push(TextChar { ch, bbox, origin: Point::new(bbox.x, bbox.y), span: 0 });
        }

        let spans = single_span(chars.len());
        Ok(Arc::new(PageText { page, text: buffer, chars, spans }))
    }

    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        if query.needle.is_empty() {
            return Ok(Vec::new());
        }
        let needle: Vec<char> = if query.case_sensitive {
            query.needle.chars().collect()
        } else {
            query.needle.to_lowercase().chars().collect()
        };

        let start_page = query.start_page.unwrap_or(PageIndex::FIRST).get() as usize;
        let mut hits = Vec::new();
        for index in start_page..self.0.geometries.len() {
            let page = PageIndex::new(index as u32);
            let text = self.page_text(page)?;
            let haystack: Vec<char> = if query.case_sensitive {
                text.text.chars().collect()
            } else {
                text.text.to_lowercase().chars().collect()
            };
            if needle.len() > haystack.len() {
                continue;
            }
            for start in 0..=(haystack.len() - needle.len()) {
                if haystack[start..start + needle.len()] != needle[..] {
                    continue;
                }
                if query.whole_word {
                    let before_ok = start == 0 || !haystack[start - 1].is_alphanumeric();
                    let after = start + needle.len();
                    let after_ok = after >= haystack.len() || !haystack[after].is_alphanumeric();
                    if !before_ok || !after_ok {
                        continue;
                    }
                }
                let range = TextRange::new(start as u32, (start + needle.len()) as u32);
                hits.push(SearchHit { page, range, rects: text.rects_for_range(range) });
                if let Some(limit) = query.limit {
                    if hits.len() >= limit {
                        return Ok(hits);
                    }
                }
            }
        }
        Ok(hits)
    }
}

/// 目前只给一个覆盖全页的段。
///
/// 字号与逐段样式尚未提取（留待 M4）：`TextSpan` 只是给 UI 的样式提示，
/// 而字符包围盒才是选中与高亮所依赖的数据。
fn single_span(total: usize) -> Vec<TextSpan> {
    if total == 0 {
        Vec::new()
    } else {
        vec![TextSpan { range: TextRange::new(0, total as u32), font_size: 0.0 }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_covers_all_chars() {
        assert!(single_span(0).is_empty());
        let spans = single_span(5);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, TextRange::new(0, 5));
    }
}
