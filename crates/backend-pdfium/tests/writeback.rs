//! S5b-1：写回 PDF 的端到端测试（需要 PDFium）。
//!
//! 这些测试直接在**当前进程**里调 `writeback::write` —— 本文件内的测试用一把互斥锁串行，
//! 因为写回 FFI 绕过 pdfium-render 的全局锁（产品路径是子进程，见 docs/current/writeback.md）。
//! 不同测试二进制之间由 cargo 顺序执行，所以不与其它集成测试并发。

use std::sync::Mutex;

use backend_pdfium::readback;
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
    // 高亮带 /CA=0.35，叠在白底上 B≈166；用"B 明显低于 G"识别偏黄，
    // 避免把灰白的抗锯齿像素也算进来（BGRA：0=B、1=G、2=R）
    pixel[2] > 200 && pixel[1] > 200 && pixel[0] as u16 + 15 < pixel[1] as u16
}

fn spec_for_all_kinds(dir: &std::path::Path) -> WritebackSpec {
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
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
    let backend = PdfiumBackend::new().with_annotations(true);
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
            if pixel[2] > 200 && pixel[1] > 200 && pixel[0] as u16 + 15 < pixel[1] as u16 {
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

    let backend = PdfiumBackend::new().with_annotations(true);
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
        updates: Vec::new(),
        deletes: Vec::new(),
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
    let backend = PdfiumBackend::new().with_annotations(true);
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
fn writing_again_without_new_pages_keeps_existing_marked_annotations() {
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
                updates: Vec::new(),
                deletes: Vec::new(),
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

    // P5：pages 只放新注释；没有新注释时再次写回不应删除/重复已有注释
    let second = dir.join("out2.pdf");
    assert_eq!(
        write_once(&first, &second, Vec::new()).expect("第二次"),
        0,
        "没有新注释"
    );
    assert_eq!(
        writeback::count_annotations(&second).expect("数注释"),
        vec![3, 1],
        "重复写回不能把注释叠两遍"
    );

    // P5：删除必须显式给 deletes；pages 为空不会隐式删 marker。
    let third = dir.join("out3.pdf");
    let pages = backend_pdfium::pdf_index::page_annotations(&std::fs::read(&second).expect("读"))
        .expect("索引");
    let deletes: Vec<_> = pages
        .iter()
        .flatten()
        .filter(|entry| matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .skip(1) // 保留第 1 页第一条高亮，删其余
        .filter_map(|entry| entry.reference)
        .collect();
    writeback::write_incremental(&WritebackSpec {
        updates: Vec::new(),
        deletes,
        source: second,
        output: third.clone(),
        pages: Vec::new(),
    })
    .expect("显式删除");
    assert_eq!(
        writeback::count_annotations(&third).expect("数注释"),
        vec![2, 0],
        "第 1 页 = 链接 + 1 条；第 2 页的标记注释应被显式删掉"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rewriting_updates_contents_of_existing_annotations() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("contents-p5");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let first = dir.join("first.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source: source.clone(),
        output: first.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(7),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(50.0, 660.0, 200.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("OLD".to_string()),
            }],
        }],
    })
    .expect("写一条");
    let reference =
        backend_pdfium::pdf_index::page_annotations(&std::fs::read(&first).expect("读"))
            .expect("索引")
            .iter()
            .flatten()
            .find(|entry| matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
            .and_then(|entry| entry.reference)
            .expect("ref");
    let second = dir.join("second.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference,
            body: Some("NEW".to_string()),
        }],
        deletes: Vec::new(),
        source: first,
        output: second.clone(),
        pages: Vec::new(),
    })
    .expect("改正文");
    let read = readback::read(&second, None).expect("读回");
    assert_eq!(read.annotations[0].body.as_deref(), Some("NEW"));

    let third = dir.join("third.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference,
            body: None,
        }],
        deletes: Vec::new(),
        source: second,
        output: third.clone(),
        pages: Vec::new(),
    })
    .expect("清空正文");
    assert_eq!(
        readback::read(&third, None).expect("读回").annotations[0].body,
        None
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 页面位图**默认不含 PDF 注释**（应用叠层自己画）；显式 `with_annotations(true)` 才烤进去。
///
/// 否则写回之后 overlay 会再画一遍，而且"删掉 overlay 注释"在界面上不会消失
/// （注释还烤在 PDF 位图里）。见 INV-14。
#[test]
fn annotations_are_not_baked_into_the_bitmap_by_default() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("not-baked");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let spec = WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: dir.join("out.pdf"),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(1),
                kind: AnnotationKind::Ink {
                    points: vec![Point::new(100.0, 700.0), Point::new(300.0, 700.0)],
                    width: 6.0,
                },
                color: [255, 0, 0],
                opacity: 1.0,
                body: None,
            }],
        }],
    };
    writeback::write(&spec).expect("写回");
    let bytes = std::fs::read(&spec.output).expect("读输出");

    let render = |backend: PdfiumBackend| {
        let opened = backend.open(Source::Bytes(&bytes), None).expect("重开");
        opened
            .raster
            .render(&RenderRequest::new(
                PageIndex::FIRST,
                Scale::IDENTITY,
                Rotation::None,
            ))
            .expect("渲染")
    };
    let plain = render(PdfiumBackend::new());
    assert_eq!(
        count_in_band(&plain, 60, 120, is_red),
        0,
        "默认不应把注释烤进位图"
    );
    let baked = render(PdfiumBackend::new().with_annotations(true));
    assert!(
        count_in_band(&baked, 60, 120, is_red) > 100,
        "显式开启后注释应当出现在位图里"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 空 `pages` = **把写过的注释全部删掉**（叠层删光后仍要能写回 PDF）。
#[test]
fn empty_pages_keep_marked_annotations_until_explicit_deletes() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("empty");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let annotated = dir.join("annotated.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: annotated.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(1),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(50.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("要删掉的正文".to_string()),
            }],
        }],
    })
    .expect("先写一条");
    assert_eq!(
        writeback::count_annotations(&annotated).expect("数"),
        vec![2, 0],
        "链接 + 1 条"
    );

    // 空 pages 不再隐式删除 marker 注释
    let kept = dir.join("kept.pdf");
    let report = writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source: annotated.clone(),
        output: kept.clone(),
        pages: vec![],
    })
    .expect("空 pages");
    assert_eq!(report.annotations, 0);
    assert_eq!(
        writeback::count_annotations(&kept).expect("数"),
        vec![2, 0],
        "P5：没有 deletes 就保留"
    );

    // 显式对象号删除
    let pages = backend_pdfium::pdf_index::page_annotations(&std::fs::read(&annotated).unwrap())
        .expect("索引");
    let reference = pages
        .iter()
        .flatten()
        .find(|entry| matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .and_then(|entry| entry.reference)
        .expect("高亮对象");
    let cleared = dir.join("cleared.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: Vec::new(),
        deletes: vec![reference],
        source: annotated,
        output: cleared.clone(),
        pages: vec![],
    })
    .expect("显式删除");
    assert_eq!(
        writeback::count_annotations(&cleared).expect("数"),
        vec![1, 0],
        "显式 deletes 才删除；链接保留"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn incremental_save_keeps_external_annotations_and_object_streams() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("incremental");
    let source = dir.join("adobe.pdf");
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/adobe.pdf");
    std::fs::copy(&fixture, &source).expect("复制样本");
    let original = std::fs::read(&source).expect("读原文件");

    let output = dir.join("out.pdf");
    let report = writeback::write_incremental(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source: source.clone(),
        output: output.clone(),
        pages: vec![],
    })
    .expect("增量保存");
    let saved = std::fs::read(&output).expect("读输出");
    assert!(report.annotations == 0);
    assert!(saved.len() > original.len(), "增量保存应当追加内容");
    assert!(
        saved.starts_with(&original[..64]),
        "增量保存应当保留原文件开头（不是全量重写）"
    );
    let text = String::from_utf8_lossy(&saved);
    assert!(text.contains("/ObjStm"), "原文件的对象流应保留");
    assert_eq!(
        writeback::count_annotations(&output).expect("数注释"),
        vec![7, 4],
        "Adobe 的注释（含 Popup 子注释）原样保留"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 我们写的注释必须是**独立对象**（`/Annots [.. 12 0 R ..]`），不能是内联字典。
/// Okular 对没有对象号的注释会“编辑后保存丢修改”（D-142）。
#[test]
fn annotations_are_written_as_indirect_objects() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("indirect");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let spec = WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: dir.join("out.pdf"),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(1),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(50.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("独立对象".to_string()),
            }],
        }],
    };
    writeback::write(&spec).expect("写回");
    let text = String::from_utf8_lossy(&std::fs::read(&spec.output).expect("读")).to_string();
    // 文件里可能还有旧版页面对象（增量更新）；只检查**最后一次**出现的 /Annots
    let start = text.rfind("/Annots").expect("/Annots");
    let start = start + text[start..].find('[').expect("[");
    let end = text[start..].find(']').expect("]") + start;
    let array = &text[start + 1..end];
    assert!(!array.contains("<<"), "注释不能内联在 /Annots 里：{array}");
    assert!(
        array.contains("0 R"),
        "/Annots 里应当是对独立对象的引用：{array}"
    );
    // 引用要能真的读回来（外部化之后 xref/对象仍然有效）
    let read = readback::read(&spec.output, None).expect("读回");
    assert_eq!(read.annotations.len(), 1);
    assert_eq!(read.annotations[0].id, Some(1));

    std::fs::remove_dir_all(&dir).ok();
}

