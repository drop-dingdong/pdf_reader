//! 页面自带 `/Rotate` 的渲染回归：**旋转只能应用一次**。
//!
//! 背景（用户实测）：`/Rotate 90` 的页在我们这里显示成 180° 且被压扁 ——
//! 原因是 `FPDF_RenderPageBitmap` 会自己应用 intrinsic `/Rotate`，而适配层又把
//! "页面 ⊕ 用户"的最终旋转传了一遍。这个文件用一个已知位置的黑矩形把方向钉死。

use backend_pdfium::PdfiumBackend;
use pdfcore::prelude::*;

fn stream_object(content: &str) -> String {
    format!(
        "<< /Length {} >>\nstream\n{}endstream",
        content.len(),
        content
    )
}

/// 200×400 的页，`/Rotate 90`，在页面空间 (0,300)-(50,350) 画一个黑矩形。
///
/// 选这个位置是因为它在四个方向上都不对称：转 90/180/270 会落到完全不同的象限。
fn rotated_page_pdf() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 400] /Rotate 90 \
         /Contents 4 0 R /Resources << >> >>"
            .to_string(),
        stream_object("q 0 g 0 300 50 50 re f Q"),
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

fn black_bbox(image: &RasterImage) -> Option<(u32, u32, u32, u32)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..image.height {
        for x in 0..image.width {
            let index = ((y * image.width + x) * 4) as usize;
            let pixel = &image.data()[index..index + 4];
            if pixel[0] < 60 && pixel[1] < 60 && pixel[2] < 60 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    (min_x != u32::MAX).then_some((min_x, min_y, max_x, max_y))
}

fn assert_close(found: (u32, u32, u32, u32), expected: (u32, u32, u32, u32), label: &str) {
    let close = |a: u32, b: u32| (a as i32 - b as i32).abs() <= 2;
    assert!(
        close(found.0, expected.0)
            && close(found.1, expected.1)
            && close(found.2, expected.2)
            && close(found.3, expected.3),
        "{label}：黑矩形落在 {found:?}，期望 {expected:?}"
    );
}

#[test]
fn intrinsic_rotate_is_applied_exactly_once() {
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let bytes = rotated_page_pdf();
    let backend = PdfiumBackend::new();
    let opened = backend.open(Source::Bytes(&bytes), None).expect("打开");

    // 最终旋转 = 页面自带 90（应用层就是这样传的）
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale::IDENTITY,
            Rotation::Clockwise90,
        ))
        .expect("渲染");
    assert_eq!((image.width, image.height), (400, 200), "90° 后宽高互换");
    // 页面空间 (0,300)-(50,350) 顺时针 90° → 显示 (300,0)-(350,50)
    assert_close(
        black_bbox(&image).expect("必须有黑矩形"),
        (300, 0, 349, 49),
        "最终旋转 90",
    );

    // 最终旋转 = 不旋转（适配层应当从 intrinsic 里减掉 90，交给 PDFium 的是 270）
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale::IDENTITY,
            Rotation::None,
        ))
        .expect("渲染");
    assert_eq!(
        (image.width, image.height),
        (200, 400),
        "不旋转时用原始尺寸"
    );
    // 页面空间 (0,300)-(50,350) 不旋转（只翻 y）→ 显示 (0,50)-(50,100)
    assert_close(
        black_bbox(&image).expect("必须有黑矩形"),
        (0, 50, 49, 99),
        "最终旋转 0",
    );
}
