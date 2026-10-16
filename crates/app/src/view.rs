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

/// 瓦片在**视图空间**中的矩形。
///
/// ## 关键不变量（黑屏缺陷的根源，D-17）
///
/// 内容空间单位在当前缩放下**就等于该页位图的像素**：`PagePlacement::rect` 已经是
/// `页点数 × 缩放`，而 `PagePlacement::pixel_size()` 就是取整后的 `rect.size()`。
/// 因此把"位图像素偏移"换算成视图偏移时 **直接相加即可，绝不能再乘/除缩放**。
///
/// v1 在这里写了 `pixels.x / scale`：748% 时 6 块瓦片被压缩到页面左上角 1/7 的小方格里，
/// 而视口落在页面中部 —— 于是"画法=瓦片 6/6"却整块画布全黑（第三轮验收）。
pub fn tile_view_rect(page_rect: Rect, pixels: Rect) -> Rect {
    Rect::new(
        page_rect.x + pixels.x,
        page_rect.y + pixels.y,
        pixels.width,
        pixels.height,
    )
}

/// 把**旧缩放的瓦片**映射到当前页面矩形上（缩放过场的预览用）。
///
/// 高倍缩放下缓存里只有瓦片、没有整页图，因此"上一档的旧图"要用旧瓦片拼出来：
/// 每块旧瓦片按 `新页像素 / 旧页像素` 的比例缩放后贴到页面矩形的对应位置。
pub fn tile_view_rect_scaled(
    page_rect: Rect,
    pixels: Rect,
    old_page: Size,
    new_page: Size,
) -> Rect {
    let sx = if old_page.width > 0.0 { new_page.width / old_page.width } else { 1.0 };
    let sy = if old_page.height > 0.0 { new_page.height / old_page.height } else { 1.0 };
    Rect::new(
        page_rect.x + pixels.x * sx,
        page_rect.y + pixels.y * sy,
        pixels.width * sx,
        pixels.height * sy,
    )
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

    /// 找同一页、同一旋转下**缩放最接近**的整页纹理。
    ///
    /// 用途：缩放过程中新比例的位图还没渲染好时，用旧比例的图**拉伸显示**，
    /// 避免出现大片白色填充（第二轮人工验收反馈）。真正的"最近邻"由 `scale_milli` 决定；
    /// 瓦片（`tile.is_some()`）不参与匹配 —— 它们的几何含义与整页位图不同。
    pub fn best_page_texture(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Option<&V> {
        self.entries
            .iter()
            .filter(|(key, _)| key.page == page && key.rotation == rotation && key.tile.is_none())
            .min_by_key(|(key, _)| key.scale_milli.abs_diff(target_scale_milli))
            .map(|(_, value)| value)
    }

    /// 该键对应的页/缩放/旋转下是否已有**任意一块瓦片**就绪。
    ///
    /// 用途：分块渲染时判断"能不能立刻画真内容"。只要能画，就不该用上一档缩放的旧图去顶替，
    /// 否则瓦片虽已就绪却永远被旧图盖住。
    pub fn has_tiles(&self, like: &RenderKey) -> bool {
        self.entries.keys().any(|key| {
            key.page == like.page
                && key.scale_milli == like.scale_milli
                && key.rotation == like.rotation
                && key.tile.is_some()
        })
    }

    /// 该页/旋转下**最近的一个有瓦片的缩放档**（`scale_milli`），排除 `exclude` 自身。
    ///
    /// 用途：缩放过场时用上一档的瓦片拼出预览（高倍缩放下缓存里没有整页图）。
    pub fn best_tile_scale(
        &self,
        page: PageIndex,
        rotation: Rotation,
        target_scale_milli: u32,
    ) -> Option<u32> {
        let mut best: Option<u32> = None;
        for key in self.entries.keys() {
            if key.page != page || key.rotation != rotation || key.tile.is_none() {
                continue;
            }
            if key.scale_milli == target_scale_milli {
                continue;
            }
            let distance = key.scale_milli.abs_diff(target_scale_milli);
            if best.map_or(true, |current| distance < current.abs_diff(target_scale_milli)) {
                best = Some(key.scale_milli);
            }
        }
        best
    }

    /// 某个缩放档下该页已就绪的全部瓦片。
    pub fn tiles_at(
        &self,
        page: PageIndex,
        rotation: Rotation,
        scale_milli: u32,
    ) -> Vec<(TileSpec, &V)> {
        self.entries
            .iter()
            .filter(|(key, _)| {
                key.page == page
                    && key.rotation == rotation
                    && key.scale_milli == scale_milli
                    && key.tile.is_some()
            })
            .filter_map(|(key, value)| key.tile.map(|tile| (tile, value)))
            .collect()
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

    /// 回归测试：瓦片位置**不得**再除以缩放。
    ///
    /// 数字取自第三轮验收的真实诊断行（748% 下的第 2 页）：
    /// `视图矩形=(-1679,-2042 3870×4785)`，网格里的第 (1,1) 块瓦片。
    /// 正确结果是"页面左上角 + (512,512)"；错误实现会把它压到 +69.4 附近 ——
    /// 视口落在页面中部时就什么都看不到（全黑）。
    #[test]
    fn tile_view_rect_is_in_content_units_not_scaled() {
        let page_rect = Rect::new(-1679.0, -2042.0, 3870.0, 4785.0);
        let pixels = Rect::new(512.0, 512.0, 512.0, 512.0);
        let rect = tile_view_rect(page_rect, pixels);
        assert_eq!(rect, Rect::new(-1167.0, -1530.0, 512.0, 512.0));

        // 反面：任何"除以缩放"的实现都会得到这个值（748% ≈ 7.38 倍）
        let wrong = Rect::new(
            page_rect.x + pixels.x / 7.38,
            page_rect.y + pixels.y / 7.38,
            pixels.width / 7.38,
            pixels.height / 7.38,
        );
        assert_ne!(rect, wrong, "瓦片位置一旦除缩放，瓦片会被压到页面左上角");
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

    /// 缩放期间用"上一档缩放"的位图拉伸占位（第二轮验收：避免大片白色填充）。
    #[test]
    fn best_page_texture_picks_the_nearest_scale() {
        let scaled = |page: u32, scale: f32, rotation: Rotation| {
            RenderKey::new(PageIndex::new(page), Scale::from_zoom(scale), rotation)
        };

        let mut store = KeyedStore::new(16);
        store.insert(scaled(0, 1.0, Rotation::None), "one");
        store.insert(scaled(0, 2.0, Rotation::None), "two");
        store.insert(scaled(1, 2.0, Rotation::None), "other-page");

        // 目标 2.1 → 最接近的整页图是 2.0
        assert_eq!(store.best_page_texture(PageIndex::new(0), Rotation::None, 2100), Some(&"two"));
        // 目标 1.1 → 1.0
        assert_eq!(store.best_page_texture(PageIndex::new(0), Rotation::None, 1100), Some(&"one"));
        // 其它页不参与
        assert_eq!(store.best_page_texture(PageIndex::new(2), Rotation::None, 2000), None);
        // 旋转不同不可顶替（否则画面会横躺）
        assert_eq!(
            store.best_page_texture(PageIndex::new(0), Rotation::Clockwise90, 2000),
            None
        );
    }

    /// 分块渲染时，判断"能不能立刻画真内容"：同页同缩放的瓦片只要有一块就绪就算能。
    #[test]
    fn has_tiles_detects_ready_tiles_for_the_same_key() {
        let mut store = KeyedStore::new(8);
        let whole = RenderKey::new(PageIndex::new(0), Scale::from_zoom(2.0), Rotation::None);
        assert!(!store.has_tiles(&whole), "空存储不应报告有瓦片");

        store.insert(
            RenderKey::tiled(
                PageIndex::new(0),
                Scale::from_zoom(2.0),
                Rotation::None,
                TileSpec::new(0, 0),
            ),
            "tile",
        );
        assert!(store.has_tiles(&whole), "同页同缩放的瓦片应被识别");
        assert!(
            !store.has_tiles(&RenderKey::new(PageIndex::new(1), Scale::from_zoom(2.0), Rotation::None)),
            "其它页的瓦片不算"
        );
        assert!(
            !store.has_tiles(&RenderKey::new(PageIndex::new(0), Scale::from_zoom(1.5), Rotation::None)),
            "其它缩放的瓦片不算"
        );
    }

    /// 高倍缩放的过场：旧瓦片要按"新页像素 / 旧页像素"等比放大回整页。
    #[test]
    fn tile_view_rect_scaled_maps_old_tiles_onto_the_new_page() {
        let page_rect = Rect::new(-100.0, -200.0, 2000.0, 4000.0);
        // 旧比例下页位图 1000×2000，瓦片 (0,0) 占 500×500 像素
        let old_page = Size::new(1000.0, 2000.0);
        let new_page = Size::new(2000.0, 4000.0);
        let rect = tile_view_rect_scaled(
            page_rect,
            Rect::new(500.0, 500.0, 500.0, 500.0),
            old_page,
            new_page,
        );
        // 放大 2 倍：偏移与原尺寸都翻倍
        assert_eq!(rect, Rect::new(900.0, 800.0, 1000.0, 1000.0));

        // 尺寸为 0 时不得产生 NaN/Inf
        let degenerate = tile_view_rect_scaled(
            page_rect,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Size::new(0.0, 0.0),
            new_page,
        );
        assert!(degenerate.x.is_finite() && degenerate.width.is_finite());
    }

    /// 预览用的旧瓦片查询：只认同页同旋转、且不等于当前缩放档的那些。
    #[test]
    fn best_tile_scale_and_tiles_at_pick_the_previous_scale() {
        let mut store = KeyedStore::new(16);
        let tile = |scale: f32, col: u16, row: u16| {
            RenderKey::tiled(
                PageIndex::new(0),
                Scale::from_zoom(scale),
                Rotation::None,
                TileSpec::new(col, row),
            )
        };
        store.insert(tile(1.0, 0, 0), "old-a");
        store.insert(tile(1.0, 1, 0), "old-b");
        store.insert(tile(2.0, 0, 0), "current");

        // 当前档 2.0 → 最近的"有瓦片的其它档"是 1.0
        assert_eq!(store.best_tile_scale(PageIndex::new(0), Rotation::None, 2000), Some(1000));
        // 当前档 1.0 → 2.0
        assert_eq!(store.best_tile_scale(PageIndex::new(0), Rotation::None, 1000), Some(2000));
        // 其它页 / 其它旋转都没有
        assert_eq!(store.best_tile_scale(PageIndex::new(1), Rotation::None, 2000), None);
        assert_eq!(store.best_tile_scale(PageIndex::new(0), Rotation::Clockwise90, 2000), None);

        let old = store.tiles_at(PageIndex::new(0), Rotation::None, 1000);
        assert_eq!(old.len(), 2, "旧档应有 2 块");
        assert!(old.iter().any(|(tile, value)| *tile == TileSpec::new(0, 0) && **value == "old-a"));
        assert_eq!(store.tiles_at(PageIndex::new(0), Rotation::None, 2000).len(), 1);
    }
}
