//! 少量持久化：偏好设置与最近文件。
//!
//! ## 为什么用一个 `KeyValueStore` trait 而不是直接写配置文件
//! * 接口层不能依赖 `serde`/`toml`/`directories` 之类的 crate（本 crate 保持零依赖）；
//! * 桌面应用需要一个真实实现（写 JSON/TOML 到配置目录），测试需要一个内存实现，
//!   trait 让两者可以互换；
//! * UI 只读 [`AppSettings`] 这个纯数据结构，不需要知道文件放在哪里。

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::layout::{PageLayoutMode, ReadingDirection};

/// 极简键值存储。
///
/// ## 这个 trait 存在的目的
/// 把"设置存哪里、用什么格式"从会话与 UI 中隔离出去：真实实现写配置文件，
/// 测试实现用 `HashMap`，从而让"设置读写"也能被单测覆盖。
///
/// ## 实现者义务
/// * `get` 对不存在的键返回 `None`（不要报错）。
/// * `set` 失败（磁盘只读等）返回错误，由 UI 决定是提示还是忽略。
pub trait KeyValueStore: Send + Sync {
    /// 读取。
    fn get(&self, key: &str) -> Option<String>;

    /// 写入（覆盖）。
    fn set(&self, key: &str, value: &str) -> Result<()>;

    /// 删除。
    fn remove(&self, key: &str);
}

/// 最近打开文件列表的默认容量。
pub const DEFAULT_RECENT_CAPACITY: usize = 12;

const KEY_RECENT: &str = "app.recent_files";
const KEY_LAST_DIR: &str = "app.last_directory";
const KEY_ZOOM: &str = "app.zoom";
const KEY_LAYOUT_MODE: &str = "app.layout_mode";
const KEY_READING_DIRECTION: &str = "app.reading_direction";

/// 最近打开的文件（去重、置顶、有容量上限）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecentFiles {
    entries: Vec<PathBuf>,
    capacity: usize,
}

impl Default for RecentFiles {
    fn default() -> Self {
        Self::new(DEFAULT_RECENT_CAPACITY)
    }
}

impl RecentFiles {
    /// 新建。
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// 当前列表（最近打开的在前）。
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    /// 加入一个文件；已存在则移到最前；超出容量则丢弃最旧的。
    pub fn add(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.entries.retain(|entry| entry != &path);
        self.entries.insert(0, path);
        self.entries.truncate(self.capacity);
    }

    /// 移除（文件被删除、用户"从列表清除"）。
    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|entry| entry != path);
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 从存储读取（每行一个路径）。
    pub fn load(store: &dyn KeyValueStore, capacity: usize) -> Self {
        let mut recent = Self::new(capacity);
        if let Some(raw) = store.get(KEY_RECENT) {
            for line in raw.lines().filter(|line| !line.trim().is_empty()) {
                recent.add(PathBuf::from(line));
            }
        }
        recent
    }

    /// 写入存储。
    pub fn save(&self, store: &dyn KeyValueStore) -> Result<()> {
        let joined = self
            .entries
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        store.set(KEY_RECENT, &joined)
    }
}

/// 应用设置。
#[derive(Debug, Clone, PartialEq)]
pub struct AppSettings {
    /// 上次打开文件所在目录（文件对话框的初始位置）。
    pub last_directory: Option<PathBuf>,
    /// 上次的缩放倍数。
    pub zoom: f32,
    /// 上次的排布模式。
    pub layout_mode: PageLayoutMode,
    /// 上次的阅读方向。
    pub reading_direction: ReadingDirection,
    /// 最近文件。
    pub recent_files: RecentFiles,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_directory: None,
            zoom: 1.0,
            layout_mode: PageLayoutMode::Continuous,
            reading_direction: ReadingDirection::LeftToRight,
            recent_files: RecentFiles::default(),
        }
    }
}

