//! 回归：链接 hover 出现的临时预览，点击后要固定为正式浮窗。

#[test]
fn clicking_an_inline_link_hover_preview_pins_it() {
    if backend_pdfium::pdfium().is_err() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-hover-pin-{}-{}",
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
    // 样本的链接是 `/Fit`（没有落点），临时预览需要 `/XYZ` 目标点；就地改一份测试 PDF。
    let mut bytes = app::replay::samples::Sample::Rotated.pdf_bytes();
    let old = b"/Dest [5 0 R /Fit]";
    let new = b"/Dest [5 0 R /XYZ 100 700 null]";
    let found = bytes
        .windows(old.len())
        .position(|window| window == old)
        .expect("样本里应有 /Fit 链接");
    bytes.splice(found..found + old.len(), new.iter().copied());
    std::fs::write(dir.join("rotated_2p.pdf"), bytes).expect("写带落点的样本");
    let script = dir.join("pin.json");
    std::fs::write(
        &script,
        r#"{"name":"pin","pdf":"rotated_2p.pdf","actions":[
             {"id":"resize","op":"resize","width":700,"height":800},
             {"id":"top","op":"set_scroll","x":0,"y":0},
             {"id":"settle","op":"wait","frames":8},
             {"id":"seed","op":"seed_dest_snapshot","page":1,"index":0},
             {"id":"hover","op":"hover_link","page":0,"link":0},
             {"id":"preview","op":"wait","frames":60},
             {"id":"click","op":"click","page":0,"x":200,"y":710},
             {"id":"after","op":"wait","frames":5}
           ]}"#,
    )
    .expect("写脚本");
    let report = app::replay::run_script_file(&script, None).expect("回放");
    let after = report
        .actions
        .iter()
        .find(|action| action.id == "after")
        .expect("after");
    assert_eq!(after.floats, 1, "点击临时 hover 预览后应固定为 1 个浮窗");
    let _ = std::fs::remove_dir_all(&dir);
}
