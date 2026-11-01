//! S8a：整理副本的端到端测试（`app::compact::run` / `app --compact`）。
//!
//! 与其它 PDFium 测试一样：同一进程里串行调用，缺 PDFium 时跳过。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use backend_pdfium::writeback::count_annotations;

static LOCK: Mutex<()> = Mutex::new(());

fn backend() -> bool {
    backend_pdfium::pdfium().is_ok()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../backend-pdfium/tests/fixtures/s6b")
        .join(name)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-compact-cli-{}-{}-{}",
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

/// 多修订样本：生成更小的副本，页数与注释数不变。
#[test]
fn compact_writes_a_smaller_copy_and_preserves_annotations() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = temp_dir("ok");
    let source = fixture("okular.pdf");
    let output = dir.join("okular.compact.pdf");

    let message = app::compact::run(&source, &output).expect("整理成功");
    assert!(message.contains("已整理"), "实际：{message}");
    let before = std::fs::metadata(&source).unwrap().len();
    let after = std::fs::metadata(&output).unwrap().len();
    assert!(after < before, "应生成更小的副本：{before} → {after}");
    assert_eq!(
        count_annotations(&source).unwrap(),
        count_annotations(&output).unwrap(),
        "逐页注释数必须一致"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// 已经很紧凑的文件：不写副本，只给一句说明。
#[test]
fn already_compact_input_is_not_written() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = temp_dir("twice");
    let source = fixture("okular.pdf");
    let first = dir.join("once.pdf");
    let second = dir.join("twice.pdf");
    app::compact::run(&source, &first).expect("第一次整理");

    let message = app::compact::run(&first, &second).expect("第二次整理应正常返回");
    assert!(message.contains("已经很紧凑"), "实际：{message}");
    assert!(!second.exists(), "不更小时不应写文件");
    std::fs::remove_dir_all(&dir).ok();
}

/// 目标已存在：拒绝覆盖。
#[test]
fn existing_target_is_refused() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = temp_dir("exists");
    let source = fixture("okular.pdf");
    let output = dir.join("taken.pdf");
    std::fs::write(&output, b"occupied").expect("占位");
    let error = app::compact::run(&source, &output).unwrap_err();
    assert!(error.contains("不覆盖"), "实际：{error}");
    assert_eq!(std::fs::read(&output).unwrap(), b"occupied");
    std::fs::remove_dir_all(&dir).ok();
}

/// 带签名：拒绝，且不产生文件。
#[test]
fn signed_pdf_is_refused_without_output() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = temp_dir("signed");
    let mut bytes = std::fs::read(fixture("okular.pdf")).expect("读样本");
    bytes.extend_from_slice(b"\n% /ByteRange [0 0 0 0]\n");
    let source = dir.join("signed.pdf");
    std::fs::write(&source, &bytes).expect("写样本");
    let output = dir.join("out.pdf");

    let error = app::compact::run(&source, &output).unwrap_err();
    assert!(error.contains("数字签名"), "实际：{error}");
    assert!(!output.exists(), "拒绝时不应产生输出");
    std::fs::remove_dir_all(&dir).ok();
}

#[allow(dead_code)]
fn exists(path: &Path) -> bool {
    path.exists()
}

/// `--compact` 子进程契约：成功打印一行摘要；被拒绝时非 0 退出、stderr 可读、不产生文件。
/// GUI 的「整理副本」就是走这条路径。
#[test]
fn cli_binary_works_in_a_child_process() {
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    if !backend() {
        return;
    }
    let dir = temp_dir("child");
    let source = fixture("okular.pdf");
    let output = dir.join("child.pdf");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_app"))
        .arg("--compact")
        .arg(&source)
        .arg(&output)
        .output()
        .expect("启动子进程");
    assert!(
        child.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&child.stderr)
    );
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(stdout.contains("已整理"), "stdout={stdout}");
    assert!(output.exists(), "应生成整理结果");

    // 带签名：拒绝、非 0、不产生输出。
    let mut signed = std::fs::read(&source).expect("读样本");
    signed.extend_from_slice(b"\n% /ByteRange [0 0 0 0]\n");
    let signed_path = dir.join("signed.pdf");
    std::fs::write(&signed_path, &signed).expect("写样本");
    let refused = dir.join("signed-out.pdf");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_app"))
        .arg("--compact")
        .arg(&signed_path)
        .arg(&refused)
        .output()
        .expect("启动子进程");
    assert!(!child.status.success(), "签名文档必须拒绝");
    assert!(
        String::from_utf8_lossy(&child.stderr).contains("数字签名"),
        "stderr={}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(!refused.exists(), "拒绝时不应产生输出");
    std::fs::remove_dir_all(&dir).ok();
}
