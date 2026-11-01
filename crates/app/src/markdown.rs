//! 文字浮窗的 Markdown 预处理。
//!
//! `egui_commonmark` 遵循 CommonMark：段落里的单个换行是**软换行**，渲染成一个空格；
//! 只有行尾两个空格或反斜杠才是硬换行。注释/便签/文字预览的内容通常是用户直接
//! 按 Enter 写出的多行文本，没有 Markdown 的硬换行标记。这里在交给渲染器之前，
//! 把代码块、数学公式之外的单换行改写成硬换行，保留用户看到的行结构。
//!
//! 只用于浮窗正文；不影响侧栏、导出摘要或写回 PDF 的源文本。

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// 与 `egui_commonmark` 的 Markdown 解析选项保持一致（暂不启用数学）。
fn parser_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
}

/// 把段落内的单个换行转成硬换行（行尾补两个空格）。
///
/// * 代码块内的换行由解析器当作代码文本，不会产生 `SoftBreak`，因此不受影响；
/// * 空行是段落分隔，不产生 `SoftBreak`，保持原样；
/// * 已经是硬换行（两个空格或反斜杠）的源码也不会再处理。
pub fn preserve_single_line_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut copied = 0usize;
    for (event, span) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        if !matches!(event, Event::SoftBreak) {
            continue;
        }
        let at = span.start.min(text.len());
        if at < copied || !text.is_char_boundary(at) {
            continue;
        }
        out.push_str(&text[copied..at]);
        out.push_str("  ");
        copied = at;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::preserve_single_line_breaks;

    #[test]
    fn single_newline_becomes_a_hard_break() {
        assert_eq!(
            preserve_single_line_breaks("第一行\n第二行"),
            "第一行  \n第二行"
        );
    }

    #[test]
    fn blank_line_stays_a_paragraph_break() {
        let source = "第一段\n\n第二段";
        assert_eq!(preserve_single_line_breaks(source), source);
    }

    #[test]
    fn existing_hard_breaks_are_untouched() {
        let spaces = "第一行  \n第二行";
        assert_eq!(preserve_single_line_breaks(spaces), spaces);
        let backslash = "第一行\\\n第二行";
        assert_eq!(preserve_single_line_breaks(backslash), backslash);
    }

    #[test]
    fn code_blocks_keep_their_newlines() {
        let source = "```text\na\nb\n```";
        assert_eq!(preserve_single_line_breaks(source), source);
    }

    #[test]
    fn list_markers_are_not_turned_into_breaks() {
        let source = "- a\n- b\n- c";
        assert_eq!(preserve_single_line_breaks(source), source);
    }

    #[test]
    fn empty_text_is_empty() {
        assert_eq!(preserve_single_line_breaks(""), "");
    }
}

/// 估算 Markdown 正文中**不方便重排**内容的最小宽度（单位与 `measure` 一致）。
///
/// 只统计：
/// * 代码块（等宽字体，按最长行）；
/// * 表格（按每列最宽单元格求和；渲染器内部还会自己横向滚动）；
/// * 段落里含 ASCII 字母数字的“不可断 token”（长 URL 等）。
///
/// CJK 句子没有空格，不能按整句当不可断内容，否则正文会被撑成一行；这类文本交给
/// 渲染器按字符换行。`measure(text, monospace)` 由调用方提供（用真实字体布局）。
pub fn min_unwrapped_width(text: &str, mut measure: impl FnMut(&str, bool) -> f32) -> f32 {
    let mut max = 0.0f32;
    let mut in_code = false;
    let mut code = String::new();
    let mut in_table = false;
    let mut cell = String::new();
    let mut column = 0usize;
    let mut columns: Vec<f32> = Vec::new();

    for (event, _) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                in_code = true;
                code.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                for line in code.lines() {
                    max = max.max(measure(line, true));
                }
                in_code = false;
            }
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                columns.clear();
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                column = 0;
            }
            Event::Start(Tag::TableCell) => cell.clear(),
            Event::End(TagEnd::TableCell) => {
                let width = measure(&cell, false);
                if column < columns.len() {
                    columns[column] = columns[column].max(width);
                } else {
                    columns.push(width);
                }
                column += 1;
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                max = max.max(columns.iter().sum());
            }
            Event::End(TagEnd::Table) => in_table = false,
            Event::Text(text) => {
                if in_code {
                    code.push_str(&text);
                } else if in_table {
                    cell.push_str(&text);
                } else {
                    for token in text.split_whitespace() {
                        if token.chars().any(|ch| ch.is_ascii_alphanumeric()) {
                            max = max.max(measure(token, false));
                        }
                    }
                }
            }
            Event::Code(text) => {
                if in_code {
                    code.push_str(&text);
                } else if in_table {
                    cell.push_str(&text);
                } else {
                    max = max.max(measure(&text, true));
                }
            }
            _ => {}
        }
    }
    max
}

#[cfg(test)]
mod width_tests {
    use super::min_unwrapped_width;

    fn fake(text: &str, monospace: bool) -> f32 {
        text.chars().count() as f32 * if monospace { 8.0 } else { 7.0 }
    }

    #[test]
    fn code_block_uses_the_longest_line() {
        let text = "```\n123456\nab\n```";
        assert_eq!(min_unwrapped_width(text, fake), 48.0);
    }

    #[test]
    fn table_uses_the_widest_cell_per_column() {
        let text = "| abc | de |\n|---|---|\n| f | ghij |";
        // 列宽：max(3,1)=3，max(2,4)=4 → (3+4)*7
        assert_eq!(min_unwrapped_width(text, fake), 49.0);
    }

    #[test]
    fn cjk_prose_is_not_treated_as_one_unbreakable_token() {
        assert_eq!(min_unwrapped_width("中文换行不应按整句", fake), 0.0);
    }

    #[test]
    fn long_ascii_token_is_measured() {
        assert_eq!(
            min_unwrapped_width("see https://example.com/a/long/path", fake),
            217.0
        );
    }
}
