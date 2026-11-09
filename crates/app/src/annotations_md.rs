//! 导出注释摘要（Markdown）。
//!
//! 纯函数：把注释行排序、格式化成 Markdown；写盘与路径由 [`crate::app`] 负责。
//! 排序与侧栏"按页"视图一致：页码升序 → 页内自上而下 → 左 → id。

use crate::view::{annotation_time_line, format_local_time};

/// 一行导出数据（从 `Annotation` 拍平，便于单测）。
#[derive(Debug, Clone)]
pub struct Row {
    /// 页号（0 起）。
    pub page: u32,
    /// 页内包围盒（页面空间）的 y，只用于排序。
    pub y: f32,
    /// 页内包围盒的 x，只用于排序。
    pub x: f32,
    /// 注释 id（稳定 tie-break）。
    pub id: u64,
    /// 类型名（高亮/下划线/删除线/墨迹）。
    pub kind: &'static str,
    /// 作者。
    pub author: Option<String>,
    /// 创建时间（Unix 秒）。
    pub created: Option<i64>,
    /// 修改时间（Unix 秒）。
    pub modified: Option<i64>,
    /// 正文。
    pub body: Option<String>,
    /// 是否外部注释。
    pub external: bool,
    /// 是否只读（外部内联）。
    pub readonly: bool,
}

/// 渲染整份 Markdown：文件名 → 总览 → 按页分组 → 每条注释。
pub fn render(file_name: &str, rows: &[Row], generated_at: i64) -> String {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then_with(|| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.id.cmp(&b.id))
    });
    let external = sorted.iter().filter(|row| row.external).count();
    let mut out = format!(
        "# {file_name} 的注释\n\n共 {} 条（含外部 {} 条）· 生成于 {}\n",
        sorted.len(),
        external,
        format_local_time(generated_at)
    );
    let mut last_page: Option<u32> = None;
    let mut index_on_page = 0usize;
    for row in sorted {
        if last_page != Some(row.page) {
            index_on_page = 0;
            out.push_str(&format!("\n## 第 {} 页\n", row.page + 1));
            last_page = Some(row.page);
        }
        index_on_page += 1;
        let mut heading = format!(
            "\n### {index_on_page}. {} · {}",
            row.kind,
            row.author.as_deref().unwrap_or("未署名")
        );
        let mut flags = Vec::new();
        if row.external {
            flags.push("外部");
        }
        if row.readonly {
            flags.push("内联");
        }
        if !flags.is_empty() {
            heading.push_str(&format!("（{}）", flags.join(" · ")));
        }
        out.push_str(&heading);
        out.push('\n');
        if let Some(line) = annotation_time_line(row.created, row.modified) {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
        let body = row.body.as_deref().unwrap_or("").trim_end();
        if body.trim().is_empty() {
            out.push_str("（无正文）\n");
        } else {
            for line in body.lines() {
                out.push('>');
                if !line.is_empty() {
                    out.push(' ');
                    out.push_str(line);
                }
                out.push('\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, page: u32, y: f32, modified: Option<i64>) -> Row {
        Row {
            page,
            y,
            x: 0.0,
            id,
            kind: "高亮",
            author: Some("张三".to_string()),
            created: Some(1_700_000_000),
            modified,
            body: Some("第一行\n第二行".to_string()),
            external: false,
            readonly: false,
        }
    }

    #[test]
    fn groups_by_page_and_sorts_top_down() {
        let rows = vec![
            row(1, 1, 100.0, None),
            row(2, 0, 10.0, None),
            row(3, 0, 50.0, None),
        ];
        let text = render("book.pdf", &rows, 1_700_000_000);
        assert!(text.starts_with("# book.pdf 的注释"), "{text}");
        assert!(text.contains("共 3 条（含外部 0 条）"), "{text}");
        let page_one = text.find("## 第 1 页").unwrap();
        let page_two = text.find("## 第 2 页").unwrap();
        assert!(page_one < page_two);
        // 第 1 页内 y 大的（50）排在前面。
        let first = text.find("### 1.").unwrap();
        let second = text.find("### 2.").unwrap();
        assert!(first < second);
        assert!(text[first..second].contains("### 1. 高亮 · 张三"));
    }

    #[test]
    fn body_is_quoted_and_missing_body_is_labelled() {
        let mut rows = vec![row(1, 0, 0.0, None)];
        rows[0].body = Some("第一行\n\n第二行".to_string());
        let text = render("a.pdf", &rows, 0);
        assert!(text.contains("> 第一行"), "{text}");
        assert!(text.contains(">\n"), "空行也要保留引用块：{text}");
        assert!(text.contains("> 第二行"), "{text}");

        rows[0].body = None;
        let text = render("a.pdf", &rows, 0);
        assert!(text.contains("（无正文）"), "{text}");
    }

    #[test]
    fn external_and_readonly_are_marked() {
        let mut rows = vec![row(1, 0, 0.0, None)];
        rows[0].external = true;
        rows[0].readonly = true;
        rows[0].author = None;
        let text = render("a.pdf", &rows, 0);
        assert!(
            text.contains("### 1. 高亮 · 未署名（外部 · 内联）"),
            "{text}"
        );
        assert!(text.contains("共 1 条（含外部 1 条）"), "{text}");
    }

    #[test]
    fn created_only_shows_once_and_modified_adds_a_second_time() {
        let mut rows = vec![row(1, 0, 0.0, Some(1_700_000_000))];
        let text = render("a.pdf", &rows, 0);
        assert!(text.contains("创建 "), "{text}");
        assert!(!text.contains("修改 "), "相同时间只显示创建：{text}");

        rows[0].modified = Some(1_700_000_600);
        let text = render("a.pdf", &rows, 0);
        assert!(text.contains(" · 修改 "), "{text}");
    }
}
