//! 用户设置文件（`settings.json`）：UI 形态、注释默认样式、每个文件的阅读状态。
//!
//! ## 为什么是"文档列表 + 全局 UI"
//!
//! * **全局一份**：窗口尺寸、侧栏宽度、注释默认样式（颜色/透明度/墨迹线宽）——
//!   这些是"使用者习惯"，与具体文件无关；
//! * **每个文件一份**：页码、缩放、横/纵排——这些是"上次看到哪"。
//!
//! 同一个列表兼作"最近文件"：`documents[0]` 就是最近打开的文件，最多 [`RECENT_LIMIT`] 条。
//! 这样不需要单独维护 `recent` 与"阅读状态"两份可能互相矛盾的数据。
//!
//! ## 规则
//!
//! * 缺失字段用默认值；未知字段忽略（只做加法，和存档的策略一致）；
//! * 数值统一夹取（透明度 0.1–1、线宽 0.5–8、缩放 0.1–10）；
//! * 解析失败：把坏文件改名成 `settings.json.bad-<时间>`，用默认值继续；
//! * 写盘：同目录临时文件 + rename（原子替换）。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use pdfcore::annotation::{INK_COLOR, TEXT_MARK_COLOR, TEXT_MARK_OPACITY};

/// 最近文件 / 每文件阅读状态的上限。
pub const RECENT_LIMIT: usize = 12;

/// 注释默认样式（新注释用）。
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationDefaults {
    /// 高亮/下划线/删除线颜色。
    pub markup_color: [u8; 3],
    /// 高亮透明度（下划线/删除线固定 1.0）。
    pub highlight_opacity: f32,
    /// 墨迹颜色。
    pub ink_color: [u8; 3],
    /// 墨迹线宽（页面空间 pt）。
    pub ink_width: f32,
}

impl Default for AnnotationDefaults {
    fn default() -> Self {
        Self {
            markup_color: TEXT_MARK_COLOR,
            highlight_opacity: TEXT_MARK_OPACITY,
            ink_color: INK_COLOR,
            ink_width: 2.0,
        }
    }
}

/// 全局 UI 形态。
#[derive(Debug, Clone, PartialEq)]
pub struct UiSettings {
    /// 上次窗口内尺寸（逻辑像素）；只用于保存，恢复交给窗口系统。
    pub window_size: [f32; 2],
    /// 侧栏宽度（逻辑像素）。
    pub sidebar_width: f32,
    /// 顶栏「Pin悬浮响应」开关。
    pub pin_hover_enabled: bool,
    /// 顶栏「横向滚动」开关。
    pub horizontal_scroll: bool,
    /// 引用显示：链接框。
    pub show_links: bool,
    /// 引用显示：引用点。
    pub show_dest_points: bool,
    /// 引用显示：标注。
    pub show_annotations: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            window_size: [1100.0, 900.0],
            sidebar_width: 260.0,
            pin_hover_enabled: true,
            horizontal_scroll: false,
            show_links: false,
            show_dest_points: false,
            show_annotations: true,
        }
    }
}

/// 保存选项。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SaveSettings {
    /// 原地保存前是否留一份 `.bak` 副本（默认关闭）。
    pub backup_in_place: bool,
}

/// 一个文件的阅读状态（同时充当"最近文件"条目）。
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentState {
    /// 文件路径（尽量规范化；规范化失败时保留原样）。
    pub path: PathBuf,
    /// 页码（0 起；打开时夹到实际页数内）。
    pub page: u32,
    /// 缩放倍数。
    pub zoom: f32,
    /// 阅读位置锚点：视口左上角在**内容空间**的坐标（与页码一起才能回到原位置）。
    /// `None` = 旧设置文件没有这个字段（只按页码恢复）。
    pub anchor: Option<[f32; 2]>,
    /// 是否横向排版。
    pub horizontal: bool,
    /// 最近一次打开的时间（Unix 秒，排序/淘汰用）。
    pub opened_at: i64,
}

/// 整份设置。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// 全局 UI。
    pub ui: UiSettings,
    /// 注释默认样式。
    pub annotation: AnnotationDefaults,
    /// 保存选项。
    pub save: SaveSettings,
    /// 按 `opened_at` 降序的文件列表。
    pub documents: Vec<DocumentState>,
}

impl Settings {
    /// 记下某个文件的阅读状态：存在则更新并移到最前，超过上限淘汰最旧的。
    pub fn remember_document(&mut self, mut state: DocumentState) {
        state.zoom = clamp(state.zoom, 0.1, 10.0);
        self.documents.retain(|item| item.path != state.path);
        self.documents.insert(0, state);
        self.documents.truncate(RECENT_LIMIT);
    }

    /// 查某个文件的上次阅读状态。
    pub fn document(&self, path: &Path) -> Option<&DocumentState> {
        self.documents.iter().find(|item| item.path == path)
    }

