//! PDF 阅读器桌面应用（egui/eframe 外壳）。
//!
//! 用法：
//! ```text
//! cargo run -p app -- <某个.pdf>      # 命令行直接打开
//! cargo run -p app                    # 启动后把 PDF 拖进窗口
//! ```
//!
//! 运行前需要 PDFium 动态库（见 `crates/backend-pdfium/README.md`，或运行
//! `scripts/fetch-pdfium.sh`）。
//!
//! 模块划分：[`app`] 是 egui 胶水（需要真实窗口），[`view`] 是可单测的纯逻辑。
#![warn(missing_docs)]

pub mod app;
pub mod view;

pub use app::PdfReaderApp;
