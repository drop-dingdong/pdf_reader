//! 注释作者 / 时间的写回与读回（2026-10-31）。
//!
//! 覆盖：新建注释写 `/T`、`/CreationDate`、`/M`；正文更新只刷新 `/M`；
//! 读回能把它们解析回 Unix 秒。缺 PDFium 时跳过。

use std::sync::Mutex;

use backend_pdfium::pdf_patch::{AnnotationMeta, InlineChanges};
use backend_pdfium::readback;
use backend_pdfium::writeback::{
    self, AnnotationMetadata, WritebackAnnotation, WritebackPage, WritebackSpec, WritebackUpdate,
};
use pdfcore::annotation::AnnotationKind;
use pdfcore::prelude::*;

static LOCK: Mutex<()> = Mutex::new(());

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-annotation-meta-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn highlight(id: u64, body: &str) -> WritebackAnnotation {
    WritebackAnnotation {
        id: Some(id),
        kind: AnnotationKind::Highlight {
            quads: vec![Rect::new(50.0, 660.0, 200.0, 14.0)],
        },
        color: [255, 255, 0],
        opacity: 0.35,
        body: Some(body.to_string()),
    }
}

#[test]
fn new_annotation_writes_author_and_dates() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("create");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let output = dir.join("out.pdf");

    let spec = WritebackSpec {
        source: source.clone(),
        output: output.clone(),
        pages: vec![WritebackPage {
            origin: pdfcore::prelude::Point::ORIGIN,
            page: PageIndex::FIRST,
            annotations: vec![highlight(7, "正文")],
        }],
        updates: Vec::new(),
        deletes: Vec::new(),
    };
    let mut meta = AnnotationMetadata::default();
    meta.by_id.insert(
        7,
        AnnotationMeta {
            author: Some("张三".to_string()),
            created: Some(1_700_000_000),
            modified: Some(1_700_000_600),
        },
    );
    writeback::write_incremental_with_meta(&spec, &InlineChanges::default(), &meta)
        .expect("写回成功");

    let text = String::from_utf8_lossy(&std::fs::read(&output).expect("读输出")).to_string();
    assert!(text.contains("/T <"), "输出里应有 UTF-16 的 /T");
    assert!(
        text.contains("/CreationDate (D:20231114221320Z)"),
        "创建时间应按 Unix 秒写出"
    );
    assert!(
        text.contains("/M (D:20231114222320Z)"),
        "修改时间应按 Unix 秒写出"
    );

    let read = readback::read(&output, None).expect("读回");
    let item = read
        .annotations
        .iter()
        .find(|annotation| annotation.id == Some(7))
        .expect("应读回 id=7 的注释");
    assert_eq!(item.author.as_deref(), Some("张三"));
    assert_eq!(item.created, Some(1_700_000_000));
    assert_eq!(item.modified, Some(1_700_000_600));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn note_round_trips_with_rect_and_body() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("note");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let output = dir.join("out.pdf");
    let rect = Rect::new(120.0, 600.0, 18.0, 18.0);
    let mut meta = AnnotationMetadata::default();
    meta.by_id.insert(
        9,
        AnnotationMeta {
            author: Some("张三".to_string()),
            created: Some(1_700_000_000),
            modified: Some(1_700_000_000),
        },
    );
    writeback::write_incremental_with_meta(
        &WritebackSpec {
            source,
            output: output.clone(),
            pages: vec![WritebackPage {
                origin: pdfcore::prelude::Point::ORIGIN,
                page: PageIndex::FIRST,
                annotations: vec![WritebackAnnotation {
                    id: Some(9),
                    kind: AnnotationKind::Note { rect },
                    color: [255, 214, 0],
                    opacity: 1.0,
                    body: Some("便签正文".to_string()),
                }],
            }],
            updates: Vec::new(),
            deletes: Vec::new(),
        },
        &InlineChanges::default(),
        &meta,
    )
    .expect("写回便签");

    let read = readback::read(&output, None).expect("读回");
    let item = read
        .annotations
        .iter()
        .find(|annotation| annotation.id == Some(9))
        .expect("应读回 id=9 的便签");
    match &item.kind {
        AnnotationKind::Note { rect: read_rect } => {
            assert!((read_rect.x - rect.x).abs() < 0.5, "{read_rect:?}");
            assert!((read_rect.width - rect.width).abs() < 0.5, "{read_rect:?}");
        }
        other => panic!("种类不对：{other:?}"),
    }
    assert_eq!(item.body.as_deref(), Some("便签正文"));
    assert_eq!(item.author.as_deref(), Some("张三"));
    assert_eq!(item.created, Some(1_700_000_000));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn body_update_refreshes_only_modified() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("update");
    let source = dir.join("in.pdf");
    std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let first = dir.join("first.pdf");

    let mut meta = AnnotationMetadata::default();
    meta.by_id.insert(
        7,
        AnnotationMeta {
            author: Some("李四".to_string()),
            created: Some(1_700_000_000),
            modified: Some(1_700_000_000),
        },
    );
    writeback::write_incremental_with_meta(
        &WritebackSpec {
            source: source.clone(),
            output: first.clone(),
            pages: vec![WritebackPage {
                origin: pdfcore::prelude::Point::ORIGIN,
                page: PageIndex::FIRST,
                annotations: vec![highlight(7, "旧正文")],
            }],
            updates: Vec::new(),
            deletes: Vec::new(),
        },
        &InlineChanges::default(),
        &meta,
    )
    .expect("第一次写回");

    // 找到注释对象号，再按对象号更新正文。
    let bytes = std::fs::read(&first).expect("读第一次输出");
    let pages = backend_pdfium::pdf_index::page_annotations(&bytes).expect("解析注释引用");
    let reference = pages[0]
        .iter()
        .find(|entry| entry.subtype == Some(9))
        .and_then(|entry| entry.reference)
        .expect("应有独立对象的高亮注释");
    let second = dir.join("second.pdf");
    let mut update_meta = AnnotationMetadata::default();
    update_meta.by_ref.insert(
        reference,
        AnnotationMeta {
            modified: Some(1_700_500_000),
            ..Default::default()
        },
    );
    writeback::write_incremental_with_meta(
        &WritebackSpec {
            source: first.clone(),
            output: second.clone(),
            pages: Vec::new(),
            updates: vec![WritebackUpdate {
                reference,
                body: Some("新正文".to_string()),
            }],
            deletes: Vec::new(),
        },
        &InlineChanges::default(),
        &update_meta,
    )
    .expect("更新正文");

    let read = readback::read(&second, None).expect("读回");
    let item = read
        .annotations
        .iter()
        .find(|annotation| annotation.id == Some(7))
        .expect("应读回 id=7");
    assert_eq!(item.body.as_deref(), Some("新正文"));
    assert_eq!(item.created, Some(1_700_000_000), "创建时间不应变");
    assert_eq!(item.modified, Some(1_700_500_000), "修改时间应刷新");
    assert_eq!(item.author.as_deref(), Some("李四"));
    std::fs::remove_dir_all(&dir).ok();
}