    /// 序列化成 JSON 文本。
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n  \"version\": 1,\n");
        out.push_str(&format!(
            "  \"ui\": {{ \"window_size\": [{:.0}, {:.0}], \"sidebar_width\": {:.0},              \"pin_hover_enabled\": {}, \"horizontal_scroll\": {}, \"show_links\": {},              \"show_dest_points\": {}, \"show_annotations\": {} }},\n",
            self.ui.window_size[0],
            self.ui.window_size[1],
            self.ui.sidebar_width,
            self.ui.pin_hover_enabled,
            self.ui.horizontal_scroll,
            self.ui.show_links,
            self.ui.show_dest_points,
            self.ui.show_annotations
        ));
        out.push_str(&format!(
            "  \"save\": {{ \"backup_in_place\": {} }},\n",
            self.save.backup_in_place
        ));
        out.push_str("  \"annotation_defaults\": {\n");
        out.push_str(&format!(
            "    \"markup_color\": [{}, {}, {}],\n",
            self.annotation.markup_color[0],
            self.annotation.markup_color[1],
            self.annotation.markup_color[2]
        ));
        out.push_str(&format!(
            "    \"highlight_opacity\": {:.2},\n",
            self.annotation.highlight_opacity
        ));
        out.push_str(&format!(
            "    \"ink_color\": [{}, {}, {}],\n",
            self.annotation.ink_color[0],
            self.annotation.ink_color[1],
            self.annotation.ink_color[2]
        ));
        out.push_str(&format!(
            "    \"ink_width\": {:.2}\n  }},\n",
            self.annotation.ink_width
        ));
        out.push_str("  \"documents\": [");
        for (index, document) in self.documents.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "\n    {{ \"path\": {}, \"page\": {}, \"zoom\": {:.3}{}, \"horizontal\": {}, \"opened_at\": {} }}",
                quote(&document.path.to_string_lossy()),
                document.page,
                document.zoom,
                document
                    .anchor
                    .map(|anchor| format!(", \"anchor\": [{:.2}, {:.2}]", anchor[0], anchor[1]))
                    .unwrap_or_default(),
                document.horizontal,
                document.opened_at
            ));
        }
        out.push_str("\n  ]\n}\n");
        out
    }

    /// 解析 JSON；字段缺失用默认，未知字段忽略。
    pub fn from_json(text: &str) -> Result<Self, String> {
        use crate::json::{Json, JsonLookup};
        let root = Json::parse(text).map_err(|error| format!("解析失败：{error}"))?;
        let Json::Object(root) = root else {
            return Err("设置文件顶层不是对象".to_string());
        };
        let root = root.as_slice();
        let mut settings = Settings::default();
        if let Some(ui) = root.field("ui").and_then(|value| value.as_object()) {
            if let Some(size) = ui.field("window_size").and_then(|value| value.as_array()) {
                let numbers: Vec<f32> = size.iter().filter_map(|value| value.as_number()).collect();
                if numbers.len() == 2 {
                    settings.ui.window_size = [
                        clamp(numbers[0], 320.0, 10_000.0),
                        clamp(numbers[1], 240.0, 10_000.0),
                    ];
                }
            }
            if let Some(width) = ui
                .field("sidebar_width")
                .and_then(|value| value.as_number())
            {
                settings.ui.sidebar_width = clamp(width, 160.0, 900.0);
            }
            settings.ui.pin_hover_enabled = ui
                .field("pin_hover_enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            settings.ui.horizontal_scroll = ui
                .field("horizontal_scroll")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            settings.ui.show_links = ui
                .field("show_links")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            settings.ui.show_dest_points = ui
                .field("show_dest_points")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            settings.ui.show_annotations = ui
                .field("show_annotations")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
        }
        if let Some(defaults) = root
            .field("annotation_defaults")
            .and_then(|value| value.as_object())
        {
            settings.annotation.markup_color =
                color(defaults.field("markup_color")).unwrap_or(settings.annotation.markup_color);
            settings.annotation.ink_color =
                color(defaults.field("ink_color")).unwrap_or(settings.annotation.ink_color);
            if let Some(opacity) = defaults
                .field("highlight_opacity")
                .and_then(|value| value.as_number())
            {
                settings.annotation.highlight_opacity = clamp(opacity, 0.1, 1.0);
            }
            if let Some(width) = defaults
                .field("ink_width")
                .and_then(|value| value.as_number())
            {
                settings.annotation.ink_width = clamp(width, 0.5, 8.0);
            }
        }
        if let Some(save) = root.field("save").and_then(|value| value.as_object()) {
            settings.save.backup_in_place = save
                .field("backup_in_place")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
        }
        if let Some(documents) = root.field("documents").and_then(|value| value.as_array()) {
            for document in documents {
                let Some(object) = document.as_object() else {
                    continue;
                };
                let Some(path) = object
                    .field("path")
                    .and_then(|value| value.as_str())
                    .filter(|path| !path.is_empty())
                else {
                    continue;
                };
                let page = object
                    .field("page")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0)
                    .max(0) as u32;
                let zoom = object
                    .field("zoom")
                    .and_then(|value| value.as_number())
                    .unwrap_or(1.0);
                let anchor = object
                    .field("anchor")
                    .and_then(|value| value.as_array())
                    .and_then(|values| {
                        let numbers: Vec<f32> = values
                            .iter()
                            .filter_map(|value| value.as_number())
                            .collect();
                        (numbers.len() == 2).then(|| {
                            [
                                clamp(numbers[0], 0.0, 1_000_000_000.0),
                                clamp(numbers[1], 0.0, 1_000_000_000.0),
                            ]
                        })
                    });
                let horizontal = object
                    .field("horizontal")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let opened_at = object
                    .field("opened_at")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                settings.documents.push(DocumentState {
                    path: PathBuf::from(path),
                    page,
                    zoom: clamp(zoom, 0.1, 10.0),
                    anchor,
                    horizontal,
                    opened_at,
                });
            }
        }
        settings.documents.truncate(RECENT_LIMIT);
        settings
            .documents
            .sort_by_key(|document| std::cmp::Reverse(document.opened_at));
        Ok(settings)
    }

    /// 设置文件的默认位置：
    /// Linux `$XDG_CONFIG_HOME/pdf-reader/settings.json`（缺省 `~/.config/…`），
    /// Windows `%APPDATA%\pdf-reader\settings.json`，
    /// macOS `~/Library/Application Support/pdf-reader/settings.json`。
    pub fn path() -> Option<PathBuf> {
        config_dir(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("APPDATA"),
            std::env::var_os("HOME"),
        )
        .map(|dir| dir.join("settings.json"))
    }

    /// 读设置；文件不存在 → 默认值。坏文件会被改名备份，并返回提示。
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = Self::path() else {
            return (Settings::default(), None);
        };
        load_from(&path)
    }

    /// 写设置（原子替换）。
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "找不到配置目录",
            ));
        };
        self.save_to(&path)
    }

    /// 写到指定路径（测试用）。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, self.to_json())?;
        std::fs::rename(&temporary, path)
    }
}

