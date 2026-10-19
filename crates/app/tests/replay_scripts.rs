//! 回放脚本集：**每个脚本都必须 0 违规地跑完**（D-105）。
//!
//! 这些脚本是"渲染行为的回归网"：分块/波段切换/滚动/跳页/旋转/超大页/多页/预览截图。
//! 找不到 PDFium 动态库时整组跳过（与 `backend-pdfium/tests/real_pdf.rs` 同一约定）。

use std::path::{Path, PathBuf};

use app::replay::samples::Sample;
use app::replay::{self, Report};

/// 脚本 → 所需样本。
const CASES: &[(&str, Sample)] = &[
    ("tiling_800.json", Sample::A4),
    ("band_walk.json", Sample::A4),
    ("scroll_walk.json", Sample::A4),
    ("page_walk.json", Sample::A4),
    ("rotated.json", Sample::Rotated),
    ("large_page.json", Sample::Large),
    ("many_pages.json", Sample::Many),
    ("preview_100.json", Sample::Preview),
    ("preview_800.json", Sample::Preview),
    ("strip_page.json", Sample::Strip),
    ("start_high.json", Sample::A4),
    // 两个"整页波段"之后再进分块波段：守住"上传集合必须是保留集合的子集"，
    // 否则开场底料/整页图会被每帧重传（实测过一次 8.4MB → 136MB）
    ("whole_pages.json", Sample::A4),
];

/// 脚本目录（测试的工作目录是 crate 目录，不是工作区根）。
fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/replay")
}

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

/// 每个测试用独立的临时目录（脚本与样本放在一起：脚本里的 `pdf` 是相对路径）。
fn stage(script: &str, sample: Sample) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-replay-{}-{}",
        std::process::id(),
        script.trim_end_matches(".json")
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let source = scripts_dir().join(script);
    let target = dir.join(script);
    std::fs::copy(&source, &target).unwrap_or_else(|error| {
        panic!("复制脚本 {} 失败：{error}", source.display());
    });
    sample.write(&dir).expect("写样本");
    target
}

fn run(script: &str, sample: Sample) -> Report {
    let path = stage(script, sample);
    replay::run_script_file(&path, None).expect("回放应能跑完")
}

#[test]
fn every_script_finishes_with_no_violations() {
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库（scripts/fetch-pdfium.sh）");
        return;
    }
    for (script, sample) in CASES {
        let report = run(script, *sample);
        assert!(
            report.ok(),
            "{script} 有违规：{:#?}\n摘要：{}",
            report.violations,
            report.summary()
        );
        assert!(report.frames() > 0, "{script} 一帧都没跑");
        for action in &report.actions {
            assert!(
                action.settled,
                "{script} 的动作 {} 在帧数上限内没稳定：{:?}",
                action.id, action.notes
            );
        }
    }
}

/// 断言 ③ 的"与缩放无关"：同一个预览在 100% 与 800% 下唤出，纹理尺寸必须一致，
/// 且等于"区域 × 记录分辨率"（800% 波段 10.486：150×80pt → 1573×839）。
#[test]
fn preview_size_does_not_depend_on_the_current_zoom() {
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库");
        return;
    }
    let at_100 = run("preview_100.json", Sample::Preview);
    let at_800 = run("preview_800.json", Sample::Preview);
    let size_of = |report: &Report| -> Option<(usize, usize)> {
        for action in &report.actions {
            if let Some((_, width, height)) = action.previews.first() {
                return Some((*width, *height));
            }
        }
        None
    };
    let low = size_of(&at_100).expect("100% 下应已生成预览纹理");
    let high = size_of(&at_800).expect("800% 下应已生成预览纹理");
    assert_eq!(low, high, "预览尺寸必须只由「区域 × 记录分辨率」决定，与当前缩放无关");
    // 150×80pt × 10.486（800% 波段上界）两端取整
    let expected = app::view::region_pixel_size(
        pdfcore::prelude::Rect::new(72.0, 640.0, 150.0, 80.0),
        pdfcore::prelude::Size::new(595.28, 841.89),
        pdfcore::prelude::Rotation::None,
        pdfcore::tiling::render_scale_for(8.0, pdfcore::tiling::DEFAULT_OVERSCAN),
    )
    .expect("区域有效");
    assert!(
        low.0.abs_diff(expected.0 as usize) <= 1 && low.1.abs_diff(expected.1 as usize) <= 1,
        "实际 {low:?} 应约等于预期 {expected:?}"
    );
}

