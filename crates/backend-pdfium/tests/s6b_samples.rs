//! S6b：真实阅读器样本的结构回归（读回字段/几何/正文）。
//!
//! 样本来源与说明见 `tests/fixtures/s6b/README.md`。裸 FFI 串行执行。

use std::path::PathBuf;
use std::sync::Mutex;

use backend_pdfium::readback;
use pdfcore::annotation::AnnotationKind;

static LOCK: Mutex<()> = Mutex::new(());

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/s6b")
        .join(name)
}

fn read(name: &str) -> readback::Readback {
    readback::read(&fixture(name), None).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn count(
    annotations: &[readback::ReadAnnotation],
    predicate: impl Fn(&AnnotationKind) -> bool,
) -> usize {
    annotations
        .iter()
        .filter(|annotation| predicate(&annotation.kind))
        .count()
}

fn assert_common(name: &str, read: &readback::Readback, has_body: bool) {
    assert_eq!(read.skipped, 0, "{name}: 不应跳过任何注释");
    for annotation in &read.annotations {
        assert_eq!(annotation.id, None, "{name}: 外部注释不应有 pd-fr id");
        assert!(annotation.page.get() < 2, "{name}: 页号越界");
        assert!(
            (annotation.opacity - 1.0).abs() <= 0.01
                || (annotation.opacity > 0.0 && annotation.opacity < 1.0),
            "{name}: 透明度异常 {}",
            annotation.opacity
        );
        if has_body {
            assert!(
                annotation
                    .body
                    .as_deref()
                    .is_some_and(|body| !body.trim().is_empty()),
                "{name}: 应有 /Contents"
            );
        } else {
            assert_eq!(annotation.body, None, "{name}: 不应有 /Contents");
        }
        match &annotation.kind {
            AnnotationKind::Note { .. } => continue,
            AnnotationKind::Ink { points, width } => {
                assert!(points.len() >= 2, "{name}: 墨迹点太少");
                assert!(*width > 0.0 && width.is_finite(), "{name}: 墨迹线宽异常");
                assert!(
                    annotation
                        .ink_strokes
                        .iter()
                        .all(|stroke| stroke.len() >= 2),
                    "{name}: 墨迹 path 太短"
                );
                for point in points.iter().chain(annotation.ink_strokes.iter().flatten()) {
                    assert!(
                        point.x.is_finite() && point.y.is_finite(),
                        "{name}: 墨迹坐标非有限"
                    );
                    assert!(
                        (-10.0..=620.0).contains(&point.x) && (-10.0..=800.0).contains(&point.y),
                        "{name}: 墨迹坐标越界 {point:?}"
                    );
                }
            }
            AnnotationKind::Highlight { quads }
            | AnnotationKind::Underline { quads }
            | AnnotationKind::StrikeOut { quads } => {
                assert!(!quads.is_empty(), "{name}: 文本类注释没有 quad");
                for quad in quads {
                    assert!(
                        quad.x.is_finite()
                            && quad.y.is_finite()
                            && quad.width > 0.0
                            && quad.height > 0.0,
                        "{name}: quad 非法 {quad:?}"
                    );
                    assert!(
                        (-10.0..=620.0).contains(&quad.x) && (-10.0..=800.0).contains(&quad.y),
                        "{name}: quad 越界 {quad:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn okular_sample_reads_all_four_kinds_with_bodies() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let read = read("okular.pdf");
    assert_common("okular", &read, true);
    assert_eq!(read.annotations.len(), 5);
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Highlight { .. }
        )),
        2
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Underline { .. }
        )),
        1
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::StrikeOut { .. }
        )),
        1
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Ink { .. }
        )),
        1
    );
    let ink = read
        .annotations
        .iter()
        .find_map(|annotation| match &annotation.kind {
            AnnotationKind::Ink { points, width } => Some((points, *width)),
            _ => None,
        })
        .expect("墨迹");
    assert_eq!(ink.0.len(), 56);
    assert!(
        (ink.1 - 2.0).abs() < 0.01,
        "Okular 墨迹线宽来自 /BS：{}",
        ink.1
    );
    assert_eq!(read.extra_ink_strokes, 0, "样本是单 path");
}

#[test]
fn okular_without_content_has_no_bodies() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let read = read("okular_without_content.pdf");
    assert_common("okular_without_content", &read, false);
    assert_eq!(read.annotations.len(), 5);
}

#[test]
fn evince_sample_reads_three_text_kinds() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let read = read("evince.pdf");
    assert_common("evince", &read, true);
    assert_eq!(read.annotations.len(), 4);
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Highlight { .. }
        )),
        2
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Underline { .. }
        )),
        1
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::StrikeOut { .. }
        )),
        1
    );
    // Evince 没给 /C，按默认黄色读回
    assert!(
        read.annotations
            .iter()
            .all(|annotation| annotation.color == [255, 255, 0]),
        "Evince 的颜色应回退为黄色"
    );
}

#[test]
fn firefox_sample_reads_highlight_and_ink() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let read = read("firefox.pdf");
    assert_common("firefox", &read, true);
    assert_eq!(read.annotations.len(), 3);
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Highlight { .. }
        )),
        2
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Ink { .. }
        )),
        1
    );
}

#[test]
fn adobe_sample_reads_from_object_streams() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let read = read("adobe.pdf");
    assert_common("adobe", &read, true);
    assert_eq!(read.annotations.len(), 5);
    for annotation in &read.annotations {
        let expected = match annotation.kind {
            AnnotationKind::Highlight { .. } => 0.40, // Adobe 显式写了 /CA=0.40
            _ => 1.0,
        };
        assert!(
            (annotation.opacity - expected).abs() < 0.01,
            "Adobe 透明度：{:?} = {}（期望 {expected}）",
            annotation.kind,
            annotation.opacity
        );
    }
    let ink = read
        .annotations
        .iter()
        .find_map(|annotation| match &annotation.kind {
            AnnotationKind::Ink { points, .. } => Some(points.len()),
            _ => None,
        })
        .expect("墨迹");
    assert_eq!(ink, 181);
}

#[test]
fn foxit_sample_reads_all_four_kinds() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let read = read("foxit.pdf");
    assert_common("foxit", &read, true);
    assert_eq!(read.annotations.len(), 5);
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Highlight { .. }
        )),
        2
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Underline { .. }
        )),
        1
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::StrikeOut { .. }
        )),
        1
    );
    assert_eq!(
        count(&read.annotations, |kind| matches!(
            kind,
            AnnotationKind::Ink { .. }
        )),
        1
    );
}