/// 读指定路径的设置；解析失败时改名备份并回退默认（返回提示）。
pub fn load_from(path: &Path) -> (Settings, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Settings::default(), None)
        }
        Err(error) => return (Settings::default(), Some(format!("读取设置失败：{error}"))),
    };
    match Settings::from_json(&text) {
        Ok(settings) => (settings, None),
        Err(error) => {
            let backup = path.with_extension(format!(
                "json.bad-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0)
            ));
            let _ = std::fs::rename(path, &backup);
            (
                Settings::default(),
                Some(format!(
                    "设置文件无法解析（{error}），已备份为 {}，本次用默认值",
                    backup.display()
                )),
            )
        }
    }
}

fn config_dir(
    xdg: Option<OsString>,
    appdata: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let app = "pdf-reader";
    if cfg!(target_os = "windows") {
        // Windows 用 %APPDATA%；没有就退回 HOME。
        if let Some(appdata) = appdata.as_ref().filter(|value| !value.is_empty()) {
            return Some(PathBuf::from(appdata).join(app));
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = home.as_ref().filter(|value| !value.is_empty()) {
            return Some(
                PathBuf::from(home)
                    .join("Library/Application Support")
                    .join(app),
            );
        }
    }
    if let Some(xdg) = xdg.as_ref().filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg).join(app));
    }
    if let Some(home) = home.as_ref().filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home).join(".config").join(app));
    }
    None
}

fn clamp(value: f32, low: f32, high: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        low
    }
}