/// 全量重写时 Adobe 的注释也会变成内联字典；后处理要把它们全部外置。
#[test]
fn full_save_externalizes_external_annotations_too() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("externalize-adobe");
    let source = dir.join("adobe.pdf");
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/adobe.pdf");
    std::fs::copy(&fixture, &source).expect("复制样本");
    let output = dir.join("out.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: output.clone(),
        pages: vec![],
    })
    .expect("全量保存");
    let text = String::from_utf8_lossy(&std::fs::read(&output).expect("读")).to_string();
    let start = text.rfind("/Annots").expect("/Annots");
    let start = start + text[start..].find('[').expect("[");
    let end = text[start..].find(']').expect("]") + start;
    let array = &text[start + 1..end];
    assert!(!array.contains("<<"), "Adobe 的注释也要外置：{array}");
    assert_eq!(
        writeback::count_annotations(&output).expect("数"),
        vec![7, 4]
    );
    assert_eq!(
        readback::read(&output, None)
            .expect("读回")
            .annotations
            .len(),
        5
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 只改正文时应当**原地更新同一个注释对象**（对象号不变），不删旧建新。
#[test]
fn content_only_edit_keeps_the_annots_ref() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("diff");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let annots = |path: &std::path::Path| -> String {
        let text = String::from_utf8_lossy(&std::fs::read(path).expect("读")).to_string();
        let start = text.rfind("/Annots").expect("/Annots");
        let start = start + text[start..].find('[').expect("[");
        let end = text[start..].find(']').expect("]") + start;
        text[start..=end]
            .split(|ch: char| ch.is_whitespace() || ch == '[' || ch == ']')
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let first = dir.join("first.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source: source.clone(),
        output: first.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(7),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(50.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("OLD".to_string()),
            }],
        }],
    })
    .expect("第一次");
    let refs1 = annots(&first);
    let reference =
        backend_pdfium::pdf_index::page_annotations(&std::fs::read(&first).expect("读"))
            .expect("索引")
            .iter()
            .flatten()
            .find(|entry| matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
            .and_then(|entry| entry.reference)
            .expect("ref");
    let second = dir.join("second.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference,
            body: Some("NEW".to_string()),
        }],
        deletes: Vec::new(),
        source: first,
        output: second.clone(),
        pages: Vec::new(),
    })
    .expect("改正文");
    assert_eq!(refs1, annots(&second), "只改正文不应改变页 /Annots 引用");
    let read = readback::read(&second, None).expect("读回");
    assert_eq!(read.annotations[0].body.as_deref(), Some("NEW"));
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
        updates: Vec::new(),
        deletes: Vec::new(),
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
        updates: Vec::new(),
        deletes: Vec::new(),
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

