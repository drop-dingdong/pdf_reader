//! 回放器冒烟测试：用真实 PDFium 打开一个生成的 PDF，按脚本缩放/滚动/跳页，
//! 检查"动作能稳定下来、诊断被按动作分组收集、帧耗时/上传字节有值、三条断言不炸"。
//!
//! 找不到 PDFium 动态库时跳过（与 `backend-pdfium/tests/real_pdf.rs` 同一约定）。

use std::path::PathBuf;

use app::clock::Clock;
use app::diag::Diagnostics;
use app::replay::{self, Action, RunOptions, Script};

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

/// 三页 A4（带满页横线），放大后必然分块。
fn sample_pdf(name: &str) -> PathBuf {
    backend_pdfium::fixtures::many_page_pdf_file(name, 3, 595.28, 841.89)
}

#[test]
fn replay_runs_a_script_and_reports_per_action() {
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库（scripts/fetch-pdfium.sh）");
        return;
    }
    let pdf = sample_pdf("replay-smoke");
    let script = Script {
        name: "smoke".to_string(),
        pdf,
        actions: vec![
            ("resize".to_string(), Action::Resize { width: 1100.0, height: 900.0 }),
            ("zoom_100".to_string(), Action::Zoom { value: 1.0 }),
            ("zoom_800".to_string(), Action::Zoom { value: 8.0 }),
            ("scroll".to_string(), Action::ScrollBy { dx: 300.0, dy: 200.0 }),
            ("page3".to_string(), Action::GoToPage { page: 2 }),
            ("back".to_string(), Action::Zoom { value: 1.0 }),
        ],
    };

    let clock = Clock::manual();
    let mut app = app::PdfReaderApp::new_headless(clock.clone(), Diagnostics::capturing(), None);
    let report = replay::run(&mut app, &script, &clock, &RunOptions::default());

    assert!(report.actions.len() == 6, "每个动作都要有一条记录");
    assert!(report.frames() > 0, "至少要跑一帧");
    for action in &report.actions {
        assert!(
            action.settled,
            "动作 {} 应在帧数上限内稳定；违规：{:?}",
            action.id, report.violations
        );
        assert!(
            action.frames >= 1,
            "动作 {} 至少跑一帧",
            action.id
        );
    }
    // 高倍缩放动作应产生逐帧诊断（贴图/画法），并被按动作分组收集
    let zoom_800 = report
        .actions
        .iter()
        .find(|action| action.id == "zoom_800")
        .expect("应有 zoom_800");
    assert!(
        zoom_800.diag.iter().any(|line| line.starts_with("[贴图]")),
        "高倍缩放应产生 [贴图] 诊断：{:?}",
        zoom_800.diag
    );

    // 忙帧耗时与上传字节必须有值（无 GPU 时 upload_bytes 仍反映"打算上传多少"）
    let (p50, _, p99, max) = report.busy_frame_percentiles();
    assert!(p50 > 0.0 && p99 > 0.0 && max >= p99, "忙帧耗时应可统计");
    assert!(report.busy_frames() > 0, "至少要有一帧真的干了活");
    assert!(report.busy_cpu_ms_total() > 0.0, "CPU 合计应可统计");

    // 结构断言（S1 版本）不应误报
    assert!(
        report.violations.is_empty(),
        "S1 的结构断言不应报违规：{:?}",
        report.violations
    );

    let _ = std::fs::remove_file(&script.pdf);
}

#[test]
fn script_json_parses_actions() {
    let text = r#"{
      "name": "demo",
      "pdf": "samples/a4.pdf",
      "actions": [
        {"id": "open", "op": "resize", "width": 1200, "height": 900},
        {"id": "z", "op": "zoom", "value": 8},
        {"id": "s", "op": "scroll_by", "dx": 10, "dy": -20},
        {"id": "p", "op": "go_to_page", "page": 3},
        {"id": "h", "op": "hover_link", "page": 0, "link": 1},
        {"id": "u", "op": "unhover"},
        {"id": "w", "op": "wait", "frames": 5}
      ]
    }"#;
    let script = replay::parse_script(text).expect("应能解析");
    assert_eq!(script.name, "demo");
    assert_eq!(script.pdf, PathBuf::from("samples/a4.pdf"));
    assert_eq!(script.actions.len(), 7);
    assert_eq!(script.actions[1].1, Action::Zoom { value: 8.0 });
    assert_eq!(script.actions[3].1, Action::GoToPage { page: 3 });
    assert!(replay::parse_script("{\"pdf\": \"x\"}").is_err(), "缺 actions 应报错");
    assert!(
        replay::parse_script("{\"pdf\": \"x\", \"actions\": [{\"op\": \"nope\"}]}").is_err(),
        "未知动作应报错"
    );
}
