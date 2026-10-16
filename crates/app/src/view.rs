//! 纯逻辑层：视图坐标换算、输入映射、纹理存储。
//!
//! 这些都不需要窗口/GPU，因此**可以被 `cargo test` 覆盖**；
//! 需要真实窗口与人工观察的部分（滚动跟手性、字形、配色）全部留给 `app.rs` 的 egui 胶水，
//! 按 agent.md 规则 2 交由用户验收。

use std::collections::{HashMap, VecDeque};

use egui::{Key, Modifiers, Vec2 as EguiVec2};
use pdfcore::prelude::*;

/// 键盘滚动步长（内容空间像素）。
pub const KEY_SCROLL_STEP: f32 = 64.0;

/// 滚轮缩放系数（每格）。
pub const WHEEL_ZOOM_FACTOR: f32 = 1.1;

/// 把内容空间的页矩形换算到**视图空间**（减去滚动偏移）。
///
/// 这是 UI 每帧都要做的事：`PagePlacement` 给出的是内容空间矩形，
/// `Viewport::scroll` 是视口左上角在内容空间的位置，两者相减即得屏幕上的落点。
pub fn view_rect_for(placement: &PagePlacement, scroll: Vec2) -> Rect {
    placement.rect.translate(Vec2::new(-scroll.x, -scroll.y))
}

/// 视图坐标 → 内容坐标（鼠标事件需要这个方向的换算）。
pub fn view_to_content(view_point: Point, scroll: Vec2) -> Point {
    Point::new(view_point.x + scroll.x, view_point.y + scroll.y)
}

/// 键盘 → 命令。`page_count` 用于 `End` 键（跳到最后一页）。
pub fn command_for_key(key: Key, modifiers: Modifiers, page_count: u32) -> Option<ViewerCommand> {
    let command_modifier = modifiers.ctrl || modifiers.command;
    match key {
        Key::ArrowDown => Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP))),
        Key::ArrowUp => Some(ViewerCommand::ScrollBy(Vec2::new(0.0, -KEY_SCROLL_STEP))),
        Key::Space => Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP * 4.0))),
        Key::PageDown => Some(ViewerCommand::NextPage),
        Key::PageUp => Some(ViewerCommand::PrevPage),
        Key::Home => Some(ViewerCommand::GoToPage(PageIndex::FIRST)),
        Key::End if page_count > 0 => Some(ViewerCommand::GoToPage(PageIndex::new(page_count - 1))),
        Key::Plus | Key::Equals => Some(ViewerCommand::ZoomIn),
        Key::Minus => Some(ViewerCommand::ZoomOut),
        Key::Num0 => Some(ViewerCommand::SetZoom(1.0)),
        Key::F if command_modifier => Some(ViewerCommand::SetSidebar(SidebarTab::Search)),
        Key::C if command_modifier => Some(ViewerCommand::CopySelection),
        Key::A if command_modifier => Some(ViewerCommand::SelectAll),
        Key::Escape => Some(ViewerCommand::SelectClear),
        _ => None,
    }
}

/// 滚轮 → 命令。
///
/// 注意方向：egui 的 `raw_scroll_delta.y` **向上为正**，而 `Viewport::scroll` 是
/// "视口左上角在内容空间的位置"（向下为正），因此这里取负号。
/// 按住 Ctrl/Cmd 时改为缩放（以当前缩放为基准按比例变化）。
pub fn command_for_scroll(
    delta: EguiVec2,
    zoom_modifier: bool,
    current_zoom: f32,
) -> Option<ViewerCommand> {
    if zoom_modifier {
        if delta.y.abs() < 0.5 {
            return None;
        }
        let factor = if delta.y > 0.0 { WHEEL_ZOOM_FACTOR } else { 1.0 / WHEEL_ZOOM_FACTOR };
        Some(ViewerCommand::SetZoom(current_zoom * factor))
    } else if delta.x.abs() > 0.0 || delta.y.abs() > 0.0 {
        Some(ViewerCommand::ScrollBy(Vec2::new(-delta.x, -delta.y)))
    } else {
        None
    }
}

