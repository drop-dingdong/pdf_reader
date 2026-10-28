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
    // S0：交互动作（点击跳链接 / 拖拽平移）—— 注释功能的自动化基础
    ("click_link.json", Sample::Rotated),
    ("drag_pan.json", Sample::A4),
    // S1：标注的几何必须只与文本层有关、与缩放无关
    ("annotate_zoom.json", Sample::Rotated),
    // S3：撤销/重做（单栈、时间序）
    ("undo_redo.json", Sample::Rotated),
    // S4：墨迹（一笔一标注、包围盒按半线宽外扩）
    ("ink.json", Sample::Rotated),
    // S4c：1000 条墨迹的压力脚本（结构基线：标注不得进入渲染管线）
    ("ink_many.json", Sample::Rotated),
];

/// 脚本目录（测试的工作目录是 crate 目录，不是工作区根）。
fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/replay")
}

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

/// 每个"一次调用"用独立的临时目录（脚本与样本放在一起：脚本里的 `pdf` 是相对路径）。
///
/// 目录名必须带上**递增序号**：两个测试都会遍历同一批脚本，若只用"进程号 + 脚本名"，
/// 它们会落到同一个目录里，一边写样本一边读 —— 会把 PDF 读成半截（曾偶发失败）。
fn stage(script: &str, sample: Sample) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "pdf-replay-{}-{}-{}",
        std::process::id(),
        sequence,
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

/// S6a：写过注释、但 overlay 文件不存在的 PDF，打开时应从 PDF 读回标注。
#[test]
fn annotations_are_recovered_from_the_pdf_when_the_overlay_is_missing() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-readback-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let pdf = dir.join("rotated_2p.pdf");
    Sample::Rotated.write_to(&pdf).expect("写样本");

    // 用写回把一条带标记/正文的高亮写进 PDF（先写 out，再替换成样本路径）
    let staged = dir.join("out.pdf");
    backend_pdfium::writeback::write(&backend_pdfium::writeback::WritebackSpec {
        source: pdf.clone(),
        output: staged.clone(),
        pages: vec![backend_pdfium::writeback::WritebackPage {
            page: pdfcore::prelude::PageIndex::FIRST,
            annotations: vec![backend_pdfium::writeback::WritebackAnnotation {
                id: Some(7),
                kind: pdfcore::annotation::AnnotationKind::Highlight {
                    quads: vec![pdfcore::prelude::Rect::new(50.0, 660.0, 100.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: Some("从 PDF 恢复的正文".to_string()),
            }],
        }],
    })
    .expect("写回");
    std::fs::rename(&staged, &pdf).expect("替换样本");
    // 没有 .annot.json：模拟 overlay 丢失

    let script = dir.join("readback.json");
    std::fs::write(
        &script,
        r#"{"name":"readback","pdf":"rotated_2p.pdf","actions":[
             {"id":"settle","op":"wait","frames":6}
           ]}"#,
    )
    .expect("写脚本");
    let report = replay::run_script_file(&script, None).expect("回放");
    assert_eq!(
        report.actions.last().map(|action| action.annotations),
        Some(1),
        "缺少 overlay 时应当从 PDF 读回 1 条标注"
    );
    // 读回后正文条目也要恢复（侧栏/点击正文能看到）
    assert!(
        !dir.join("rotated_2p.pdf.annot.json").exists(),
        "读回不应自动创建 overlay 文件"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// S6b：外部阅读器（Okular 样本）的注释以只读层显示；不写 overlay、不计入可编辑标注。
#[test]
fn foreign_annotations_are_shown_read_only() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "pdf-foreign-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../backend-pdfium/tests/fixtures/s6b/okular.pdf");
    std::fs::copy(&fixture, dir.join("rotated_2p.pdf")).expect("复制样本");
    let script = dir.join("foreign.json");
    std::fs::write(
        &script,
        r#"{"name":"foreign","pdf":"rotated_2p.pdf","actions":[
             {"id":"settle","op":"wait","frames":6}
           ]}"#,
    )
    .expect("写脚本");
    let report = replay::run_script_file(&script, None).expect("回放");
    let action = report.actions.last().expect("动作");
    assert_eq!(action.annotations, 0, "外部注释不应进入可编辑 overlay");
    assert_eq!(
        action.foreign_annotations, 5,
        "Okular 样本应有 5 条只读注释"
    );
    assert!(
        !dir.join("rotated_2p.pdf.annot.json").exists(),
        "只读层不应写 overlay"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// S0：`click` / `drag` 动作必须真的驱动 UI，而不是"跑完没报错"。
#[test]
fn click_navigates_and_drag_pans() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let clicked = run("click_link.json", Sample::Rotated);
    let action = clicked
        .actions
        .iter()
        .find(|action| action.id == "click_link")
        .expect("click 动作");
    assert!(action.settled, "click 动作应当稳定");
    assert_eq!(
        action.page_after, 1,
        "点击第 1 页的链接应当跳到第 2 页（0 起），实际 {}",
        action.page_after
    );

    let dragged = run("drag_pan.json", Sample::A4);
    let action = dragged
        .actions
        .iter()
        .find(|action| action.id == "drag")
        .expect("drag 动作");
    assert!(action.settled, "drag 动作应当稳定");
    assert!(
        action.scroll_after.1.abs() > 100.0,
        "拖拽应当平移画面，实际 scroll={:?}",
        action.scroll_after
    );
}

