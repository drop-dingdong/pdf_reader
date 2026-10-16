//! 测试用 PDF 生成器：**用 Rust 现场拼出一个合法 PDF**，避免把二进制夹具提交进仓库。
//!
// 该模块被多个测试二进制共享，某个二进制不用到的辅助函数不应报警告。
#![allow(dead_code)]
//!
//! 生成的文档包含：
//! * 2 页：第 1 页 612×792、第 2 页 200×400 且带 `/Rotate 90`；
//! * 每页一段文本（标准 Type1 字体 Helvetica，无需嵌入）；
//! * 第 1 页一个指向第 2 页的链接注释；
//! * 一个大纲项（指向第 1 页）。
//!
//! 交叉引用表（xref）的字节偏移在生成时精确计算 —— 手写 PDF 最容易错的就是这里。

/// 生成测试 PDF 的字节。
pub fn two_page_pdf() -> Vec<u8> {
    let content1 = "BT /F1 24 Tf 72 700 Td (Hello World) Tj ET\n";
    let content2 = "BT /F1 12 Tf 20 300 Td (Second page) Tj ET\n";

    let objects: Vec<String> = vec![
        // 1: Catalog（含大纲根）
        "<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R /PageMode /UseOutlines >>".to_string(),
        // 2: Pages
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string(),
        // 3: 第 1 页（含链接注释：跳到第 2 页）
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R /Annots [9 0 R] >>"
            .to_string(),
        // 4: 第 1 页内容流
        stream_object(content1),
        // 5: 第 2 页（/Rotate 90）
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 400] /Rotate 90 \
         /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>"
            .to_string(),
        // 6: 第 2 页内容流
        stream_object(content2),
        // 7: 字体
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        // 8: 大纲
        "<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>".to_string(),
        // 9: 链接注释（矩形 + 跳转到第 2 页顶部）
        "<< /Type /Annot /Subtype /Link /Rect [72 690 300 730] /Border [0 0 1] \
         /Dest [5 0 R /Fit] >>"
            .to_string(),
        // 10: 大纲项
        "<< /Title (Chapter One) /Parent 8 0 R /Dest [3 0 R /Fit] >>".to_string(),
    ];

    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }

    let xref_offset = out.len();
    let size = objects.len() + 1;
    out.push_str(&format!("xref\n0 {size}\n"));
    out.push_str("0000000000 65535 f \n");
    for offset in &offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    ));

    out.into_bytes()
}

/// 把内容包成流对象（`/Length` 必须等于流数据的字节数）。
fn stream_object(content: &str) -> String {
    format!("<< /Length {} >>\nstream\n{}endstream", content.len(), content)
}

/// 把测试 PDF 写到临时文件，返回路径（供 `Source::Path` 路径测试使用）。
pub fn two_page_pdf_file(name: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("pdfcore-fixture-{name}.pdf"));
    std::fs::write(&path, two_page_pdf()).expect("写入临时 PDF");
    path
}
