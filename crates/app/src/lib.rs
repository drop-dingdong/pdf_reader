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
// 我们刻意用 `!(a > b)` 表达"NaN 也算不满足"（`!(width > 0.0)` 同时挡住 NaN 与 0），
// clippy 建议的 `partial_cmp` 写法更长也更难读；这条 lint 对本项目是噪音。
#![allow(clippy::neg_cmp_op_on_partial_ord)]

pub mod annotations_md;
pub mod app;
pub mod clock;
pub mod compact;
pub mod diag;
pub mod export;
pub mod history;
pub(crate) mod ink;
pub(crate) mod json;
pub mod persist;
pub mod settings;
pub mod view;
pub mod writeback;

pub use app::replay;

pub use app::PdfReaderApp;
