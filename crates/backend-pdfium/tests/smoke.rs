//! 冒烟测试：验证"绑定 PDFium → 打开现场生成的 PDF → 读到页数与页尺寸"这条最关键路径。
//!
//! 找不到 PDFium 动态库时**跳过并给出指引**（不静默忽略）：先跑 `scripts/fetch-pdfium.sh`。

mod common;

#[test]
fn binds_pdfium_and_reads_page_geometry() {
    let Ok(pdfium) = backend_pdfium::pdfium() else {
        eprintln!("跳过：未找到 PDFium 动态库。请先运行 scripts/fetch-pdfium.sh 或设置 PDFIUM_PATH。");
        return;
    };

    let bytes = common::two_page_pdf();
    let document = pdfium
        .load_pdf_from_reader(std::io::Cursor::new(bytes), None)
        .expect("应能打开现场生成的 PDF");

    assert_eq!(document.pages().len(), 2, "两页");

    let first = document.pages().get(0).expect("第 1 页");
    assert!((first.width().value - 612.0).abs() < 1.0, "第 1 页宽 612pt");
    assert!((first.height().value - 792.0).abs() < 1.0, "第 1 页高 792pt");

    let second = document.pages().get(1).expect("第 2 页");
    // 注意：PDFium 的 FPDF_GetPageWidthF/HeightF 返回**已应用 /Rotate 的显示尺寸**，
    // 因此 MediaBox 为 200×400 且带 /Rotate 90 的这页在这里是 400×200。
    // 适配层负责把它还原成 MediaBox（见 tests/real_pdf.rs 的几何断言）。
    assert!((second.width().value - 400.0).abs() < 1.0, "第 2 页显示宽 400pt（已旋转）");
    assert!((second.height().value - 200.0).abs() < 1.0, "第 2 页显示高 200pt（已旋转）");

    // 文本提取必须拿到内容（验证标准字体与内容流都正确）
    let text = first.text().expect("第 1 页文本");
    let extracted = text.all();
    assert!(extracted.contains("Hello World"), "实际提取到：{extracted:?}");

    // 逐字符包围盒必须落在页面范围内（PDF 页面空间，左下原点）
    let mut checked = 0;
    for character in text.chars().iter() {
        if let Ok(bounds) = character.tight_bounds() {
            assert!(bounds.left().value >= -1.0 && bounds.right().value <= 613.0);
            assert!(bounds.bottom().value >= -1.0 && bounds.top().value <= 793.0);
            checked += 1;
        }
    }
    assert!(checked >= 10, "至少应有 10 个字符带包围盒，实际 {checked}");
}
