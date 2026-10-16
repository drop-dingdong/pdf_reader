//! `PdfiumError` → [`pdfcore::CoreError`] 的映射。
//!
//! 关键点：**密码相关错误必须映射成 `CoreError` 的"非故障"分支**
//! （[`CoreError::PasswordRequired`] / [`CoreError::IncorrectPassword`]），
//! 这样 UI 才知道该弹密码框而不是弹错误提示。

use pdfcore::prelude::*;
use pdfium_render::prelude::{PdfiumError, PdfiumInternalError};

/// 把 pdfium 的错误翻译成接口层错误。
pub fn to_core_error(error: PdfiumError) -> CoreError {
    match &error {
        // FPDF_ERR_PASSWORD：文档加密，调用方应带上密码重试
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            CoreError::PasswordRequired
        }
        // 文件不存在 / 无权限等
        PdfiumError::IoError(io) => CoreError::Io(io.to_string()),
        // 页号越界：pdfium 不提供具体页号，且适配层在调用前已按 page_count 校验过，
        // 因此这里没有足够信息构造带 page/page_count 的 CoreError，如实归入后端错误。
        PdfiumError::PageIndexOutOfBounds => CoreError::Backend(error.to_string()),
        // 其余统一归入后端错误（保留原始文案便于排查）
        _ => CoreError::Backend(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_error_is_not_a_failure() {
        let mapped = to_core_error(PdfiumError::PdfiumLibraryInternalError(
            PdfiumInternalError::PasswordError,
        ));
        assert_eq!(mapped, CoreError::PasswordRequired);
        assert!(mapped.is_auth_related(), "UI 据此弹密码框而不是错误提示");
    }

    #[test]
    fn other_errors_become_backend_errors() {
        let mapped = to_core_error(PdfiumError::PdfiumLibraryInternalError(
            PdfiumInternalError::FileError,
        ));
        assert!(!mapped.is_auth_related());
        assert!(matches!(mapped, CoreError::Backend(_)));
    }
}
