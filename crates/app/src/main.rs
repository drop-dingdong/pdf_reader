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
        let out = args
            .iter()
            .position(|arg| arg == "--out")
            .and_then(|index| args.get(index + 1))
            .map(PathBuf::from);
        match app::replay::run_script_file(&script, out.as_deref()) {
            Ok(report) => {
                println!("{}", report.summary());
                for action in &report.actions {
                    println!(
                        "  {} [{}] 帧={} 稳定={} 违规={} 观察={}",
                        action.id,
                        action.action,
                        action.frames,
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

    // 生成回放用的样本 PDF（仓库里不放二进制夹具，现场生成）
    if args.first().map(String::as_str) == Some("--sample") {
        let kind = args.get(1).map(String::as_str).unwrap_or("a4");
        let Some(out) = args.get(2).map(PathBuf::from) else {
            eprintln!("用法：app --sample <a4|two-page|many|offset> <输出.pdf>");
            std::process::exit(2);
        };
        let bytes = match kind {
            // A4 三页，满页横线：放大后必然分块
            "a4" => backend_pdfium::fixtures::many_page_pdf(3, 595.28, 841.89),
            // 两页，其中一页 /Rotate 90，带链接与大纲
            "two-page" => backend_pdfium::fixtures::two_page_pdf(),
            // 大页（图纸尺寸），低倍就分块
            "many" => backend_pdfium::fixtures::many_page_pdf(120, 595.28, 841.89),
            // MediaBox 原点非 0
            "offset" => backend_pdfium::fixtures::offset_media_box_pdf(),
            other => {
                eprintln!("未知样本 {other}（可选 a4 / two-page / many / offset）");
                std::process::exit(2);
            }
        };
        if let Err(error) = std::fs::write(&out, bytes) {
            eprintln!("写入 {} 失败：{error}", out.display());
            std::process::exit(2);
        }
        println!("已生成 {}", out.display());
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
