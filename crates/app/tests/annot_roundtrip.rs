//! S2 验收：**保存 → 重新打开**，标注与正文都还在（用户实测踩到"只存了正文、没存标注"）。
//!
//! 两次回放共用同一个临时目录：第一次"标注 + 保存"，第二次"打开同一份 PDF"。

fn backend() -> bool {
    backend_pdfium::pdfium().is_ok()
}

#[test]
fn annotation_and_body_survive_a_save_and_reopen() {
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

    // 第 1 次：标注后保存（`save` 与点"保存"按钮/Ctrl+S 等价）
    let first = script(
        "step1.json",
        r#"
    { "id": "resize", "op": "resize", "width": 700, "height": 800 },
    { "id": "top", "op": "set_scroll", "x": 0, "y": 0 },
    { "id": "settle", "op": "wait", "frames": 8 },
    { "id": "annotate", "op": "highlight", "page": 0, "x": 50, "y": 690, "width": 450, "height": 50 },
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

    // 两个文件都要真的写出来
    let overlay_path = dir.join("rotated_2p.pdf.annot.json");
    let sidecar_path = dir.join("rotated_2p.pdf.json");
    assert!(
        overlay_path.exists(),
        "标注图层必须落盘：{}",
        overlay_path.display()
    );
    assert!(
        sidecar_path.exists(),
        "主存档（正文所在）必须落盘：{}",
        sidecar_path.display()
    );
    let overlay = std::fs::read_to_string(&overlay_path).expect("读图层");
    assert!(
        overlay.contains("highlight"),
        "图层里应当有 highlight：{overlay}"
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
        settle.annotations, 1,
        "重新打开后应当加载回那条标注（用户实测：这里为 0 = 图层没落盘/没读回）"
    );

    std::fs::remove_dir_all(&dir).ok();
}