impl AppSettings {
    /// 从存储读取；任何字段缺失或损坏都退化为默认值（设置损坏不应阻止应用启动）。
    pub fn load(store: &dyn KeyValueStore) -> Self {
        let mut settings = Self {
            last_directory: store
                .get(KEY_LAST_DIR)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            ..Self::default()
        };
        if let Some(zoom) = store.get(KEY_ZOOM).and_then(|raw| raw.parse::<f32>().ok()) {
            if zoom.is_finite() && zoom > 0.0 {
                settings.zoom = zoom;
            }
        }
        settings.layout_mode = match store.get(KEY_LAYOUT_MODE).as_deref() {
            Some("single_page") => PageLayoutMode::SinglePage,
            _ => PageLayoutMode::Continuous,
        };
        settings.reading_direction = match store.get(KEY_READING_DIRECTION).as_deref() {
            Some("rtl") => ReadingDirection::RightToLeft,
            _ => ReadingDirection::LeftToRight,
        };
        settings.recent_files = RecentFiles::load(store, DEFAULT_RECENT_CAPACITY);
        settings
    }

    /// 写入存储。
    pub fn save(&self, store: &dyn KeyValueStore) -> Result<()> {
        if let Some(directory) = &self.last_directory {
            store.set(KEY_LAST_DIR, &directory.to_string_lossy())?;
        }
        store.set(KEY_ZOOM, &format!("{}", self.zoom))?;
        store.set(
            KEY_LAYOUT_MODE,
            match self.layout_mode {
                PageLayoutMode::Continuous => "continuous",
                PageLayoutMode::SinglePage => "single_page",
            },
        )?;
        store.set(
            KEY_READING_DIRECTION,
            match self.reading_direction {
                ReadingDirection::LeftToRight => "ltr",
                ReadingDirection::RightToLeft => "rtl",
            },
        )?;
        self.recent_files.save(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MemoryStore;

    #[test]
    fn recent_files_are_deduplicated_and_capped() {
        let mut recent = RecentFiles::new(2);
        recent.add("/a.pdf");
        recent.add("/b.pdf");
        recent.add("/a.pdf"); // 置顶
        assert_eq!(recent.entries()[0], PathBuf::from("/a.pdf"));
        recent.add("/c.pdf"); // 超出容量，丢掉最旧的 /b.pdf
        assert_eq!(recent.entries().len(), 2);
        assert!(!recent.entries().contains(&PathBuf::from("/b.pdf")));
        recent.remove(Path::new("/a.pdf"));
        assert_eq!(recent.entries(), &[PathBuf::from("/c.pdf")]);
    }

    #[test]
    fn settings_round_trip_through_store() {
        let store = MemoryStore::new();
        let mut settings = AppSettings {
            zoom: 1.75,
            layout_mode: PageLayoutMode::SinglePage,
            reading_direction: ReadingDirection::RightToLeft,
            last_directory: Some(PathBuf::from("/home/user/docs")),
            ..AppSettings::default()
        };
        settings.recent_files.add("/home/user/docs/book.pdf");
        settings.save(&store).unwrap();

        let loaded = AppSettings::load(&store);
        assert_eq!(loaded.zoom, 1.75);
        assert_eq!(loaded.layout_mode, PageLayoutMode::SinglePage);
        assert_eq!(loaded.reading_direction, ReadingDirection::RightToLeft);
        assert_eq!(
            loaded.last_directory,
            Some(PathBuf::from("/home/user/docs"))
        );
        assert_eq!(
            loaded.recent_files.entries()[0],
            PathBuf::from("/home/user/docs/book.pdf")
        );
    }

    #[test]
    fn corrupted_settings_fall_back_to_defaults() {
        let store = MemoryStore::new();
        store.set(KEY_ZOOM, "not-a-number").unwrap();
        store.set(KEY_LAYOUT_MODE, "???").unwrap();
        let loaded = AppSettings::load(&store);
        assert_eq!(loaded.zoom, 1.0);
        assert_eq!(loaded.layout_mode, PageLayoutMode::Continuous);
    }
}
