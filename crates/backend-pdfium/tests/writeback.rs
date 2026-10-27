//! S5b-1：写回 PDF 的端到端测试（需要 PDFium）。
//!
//! 这些测试直接在**当前进程**里调 `writeback::write` —— 本文件内的测试用一把互斥锁串行，
//! 因为写回 FFI 绕过 pdfium-render 的全局锁（产品路径是子进程，见 docs/current/writeback.md）。
//! 不同测试二进制之间由 cargo 顺序执行，所以不与其它集成测试并发。

use std::sync::Mutex;

use backend_pdfium::writeback::{self, WritebackAnnotation, WritebackPage, WritebackSpec};
use backend_pdfium::PdfiumBackend;
use pdfcore::annotation::AnnotationKind;
use pdfcore::prelude::*;

static LOCK: Mutex<()> = Mutex::new(());

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-writeback-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn count_in_band(
    image: &RasterImage,
    y0: u32,
    y1: u32,
    predicate: impl Fn(&[u8]) -> bool,
) -> usize {
    let mut count = 0;
    for y in y0..y1.min(image.height) {
        for x in 0..image.width {
            let index = ((y * image.width + x) * 4) as usize;
            let pixel = &image.data()[index..index + 4];
            if predicate(pixel) {
                count += 1;
            }
        }
    }
    count
}

fn is_red(pixel: &[u8]) -> bool {
    pixel[2] > 180 && pixel[1] < 100 && pixel[0] < 100
}

fn is_yellow(pixel: &[u8]) -> bool {
    pixel[2] > 200 && pixel[1] > 200 && pixel[0] < 120
}

fn spec_for_all_kinds(dir: &std::path::Path) -> WritebackSpec {
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    WritebackSpec {
        source,
        output: dir.join("out.pdf"),
        pages: vec![
            WritebackPage {
                page: PageIndex::FIRST,
                annotations: vec![
                    WritebackAnnotation {
                        kind: AnnotationKind::Ink {
                            points: vec![Point::new(50.0, 720.0), Point::new(300.0, 720.0)],
                            width: 2.0,
                        },
                        color: [255, 0, 0],
                        opacity: 1.0,
                        body: Some("墨迹".to_string()),
                    },
                    WritebackAnnotation {
                        kind: AnnotationKind::Highlight {
                            quads: vec![Rect::new(50.0, 660.0, 200.0, 14.0)],
                        },
                        color: [255, 255, 0],
                        opacity: 0.35,
                        body: None,
                    },
                    WritebackAnnotation {
                        kind: AnnotationKind::Underline {
                            quads: vec![Rect::new(50.0, 620.0, 200.0, 14.0)],
                        },
                        color: [255, 0, 0],
                        opacity: 1.0,
                        body: None,
                    },
                    WritebackAnnotation {
                        kind: AnnotationKind::StrikeOut {
                            quads: vec![Rect::new(50.0, 580.0, 200.0, 14.0)],
                        },
                        color: [255, 0, 0],
                        opacity: 1.0,
                        body: None,
                    },
                ],
            },
            // 第 2 页带 /Rotate 90（200×400）
            WritebackPage {
                page: PageIndex::new(1),
                annotations: vec![WritebackAnnotation {
                    kind: AnnotationKind::Highlight {
                        quads: vec![Rect::new(50.0, 100.0, 100.0, 20.0)],
                    },
                    color: [255, 255, 0],
                    opacity: 0.35,
                    body: None,
                }],
            },
        ],
    }
}