#[test]
fn external_annotation_body_update_and_delete_by_object_ref() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("external-ref");
    let source = dir.join("in.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/okular.pdf"),
        &source,
    )
    .expect("复制 Okular 样本");
    let bytes = std::fs::read(&source).expect("读源");
    let pages = backend_pdfium::pdf_index::page_annotations(&bytes).expect("解析对象号");
    eprintln!(
        "entries: {:?}",
        pages
            .iter()
            .map(|p| p
                .iter()
                .map(|e| (e.reference, e.subtype, e.popup, e.hidden))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    let supported: Vec<_> = pages
        .iter()
        .flatten()
        .filter(|entry| !entry.hidden && matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .collect();
    assert!(supported.len() >= 2, "Okular 样本至少两条四种注释");
    let update_ref = supported[0].reference.expect("独立对象");
    let delete = supported[1];
    let delete_ref = delete.reference.expect("独立对象");
    let popup_count = usize::from(delete.popup.is_some());

    let before: usize = pages.iter().map(Vec::len).sum();
    let output = dir.join("out.pdf");
    let report = writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: update_ref,
            body: Some("P2 改正文".to_string()),
        }],
        deletes: vec![delete_ref],
        source: source.clone(),
        output: output.clone(),
        pages: Vec::new(),
    })
    .expect("按对象号改/删");
    assert_eq!(report.annotations, 0);

    let bodies: Vec<String> = backend_pdfium::readback::read(&output, None)
        .expect("读回")
        .annotations
        .into_iter()
        .filter_map(|annotation| annotation.body)
        .collect();
    assert!(
        bodies.iter().any(|body| body == "P2 改正文"),
        "改正文应写进 PDF：{bodies:?}"
    );
    let after_pages =
        backend_pdfium::pdf_index::page_annotations(&std::fs::read(&output).expect("读输出"))
            .expect("解析输出对象号");
    let after: usize = after_pages.iter().map(Vec::len).sum();
    assert_eq!(after, before - 1 - popup_count, "删除父注释时 Popup 也要删");
}

