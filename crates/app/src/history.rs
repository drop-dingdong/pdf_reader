//! 编辑历史（撤销 / 重做）—— D-118 S3。
//!
//! **一条栈，严格时间序**：注释编辑与"重选截图/引用点"混在同一个历史里。分成两条栈会让
//! Ctrl+Z 的语义变得不可预测（"撤销刚才那件事"必须按时间，而不是按类别）。
//!
//! 三个设计点：
//! * **按字节设上限**（不是按条数）：注释命令一字节量级、截图命令数 MB，
//!   按字节才能让"注释留很多、截图留很少"自然发生；
//! * **命令持有旧值**：撤销时把值**移回**应用，不做深拷贝（否则截图会让内存翻 N 倍）；
//! * **可撤销粒度由调用方决定**：一次拖拽 = 一条命令（不是每帧一条）。

/// 一条历史记录：描述（给人看，也用于可视列表）+ 命令 + 估算字节数。
#[derive(Debug, Clone)]
pub struct Entry<T> {
    /// 中文描述，例如"高亮（第 3 页）"。
    pub description: String,
    /// 命令本身（撤销/重做时原样交还给应用）。
    pub command: T,
    /// 这条记录占多大（用来按字节裁剪）。
    pub weight: usize,
}

/// 单栈历史（撤销 + 重做）。
#[derive(Debug, Clone)]
pub struct History<T> {
    undo: Vec<Entry<T>>,
    redo: Vec<Entry<T>>,
    /// 上限（字节）。`0` = 不限制。
    cap: usize,
    /// 已经裁掉多少条（可视列表里可以提示"更早的记录已丢弃"）。
    dropped: usize,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> History<T> {
    /// 默认上限：64 MiB（够放很多注释命令、但放不下几十张截图）。
    pub const DEFAULT_CAP: usize = 64 * 1024 * 1024;

    /// 空历史（默认上限）。
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            cap: Self::DEFAULT_CAP,
            dropped: 0,
        }
    }

    /// 改上限（字节；`0` = 不限制）。
    pub fn set_cap(&mut self, cap: usize) {
        self.cap = cap;
        self.trim();
    }

    /// 记录一条新的编辑。**会清空 redo**（新分支开始了）。
    pub fn push(&mut self, description: impl Into<String>, command: T, weight: usize) {
        self.undo.push(Entry {
            description: description.into(),
            command,
            weight,
        });
        self.redo.clear();
        self.trim();
    }

    /// 撤销：返回需要应用的那条命令（已移入重做栈）。
    ///
    /// 要求 `T: Clone` —— 但调用方的命令类型**必须让 clone 是廉价的**
    /// （截图之类的大对象用 `Arc` 包一层），否则撤销会把内存放大 N 倍。
    pub fn undo(&mut self) -> Option<Entry<T>>
    where
        T: Clone,
    {
        let entry = self.undo.pop()?;
        self.redo.push(entry.clone());
        Some(entry)
    }

    /// 重做：返回需要应用的那条命令。
    pub fn redo(&mut self) -> Option<Entry<T>>
    where
        T: Clone,
    {
        let entry = self.redo.pop()?;
        self.undo.push(entry.clone());
        Some(entry)
    }

    /// 是否可撤销。
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// 是否可重做。
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// 下一条将被撤销的描述（用于提示"已撤销：…"之前先取）。
    pub fn peek_undo(&self) -> Option<&Entry<T>> {
        self.undo.last()
    }

    /// 下一条将被重做的描述。
    pub fn peek_redo(&self) -> Option<&Entry<T>> {
        self.redo.last()
    }

    /// 当前可撤销的全部描述（旧 → 新），可视列表直接渲染它。
    pub fn descriptions(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(|entry| entry.description.as_str())
    }

    /// 已丢弃的条数。
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// 清空（换文档时）。
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.dropped = 0;
    }

    /// 按字节裁剪：从**最旧**的一端丢。
    fn trim(&mut self) {
        if self.cap == 0 {
            return;
        }
        let mut total: usize = self.undo.iter().map(|entry| entry.weight).sum();
        while total > self.cap && self.undo.len() > 1 {
            let removed = self.undo.remove(0);
            total = total.saturating_sub(removed.weight);
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_walk_in_order() {
        let mut history = History::new();
        history.push("第一条", 1, 10);
        history.push("第二条", 2, 10);

        assert_eq!(history.undo().map(|entry| entry.command), Some(2));
        assert_eq!(history.peek_undo().map(|entry| entry.command), Some(1));
        assert_eq!(history.undo().map(|entry| entry.command), Some(1));
        assert!(!history.can_undo());
        assert!(history.undo().is_none(), "栈空时返回 None");

        assert_eq!(history.redo().map(|entry| entry.command), Some(1));
        assert_eq!(history.redo().map(|entry| entry.command), Some(2));
        assert!(!history.can_redo());
    }

    #[test]
    fn pushing_a_new_edit_drops_the_redo_branch() {
        let mut history = History::new();
        history.push("a", 1, 1);
        history.push("b", 2, 1);
        history.undo();
        assert!(history.can_redo());
        history.push("c", 3, 1);
        assert!(!history.can_redo(), "新编辑必须清空重做分支");
        assert_eq!(
            history.descriptions().collect::<Vec<_>>(),
            vec!["a", "c"],
            "撤销掉的那条不应留在撤销栈里"
        );
    }

    #[test]
    fn byte_cap_drops_the_oldest_entries() {
        let mut history = History::new();
        history.set_cap(25);
        history.push("a", 1, 10);
        history.push("b", 2, 10);
        history.push("c", 3, 10);
        assert_eq!(
            history.descriptions().collect::<Vec<_>>(),
            vec!["b", "c"],
            "超出上限时丢最旧的"
        );
        assert_eq!(history.dropped(), 1);
    }

    #[test]
    fn a_single_huge_entry_is_kept() {
        let mut history = History::new();
        history.set_cap(10);
        history.push("一张截图", 1, 4096);
        assert_eq!(
            history.descriptions().count(),
            1,
            "单条超限也保留（否则撤销等于失效）"
        );
    }

    #[test]
    fn clear_resets_everything() {
        let mut history = History::new();
        history.push("a", 1, 1);
        history.undo();
        history.clear();
        assert!(!history.can_undo() && !history.can_redo());
        assert_eq!(history.descriptions().count(), 0);
    }
}
