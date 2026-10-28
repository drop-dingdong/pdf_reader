//! S6a：读回 PDF 注释的端到端测试（需要 PDFium，串行）。
//!
//! 与 `writeback.rs` 同理：裸 FFI 绕过 pdfium-render 的全局锁，文件内用互斥锁串行。

use std::sync::Mutex;

use backend_pdfium::readback;
use backend_pdfium::writeback::{self, WritebackAnnotation, WritebackPage, WritebackSpec};
use pdfcore::annotation::AnnotationKind;
use pdfcore::prelude::*;

static LOCK: Mutex<()> = Mutex::new(());

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-readback-{}-{}-{}",
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

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.01
}

fn rect_close(a: Rect, b: Rect) -> bool {
    close(a.x, b.x) && close(a.y, b.y) && close(a.width, b.width) && close(a.height, b.height)
}

fn four_kinds() -> Vec<WritebackAnnotation> {
    vec![
        WritebackAnnotation {
            id: Some(1),
            kind: AnnotationKind::Highlight {
                quads: vec![Rect::new(50.0, 660.0, 200.0, 14.0)],
            },
            color: [255, 255, 0],
            opacity: 0.35,
            body: Some("高亮正文".to_string()),
        },
        WritebackAnnotation {
            id: Some(2),
            kind: AnnotationKind::Underline {
                quads: vec![Rect::new(50.0, 600.0, 200.0, 14.0)],
            },
            color: [255, 0, 0],
            opacity: 0.8,
            body: Some("下划线正文".to_string()),
        },
        WritebackAnnotation {
            id: Some(3),
            kind: AnnotationKind::StrikeOut {
                quads: vec![Rect::new(50.0, 540.0, 200.0, 14.0)],
            },
            color: [0, 0, 255],
            opacity: 0.6,
            body: Some("删除线正文".to_string()),
        },
        WritebackAnnotation {
            id: Some(4),
            kind: AnnotationKind::Ink {
                points: vec![Point::new(100.0, 700.0), Point::new(300.0, 740.0)],
                width: 3.5,
            },
            color: [10, 20, 30],
            opacity: 1.0,
            body: Some("墨迹正文".to_string()),
        },
    ]
}

#[test]
fn four_kinds_round_trip_with_ids_bodies_and_opacity() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("four");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let expected = four_kinds();
    let spec = WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: dir.join("out.pdf"),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: expected.clone(),
        }],
    };
    writeback::write(&spec).expect("写回");
    let result = readback::read(&spec.output, None).expect("读回");
    assert_eq!(result.annotations.len(), 4, "四种注释都要读回");
    assert_eq!(result.skipped, 0);

    for (index, (read, want)) in result.annotations.iter().zip(expected.iter()).enumerate() {
        assert_eq!(read.id, want.id, "#{index} id");
        assert_eq!(read.body, want.body, "#{index} 正文");
        assert_eq!(read.color, want.color, "#{index} 颜色");
        assert!(
            (read.opacity - want.opacity).abs() <= 0.01,
            "#{index} 透明度：{} vs {}",
            read.opacity,
            want.opacity
        );
        match (&read.kind, &want.kind) {
            (AnnotationKind::Highlight { quads: got }, AnnotationKind::Highlight { quads }) => {
                assert_eq!(got.len(), quads.len());
                assert!(
                    rect_close(got[0], quads[0]),
                    "高亮几何 {got:?} vs {quads:?}"
                );
            }
            (AnnotationKind::Underline { quads: got }, AnnotationKind::Underline { quads }) => {
                assert!(rect_close(got[0], quads[0]))
            }
            (AnnotationKind::StrikeOut { quads: got }, AnnotationKind::StrikeOut { quads }) => {
                assert!(rect_close(got[0], quads[0]))
            }
            (
                AnnotationKind::Ink {
                    points: got,
                    width: got_width,
                },
                AnnotationKind::Ink { points, width },
            ) => {
                assert_eq!(got.len(), points.len());
                for (a, b) in got.iter().zip(points.iter()) {
                    assert!(close(a.x, b.x) && close(a.y, b.y), "墨迹点 {a:?} vs {b:?}");
                }
                assert!(close(*got_width, *width));
            }
            (got, want) => panic!("#{index} 类型不一致：{got:?} vs {want:?}"),
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reading_again_after_rewrite_is_stable() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("stable");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let annotations = four_kinds();
    let first = dir.join("out1.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: first.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: annotations.clone(),
        }],
    })
    .expect("第一次");
    let second = dir.join("out2.pdf");
    writeback::write(&WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source: first.clone(),
        output: second.clone(),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations,
        }],
    })
    .expect("第二次");

    let a = readback::read(&first, None).expect("读第一次");
    let b = readback::read(&second, None).expect("读第二次");
    assert_eq!(a.annotations.len(), b.annotations.len());
    for (left, right) in a.annotations.iter().zip(b.annotations.iter()) {
        assert_eq!(left.id, right.id);
        assert_eq!(left.body, right.body);
        assert_eq!(left.color, right.color);
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rotated_page_keeps_unrotated_page_space() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("rotated");
    let source = dir.join("rotated.pdf");
    std::fs::write(&source, rotated_page_pdf()).expect("写样本");
    let spec = WritebackSpec {
        updates: Vec::new(),
        deletes: Vec::new(),
        source,
        output: dir.join("out.pdf"),
        pages: vec![WritebackPage {
            page: PageIndex::FIRST,
            annotations: vec![WritebackAnnotation {
                id: Some(9),
                kind: AnnotationKind::Ink {
                    points: vec![Point::new(20.0, 30.0), Point::new(60.0, 80.0)],
                    width: 2.0,
                },
                color: [0, 0, 0],
                opacity: 1.0,
                body: Some("旋转页".to_string()),
            }],
        }],
    };
    writeback::write(&spec).expect("写回");
    let result = readback::read(&spec.output, None).expect("读回");
    let AnnotationKind::Ink { points, .. } = &result.annotations[0].kind else {
        panic!("应为墨迹");
    };
    assert!(close(points[0].x, 20.0) && close(points[0].y, 30.0));
    assert!(close(points[1].x, 60.0) && close(points[1].y, 80.0));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn annotations_without_our_marker_have_no_id() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("foreign");
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
                id: None, // 模拟外部注释：没有 /NM 标记
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(50.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("外部".to_string()),
            }],
        }],
    };
    writeback::write(&spec).expect("写回");
    let result = readback::read(&spec.output, None).expect("读回");
    assert_eq!(result.annotations.len(), 1);
    assert_eq!(result.annotations[0].id, None);
    assert_eq!(result.annotations[0].body.as_deref(), Some("外部"));

    std::fs::remove_dir_all(&dir).ok();
}

/// 与 `intrinsic_rotation.rs` 同款：200×400、`/Rotate 90` 的单页 PDF。
fn rotated_page_pdf() -> Vec<u8> {
    let content = "q 0 g 0 300 50 50 re f Q";
    let stream = format!(
        "<< /Length {} >>\nstream\n{}endstream",
        content.len(),
        content
    );
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 400] /Rotate 90 \
         /Contents 4 0 R /Resources << >> >>"
            .to_string(),
        stream,
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
