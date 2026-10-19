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
