//! 测试夹具：实现已移到 crate 的 `src/fixtures.rs`（app 的回放器也要用同一套生成器）。
//!
//! 这里只做再导出，保持各测试文件原有的 `common::…` 调用不变。
#![allow(unused_imports)]

pub use backend_pdfium::fixtures::*;