/// S1 验收：同一块页面区域，在 1× 与 8× 下产生的标注几何必须**逐值相同**。
///
/// 这是"几何只存页面空间"的直接证据：如果哪天有人改成存屏幕坐标（或按当前缩放算几何），
/// 两条 bounds 就会分叉。
#[test]
fn annotation_geometry_does_not_depend_on_zoom() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let report = run("annotate_zoom.json", Sample::Rotated);
    let action = |id: &str| {
        report
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("缺动作 {id}"))
    };
    let at_100 = action("annotate_100");
    let at_800 = action("annotate_800");
    assert_eq!(at_100.annotations, 1, "1× 下应当产生一条标注");
    assert_eq!(at_800.annotations, 2, "8× 下再产生一条");
    let (a, b) = (
        at_100.annotation_bounds.expect("1× 的包围盒"),
        at_800.annotation_bounds.expect("8× 的包围盒"),
    );
    for (left, right) in [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)] {
        assert!(
            (left - right).abs() < 0.01,
            "同一区域在不同缩放下的标注几何必须相同：{a:?} vs {b:?}"
        );
    }
}

/// S3 验收：3 条标注 → 撤销 2 次剩 1 → 重做 1 次回到 2。
#[test]
fn undo_and_redo_walk_the_annotation_stack() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let report = run("undo_redo.json", Sample::Rotated);
    let action = |id: &str| {
        report
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("缺动作 {id}"))
    };
    assert_eq!(action("a3").annotations, 3, "三条标注都建好了");
    assert_eq!(action("undo1").annotations, 2, "撤销一次剩两条");
    assert_eq!(action("undo2").annotations, 1, "再撤销一次剩一条");
    assert_eq!(action("redo1").annotations, 2, "重做一次回到两条");
    // 正文条目必须跟着标注一起撤销/重做（否则重做回来的标注点开是"正文已丢失"）
    assert_eq!(
        action("undo2").previews_total,
        1,
        "撤销后正文条目也应只剩一条"
    );
    assert_eq!(
        action("redo1").previews_total,
        2,
        "重做后正文条目必须一起回来"
    );
}

