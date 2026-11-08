//! 回归：旧文件里重复的 `/NM (pd-fr:<id>)` 不应导致丢注释/保存计数错误。

#[test]
fn duplicate_annotation_markers_are_renumbered_and_saved() {
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-annotation-repair-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建目录");
    // 用**格式正确**的 PDF 生成重复 /NM，避免旧样本的坏 QuadPoints 干扰本回归。
    let source = backend_pdfium::fixtures::two_page_pdf();
    let quads = vec![pdfcore::prelude::Rect::new(100.0, 700.0, 200.0, 12.0)];
    let rect = quads[0];
    let markups = vec![
        backend_pdfium::pdf_patch::NewMarkup {
            page: 0,
            id: Some(1),
            rect,
            quads: quads.clone(),
            kind: backend_pdfium::pdf_patch::MarkupKind::Highlight,
            color: [255, 214, 0],
            opacity: 0.35,
            body: None,
        },
        backend_pdfium::pdf_patch::NewMarkup {
            page: 1,
            id: Some(1),
            rect,
            quads,
            kind: backend_pdfium::pdf_patch::MarkupKind::Highlight,
            color: [255, 214, 0],
            opacity: 0.35,
            body: None,
        },
    ];
    let bytes = backend_pdfium::pdf_patch::append_new_annotations(
        &source,
        &markups,
        &[],
        &[],
        &Default::default(),
    )
    .expect("生成重复标记样本");
    std::fs::write(dir.join("loss.pdf"), bytes).expect("写样本");
    std::env::set_var("PDF_READER_WRITEBACK_EXE", env!("CARGO_BIN_EXE_app"));

    let script = dir.join("repair.json");
    std::fs::write(
        &script,
        r#"{"name":"repair","pdf":"loss.pdf","actions":[
             {"id":"resize","op":"resize","width":700,"height":800},
             {"id":"settle","op":"wait","frames":8},
             {"id":"before","op":"wait","frames":2},
             {"id":"save","op":"save"},
             {"id":"after","op":"wait","frames":5}
           ]}"#,
    )
    .expect("写脚本");
    let report = app::replay::run_script_file(&script, None).expect("回放");
    let before = report
        .actions
        .iter()
        .find(|action| action.id == "before")
        .expect("before");
    assert_eq!(before.annotations, 2, "重复标记也应按两条注释载入");
    let after = report
        .actions
        .iter()
        .find(|action| action.id == "after")
        .expect("after");
    assert_eq!(after.annotations, 2, "保存/重开后仍应有两条");
    let readback = backend_pdfium::readback::read(&dir.join("loss.pdf"), None).expect("读回");
    let ids: Vec<u64> = readback
        .annotations
        .iter()
        .filter_map(|annotation| annotation.id)
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "保存后 /NM 必须唯一");
    let _ = std::fs::remove_dir_all(&dir);
}
