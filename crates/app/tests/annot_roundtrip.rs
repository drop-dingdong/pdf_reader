//! S2 验收：**保存 → 重新打开**，标注与正文都还在（用户实测踩到"只存了正文、没存标注"）。
//!
//! 两次回放共用同一个临时目录：第一次"标注 + 保存"，第二次"打开同一份 PDF"。

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn backend() -> bool {
    backend_pdfium::pdfium().is_ok()
}

#[test]
fn annotation_and_body_survive_a_save_and_reopen() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-annot-roundtrip-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    app::replay::samples::Sample::Rotated
        .write(&dir)
        .expect("写样本");

    let script = |name: &str, actions: &str| {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"pdf\": \"rotated_2p.pdf\",\n  \"actions\": [{actions}]\n}}\n"
            ),
        )
        .expect("写脚本");
        path
    };

    // P3 保存会起真实 app 子进程写 PDF；测试进程本身不处理 `--writeback`
    std::env::set_var("PDF_READER_WRITEBACK_EXE", env!("CARGO_BIN_EXE_app"));

    // 第 1 次：标注后保存（`save` 与点"保存"按钮/Ctrl+S 等价）
    let first = script(
        "step1.json",
        r#"
    { "id": "resize", "op": "resize", "width": 700, "height": 800 },
    { "id": "top", "op": "set_scroll", "x": 0, "y": 0 },
    { "id": "settle", "op": "wait", "frames": 8 },
    { "id": "annotate", "op": "highlight", "page": 0, "x": 50, "y": 690, "width": 450, "height": 50 },
    { "id": "tool", "op": "set_tool", "tool": "ink" },
    { "id": "ink", "op": "ink", "page": 0,
      "points": [[50, 700], [120, 720], [200, 690], [280, 730]] },
    { "id": "save", "op": "save" },
    { "id": "after", "op": "wait", "frames": 3 }"#,
    );
    let report = app::replay::run_script_file(&first, None).expect("第一次回放");
    let annotate = report
        .actions
        .iter()
        .find(|action| action.id == "annotate")
        .expect("标注动作");
    assert_eq!(annotate.annotations, 1, "应当产生一条标注");

    // P3：保存 = 写 PDF；不再生成/更新 `.annot.json`
    let overlay_path = dir.join("rotated_2p.pdf.annot.json");
    assert!(
        !overlay_path.exists(),
        "P3 后保存不应创建 .annot.json：{}",
        overlay_path.display()
    );
    let ink = report
        .actions
        .iter()
        .find(|action| action.id == "ink")
        .expect("墨迹动作");
    assert_eq!(ink.annotations, 2, "高亮之后再加一笔墨迹");

    let readback = backend_pdfium::readback::read(&dir.join("rotated_2p.pdf"), None)
        .expect("读回保存后的 PDF");
    assert_eq!(readback.annotations.len(), 2, "PDF 里应有两条注释");
    assert!(
        readback.annotations.iter().any(|annotation| matches!(
            annotation.kind,
            pdfcore::annotation::AnnotationKind::Ink { .. }
        )),
        "墨迹应写进 PDF"
    );

    // 第 2 次：重新打开同一份 PDF → 标注应当被加载回来
    let second = script(
        "step2.json",
        r#"
    { "id": "resize", "op": "resize", "width": 700, "height": 800 },
    { "id": "settle", "op": "wait", "frames": 8 }"#,
    );
    let reopened = app::replay::run_script_file(&second, None).expect("第二次回放");
    let settle = reopened
        .actions
        .iter()
        .find(|action| action.id == "settle")
        .expect("等待动作");
    assert_eq!(
        settle.annotations, 2,
        "重新打开后应当加载回两条标注（用户实测：这里为 0 = 图层没落盘/没读回）"
    );
    // `annotation_bounds` 取最后一条（加载顺序保留：高亮在前、墨迹在后）
    let (x, y, width, height) = settle.annotation_bounds.expect("墨迹包围盒");
    for (label, actual, expected) in [
        ("x", x, 49.0),
        ("y", y, 689.0),
        ("width", width, 232.0),
        ("height", height, 42.0),
    ] {
        assert!(
            (actual - expected).abs() < 0.01,
            "重开后墨迹的 {label} 应为 {expected}，实际 {actual}（几何必须原样回来）"
        );
    }

    std::env::remove_var("PDF_READER_WRITEBACK_EXE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn external_highlight_with_ca_one_keeps_legacy_35_percent() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-external-opacity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let source = dir.join("rotated_2p.pdf");
    app::replay::samples::Sample::Rotated
        .write(&dir)
        .expect("写样本");
    let output = dir.join("external.pdf");
    let spec = backend_pdfium::writeback::WritebackSpec {
        source: source.clone(),
        output: output.clone(),
        pages: vec![backend_pdfium::writeback::WritebackPage {
            page: pdfcore::prelude::PageIndex::FIRST,
            origin: pdfcore::prelude::Point::ORIGIN,
            annotations: vec![backend_pdfium::writeback::WritebackAnnotation {
                // 没有 /NM：模拟其它阅读器写出的外部高亮。
                id: None,
                kind: pdfcore::annotation::AnnotationKind::Highlight {
                    quads: vec![pdfcore::prelude::Rect::new(50.0, 690.0, 450.0, 50.0)],
                },
                color: [255, 214, 0],
                opacity: 1.0,
                body: None,
            }],
        }],
        updates: Vec::new(),
        deletes: Vec::new(),
    };
    backend_pdfium::writeback::write(&spec).expect("写外部注释");
    let readback = backend_pdfium::readback::read(&output, None).expect("读回外部注释");
    let annotation = readback.annotations.first().expect("一条外部注释");
    assert!(annotation.id.is_none(), "外部注释不应有 /NM");
    assert!(
        (annotation.opacity - 0.35).abs() < 0.01,
        "外部 /CA 1.0 应保持旧的 35% 兼容显示，实际 {}",
        annotation.opacity
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn restyled_highlight_opacity_is_saved() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-opacity-roundtrip-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    app::replay::samples::Sample::Rotated
        .write(&dir)
        .expect("写样本");
    std::env::set_var("PDF_READER_WRITEBACK_EXE", env!("CARGO_BIN_EXE_app"));
    let script = dir.join("opacity.json");
    std::fs::write(
        &script,
        r#"{"name":"opacity","pdf":"rotated_2p.pdf","actions":[
             {"id":"resize","op":"resize","width":700,"height":800},
             {"id":"top","op":"set_scroll","x":0,"y":0},
             {"id":"settle","op":"wait","frames":8},
             {"id":"a","op":"highlight","page":0,"x":50,"y":690,"width":450,"height":50},
             {"id":"save1","op":"save"},
             {"id":"settle1","op":"wait","frames":3},
             {"id":"opacity","op":"opacity","value":1.0},
             {"id":"save2","op":"save"},
             {"id":"after","op":"wait","frames":3}
           ]}"#,
    )
    .expect("写脚本");
    app::replay::run_script_file(&script, None).expect("回放");
    let readback = backend_pdfium::readback::read(&dir.join("rotated_2p.pdf"), None)
        .expect("读回保存后的 PDF");
    let annotation = readback.annotations.first().expect("一条注释");
    assert!(
        (annotation.opacity - 1.0).abs() < 0.01,
        "保存后的透明度应为 1.0，实际 {}",
        annotation.opacity
    );
    std::env::remove_var("PDF_READER_WRITEBACK_EXE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn consecutive_saves_keep_earlier_annotations() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-p3-consecutive-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建目录");
    app::replay::samples::Sample::Rotated
        .write(&dir)
        .expect("写样本");
    std::env::set_var("PDF_READER_WRITEBACK_EXE", env!("CARGO_BIN_EXE_app"));
    let script = dir.join("twice.json");
    std::fs::write(
        &script,
        r#"{"name":"twice","pdf":"rotated_2p.pdf","actions":[
             {"id":"resize","op":"resize","width":700,"height":800},
             {"id":"a","op":"highlight","page":0,"x":50,"y":690,"width":200,"height":20},
             {"id":"save1","op":"save"},
             {"id":"settle","op":"wait","frames":8},
             {"id":"b","op":"highlight","page":0,"x":50,"y":690,"width":200,"height":20},
             {"id":"save2","op":"save"},
             {"id":"after","op":"wait","frames":3}
           ]}"#,
    )
    .expect("写脚本");
    let report = app::replay::run_script_file(&script, None).expect("回放");
    assert_eq!(
        report
            .actions
            .iter()
            .find(|a| a.id == "after")
            .unwrap()
            .annotations,
        2,
        "两次保存后内存里应看到两条"
    );
    let readback = backend_pdfium::readback::read(&dir.join("rotated_2p.pdf"), None).expect("读回");
    assert_eq!(
        readback.annotations.len(),
        2,
        "第二次写回不能把第一次的注释删掉"
    );
    assert!(
        !dir.join("rotated_2p.pdf.annot.json").exists(),
        "连续保存也不应生成 A"
    );
    std::env::remove_var("PDF_READER_WRITEBACK_EXE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn undoing_and_redoing_after_save_then_new_annotation_keeps_both() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-p3-undoredo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建目录");
    app::replay::samples::Sample::Rotated
        .write(&dir)
        .expect("写样本");
    std::env::set_var("PDF_READER_WRITEBACK_EXE", env!("CARGO_BIN_EXE_app"));
    let script = dir.join("undo_redo_save.json");
    std::fs::write(
        &script,
        r#"{"name":"undo_redo_save","pdf":"rotated_2p.pdf","actions":[
             {"id":"resize","op":"resize","width":700,"height":800},
             {"id":"a","op":"highlight","page":0,"x":50,"y":690,"width":200,"height":20},
             {"id":"save1","op":"save"},
             {"id":"settle","op":"wait","frames":8},
             {"id":"undo","op":"undo"},
             {"id":"redo","op":"redo"},
             {"id":"b","op":"highlight","page":0,"x":50,"y":690,"width":200,"height":20},
             {"id":"save2","op":"save"},
             {"id":"after","op":"wait","frames":3}
           ]}"#,
    )
    .expect("写脚本");
    let report = app::replay::run_script_file(&script, None).expect("回放");
    assert_eq!(
        report
            .actions
            .iter()
            .find(|a| a.id == "after")
            .unwrap()
            .annotations,
        2,
        "撤销/重做后新增，二次保存后应有两条"
    );
    let readback = backend_pdfium::readback::read(&dir.join("rotated_2p.pdf"), None).expect("读回");
    assert_eq!(readback.annotations.len(), 2, "撤销重做的那条不能被误删");
    std::env::remove_var("PDF_READER_WRITEBACK_EXE");
    std::fs::remove_dir_all(&dir).ok();
}