fn color(value: Option<&crate::json::Json>) -> Option<[u8; 3]> {
    let channels = value?.as_array()?;
    if channels.len() < 3 {
        return None;
    }
    let mut rgb = [0u8; 3];
    for (index, channel) in channels.iter().take(3).enumerate() {
        rgb[index] = channel.as_number()?.clamp(0.0, 255.0) as u8;
    }
    Some(rgb)
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        let mut settings = Settings {
            ui: UiSettings {
                window_size: [1400.0, 900.0],
                sidebar_width: 320.0,
                ..UiSettings::default()
            },
            annotation: AnnotationDefaults {
                markup_color: [235, 70, 70],
                highlight_opacity: 0.5,
                ink_color: [30, 90, 210],
                ink_width: 3.5,
            },
            save: SaveSettings::default(),
            documents: Vec::new(),
        };
        settings.remember_document(DocumentState {
            path: PathBuf::from("/tmp/a.pdf"),
            page: 3,
            zoom: 1.25,
            anchor: Some([10.0, 20.0]),
            horizontal: true,
            opened_at: 1_700_000_000,
        });
        settings
    }

    #[test]
    fn round_trips_through_json() {
        let settings = sample();
        let text = settings.to_json();
        let parsed = Settings::from_json(&text).expect("应能解析");
        assert_eq!(parsed, settings);
    }

    #[test]
    fn ui_toggles_and_backup_round_trip() {
        let mut settings = sample();
        settings.ui.pin_hover_enabled = false;
        settings.ui.horizontal_scroll = true;
        settings.ui.show_links = true;
        settings.ui.show_dest_points = true;
        settings.ui.show_annotations = false;
        settings.save.backup_in_place = true;
        let parsed = Settings::from_json(&settings.to_json()).expect("应能解析");
        assert!(!parsed.ui.pin_hover_enabled);
        assert!(parsed.ui.horizontal_scroll);
        assert!(parsed.ui.show_links);
        assert!(parsed.ui.show_dest_points);
        assert!(!parsed.ui.show_annotations);
        assert!(parsed.save.backup_in_place);
    }

    #[test]
    fn unknown_fields_are_ignored_and_defaults_fill_gaps() {
        let parsed = Settings::from_json(r#"{"version":1,"future":{"x":1}}"#).expect("应能解析");
        assert_eq!(parsed, Settings::default());
    }

    #[test]
    fn missing_anchor_is_none_and_present_anchor_round_trips() {
        let parsed = Settings::from_json(
            r#"{"documents":[{"path":"/tmp/a.pdf","page":2,"anchor":[12.5,34.0],"opened_at":1}]}"#,
        )
        .expect("应能解析");
        assert_eq!(parsed.documents[0].anchor, Some([12.5, 34.0]));

        let legacy =
            Settings::from_json(r#"{"documents":[{"path":"/tmp/a.pdf","page":2,"opened_at":1}]}"#)
                .expect("应能解析");
        assert_eq!(legacy.documents[0].anchor, None, "旧文件没有锚点");
    }

    #[test]
    fn values_are_clamped() {
        let parsed = Settings::from_json(
            r#"{"annotation_defaults":{"highlight_opacity":5.0,"ink_width":99.0},
                "ui":{"sidebar_width":1.0,"window_size":[100.0,100.0]},
                "documents":[{"path":"/tmp/a.pdf","page":3,"zoom":100.0,"opened_at":1}]}"#,
        )
        .expect("应能解析");
        assert_eq!(parsed.annotation.highlight_opacity, 1.0);
        assert_eq!(parsed.annotation.ink_width, 8.0);
        assert_eq!(parsed.ui.sidebar_width, 160.0);
        assert_eq!(parsed.ui.window_size, [320.0, 240.0]);
        assert_eq!(parsed.documents[0].zoom, 10.0);
    }

    #[test]
    fn document_list_is_capped_and_ordered() {
        let mut settings = Settings::default();
        for index in 0..20 {
            settings.remember_document(DocumentState {
                path: PathBuf::from(format!("/tmp/{index}.pdf")),
                page: 0,
                zoom: 1.0,
                anchor: None,
                horizontal: false,
                opened_at: index,
            });
        }
        assert_eq!(settings.documents.len(), RECENT_LIMIT);
        assert_eq!(settings.documents[0].path, PathBuf::from("/tmp/19.pdf"));
        assert!(settings.document(Path::new("/tmp/0.pdf")).is_none());
    }

    #[test]
    fn same_document_is_moved_to_front_not_duplicated() {
        let mut settings = Settings::default();
        let mut state = DocumentState {
            path: PathBuf::from("/tmp/a.pdf"),
            page: 1,
            zoom: 1.0,
            anchor: None,
            horizontal: false,
            opened_at: 10,
        };
        settings.remember_document(state.clone());
        state.page = 5;
        state.opened_at = 20;
        settings.remember_document(state);
        assert_eq!(settings.documents.len(), 1);
        assert_eq!(settings.documents[0].page, 5);
    }

    #[test]
    fn broken_file_is_backed_up_and_defaults_are_used() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{not json").unwrap();
        let (settings, warning) = load_from(&path);
        assert_eq!(settings, Settings::default());
        assert!(warning.is_some(), "应给出提示");
        assert!(!path.exists(), "坏文件应被改名");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".bad-"))
            .collect();
        assert_eq!(backups.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_and_load_round_trip_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-settings-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let path = dir.join("settings.json");
        let settings = sample();
        settings.save_to(&path).expect("应能写");
        let (loaded, warning) = load_from(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, settings);
        std::fs::remove_dir_all(&dir).ok();
    }
}
