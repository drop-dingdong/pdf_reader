//! 桌面应用入口。
//!
//! ```text
//! cargo run -p app -- <文件.pdf>                      # 正常打开
//! cargo run -p app -- --replay <脚本.json> [--out x.jsonl]   # 无窗口回放（回归断言/性能基线）
//! ```

use std::path::PathBuf;

use app::PdfReaderApp;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // 回放模式：不需要窗口，也不需要 PDFium 的窗口集成
    if args.first().map(String::as_str) == Some("--replay") {
        let Some(script) = args.get(1).map(PathBuf::from) else {
            eprintln!("用法：app --replay <脚本.json> [--out <输出.jsonl>]");
            std::process::exit(2);
        };
        let arg_value = |flag: &str| -> Option<PathBuf> {
            args.iter()
                .position(|arg| arg == flag)
                .and_then(|index| args.get(index + 1))
                .map(PathBuf::from)
        };
        let out = arg_value("--out");
        let record = arg_value("--record");
        let record_structural = arg_value("--record-structural");
        let compare = arg_value("--compare");
        let timing = args.iter().any(|arg| arg == "--timing");
        match app::replay::run_script_file_with(&script, out.as_deref(), timing) {
            Ok(report) => {
                println!("{}", report.summary());
                for action in &report.actions {
                    println!(
                        "  {} [{}] 帧={}(忙 {}) 墙钟={:.0}ms CPU={:.1}ms 提交={} 稳定={} 违规={} 观察={}",
                        action.id,
                        action.action,
                        action.frames,
                        action.busy_frames,
                        action.elapsed_ms,
                        action.busy_cpu_ms_total,
                        action.submitted,
                        action.settled,
                        action.violations.len(),
                        action.notes.len()
                    );
                    for note in &action.notes {
                        println!("      观察：{note}");
                    }
                }
                if !report.ok() {
                    eprintln!("违规 {} 条：", report.violations.len());
                    for violation in &report.violations {
                        eprintln!("  - {violation}");
                    }
                    if record.is_some() {
                        eprintln!("本次回放有违规，**不记录**基线（先修好再录）");
                    }
                    std::process::exit(1);
                }
                // 性能基线：记录 / 比较（结构指标硬门禁，时序指标只提示）
                use app::replay::baseline::{self, Baseline, Machine, Severity};
                let machine = Machine::detect();
                println!("机器：{}", machine.summary());
                let mut gate_failed = false;
                if let Some(path) = &record_structural {
                    let data = Baseline::record_structural(&report);
                    if let Err(error) = data.write(path) {
                        eprintln!("写结构基线 {} 失败：{error}", path.display());
                        std::process::exit(2);
                    }
                    println!("已记录结构基线 {}", path.display());
                }
                if let Some(path) = &record {
                    let data = Baseline::record(&report, machine.clone());
                    if let Err(error) = data.write(path) {
                        eprintln!("写基线 {} 失败：{error}", path.display());
                        std::process::exit(2);
                    }
                    println!("已记录基线 {}", path.display());
                }
                if let Some(path) = &compare {
                    match Baseline::read(path) {
                        Ok(data) => {
                            let findings = baseline::compare(&data, &report, &machine);
                            if findings.is_empty() {
                                println!("与基线一致（{}）", path.display());
                            }
                            for finding in &findings {
                                let tag = match finding.severity {
                                    Severity::Gate => "门禁",
                                    Severity::Warn => "提示",
                                };
                                eprintln!("[{tag}] {}", finding.message);
                                gate_failed |= finding.severity == Severity::Gate;
                            }
                        }
                        Err(error) => {
                            eprintln!("读基线 {} 失败：{error}", path.display());
                            std::process::exit(2);
                        }
                    }
                }
                if gate_failed {
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("回放失败：{error}");
                std::process::exit(2);
            }
        }
        return Ok(());
    }

    // 生成回放用的样本（仓库里不放二进制夹具，现场生成；`preview` 会一并写 sidecar）
    if args.first().map(String::as_str) == Some("--sample") {
        let kind = args.get(1).map(String::as_str).unwrap_or("a4");
        let Some(out) = args.get(2).map(PathBuf::from) else {
            eprintln!("用法：app --sample <a4|rotated|large|many|preview|strip> <输出.pdf>");
            std::process::exit(2);
        };
        let Some(sample) = app::replay::samples::Sample::parse(kind) else {
            eprintln!("未知样本 {kind}（可选 a4 / rotated / large / many / preview / strip）");
            std::process::exit(2);
        };
        match sample.write_to(&out) {
            Ok(path) => println!("已生成 {}", path.display()),
            Err(error) => {
                eprintln!("写入 {} 失败：{error}", out.display());
                std::process::exit(2);
            }
        }
        return Ok(());
    }

    // 自检：能不能加载 PDFium（打包后用它在目标机器上验证，见 scripts/package.sh）
    if args.first().map(String::as_str) == Some("--selftest") {
        let platform = backend_pdfium::library::platform_tag();
        match backend_pdfium::library::pdfium() {
            Ok(_) => {
                println!("自检通过：platform={platform} pdfium=已加载");
                return Ok(());
            }
            Err(error) => {
                eprintln!("自检失败：platform={platform}\n{error}");
                std::process::exit(1);
            }
        }
    }

    // 一次性把所有回放样本写进某目录（CI 用：脚本里的 pdf 是相对路径）
    if args.first().map(String::as_str) == Some("--samples") {
        let Some(dir) = args.get(1).map(PathBuf::from) else {
            eprintln!("用法：app --samples <目录>");
            std::process::exit(2);
        };
        for sample in app::replay::samples::Sample::all() {
            match sample.write(&dir) {
                Ok(path) => println!("已生成 {}", path.display()),
                Err(error) => {
                    eprintln!("写入 {} 失败：{error}", dir.display());
                    std::process::exit(2);
                }
            }
        }
        return Ok(());
    }

    // 命令行参数：第一个参数视为要打开的 PDF
    let initial: Option<PathBuf> = args.first().map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 900.0])
            .with_min_inner_size([600.0, 400.0])
            .with_title("PDF Reader"),
        ..Default::default()
    };

    eframe::run_native(
        "PDF Reader",
        options,
        Box::new(move |cc| Ok(Box::new(PdfReaderApp::new(cc, initial.clone())))),
    )
}
