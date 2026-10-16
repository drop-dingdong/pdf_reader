//! 统一错误类型。
//!
//! 设计要点：**接口层的错误必须能表达"后端不支持"与"需要密码"这两种"非故障"情况**，
//! UI 才能据此降级（隐藏搜索框）或弹出密码框，而不是用一个笼统的 `String` 错误。

use std::fmt;

use crate::ids::PageIndex;

/// 接口层统一的错误类型。所有 trait 方法都用 [`Result`] 返回它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// 文件读取失败（路径不存在、权限不足、磁盘错误等）。
    Io(String),
    /// 文档已加密，需要调用方提供密码后重试。
    ///
    /// 这不是故障：UI 应当弹出密码输入框，并带着密码重新调用
    /// [`crate::document::PdfBackend::open`]。
    PasswordRequired,
    /// 提供了密码但校验失败。
    IncorrectPassword,
    /// 文件不是有效 PDF，或结构损坏到无法解析。
    CorruptDocument(String),
    /// 页号超出 `0..page_count` 范围。
    PageOutOfRange {
        /// 请求的页号。
        page: PageIndex,
        /// 文档实际页数。
        page_count: u32,
    },
    /// 当前后端不支持该能力（例如 hayro 后端没有文本层）。
    ///
    /// UI 应通过 [`crate::document::Capabilities`] 提前判断，此错误只是兜底。
    Unsupported(&'static str),
    /// 任务被取消（渲染参数变化、页面滚出视口等）。
    Cancelled,
    /// 后端（PDFium 等）返回的原始错误。
    Backend(String),
    /// 接口层内部不变量被破坏（属于 bug，应写测试而不是让用户看到）。
    Internal(String),
}

impl CoreError {
    /// 是否为"能力缺失"。UI 可据此静默降级而不报错。
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }

    /// 是否为"需要密码 / 密码错误"，UI 据此决定是否弹密码框。
    pub fn is_auth_related(&self) -> bool {
        matches!(self, Self::PasswordRequired | Self::IncorrectPassword)
    }

    /// 取消不是错误，UI 不应弹提示。
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// 缺失的能力名（用于日志与提示文案）。
    pub fn missing_feature(&self) -> Option<&'static str> {
        match self {
            Self::Unsupported(feature) => Some(*feature),
            _ => None,
        }
    }

    /// 便捷构造：能力缺失。
    pub fn unsupported(feature: &'static str) -> Self {
        Self::Unsupported(feature)
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "I/O 错误：{msg}"),
            Self::PasswordRequired => write!(f, "文档已加密，需要密码"),
            Self::IncorrectPassword => write!(f, "密码错误"),
            Self::CorruptDocument(msg) => write!(f, "PDF 无法解析：{msg}"),
            Self::PageOutOfRange { page, page_count } => {
                write!(f, "页号越界：{page}（共 {page_count} 页）")
            }
            Self::Unsupported(feature) => write!(f, "当前后端不支持：{feature}"),
            Self::Cancelled => write!(f, "任务已取消"),
            Self::Backend(msg) => write!(f, "后端错误：{msg}"),
            Self::Internal(msg) => write!(f, "内部错误：{msg}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// 接口层统一的 `Result` 别名。
pub type Result<T> = std::result::Result<T, CoreError>;

// 便于后端实现把底层错误直接 `?` 上来。
impl From<std::io::Error> for CoreError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}