#[test]
fn deleting_an_external_annotation_with_popup_removes_both() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("external-popup");
    let source = dir.join("in.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/foxit.pdf"),
        &source,
    )
    .expect("复制 Foxit 样本");
    let bytes = std::fs::read(&source).expect("读源");
    let pages = backend_pdfium::pdf_index::page_annotations(&bytes).expect("解析");
    let (page, entry) = pages
        .iter()
        .enumerate()
        .find_map(|(page, entries)| {
            entries
                .iter()
                .find(|entry| {
                    entry.popup.is_some() && matches!(entry.subtype, Some(9 | 10 | 12 | 15))
                })
                .map(|entry| (page, *entry))
        })
        .expect("Foxit 样本应有带 Popup 的注释");
    let reference = entry.reference.expect("独立对象");
    let before: usize = pages.iter().map(Vec::len).sum();
    let output = dir.join("out.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: Vec::new(),
        deletes: vec![reference],
        source: source.clone(),
        output: output.clone(),
        pages: Vec::new(),
    })
    .expect("删除父+Popup");
    let after_pages =
        backend_pdfium::pdf_index::page_annotations(&std::fs::read(&output).expect("读输出"))
            .expect("解析输出");
    let after: usize = after_pages.iter().map(Vec::len).sum();
    assert_eq!(after, before - 2, "父注释与 Popup（第 {page} 页）都应删除");
}

#[test]
fn incremental_update_with_generation_1_uses_pdf_patch() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("dbg-hl");
    let source = dir.join("in.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/okular.pdf"),
        &source,
    )
    .unwrap();
    let output = dir.join("out.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: backend_pdfium::pdf_index::PdfObjectRef { num: 11, gen: 1 },
            body: Some("TESTBODY".to_string()),
        }],
        deletes: vec![],
        source: source.clone(),
        output: output.clone(),
        pages: vec![],
    })
    .unwrap();
    let r = backend_pdfium::readback::read(&output, None).unwrap();
    assert!(r
        .annotations
        .iter()
        .any(|a| a.body.as_deref() == Some("TESTBODY")));
    assert!(
        std::fs::metadata(&output).unwrap().len() < 9_500,
        "同 generation 追加应只增加少量字节，而不是全量重写"
    );
}

