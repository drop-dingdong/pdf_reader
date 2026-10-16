//! 标识符类型：页号、任务号。

use std::fmt;

/// 页索引，**从 0 开始**（与 PDFium 的 `FPDF_PAGE` 索引一致）。
///
/// 注意 0-based 与 1-based 的边界：类型内部一律 0-based，面向用户的文案请显式调用
/// [`PageIndex::display_number`]（或直接 `{}`，[`fmt::Display`] 已输出 1-based 形式），
/// 避免状态栏出现"第 0 页 / 共 10 页"这类错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PageIndex(pub u32);

impl PageIndex {
    /// 第一页。
    pub const FIRST: Self = Self(0);

    /// 构造页索引。
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// 原始 0-based 值（用于与后端 API 交互）。
    pub const fn get(self) -> u32 {
        self.0
    }

    /// 供 UI 显示的 1-based 页号。
    pub const fn display_number(self) -> u32 {
        self.0 + 1
    }

    /// 下一页；已是最后一页时返回 `None`（调用方需结合页数判断）。
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// 上一页；已经是第一页时返回 `None`。
    pub const fn prev(self) -> Option<Self> {
        if self.0 == 0 {
            None
        } else {
            Some(Self(self.0 - 1))
        }
    }
}

impl fmt::Display for PageIndex {
    /// 以 **1-based** 形式输出，便于日志中直接阅读。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page#{}", self.display_number())
    }
}

/// 渲染任务号，由 [`crate::render::RenderScheduler::submit`] 返回，用于取消单个任务。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);