/// S4b：`ink` 走**真实拖拽**（按下 → 移动 → 抬起），一笔 = 一条标注 = 一次撤销。
///
/// 三个断言一起钉住 S4 的验收点：
/// 1. 一次拖拽只产生 1 条标注（拖动过程不进集合，抬笔才入库）；
/// 2. Esc 取消 → 整笔丢弃（数量不变、不留正文条目）；
/// 3. 1× 与 2× 下画同一组**页面空间**点 → 包围盒逐值相同（笔画中心不随缩放漂移）。
#[test]
fn ink_stroke_is_one_annotation_that_undo_and_redo_can_move() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let report = run("ink.json", Sample::Rotated);
    let action = |id: &str| {
        report
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("缺动作 {id}"))
    };

    let stroke = action("stroke");
    assert_eq!(stroke.annotations, 1, "一次拖拽应当只产生一条标注");
    assert_eq!(stroke.previews_total, 1, "墨迹也要有配对的正文条目");
    let saved = stroke.annotation_bounds.expect("墨迹包围盒");
    for (label, actual, expected) in [
        ("x", saved.0, 279.0),
        ("y", saved.1, 389.0),
        ("width", saved.2, 82.0),
        ("height", saved.3, 22.0),
    ] {
        assert!(
            (actual - expected).abs() < 0.01,
            "墨迹包围盒 {label} 应为 {expected}（点集外扩半线宽 1.0），实际 {actual}"
        );
    }

    assert_eq!(action("undo1").annotations, 0, "撤销一次整笔消失");
    assert_eq!(
        action("undo1").previews_total,
        0,
        "撤销必须同时收走配对正文条目"
    );
    assert_eq!(action("redo1").annotations, 1, "重做后墨迹回来");
    assert_eq!(
        action("redo1").previews_total,
        1,
        "重做必须同时放回配对正文条目"
    );

    assert_eq!(
        action("cancel").annotations,
        1,
        "拖到一半按 Esc：整笔丢弃，既不多一条也不该弄丢已有的那条"
    );
    assert_eq!(
        action("cancel").previews_total,
        1,
        "取消的一笔不该留下正文条目"
    );

    // 2× 下再画同一组页面空间点：几何必须逐值相同（缩放不参与几何）
    let at_200 = action("stroke_200");
    assert_eq!(at_200.annotations, 2, "2× 下再画一笔");
    let zoomed = at_200.annotation_bounds.expect("2× 的包围盒");
    for (left, right) in [
        (saved.0, zoomed.0),
        (saved.1, zoomed.1),
        (saved.2, zoomed.2),
        (saved.3, zoomed.3),
    ] {
        assert!(
            (left - right).abs() < 0.01,
            "同一组页面空间点在不同缩放下的墨迹几何必须相同：{saved:?} vs {zoomed:?}"
        );
    }
}