#[test]
fn writes_all_four_kinds_at_page_coordinates_and_preserves_the_rest() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("all");
    let spec = spec_for_all_kinds(&dir);

    let report = writeback::write(&spec).expect("写回成功");
    assert_eq!(report.annotations, 5);
    assert!(spec.output.is_file(), "输出必须存在");
    assert!(report.bytes > 0);

    // 原有注释（第 1 页有一个链接）必须保留：4 新增 + 1 链接 = 5
    let counts = writeback::count_annotations(&spec.output).expect("数注释");
    assert_eq!(counts, vec![5, 1], "第 1 页应有 4 条新注释 + 1 个链接");

    // 用现有后端重开：页数、链接、页面上的可见位置都要对
    let backend = PdfiumBackend::new();
    let bytes = std::fs::read(&spec.output).expect("读输出");
    let opened = backend.open(Source::Bytes(&bytes), None).expect("重开");
    assert_eq!(opened.document.info().page_count, 2);
    let links = opened
        .document
        .navigation()
        .expect("导航")
        .links_on(PageIndex::FIRST)
        .len();
    assert_eq!(links, 1, "既有链接不能被写回破坏");

    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale::IDENTITY,
            Rotation::None,
        ))
        .expect("渲染第 1 页");
    // 墨迹：页面 y=720 → 位图行 792-720=72
    assert!(
        count_in_band(&image, 68, 78, is_red) > 100,
        "墨迹没画在 y=720 附近"
    );
    // 高亮：quad y 660..674 → 行 118..132
    assert!(
        count_in_band(&image, 114, 136, is_yellow) > 1000,
        "高亮没画在 y=660..674"
    );
    // 下划线：quad y=620 → 行 172
    assert!(
        count_in_band(&image, 168, 178, is_red) > 100,
        "下划线没画在 y=620"
    );
    // 删除线：quad 中线 y=587 → 行 205
    assert!(
        count_in_band(&image, 201, 211, is_red) > 100,
        "删除线没画在 y=587"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **旋转页**：目前只断言"写进去的几何是页面空间、注释数正确"。
///
/// 为什么不断言像素位置：PDFium 的 `FPDF_RenderPageBitmap`（pdfium-render 走的就是它）
/// 在**页面自带 `/Rotate`** 时对注释应用了一个带纵横比缩放的错误矩阵 ——
/// 用**原始 PDF**（不经过我们的写回）复现同样错位：`/Rotate 90` + 四角
/// 渲染时，页面空间 (50,100)-(150,120) 的高亮会出现在 (50,100)-(59,299)。
/// 这是渲染器路径的问题，不是写回的几何问题（`/QuadPoints` 与输入逐值相同）。
/// 真实阅读器（Chrome 的注释绘制走 `FPDF_FFLDraw`）需要**人工验收**，见
/// `docs/current/writeback.md` 的"已知限制"。
#[test]
fn a_rotated_page_keeps_page_space_geometry_and_the_right_annotation_count() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("rotated");
    let spec = spec_for_all_kinds(&dir);
    writeback::write(&spec).expect("写回成功");

    let counts = writeback::count_annotations(&spec.output).expect("数注释");
    assert_eq!(counts, vec![5, 1], "旋转页上的高亮必须写进去");

    let backend = PdfiumBackend::new();
    let bytes = std::fs::read(&spec.output).expect("读输出");
    let opened = backend.open(Source::Bytes(&bytes), None).expect("重开");
    let page = PageIndex::new(1);
    assert_eq!(
        opened.document.page_rotation(page).expect("页旋转"),
        Rotation::Clockwise90,
        "夹具第 2 页的 /Rotate 必须原样保留"
    );

    // 保存后的字典里仍应是**未旋转页面空间**的 QuadPoints（原点在左下）
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("QuadPoints[ 50 120 150 120 50 100 150 100]"),
        "旋转页的 QuadPoints 必须保持页面空间、不得预旋转"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn refuses_to_overwrite_the_source_file() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("same");
    let path = dir.join("same.pdf");
    std::fs::write(&path, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let spec = WritebackSpec {
        source: path.clone(),
        output: path,
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(10.0, 10.0, 20.0, 10.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: None,
            }],
        }],
    };
    let error = writeback::write(&spec).expect_err("必须拒绝原地覆盖");
    assert!(
        error.to_string().contains("输出路径与源文件相同"),
        "实际：{error}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_geometry_that_the_model_would_reject() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("bad");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let spec = WritebackSpec {
        source,
        output: dir.join("out.pdf"),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                // 单点墨迹：模型不变量应当拒绝
                kind: AnnotationKind::Ink {
                    points: vec![Point::new(10.0, 10.0)],
                    width: 2.0,
                },
                color: [0, 0, 0],
                opacity: 1.0,
                body: None,
            }],
        }],
    };
    let error = writeback::write(&spec).expect_err("必须拒绝退化几何");
    assert!(error.to_string().contains("几何无效"), "实际：{error}");
    assert!(!spec.output.exists(), "失败时不应留下输出文件");
    std::fs::remove_dir_all(&dir).ok();
}
