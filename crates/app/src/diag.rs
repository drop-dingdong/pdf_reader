//! 诊断输出的**去处**。
//!
//! ## 现状（D-103）
//! 所有运行期诊断都从这里走：
//! * 正常运行时：`enabled` 由 `PDF_READER_DIAGNOSE` 决定，打开则写 stderr；
//! * 回放器（`--replay`）：强制打开并**收集到内存**，按"动作"分组输出 JSONL，供断言使用。
//!
//! 这样做的直接收益：回放时能拿到"某个动作从开始到稳定之间的全部输出"，
//! 而不再需要人盯着终端（历史上多次出现"拖动时看不了日志"）。
//!
//! 注意：**失败类**信息（存档解析失败、迁移说明）不走这里，直接写 stderr —— 它们很少发生，
//! 且与"逐帧诊断"不是一回事。

/// 诊断开关 + 可选的收集缓冲。
#[derive(Debug, Default)]
pub struct Diagnostics {
    enabled: bool,
    captured: Option<Vec<String>>,
}

impl Diagnostics {
    /// 按环境变量 `PDF_READER_DIAGNOSE` 决定（存在即打开）。
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var_os("PDF_READER_DIAGNOSE").is_some(),
            captured: None,
        }
    }

    /// 关闭。
    pub fn quiet() -> Self {
        Self::default()
    }

    /// 打开并**收集**（不写 stderr）。回放器用这个。
    pub fn capturing() -> Self {
        Self {
            enabled: true,
            captured: Some(Vec::new()),
        }
    }

    /// 是否打开。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 是否处于"收集"模式。
    pub fn is_capturing(&self) -> bool {
        self.captured.is_some()
    }

    /// 输出一行诊断。
    pub fn line(&mut self, text: impl Into<String>) {
        if !self.enabled {
            return;
        }
        let text = text.into();
        match &mut self.captured {
            Some(buffer) => buffer.push(text),
            None => eprintln!("{text}"),
        }
    }

    /// 取走已收集的内容（收集模式下清空缓冲；stderr 模式返回空）。
    pub fn take(&mut self) -> Vec<String> {
        self.captured
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// 当前缓冲的行数（诊断/断言用）。
    pub fn pending_lines(&self) -> usize {
        self.captured.as_ref().map_or(0, |buffer| buffer.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_collects_nothing() {
        let mut diag = Diagnostics::quiet();
        diag.line("hello");
        assert!(!diag.enabled());
        assert!(diag.take().is_empty());
    }

    #[test]
    fn capturing_keeps_lines_in_order_and_take_clears() {
        let mut diag = Diagnostics::capturing();
        assert!(diag.enabled() && diag.is_capturing());
        diag.line("a");
        diag.line(String::from("b"));
        assert_eq!(diag.pending_lines(), 2);
        assert_eq!(diag.take(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(diag.pending_lines(), 0);
    }
}