/// S4c：1000 条墨迹 —— **结构上**不进渲染管线（确定、与机器无关），
/// **时间上**不退化成 O(n²)（阈值是 debug 构建的宽松上限，数字会打印出来备查）。
///
/// 时间数字（本机 debug）：造 100 笔 ~2.6ms、造 1000 笔 ~3.9ms、画 1000 笔一帧 ~2.0ms、
/// 撤销后打开历史窗口 ~16ms。最后一项曾经是 **323ms** —— 历史列表一次排版 1001 行；
/// 虚拟化之后才降下来，这个断言就是它的回归网。
#[test]
fn a_thousand_ink_annotations_stay_out_of_the_render_pipeline_and_cheap() {
    let _guard = test_lock();
    if !backend_available() {
        return;
    }
    let report = run("ink_many.json", Sample::Rotated);
    let action = |id: &str| {
        report
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("缺动作 {id}"))
    };
    let frame_max = |id: &str| {
        action(id)
            .frames_data
            .iter()
            .map(|frame| frame.cpu_ms)
            .fold(0.0_f64, f64::max)
    };

    assert_eq!(action("seed_100").annotations, 100);
    assert_eq!(action("seed_900").annotations, 1000);

    // 结构断言（与硬件无关）：标注不产生渲染请求，也不改变纹理层峰值
    let baseline_peak = action("settle").texture_bytes_peak;
    for id in ["seed_100", "settle_100", "seed_900", "settle_1000"] {
        assert_eq!(
            action(id).submitted,
            0,
            "{id}: 造标注不得提交渲染请求（注释不进渲染管线）"
        );
        assert_eq!(
            action(id).texture_bytes_peak,
            baseline_peak,
            "{id}: 标注不得占用纹理层"
        );
    }
    assert_eq!(
        action("stroke").annotations,
        1001,
        "1000 条之后还能再画一笔"
    );

    // 时间断言：宽松的 debug 上限 + 反二次退化
    let at_100 = action("settle_100").frames_data.len();
    assert!(at_100 > 0, "settle_100 应当跑过至少一帧");
    let draw_100 = frame_max("settle_100").max(0.5);
    let draw_1000 = frame_max("settle_1000");
    eprintln!(
        "[S4c] 造 100 笔 {:.2}ms / 造 1000 笔 {:.2}ms / 画 100 笔一帧 {:.2}ms /          画 1000 笔一帧 {draw_1000:.2}ms / 撤销+历史窗口 {:.2}ms",
        frame_max("seed_100"),
        frame_max("seed_900"),
        draw_100,
        frame_max("undo1"),
    );
    assert!(
        draw_1000 < 50.0,
        "1000 条墨迹的绘制帧 {draw_1000:.2}ms 超过 50ms（debug 上限）"
    );
    assert!(
        draw_1000 <= 25.0 * draw_100,
        "标注从 100 增到 1000，帧时间涨了 {:.1} 倍（>25 倍说明退化成 O(n²)）",
        draw_1000 / draw_100
    );
    assert!(
        frame_max("seed_900") < 100.0,
        "造 900 笔卡了 {:.2}ms（上限 100ms）",
        frame_max("seed_900")
    );
    assert!(
        frame_max("undo1") < 100.0,
        "撤销后打开历史窗口的帧 {:.2}ms 超过 100ms —— 历史列表是不是又没虚拟化？         （虚拟化之前实测 323ms）",
        frame_max("undo1")
    );
}

#[test]
fn every_script_finishes_with_no_violations() {
    let _guard = test_lock();
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
    let _guard = test_lock();
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
    assert_eq!(
        low, high,
        "预览尺寸必须只由「区域 × 记录分辨率」决定，与当前缩放无关"
    );
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
    let _guard = test_lock();
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
    assert!(
        submitted > 0,
        "整段脚本一次渲染都没发生，测出来的基数是假的"
    );
}

/// D-111 的"软上限"路径：预算小到连当前视口都放不下时，
/// **不能**崩、**不能**露白，只是把底料/旧内容让出来（并把提示写进诊断）。
#[test]
fn a_tiny_texture_budget_still_keeps_the_view_correct() {
    let _guard = test_lock();
    if !backend_available() {
        eprintln!("跳过：未找到 PDFium 动态库");
        return;
    }
    let path = stage("page_walk.json", Sample::A4);
    let script = replay::load_script(&path).expect("读脚本");
    let clock = app::clock::Clock::manual();
    let mut reader =
        app::PdfReaderApp::new_headless(clock.clone(), app::diag::Diagnostics::capturing(), None);
    // 4 MB：远小于"3 页 × 13 MB 的整页图"，必然触发软上限
    reader.set_texture_budget(4 * 1024 * 1024);
    let report = replay::run(&mut reader, &script, &clock, &replay::RunOptions::default());
    assert!(
        report.ok(),
        "软上限路径不应产生违规：{:#?}",
        report.violations
    );
    for action in &report.actions {
        assert!(action.settled, "动作 {} 没稳定", action.id);
    }
}

/// 性能基线：记录一次 → 立刻比较，不应出现**门禁级**结论（时序提示允许，机器噪声）。
#[test]
fn a_freshly_recorded_baseline_has_no_gate_findings() {
    let _guard = test_lock();
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
    let _guard = test_lock();
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

/// 这些回放测试会真的打开 PDF/渲染；读回（S6a）用的是裸 FFI，必须串行，否则和
/// pdfium-render 的渲染线程并发会踩内存（实测 SIGSEGV）。用一把进程内锁把测试串起来。
fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}
