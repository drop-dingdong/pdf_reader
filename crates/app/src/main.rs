//! 桌面应用入口。

use std::path::PathBuf;

use app::PdfReaderApp;

fn main() -> eframe::Result<()> {
    // 命令行参数：第一个参数视为要打开的 PDF
    let initial: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);

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
        Box::new(move |_cc| Ok(Box::new(PdfReaderApp::new(initial.clone())))),
    )
}
