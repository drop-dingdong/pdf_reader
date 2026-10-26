//! 同一个标注被点击多次时，只应存在**一个**浮窗（用户实测：每点一次多一个）。
//!
//! 这里不模拟鼠标，而是直接走"点击标注"的那条代码路径：先建标注，再连续两次打开它的正文。

fn backend() -> bool {
    backend_pdfium::pdfium().is_ok()
}

#[test]
fn clicking_the_same_annotation_twice_reuses_one_float() {
    if !backend() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-annot-click-{}-{}",
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

    // 第一次回放：建标注 + 保存（正文条目因此存在）
    let first = dir.join("step1.json");
    std::fs::write(
        &first,
        r#"{"name":"step1","pdf":"rotated_2p.pdf","actions":[
            {"id":"resize","op":"resize","width":700,"height":800},
            {"id":"top","op":"set_scroll","x":0,"y":0},
            {"id":"settle","op":"wait","frames":8},
            {"id":"annotate","op":"highlight","page":0,"x":50,"y":690,"width":450,"height":50},
            {"id":"save","op":"save"},
            {"id":"click1","op":"click","page":0,"x":186,"y":706},
            {"id":"click2","op":"click","page":0,"x":186,"y":706}
        ]}"#,
    )
    .expect("写脚本");

    let report = app::replay::run_script_file(&first, None).expect("回放");
    let click = |id: &str| {
        report
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("缺动作 {id}"))
    };
    // 两次点击之后浮窗数量必须停在 1（第二次是"高亮置顶"，不是新建）
    assert_eq!(
        click("click1").floats,
        click("click2").floats,
        "第二次点击同一标注不应当新增浮窗"
    );
    assert_eq!(click("click2").floats, 1, "应当只有一个正文浮窗");

    std::fs::remove_dir_all(&dir).ok();
}
