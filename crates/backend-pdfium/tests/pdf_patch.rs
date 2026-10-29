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
