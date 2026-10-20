//! PDFium 后端：把 `pdfium-render` 适配成 [`pdfcore`] 的 trait 实现。
//!
//! ## 分层位置
//! ```text
//!   app（egui）──▶ pdfcore（trait）◀── backend-pdfium（本 crate）
//! ```
//! 本 crate 只依赖 `pdfcore` 与 `pdfium-render`，**不依赖任何 UI 类型**。
//!
//! ## 运行期绑定
//! `pdfium-render` 在**运行期**加载 PDFium 动态库（编译不需要它）。查找顺序见
//! [`library::pdfium`]；开发机可用 `scripts/fetch-pdfium.sh` 一键获取。
//!
//! ## 与 pdfcore 设计的对应
//! * **D-01 批量页几何**：`open` 时一次性读出全部页尺寸与 `/Rotate`（见 [`document`]）；
//! * **D-02 分块渲染**：瓦片走 PDFium 原生的部分渲染（见 [`raster`]），
//!   **不是**"整页渲染后裁剪"；
//! * **D-08 导航索引**：大纲与逐页链接在 `open` 时采集完毕；PDFium 本身已把命名目标
//!   解析成具体页（`PdfDestination::page_index`），因此 UI 永远拿不到"未解析"的目标。
//!
//! ## 快速开始
//! ```
//! use pdfcore::prelude::*;
//! let backend = backend_pdfium::PdfiumBackend::new();
//! // 找不到 PDFium 动态库时，open 会返回可操作的错误而不是 panic
//! let _ = backend.open(Source::Bytes(b"%PDF-1.4\n"), None);
//! ```
#![warn(missing_docs)]
// 我们刻意用 `!(a > b)` 表达"NaN 也算不满足"（`!(width > 0.0)` 同时挡住 NaN 与 0），
// clippy 建议的 `partial_cmp` 写法更长也更难读；这条 lint 对本项目是噪音。
#![allow(clippy::neg_cmp_op_on_partial_ord)]

pub mod backend;
#[doc(hidden)]
pub mod fixtures;
pub mod document;
pub mod error;
pub mod library;
pub mod raster;
pub mod text;

pub use backend::PdfiumBackend;
pub use error::to_core_error;
pub use library::pdfium;
