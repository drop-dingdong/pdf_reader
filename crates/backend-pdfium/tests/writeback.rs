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
                        id: Some(1),
                        kind: AnnotationKind::Ink {
                            points: vec![Point::new(50.0, 720.0), Point::new(300.0, 720.0)],
                            width: 2.0,
                        },
                        color: [255, 0, 0],
                        opacity: 1.0,
                        body: Some("墨迹".to_string()),
                    },
                    WritebackAnnotation {
                        id: Some(2),
                        kind: AnnotationKind::Highlight {
                            quads: vec![Rect::new(50.0, 660.0, 200.0, 14.0)],
                        },
                        color: [255, 255, 0],
                        opacity: 0.35,
                        body: None,
                    },
                    WritebackAnnotation {
                        id: Some(3),
                        kind: AnnotationKind::Underline {
                            quads: vec![Rect::new(50.0, 620.0, 200.0, 14.0)],
                        },
                        color: [255, 0, 0],
                        opacity: 1.0,
                        body: None,
                    },
                    WritebackAnnotation {
                        id: Some(4),
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
                    id: Some(5),
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

/// 旋转页：`/QuadPoints` 保持页面空间，且渲染位置正确（旋转只应用一次）。
fn yellow_bbox(image: &RasterImage) -> Option<(u32, u32, u32, u32)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..image.height {
        for x in 0..image.width {
            let index = ((y * image.width + x) * 4) as usize;
            let pixel = &image.data()[index..index + 4];
            if pixel[2] > 200 && pixel[1] > 200 && pixel[0] < 120 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    (min_x != u32::MAX).then_some((min_x, min_y, max_x, max_y))
}

#[test]
fn a_highlight_on_a_rotated_page_lands_where_the_rotation_says() {
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
    let rotation = opened.document.page_rotation(page).expect("页旋转");
    assert_eq!(rotation, Rotation::Clockwise90, "夹具第 2 页是 /Rotate 90");

    // 保存后的字典里仍应是**未旋转页面空间**的 QuadPoints
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("QuadPoints[ 50 120 150 120 50 100 150 100]"),
        "旋转页的 QuadPoints 必须保持页面空间、不得预旋转"
    );

    let image = opened
        .raster
        .render(&RenderRequest::new(page, Scale::IDENTITY, rotation))
        .expect("渲染旋转页");
    assert_eq!(
        (image.width, image.height),
        (400, 200),
        "旋转 90 后宽高互换"
    );

    // 页面空间 quad (50,100,100,20) 在顺时针 90° 下的显示位置：
    // (x,y) → (y, x)，所以显示区域 = x 100..120、y 50..150
    let found = yellow_bbox(&image).expect("旋转页上应当有高亮");
    let close = |a: u32, b: u32| (a as i32 - b as i32).abs() <= 3;
    assert!(
        close(found.0, 100) && close(found.1, 50) && close(found.2, 119) && close(found.3, 149),
        "旋转页上的高亮位置不对：{found:?}（期望 x 100..120、y 50..150）"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 粗墨迹的直角拐弯处不能有缺口。
///
/// 旧实现给每一段都写 `m … l`（各自一条子路径、平头端帽），拐角外侧会留一个菱形缺口；
/// 现在一条笔画一个子路径 + 圆角连接，拐角内侧的点必须是实色。
#[test]
fn a_thick_right_angle_has_no_gap_at_the_joint() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("corner");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let spec = WritebackSpec {
        source,
        output: dir.join("out.pdf"),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(6), // 从下往上，再向右：顶点 (100,760)，线宽 10 → 半径 5
                kind: AnnotationKind::Ink {
                    points: vec![
                        Point::new(100.0, 700.0),
                        Point::new(100.0, 760.0),
                        Point::new(160.0, 760.0),
                    ],
                    width: 10.0,
                },
                color: [255, 0, 0],
                opacity: 1.0,
                body: None,
            }],
        }],
    };
    writeback::write(&spec).expect("写回成功");

    let bytes = std::fs::read(&spec.output).expect("读输出");
    let backend = PdfiumBackend::new();
    let opened = backend.open(Source::Bytes(&bytes), None).expect("重开");
    let scale = 2.0;
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale(scale),
            Rotation::None,
        ))
        .expect("渲染");

    let pixel = |page_x: f32, page_y: f32| -> [u8; 4] {
        let x = (page_x * scale) as u32;
        let y = ((792.0 - page_y) * scale) as u32;
        let index = ((y * image.width + x) * 4) as usize;
        [
            image.data()[index],
            image.data()[index + 1],
            image.data()[index + 2],
            image.data()[index + 3],
        ]
    };
    // 拐角内侧：距顶点 (100,760) 约 3.6pt < 半径 5 → 必须是红色。
    // 旧实现（每段一条子路径）这里是白的。
    let inside = pixel(97.0, 762.0);
    assert!(
        inside[2] > 150 && inside[1] < 100 && inside[0] < 100,
        "拐角处有缺口：页面 (97,762) 的像素是 {inside:?}（BGRA，应当是红色）"
    );
    // 顶点本身也必须实心
    let joint = pixel(100.0, 760.0);
    assert!(joint[2] > 150 && joint[1] < 100, "顶点处不实心：{joint:?}");
    // 远离笔画的地方保持白底（防止把整页染色当成通过）
    let outside = pixel(80.0, 780.0);
    assert!(
        outside[2] > 200 && outside[1] > 200 && outside[0] > 200,
        "笔画外的像素不应被染色：{outside:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 原地重复写回必须**幂等**：靠 `/NM` 标记跳过已写入的，删掉 overlay 里已不存在的标记注释，
/// 其它注释（链接等）不动。
#[test]
fn writing_twice_is_idempotent_and_drops_stale_marked_annotations() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("sync");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");

    let highlight = |id: u64, x: f32| WritebackAnnotation {
        id: Some(id),
        kind: AnnotationKind::Highlight {
            quads: vec![Rect::new(x, 660.0, 100.0, 14.0)],
        },
        color: [255, 255, 0],
        opacity: 0.35,
        body: None,
    };
    let first = dir.join("out1.pdf");
    let write_once =
        |source: &std::path::Path, output: &std::path::Path, pages: Vec<WritebackPage>| {
            let spec = WritebackSpec {
                source: source.to_path_buf(),
                output: output.to_path_buf(),
                pages,
            };
            writeback::write(&spec).map(|report| report.annotations)
        };

    let pages = vec![
        WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![highlight(1, 50.0), highlight(2, 200.0)],
        },
        WritebackPage {
            page: PageIndex::new(1),
            annotations: vec![highlight(3, 50.0)],
        },
    ];
    assert_eq!(
        write_once(&source, &first, pages.clone()).expect("第一次"),
        3
    );
    // 第 1 页原有 1 个链接 + 2 条新注释；第 2 页 1 条
    assert_eq!(
        writeback::count_annotations(&first).expect("数注释"),
        vec![3, 1]
    );

    // 同一份 overlay 再写一次：一条都不应该重复添加
    let second = dir.join("out2.pdf");
    assert_eq!(
        write_once(&first, &second, pages.clone()).expect("第二次"),
        3,
        "标记注释会被删掉后按 overlay 重建（不是叠加）"
    );
    assert_eq!(
        writeback::count_annotations(&second).expect("数注释"),
        vec![3, 1],
        "重复写回不能把注释叠两遍"
    );

    // overlay 里删掉 id=2 与第 2 页的 id=3：再写应当把它们从 PDF 里删掉，链接保留
    let third = dir.join("out3.pdf");
    let reduced = vec![WritebackPage {
        page: PageIndex::FIRST,
        annotations: vec![highlight(1, 50.0)],
    }];
    assert_eq!(
        write_once(&second, &third, reduced).expect("第三次"),
        1,
        "只重建留下的一条"
    );
    assert_eq!(
        writeback::count_annotations(&third).expect("数注释"),
        vec![2, 0],
        "第 1 页 = 链接 + 1 条；第 2 页的标记注释应被删掉"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 已存在的标记注释要**同步正文与颜色**：删/改 overlay 里的正文后，下次写回不能还是旧文本。
#[test]
fn rewriting_updates_contents_of_existing_marked_annotations() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("contents");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");

    let write_with_body =
        |source: &std::path::Path, output: &std::path::Path, body: Option<&str>| {
            let spec = WritebackSpec {
                source: source.to_path_buf(),
                output: output.to_path_buf(),
                pages: vec![WritebackPage {
                    page: PageIndex::FIRST,
                    annotations: vec![WritebackAnnotation {
                        id: Some(7),
                        kind: AnnotationKind::Highlight {
                            quads: vec![Rect::new(50.0, 660.0, 200.0, 14.0)],
                        },
                        color: [255, 255, 0],
                        opacity: 0.35,
                        body: body.map(str::to_string),
                    }],
                }],
            };
            writeback::write(&spec).expect("写回")
        };

    let first = dir.join("out1.pdf");
    write_with_body(&source, &first, Some("OLD"));
    let second = dir.join("out2.pdf");
    write_with_body(&first, &second, Some("NEW"));
    let second_bytes = std::fs::read(&second).expect("读输出");
    let text = String::from_utf8_lossy(&second_bytes);
    assert!(text.contains("(NEW)"), "正文必须更新成 NEW：{text}");
    assert!(!text.contains("(OLD)"), "旧正文不能残留：{text}");

    // overlay 里正文被删掉（None）→ 写回应当清空 /Contents
    let third = dir.join("out3.pdf");
    write_with_body(&second, &third, None);
    let third_bytes = std::fs::read(&third).expect("读输出");
    let text = String::from_utf8_lossy(&third_bytes);
    assert!(!text.contains("(NEW)"), "正文删除后不能残留：{text}");

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
                id: Some(7),
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
                id: Some(8), // 单点墨迹：模型不变量应当拒绝
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