#[test]
fn full_save_then_incremental_update_reads_back() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("dbg-full-inc");
    let source = dir.join("in.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/okular.pdf"),
        &source,
    )
    .unwrap();
    let out1 = dir.join("out1.pdf");
    let spec1 = WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: backend_pdfium::pdf_index::PdfObjectRef { num: 11, gen: 1 },
            body: Some("A".to_string()),
        }],
        deletes: vec![],
        source: source.clone(),
        output: out1.clone(),
        pages: vec![],
    };
    writeback::write(&spec1).unwrap();
    let pages =
        backend_pdfium::pdf_index::page_annotations(&std::fs::read(&out1).unwrap()).unwrap();
    let ref0 = pages[0]
        .iter()
        .find(|e| matches!(e.subtype, Some(9)))
        .unwrap()
        .reference
        .unwrap();
    let out2 = dir.join("out2.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: ref0,
            body: Some("B".to_string()),
        }],
        deletes: vec![],
        source: out1,
        output: out2.clone(),
        pages: vec![],
    })
    .unwrap();
    let r = backend_pdfium::readback::read(&out2, None).unwrap();
    assert!(
        r.annotations
            .iter()
            .any(|annotation| annotation.body.as_deref() == Some("B")),
        "全量保存后的文件应能继续增量更新"
    );
}

#[test]
fn incremental_body_update_on_adobe_object_stream_uses_pdf_patch() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("adobe-patch-update");
    let source = dir.join("in.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/adobe.pdf"),
        &source,
    )
    .expect("复制 Adobe");
    let output = dir.join("out.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: backend_pdfium::pdf_index::PdfObjectRef { num: 36, gen: 0 },
            body: Some("P4B Adobe".to_string()),
        }],
        deletes: Vec::new(),
        source: source.clone(),
        output: output.clone(),
        pages: Vec::new(),
    })
    .expect("增量更新");
    let bytes = std::fs::read(&output).expect("读输出");
    assert!(
        bytes.len() < 14_000,
        "外部更新应走源字节 patch，而不是 PDFium 整份追加"
    );
    assert!(
        bytes.windows(7).any(|window| window == b"/ObjStm"),
        "ObjStm 必须保留"
    );
    let read = backend_pdfium::readback::read(&output, None).expect("读回");
    assert!(
        read.annotations
            .iter()
            .any(|annotation| annotation.body.as_deref() == Some("P4B Adobe")),
        "更新后的正文应读回"
    );
}

#[test]
fn external_body_can_be_rewritten_twice() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("rewrite-twice");
    let source = dir.join("in.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s6b/okular.pdf"),
        &source,
    )
    .expect("复制 Okular");
    let first = dir.join("first.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: backend_pdfium::pdf_index::PdfObjectRef { num: 11, gen: 1 },
            body: Some("第一次".to_string()),
        }],
        deletes: Vec::new(),
        source: source.clone(),
        output: first.clone(),
        pages: Vec::new(),
    })
    .expect("第一次");
    let second = dir.join("second.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: vec![backend_pdfium::writeback::WritebackUpdate {
            reference: backend_pdfium::pdf_index::PdfObjectRef { num: 11, gen: 1 },
            body: Some("第二次".to_string()),
        }],
        deletes: Vec::new(),
        source: first,
        output: second.clone(),
        pages: Vec::new(),
    })
    .expect("第二次");
    let read = backend_pdfium::readback::read(&second, None).expect("读回");
    assert!(
        read.annotations
            .iter()
            .any(|annotation| annotation.body.as_deref() == Some("第二次")),
        "连续两次写回后应读到最新正文"
    );
}

#[test]
fn mixed_new_and_delete_uses_page_patch_after_save() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("mixed-p5");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let old = dir.join("old.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source: source.clone(),
        output: old.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(7),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(50.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("old".to_string()),
            }],
        }],
    })
    .expect("旧注释");
    let reference = backend_pdfium::pdf_index::page_annotations(&std::fs::read(&old).unwrap())
        .unwrap()
        .iter()
        .flatten()
        .find(|entry| matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .and_then(|entry| entry.reference)
        .unwrap();
    let output = dir.join("out.pdf");
    writeback::write_incremental(&WritebackSpec {
        updates: Vec::new(),
        deletes: vec![reference],
        source: old,
        output: output.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(8),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(200.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("new".to_string()),
            }],
        }],
    })
    .expect("新+删");
    let read = readback::read(&output, None).expect("读回");
    assert_eq!(read.annotations.len(), 1, "链接不计入 readback；只剩新注释");
    assert!(read
        .annotations
        .iter()
        .any(|annotation| annotation.body.as_deref() == Some("new")));
    assert!(!read
        .annotations
        .iter()
        .any(|annotation| annotation.body.as_deref() == Some("old")));
}
