//! S5b-1：`app --writeback <job.json>` 的端到端测试（需要 PDFium）。
//!
//! 覆盖：job 解析（相对路径、四种 kind）、真正写文件、注释计数；
//! 以及"未知 kind / 退化几何"这类**在写之前就要失败**的路径。
//! 写回 FFI 不能用 pdfium-render 的全局锁，所以本文件的测试用互斥锁串行。

use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-writeback-cli-{}-{}-{}",
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

#[test]
fn a_job_file_exports_the_annotations_next_to_it() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("ok");
    // pdf 与 output 用**相对路径**：必须相对 job 文件所在目录解析
    std::fs::write(dir.join("in.pdf"), backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let job = dir.join("job.json");
    std::fs::write(
        &job,
        r#"{
          "pdf": "in.pdf",
          "output": "out.pdf",
          "pages": [
            { "page": 0, "annotations": [
              { "kind": "ink", "points": [[50, 720], [300, 720]], "width": 2.0,
                "color": [255, 0, 0], "opacity": 1.0, "body": "墨迹" },
              { "kind": "highlight", "quads": [[50, 660, 200, 14]],
                "color": [255, 255, 0], "opacity": 0.35, "body": "高亮" }
            ] }
          ]
        }"#,
    )
    .expect("写 job");

    let summary = app::writeback::run_job(&job).expect("写回成功");
    assert!(summary.contains("2 条标注"), "实际：{summary}");
    let output = dir.join("out.pdf");
    assert!(output.is_file(), "输出文件必须存在");
    // 第 1 页原有 1 个链接注释 + 2 条新增
    let counts = backend_pdfium::writeback::count_annotations(&output).expect("数注释");
    assert_eq!(counts, vec![3, 0]);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_kind_is_rejected_before_touching_any_file() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("bad-kind");
    std::fs::write(dir.join("in.pdf"), backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let job = dir.join("job.json");
    std::fs::write(
        &job,
        r#"{ "pdf": "in.pdf", "output": "out.pdf",
             "pages": [ { "page": 0, "annotations": [ { "kind": "scribble" } ] } ] }"#,
    )
    .expect("写 job");

    let error = app::writeback::run_job(&job).expect_err("未知种类必须被拒绝");
    assert!(error.contains("未知标注种类"), "实际：{error}");
    assert!(!dir.join("out.pdf").exists(), "失败时不能产出文件");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_degenerate_ink_stroke_is_rejected_with_a_readable_reason() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("short-ink");
    std::fs::write(dir.join("in.pdf"), backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
    let job = dir.join("job.json");
    std::fs::write(
        &job,
        r#"{ "pdf": "in.pdf", "output": "out.pdf",
             "pages": [ { "page": 0, "annotations": [
               { "kind": "ink", "points": [[10, 10]], "width": 2.0 } ] } ] }"#,
    )
    .expect("写 job");

    let error = app::writeback::run_job(&job).expect_err("单点墨迹必须被拒绝");
    assert!(
        error.contains("几何无效") || error.contains("至少需要两个点"),
        "实际：{error}"
    );
    assert!(!dir.join("out.pdf").exists());

    std::fs::remove_dir_all(&dir).ok();
}