/// 触控板/Ctrl+滚轮产生的**缩放增量** → 命令。
///
/// egui 会把 `Ctrl`+滚轮识别成缩放而不是滚动（见 `InputOptions::zoom_modifier`）：
/// 此时 `smooth_scroll_delta` 为 0，而 `InputState::zoom_delta()` 给出倍率。
/// 人工验收反馈的"Ctrl+滚轮不生效"根因就在这里 —— v1 只看滚动增量。
pub fn command_for_zoom_delta(zoom_delta: f32, current_zoom: f32) -> Option<ViewerCommand> {
    if !zoom_delta.is_finite() || (zoom_delta - 1.0).abs() < 1e-3 {
        return None;
    }
    Some(ViewerCommand::SetZoom(current_zoom * zoom_delta))
}

/// 一个带容量上限的键值存储（用于 `RenderKey` → 纹理）。
///
/// 与 LRU 位图缓存的区别：这里存的是 GPU 纹理句柄（`egui::TextureHandle`），
/// 丢失后可以按需从位图缓存重新上传，因此策略是"容量 + 按可见性裁剪"，
/// 而不是精确的字节计量。
#[derive(Debug)]
pub struct KeyedStore<V> {
    entries: HashMap<RenderKey, V>,
    order: VecDeque<RenderKey>,
    capacity: usize,
}

impl<V> KeyedStore<V> {
    /// 构造（容量至少为 1）。
    pub fn new(capacity: usize) -> Self {
        Self { entries: HashMap::new(), order: VecDeque::new(), capacity: capacity.max(1) }
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 是否存在。
    pub fn contains(&self, key: &RenderKey) -> bool {
        self.entries.contains_key(key)
    }

    /// 查询。
    pub fn get(&self, key: &RenderKey) -> Option<&V> {
        self.entries.get(key)
    }

    /// 写入并维护 LRU 顺序；超出容量时淘汰最旧的。
    pub fn insert(&mut self, key: RenderKey, value: V) {
        self.entries.insert(key, value);
        self.touch(key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else { break };
            self.entries.remove(&oldest);
        }
    }

    /// 丢弃所有不满足 `keep` 的条目（用于"只保留可见页的纹理"）。
    pub fn retain(&mut self, mut keep: impl FnMut(&RenderKey) -> bool) {
        self.entries.retain(|key, _| keep(key));
        self.order.retain(|key| self.entries.contains_key(key));
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn touch(&mut self, key: RenderKey) {
        if let Some(position) = self.order.iter().position(|existing| *existing == key) {
            self.order.remove(position);
        }
        self.order.push_back(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement() -> PagePlacement {
        PagePlacement::new(
            PageIndex::new(0),
            Rect::new(12.0, 12.0, 612.0, 792.0),
            Scale::IDENTITY,
            Rotation::None,
            Size::new(612.0, 792.0),
        )
    }

    fn key(page: u32) -> RenderKey {
        RenderKey::new(PageIndex::new(page), Scale::IDENTITY, Rotation::None)
    }

    #[test]
    fn view_rect_subtracts_scroll() {
        let rect = view_rect_for(&placement(), Vec2::new(100.0, 250.0));
        assert_eq!(rect, Rect::new(-88.0, -238.0, 612.0, 792.0));
    }

    #[test]
    fn view_and_content_coordinates_round_trip() {
        let scroll = Vec2::new(33.0, 77.0);
        let content = Point::new(120.0, 340.0);
        let view = Point::new(content.x - scroll.x, content.y - scroll.y);
        assert_eq!(view_to_content(view, scroll), content);
    }

    #[test]
    fn key_mapping_covers_navigation_and_zoom() {
        let none = Modifiers::NONE;
        assert_eq!(
            command_for_key(Key::PageDown, none, 10),
            Some(ViewerCommand::NextPage)
        );
        assert_eq!(
            command_for_key(Key::End, none, 10),
            Some(ViewerCommand::GoToPage(PageIndex::new(9)))
        );
        assert_eq!(command_for_key(Key::End, none, 0), None, "空文档不应越界");
        assert_eq!(command_for_key(Key::Num0, none, 3), Some(ViewerCommand::SetZoom(1.0)));
        assert_eq!(
            command_for_key(Key::ArrowDown, none, 3),
            Some(ViewerCommand::ScrollBy(Vec2::new(0.0, KEY_SCROLL_STEP)))
        );
        assert_eq!(command_for_key(Key::F, none, 3), None, "单独按 F 不映射");
        assert_eq!(
            command_for_key(Key::F, Modifiers::COMMAND, 3),
            Some(ViewerCommand::SetSidebar(SidebarTab::Search))
        );
        assert_eq!(
            command_for_key(Key::C, Modifiers::COMMAND, 3),
            Some(ViewerCommand::CopySelection)
        );
        assert_eq!(command_for_key(Key::Q, none, 3), None);
    }

    #[test]
    fn scroll_mapping_flips_sign_and_supports_zoom_modifier() {
        // 滚轮向上（egui 中 y 为正）→ 滚动偏移减小
        assert_eq!(
            command_for_scroll(EguiVec2::new(0.0, 40.0), false, 1.0),
            Some(ViewerCommand::ScrollBy(Vec2::new(0.0, -40.0)))
        );
        // 横向滚动同理
        assert_eq!(
            command_for_scroll(EguiVec2::new(15.0, 0.0), false, 1.0),
            Some(ViewerCommand::ScrollBy(Vec2::new(-15.0, 0.0)))
        );
        // Ctrl + 滚轮 → 以当前缩放为基准放大
        match command_for_scroll(EguiVec2::new(0.0, 10.0), true, 2.0) {
            Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 2.2).abs() < 1e-4),
            other => panic!("期望 SetZoom，实际 {other:?}"),
        }
        // 无输入 → 无命令
        assert_eq!(command_for_scroll(EguiVec2::ZERO, false, 1.0), None);
        assert_eq!(command_for_scroll(EguiVec2::new(0.0, 0.1), true, 1.0), None);
    }

    /// Ctrl+滚轮 / 触控板捏合的缩放增量必须被翻译成文档缩放（v1 遗漏了这条路径）。
    #[test]
    fn zoom_delta_maps_ctrl_wheel_and_pinch() {
        assert_eq!(command_for_zoom_delta(1.0, 2.0), None, "无缩放时不产生命令");
        assert_eq!(command_for_zoom_delta(f32::NAN, 2.0), None, "异常值必须被忽略");

        match command_for_zoom_delta(1.1, 2.0) {
            Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 2.2).abs() < 1e-4),
            other => panic!("期望 SetZoom，实际 {other:?}"),
        }
        match command_for_zoom_delta(0.5, 2.0) {
            Some(ViewerCommand::SetZoom(zoom)) => assert!((zoom - 1.0).abs() < 1e-4),
            other => panic!("期望 SetZoom，实际 {other:?}"),
        }
    }

