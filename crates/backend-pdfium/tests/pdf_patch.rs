use std::path::PathBuf;
use std::sync::Mutex;

use backend_pdfium::pdf_index::{page_annotations, PdfObjectRef};
use backend_pdfium::pdf_patch::append_content_updates;

static LOCK: Mutex<()> = Mutex::new(());

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/s6b")
        .join(name)
}

fn supported_refs(pdf: &[u8]) -> Vec<PdfObjectRef> {
    page_annotations(pdf)
        .expect("索引")
        .iter()
        .flatten()
        .filter(|entry| !entry.hidden && matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .filter_map(|entry| entry.reference)
        .collect()
}

fn read_bytes(bytes: &[u8]) -> backend_pdfium::readback::Readback {
    let path = std::env::temp_dir().join(format!(
        "pdf-patch-{}-{}.pdf",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, bytes).expect("写临时 PDF");
    let result = backend_pdfium::readback::read(&path, None).expect("readback");
    let _ = std::fs::remove_file(path);
    result
}

#[test]
fn appends_same_generation_update_for_okular_highlight() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = std::fs::read(fixture("okular.pdf")).expect("读样本");
    let refs = supported_refs(&source);
    let reference = *refs
        .iter()
        .find(|reference| reference.num == 11 && reference.gen == 1)
        .expect("Okular highlight 11 1");
    let output = append_content_updates(&source, &[(reference, Some("P4A 正文".to_string()))])
        .expect("patch");
    assert!(output.len() < source.len() + 1200, "只应追加少量字节");
    assert!(
        output.windows(4).any(|window| window == b"/M ("),
        "正文更新应刷新 /M"
    );
    assert_eq!(
        page_annotations(&output)
            .expect("重索引")
            .iter()
            .flatten()
            .filter(|e| matches!(e.subtype, Some(9 | 10 | 12 | 15)))
            .count(),
        refs.len(),
        "注释数不变"
    );
    let read = read_bytes(&output);
    assert!(
        read.annotations
            .iter()
            .any(|annotation| annotation.body.as_deref() == Some("P4A 正文")),
        "读回新正文"
    );
}

#[test]
fn appends_same_generation_update_for_adobe_object_stream_ref() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = std::fs::read(fixture("adobe.pdf")).expect("读样本");
    let reference = supported_refs(&source)[0];
    let output = append_content_updates(&source, &[(reference, Some("P4A Adobe".to_string()))])
        .expect("patch");
    assert!(
        output.windows(7).any(|window| window == b"/ObjStm"),
        "ObjStm 应保留"
    );
    let read = read_bytes(&output);
    assert!(
        read.annotations
            .iter()
            .any(|annotation| annotation.body.as_deref() == Some("P4A Adobe")),
        "ObjStm 对象也要能读回"
    );
}

#[test]
fn same_annotation_can_be_patched_twice() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = std::fs::read(fixture("okular.pdf")).expect("读样本");
    let reference = *supported_refs(&source)
        .iter()
        .find(|reference| reference.num == 11 && reference.gen == 1)
        .expect("11 1");
    let once = append_content_updates(&source, &[(reference, Some("第一次".to_string()))])
        .expect("第一次 patch");
    let twice = append_content_updates(&once, &[(reference, Some("第二次".to_string()))])
        .expect("第二次 patch");
    let read = read_bytes(&twice);
    assert!(
        read.annotations
            .iter()
            .any(|annotation| annotation.body.as_deref() == Some("第二次")),
        "连续 patch 后应读到最新正文"
    );
}

#[test]
fn appends_page_object_delete_for_okular_annotation() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = std::fs::read(fixture("okular.pdf")).expect("读样本");
    let before = supported_refs(&source).len();
    let reference = supported_refs(&source)[0];
    let output =
        backend_pdfium::pdf_patch::append_deletes(&source, &[reference]).expect("delete patch");
    let read = read_bytes(&output);
    assert_eq!(read.annotations.len(), before - 1, "只删一条");
    assert!(
        output.len() < source.len() + 1200,
        "删除只应追加页对象 + xref"
    );
}

#[test]
fn appends_page_object_delete_with_popup() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = std::fs::read(fixture("foxit.pdf")).expect("读样本");
    let pages = page_annotations(&source).expect("索引");
    let before: usize = pages.iter().map(Vec::len).sum();
    let reference = pages
        .iter()
        .flatten()
        .find(|entry| entry.popup.is_some() && matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .and_then(|entry| entry.reference)
        .expect("带 Popup 的父注释");
    let output =
        backend_pdfium::pdf_patch::append_deletes(&source, &[reference]).expect("delete patch");
    let after_pages = page_annotations(&output).expect("重索引");
    let after: usize = after_pages.iter().map(Vec::len).sum();
    assert_eq!(after, before - 2, "父注释与 Popup 都应删除");
    assert!(
        output.len() < source.len() + 2_000,
        "删除只应追加页对象，不应整份重写"
    );
}

#[test]
fn appends_page_object_delete_for_indirect_annots_array() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = std::fs::read(fixture("okular.pdf")).expect("读样本");
    let pages = page_annotations(&source).expect("索引");
    let before = read_bytes(&source).annotations.len();
    let reference = pages[1]
        .iter()
        .find(|entry| matches!(entry.subtype, Some(9 | 10 | 12 | 15)))
        .and_then(|entry| entry.reference)
        .expect("第 2 页的注释");
    let output =
        backend_pdfium::pdf_patch::append_deletes(&source, &[reference]).expect("delete patch");
    let read = read_bytes(&output);
    assert_eq!(read.annotations.len(), before - 1);
}

fn inline_pdf() -> Vec<u8> {
    let content = "BT /F1 12 Tf 20 100 Td (Inline) Tj ET\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R \
         /Annots [<< /Type /Annot /Subtype /Highlight /Rect [50 100 150 114] \
         /QuadPoints [50 114 150 114 50 100 150 100] /C [1 1 0] /CA 0.35 \
         /Contents (OLD) >>] >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{}endstream",
            content.len(),
            content
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for offset in offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

#[test]
fn inline_annotation_body_update_and_delete() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let source = inline_pdf();
    let updated = backend_pdfium::pdf_patch::append_inline_changes(
        &source,
        &backend_pdfium::pdf_patch::InlineChanges {
            updates: vec![backend_pdfium::pdf_patch::InlineUpdate {
                page: 0,
                position: 0,
                body: Some("NEW".to_string()),
            }],
            deletes: Vec::new(),
        },
    )
    .expect("inline update");
    // readback 需要路径；先写临时文件
    std::fs::write("/tmp/pdf-inline-test.pdf", &updated).expect("write");
    let read = backend_pdfium::readback::read(&PathBuf::from("/tmp/pdf-inline-test.pdf"), None)
        .expect("read");
    assert_eq!(read.annotations.len(), 1);
    assert_eq!(read.annotations[0].body.as_deref(), Some("NEW"));

    let deleted = backend_pdfium::pdf_patch::append_inline_changes(
        &updated,
        &backend_pdfium::pdf_patch::InlineChanges {
            updates: Vec::new(),
            deletes: vec![backend_pdfium::pdf_patch::InlineDelete {
                page: 0,
                position: 0,
            }],
        },
    )
    .expect("inline delete");
    assert!(page_annotations(&deleted).expect("index")[0].is_empty());
    let _ = std::fs::remove_file("/tmp/pdf-inline-test.pdf");
}