/// **timing 模式**也必须收敛（回归：快机器上一帧只有 0.1ms，240 帧撑不到 160ms 的预览窗口，
/// 动作会"用光帧数却没稳定"，并把一份什么都没做的基线记录下来）。
#[test]
fn timing_mode_settles_and_actually_renders() {
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库");
        return;
    }
    let path = stage("tiling_800.json", Sample::A4);
    let report = replay::run_script_file_with(&path, None, true).expect("timing 回放应能跑完");
    assert!(report.ok(), "timing 模式有违规：{:#?}", report.violations);
    for action in &report.actions {
        assert!(action.settled, "timing 模式下动作 {} 没稳定", action.id);
    }
    let submitted: u64 = report.actions.iter().map(|action| action.submitted).sum();
    assert!(submitted > 0, "整段脚本一次渲染都没发生，测出来的基数是假的");
}

/// D-111 的"软上限"路径：预算小到连当前视口都放不下时，
/// **不能**崩、**不能**露白，只是把底料/旧内容让出来（并把提示写进诊断）。
#[test]
fn a_tiny_texture_budget_still_keeps_the_view_correct() {
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库");
        return;
    }
    let path = stage("page_walk.json", Sample::A4);
    let script = replay::load_script(&path).expect("读脚本");
    let clock = app::clock::Clock::manual();
    let mut reader = app::PdfReaderApp::new_headless(
        clock.clone(),
        app::diag::Diagnostics::capturing(),
        None,
    );
    // 4 MB：远小于"3 页 × 13 MB 的整页图"，必然触发软上限
    reader.set_texture_budget(4 * 1024 * 1024);
    let report = replay::run(&mut reader, &script, &clock, &replay::RunOptions::default());
    assert!(report.ok(), "软上限路径不应产生违规：{:#?}", report.violations);
    for action in &report.actions {
        assert!(action.settled, "动作 {} 没稳定", action.id);
    }
}

/// 性能基线：记录一次 → 立刻比较，不应出现**门禁级**结论（时序提示允许，机器噪声）。
#[test]
fn a_freshly_recorded_baseline_has_no_gate_findings() {
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库");
        return;
    }
    use app::replay::baseline::{self, Baseline, Machine, Severity};
    let report = run("tiling_800.json", Sample::A4);
    let machine = Machine::detect();
    let recorded = Baseline::record(&report, machine.clone());
    // 落盘再读回，确保格式闭环（真实用法就是"记一次、之后每次都读文件"）
    let path = std::env::temp_dir().join(format!("pdf-baseline-{}.json", std::process::id()));
    recorded.write(&path).expect("写基线");
    let loaded = Baseline::read(&path).expect("读基线");
    let findings = baseline::compare(&loaded, &report, &machine);
    let gates: Vec<_> = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Gate)
        .collect();
    assert!(gates.is_empty(), "新记录的基线不该有门禁结论：{gates:?}");
    assert_eq!(loaded.machine.cpu, machine.cpu, "基线必须带本机硬件信息");
    let _ = std::fs::remove_file(&path);
}

/// 脚本文件本身的自检：能被解析、动作数 > 0、样本名合法（不需要 PDFium）。
#[test]
fn all_scripts_parse() {
    for (script, sample) in CASES {
        let path = scripts_dir().join(script);
        let parsed = replay::load_script(&path).unwrap_or_else(|error| panic!("{script}: {error}"));
        assert!(!parsed.actions.is_empty(), "{script} 没有动作");
        assert_eq!(
            parsed.pdf.file_name().and_then(|name| name.to_str()),
            Some(sample.file_name()),
            "{script} 引用的样本应与用例表一致"
        );
    }
}