    #[test]
    fn keyed_store_evicts_oldest_beyond_capacity() {
        let mut store = KeyedStore::new(2);
        store.insert(key(0), "a");
        store.insert(key(1), "b");
        assert_eq!(store.len(), 2);
        store.insert(key(2), "c");
        assert_eq!(store.len(), 2, "容量上限必须生效");
        assert!(!store.contains(&key(0)), "最旧的应被淘汰");
        assert_eq!(store.get(&key(2)), Some(&"c"));
    }

    #[test]
    fn keyed_store_reinsert_moves_to_newest() {
        let mut store = KeyedStore::new(2);
        store.insert(key(0), "a");
        store.insert(key(1), "b");
        // 重新插入 0 → 它变成"最新"，被淘汰的应该是 1
        store.insert(key(0), "a2");
        store.insert(key(2), "c");
        assert!(store.contains(&key(0)));
        assert!(!store.contains(&key(1)));
        assert_eq!(store.get(&key(0)), Some(&"a2"));
    }

    #[test]
    fn keyed_store_retain_drops_stale_entries() {
        let mut store = KeyedStore::new(16);
        for page in 0..5 {
            store.insert(key(page), page);
        }
        store.retain(|key| key.page.get() < 2);
        assert_eq!(store.len(), 2);
        assert!(store.contains(&key(0)) && store.contains(&key(1)));

        // retain 之后仍能正常插入（内部顺序表未损坏）
        store.insert(key(9), 9);
        assert_eq!(store.get(&key(9)), Some(&9));
        assert_eq!(store.len(), 3);

        store.clear();
        assert!(store.is_empty());
    }
}
