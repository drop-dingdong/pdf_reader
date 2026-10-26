//! 自定义数据的序列化（JSON，无第三方依赖）。
//!
//! 只存**可重建图像所需的元数据**：图片像素一律不入档 ——
//! 被引用点与自定义图片 preview 都记录了"页号 + 页面空间矩形"，
//! 载入后按当前缩放重新合成即可（见 `PdfReaderApp::snapshot_for`）。
//!
//! 文件与 PDF 同目录同名，后缀追加 `.json`（`aa/bb.pdf` → `aa/bb.pdf.json`）。

use std::fmt::Write as _;

use pdfcore::prelude::*;

use crate::json::{Json, JsonLookup, Parsed};

/// 存档格式的当前版本。
///
/// 约定（加版本时请遵守，见 `docs/current/persistence.md`）：
/// * 读取侧必须能读**所有更旧**的版本，并把它升级到当前版本；
/// * 升级只做**加法**（给新字段填默认值），不删除、不改名、不改变已有字段的语义；
/// * `to_json` **永远写当前版本**；读到旧版本后由调用方置脏，下次保存即写回新版本。
///
/// 历史：
/// * **v1**：没有 `version` 字段（或写成 `1`）。只记录页号 / 页面空间矩形 / 正文尺寸 / 题目 / 文字。
/// * **v2**：被引用点与自定义**图片** preview 增加 `scale`（截图时的**显示比例**，`1.0` = 100%）。
///   预览图不再"按当前缩放合成"，而是按这个记录值请求渲染 —— 否则缩放到别处后
///   缓存里没有那块像素，初次生成会只剩白的/干脆没有图。
pub const CURRENT_VERSION: u32 = 2;

/// 旧存档缺 `scale` 时的默认分辨率（用户要求：按 100% 处理）。
pub const DEFAULT_SCALE: f32 = 1.0;

/// 版本迁移的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// 读到的原始版本（缺 `version` 字段时按 1）。
    pub from: u32,
    /// 迁移后的版本（比当前版本新时保持原值）。
    pub to: u32,
    /// 是否改动了数据 —— 调用方据此**置脏**，让下次保存写回当前版本。
    pub changed: bool,
    /// 给人看的说明（进日志 / 状态栏）。
    pub notes: Vec<String>,
}

impl Migration {
    /// 一行摘要（状态栏用）。
    pub fn summary(&self) -> String {
        if self.changed {
            format!("存档已从 v{} 升级到 v{}（保存后生效）", self.from, self.to)
        } else {
            format!("存档版本 v{}（当前）", self.to)
        }
    }
}

/// 一条被引用点的可持久化数据（不含截图）。
#[derive(Debug, Clone, PartialEq)]
pub struct SavedMark {
    /// 页内坐标
    pub point: Point,
    /// 页面空间矩形
    pub rect: Rect,
    /// 浮窗正文尺寸（egui 单位）
    pub body: [f32; 2],
    /// ▲v2 截图时的**显示比例**（`1.0` = 100%）。预览图按它请求渲染。
    pub scale: f32,
}

/// 一条自定义 preview 的可持久化数据（不含截图）。
#[derive(Debug, Clone, PartialEq)]
pub struct SavedCustom {
    /// 所在页
    pub page: PageIndex,
    /// 页内坐标
    pub point: Point,
    /// 页面空间矩形
    pub rect: Rect,
    /// 浮窗正文尺寸
    pub body: [f32; 2],
    /// 题目
    pub title: String,
    /// 仅文字预览有内容
    pub text: Option<String>,
    /// ▲v2 截图时的**显示比例**（`1.0` = 100%）。文字预览用不到，写 1.0。
    pub scale: f32,
}

/// 整个存档。
#[derive(Debug, Clone, PartialEq)]
pub struct SavedData {
    /// 读到 / 写入的格式版本（`from_json` 填的是**文件里的**版本）。
    pub version: u32,
    /// 每页的链接（含目的地的类型/页号/坐标/URI）
    pub links: Vec<Vec<Link>>,
    /// 每页的被引用点
    pub dest_marks: Vec<Vec<SavedMark>>,
    /// 自定义图片 preview
    pub custom_images: Vec<SavedCustom>,
    /// 自定义文字 preview
    pub custom_texts: Vec<SavedCustom>,
}

impl Default for SavedData {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            links: Vec::new(),
            dest_marks: Vec::new(),
            custom_images: Vec::new(),
            custom_texts: Vec::new(),
        }
    }
}

impl SavedData {
    /// 序列化为 JSON 文本。
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{{\n  \"version\": {CURRENT_VERSION},\n");

        // links
        out.push_str("  \"links\": [");
        for (page, links) in self.links.iter().enumerate() {
            if page > 0 {
                out.push(',');
            }
            out.push_str("\n    [");
            for (index, link) in links.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('{');
                write_rect(&mut out, "rect", link.rect);
                out.push_str(", \"dest\": ");
                write_destination(&mut out, &link.destination);
                out.push('}');
            }
            out.push(']');
        }
        out.push_str("\n  ],\n");

        // dest_marks
        out.push_str("  \"dest_marks\": [");
        for (page, marks) in self.dest_marks.iter().enumerate() {
            if page > 0 {
                out.push(',');
            }
            out.push_str("\n    [");
            for (index, mark) in marks.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('{');
                write_point(&mut out, "point", mark.point);
                out.push_str(", ");
                write_rect(&mut out, "rect", mark.rect);
                let _ = write!(
                    out,
                    ", \"body\": [{}, {}], \"scale\": {}",
                    num(mark.body[0]),
                    num(mark.body[1]),
                    num(mark.scale)
                );
                out.push('}');
            }
            out.push(']');
        }
        out.push_str("\n  ],\n");

        write_customs(&mut out, "custom_images", &self.custom_images);
        out.push_str(",\n");
        write_customs(&mut out, "custom_texts", &self.custom_texts);
        out.push_str("\n}\n");
        out
    }

    /// 解析（本模块自己写出的格式；非法输入返回 `Err`）。
    pub fn from_json(text: &str) -> Parsed<Self> {
        let value = Json::parse(text)?;
        let root = value.as_object().ok_or("根节点不是对象")?;
        // 缺 `version` 字段 = v1（本模块在 v2 之前确实写过 `"version": 1`，也兼容没有它的文件）
        let mut data = SavedData {
            version: root
                .field("version")
                .and_then(|value| value.as_number())
                .map(|value| value.round().max(1.0) as u32)
                .unwrap_or(1),
            ..SavedData::default()
        };
        if let Some(links) = root.field("links") {
            for page in links.as_array().ok_or("links 不是数组")? {
                let mut page_links = Vec::new();
                for link in page.as_array().ok_or("links 页不是数组")? {
                    page_links.push(parse_link(link)?);
                }
                data.links.push(page_links);
            }
        }
        if let Some(marks) = root.field("dest_marks") {
            for page in marks.as_array().ok_or("dest_marks 不是数组")? {
                let mut page_marks = Vec::new();
                for mark in page.as_array().ok_or("dest_marks 页不是数组")? {
                    page_marks.push(parse_mark(mark)?);
                }
                data.dest_marks.push(page_marks);
            }
        }
        if let Some(items) = root.field("custom_images") {
            for item in items.as_array().ok_or("custom_images 不是数组")? {
                data.custom_images.push(parse_custom(item)?);
            }
        }
        if let Some(items) = root.field("custom_texts") {
            for item in items.as_array().ok_or("custom_texts 不是数组")? {
                data.custom_texts.push(parse_custom(item)?);
            }
        }
        Ok(data)
    }
}

/// 载入存档失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// 内容不是合法的存档 JSON。
    Parse(String),
    /// 存档由**更新版本**的程序写入 —— 禁止读取（见 D-107）。
    NewerVersion {
        /// 存档里的版本号。
        found: u32,
        /// 本程序支持的版本号。
        supported: u32,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "不是合法的存档：{error}"),
            Self::NewerVersion { found, supported } => write!(
                f,
                "存档由更新版本写入（v{found} > v{supported}），本程序拒绝读取，也不会覆盖它"
            ),
        }
    }
}

/// 一次成功载入：已迁移到当前版本的数据。
#[derive(Debug, Clone)]
pub struct Loaded {
    /// 迁移后的数据。
    pub data: SavedData,
    /// 这次迁移做了什么。
    pub migration: Migration,
}

/// **载入存档的唯一入口**：解析 → 版本闸门 → 迁移。
///
/// 版本策略（D-107）：
/// * **向后兼容**：旧版本（v1、无 `version`）照常读，就地升级并置脏，保存后写回当前版本；
/// * **禁止向前兼容**：`version > CURRENT_VERSION` 直接拒绝，**不读也不写**。
///   旧程序读新存档必然丢字段，读一半再保存就是"用残缺数据覆盖完整数据"，
///   比"什么都不做"更糟 —— 所以宁可不给用。
pub fn load(text: &str) -> std::result::Result<Loaded, LoadError> {
    let mut data = SavedData::from_json(text).map_err(LoadError::Parse)?;
    if data.version > CURRENT_VERSION {
        return Err(LoadError::NewerVersion {
            found: data.version,
            supported: CURRENT_VERSION,
        });
    }
    let migration = migrate(&mut data);
    Ok(Loaded { data, migration })
}

/// 把旧版本的存档**就地升级**到 [`CURRENT_VERSION`]。
///
/// * 只做加法：给新字段填默认值（v1 → v2 是给 `scale` 填 [`DEFAULT_SCALE`]）；
/// * 幂等：同一份数据再调一次不会有变化（`changed == false`）；
/// * 比当前版本**新**的存档不改动，只给一条提示（未识别字段在保存时会丢）。
pub fn migrate(data: &mut SavedData) -> Migration {
    let from = data.version;
    let mut notes = Vec::new();

    if data.version < 2 {
        let mut patched = 0usize;
        for mark in data.dest_marks.iter_mut().flatten() {
            if !(mark.scale > 0.0 && mark.scale.is_finite()) {
                mark.scale = DEFAULT_SCALE;
                patched += 1;
            }
        }
        for item in data.custom_images.iter_mut() {
            if !(item.scale > 0.0 && item.scale.is_finite()) {
                item.scale = DEFAULT_SCALE;
                patched += 1;
            }
        }
        notes.push(format!(
            "v1 → v2：{patched} 处截图没有记录分辨率，按 100% 处理（保存后写回 v{CURRENT_VERSION}）"
        ));
        data.version = 2;
    }

    Migration {
        from,
        to: data.version,
        changed: from != data.version,
        notes,
    }
}

/// 与 PDF 同目录同名、追加 `.json` 的存档路径。
pub fn sidecar_path(pdf: &std::path::Path) -> std::path::PathBuf {
    let mut name = pdf.as_os_str().to_owned();
    name.push(".json");
    std::path::PathBuf::from(name)
}

// ---------- 写 ----------

fn num(value: f32) -> String {
    let mut text = format!("{value:.3}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text.is_empty() || text == "-" {
        text.push('0');
    }
    text
}

fn write_point(out: &mut String, key: &str, point: Point) {
    let _ = write!(out, "\"{key}\": [{}, {}]", num(point.x), num(point.y));
}

fn write_rect(out: &mut String, key: &str, rect: Rect) {
    let _ = write!(
        out,
        "\"{key}\": [{}, {}, {}, {}]",
        num(rect.x),
        num(rect.y),
        num(rect.width),
        num(rect.height)
    );
}

fn write_destination(out: &mut String, destination: &Destination) {
    match destination {
        Destination::Page { page, offset } => {
            let _ = write!(out, "{{\"kind\": \"page\", \"page\": {}", page.get());
            if let Some(offset) = offset {
                let _ = write!(out, ", \"offset\": [{}, {}]", num(offset.x), num(offset.y));
            }
            out.push('}');
        }
        Destination::Uri(uri) => {
            let _ = write!(out, "{{\"kind\": \"uri\", \"uri\": {}}}", json_string(uri));
        }
    }
}

fn write_customs(out: &mut String, key: &str, items: &[SavedCustom]) {
    let _ = write!(out, "  \"{key}\": [");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    {\"page\": ");
        let _ = write!(out, "{}", item.page.get());
        out.push_str(", ");
        write_point(out, "point", item.point);
        out.push_str(", ");
        write_rect(out, "rect", item.rect);
        let _ = write!(
            out,
            ", \"body\": [{}, {}], \"scale\": {}, \"title\": {}",
            num(item.body[0]),
            num(item.body[1]),
            num(item.scale),
            json_string(&item.title)
        );
        if let Some(text) = &item.text {
            let _ = write!(out, ", \"text\": {}", json_string(text));
        }
        out.push('}');
    }
    out.push(']');
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

// ---------- 读 ----------

fn parse_link(value: &Json) -> Parsed<Link> {
    let object = value.as_object().ok_or("link 不是对象")?;
    let rect = parse_rect(object.field("rect").ok_or("link 缺 rect")?)?;
    let dest = object
        .field("dest")
        .ok_or("link 缺 dest")?
        .as_object()
        .ok_or("dest 不是对象")?;
    let kind = dest
        .field("kind")
        .and_then(|v| v.as_str())
        .ok_or("dest 缺 kind")?;
    let destination = match kind {
        "page" => {
            let page = dest
                .field("page")
                .and_then(|v| v.as_number())
                .ok_or("dest 缺 page")? as u32;
            let offset = dest.field("offset").map(parse_point).transpose()?;
            Destination::Page {
                page: PageIndex::new(page),
                offset,
            }
        }
        "uri" => {
            let uri = dest
                .field("uri")
                .and_then(|v| v.as_str())
                .ok_or("dest 缺 uri")?;
            Destination::Uri(uri.to_string())
        }
        other => return Err(format!("未知的 dest kind: {other}")),
    };
    Ok(Link { rect, destination })
}

fn parse_mark(value: &Json) -> Parsed<SavedMark> {
    let object = value.as_object().ok_or("mark 不是对象")?;
    Ok(SavedMark {
        point: parse_point(object.field("point").ok_or("mark 缺 point")?)?,
        rect: parse_rect(object.field("rect").ok_or("mark 缺 rect")?)?,
        body: parse_pair(object.field("body").ok_or("mark 缺 body")?)?,
        scale: parse_scale(object),
    })
}

/// `scale` 是 v2 新增字段：缺省（旧存档）按 [`DEFAULT_SCALE`]，非法值同样退回默认。
fn parse_scale(object: &[(String, Json)]) -> f32 {
    object
        .field("scale")
        .and_then(|value| value.as_number())
        .filter(|value| *value > 0.0 && value.is_finite())
        .unwrap_or(DEFAULT_SCALE)
}

fn parse_custom(value: &Json) -> Parsed<SavedCustom> {
    let object = value.as_object().ok_or("custom 不是对象")?;
    let page = object
        .field("page")
        .and_then(|v| v.as_number())
        .ok_or("custom 缺 page")? as u32;
    Ok(SavedCustom {
        page: PageIndex::new(page),
        point: parse_point(object.field("point").ok_or("custom 缺 point")?)?,
        rect: parse_rect(object.field("rect").ok_or("custom 缺 rect")?)?,
        body: parse_pair(object.field("body").ok_or("custom 缺 body")?)?,
        title: object
            .field("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        text: object
            .field("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        scale: parse_scale(object),
    })
}

fn parse_point(value: &Json) -> Parsed<Point> {
    let pair = parse_pair(value)?;
    Ok(Point::new(pair[0], pair[1]))
}

fn parse_rect(value: &Json) -> Parsed<Rect> {
    let items = value.as_array().ok_or("矩形不是数组")?;
    if items.len() != 4 {
        return Err("矩形需要 4 个数字".to_string());
    }
    let mut values = [0.0f32; 4];
    for (slot, item) in values.iter_mut().zip(items) {
        *slot = item.as_number().ok_or("矩形元素不是数字")?;
    }
    Ok(Rect::new(values[0], values[1], values[2], values[3]))
}

fn parse_pair(value: &Json) -> Parsed<[f32; 2]> {
    let items = value.as_array().ok_or("不是二元数组")?;
    if items.len() != 2 {
        return Err("需要 2 个数字".to_string());
    }
    Ok([
        items[0].as_number().ok_or("不是数字")?,
        items[1].as_number().ok_or("不是数字")?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SavedData {
        SavedData {
            version: CURRENT_VERSION,
            links: vec![
                vec![
                    Link {
                        rect: Rect::new(10.0, 20.0, 30.0, 8.5),
                        destination: Destination::Page {
                            page: PageIndex::new(3),
                            offset: Some(Point::new(11.5, 42.25)),
                        },
                    },
                    Link {
                        rect: Rect::new(0.0, 0.0, 5.0, 5.0),
                        destination: Destination::Uri("https://example.com/a?b=c".into()),
                    },
                ],
                Vec::new(),
            ],
            dest_marks: vec![
                vec![SavedMark {
                    point: Point::new(100.0, 200.0),
                    rect: Rect::new(40.0, 170.0, 120.0, 60.0),
                    body: [120.0, 60.0],
                    scale: 8.0,
                }],
                Vec::new(),
            ],
            custom_images: vec![SavedCustom {
                page: PageIndex::new(1),
                point: Point::new(50.0, 60.0),
                rect: Rect::new(10.0, 20.0, 30.0, 40.0),
                body: [30.0, 40.0],
                title: "图 \"一\"".into(),
                text: None,
                scale: 2.5,
            }],
            custom_texts: vec![SavedCustom {
                page: PageIndex::new(2),
                point: Point::new(5.0, 6.0),
                rect: Rect::new(5.0, 6.0, 0.0, 0.0),
                body: [300.0, 170.0],
                title: "题\n目".into(),
                text: Some("内容\\带转义".into()),
                scale: DEFAULT_SCALE,
            }],
        }
    }

    /// 往返：写出去再读回来，必须完全一致（含转义与中文）。
    #[test]
    fn round_trip_preserves_everything() {
        let data = sample();
        let text = data.to_json();
        let back = SavedData::from_json(&text).expect("应能解析自己写出的 JSON");
        assert_eq!(back, data);
    }

    /// 存档路径：原文件名后追加 `.json`。
    #[test]
    fn sidecar_path_appends_json() {
        let path = sidecar_path(std::path::Path::new("aa/bb.pdf"));
        assert_eq!(path, std::path::PathBuf::from("aa/bb.pdf.json"));
    }

    /// v1（没有 `version`、没有 `scale`）的存档必须能读，并按 100% 处理分辨率。
    #[test]
    fn v1_sidecar_is_read_with_default_scale_and_migrated() {
        // 手写一份 v1：没有 version 字段、被引用点/自定义图没有 scale
        let text = r#"{
  "links": [],
  "dest_marks": [
    [{"point": [100, 200], "rect": [40, 170, 120, 60], "body": [120, 60]}]
  ],
  "custom_images": [
    {"page": 1, "point": [50, 60], "rect": [10, 20, 30, 40], "body": [30, 40], "title": "旧图"}
  ],
  "custom_texts": [
    {"page": 2, "point": [5, 6], "rect": [5, 6, 0, 0], "body": [300, 170], "title": "旧文字", "text": "x"}
  ]
}
"#;
        let mut data = SavedData::from_json(text).expect("v1 应能解析");
        assert_eq!(data.version, 1, "缺 version 字段按 v1");
        assert_eq!(data.dest_marks[0][0].scale, DEFAULT_SCALE);
        assert_eq!(data.custom_images[0].scale, DEFAULT_SCALE);

        let migration = migrate(&mut data);
        assert_eq!((migration.from, migration.to), (1, CURRENT_VERSION));
        assert!(migration.changed, "旧版本必须让调用方置脏");
        assert!(migration.notes.iter().any(|note| note.contains("100%")));
        assert_eq!(data.version, CURRENT_VERSION);
        assert_eq!(data.dest_marks[0][0].scale, DEFAULT_SCALE);
        assert_eq!(data.custom_images[0].scale, DEFAULT_SCALE);

        // 幂等：再迁移一次什么都不变
        let again = migrate(&mut data);
        assert!(!again.changed);
        assert_eq!(again.from, CURRENT_VERSION);

        // 保存后写回当前版本，往返仍然一致
        let text = data.to_json();
        assert!(text.contains(&format!("\"version\": {CURRENT_VERSION}")));
        let back = SavedData::from_json(&text).expect("v2 应能解析");
        assert_eq!(back, data);
    }

    /// v2 的 `scale` 必须原样往返，不能被迁移改写。
    #[test]
    fn v2_scale_survives_a_round_trip_and_migration() {
        let data = sample();
        let mut back = SavedData::from_json(&data.to_json()).expect("应能解析");
        assert_eq!(back.version, CURRENT_VERSION);
        assert_eq!(back.dest_marks[0][0].scale, 8.0);
        assert_eq!(back.custom_images[0].scale, 2.5);
        let migration = migrate(&mut back);
        assert!(!migration.changed, "当前版本不需要迁移");
        assert_eq!(back, data);
    }

    /// 比当前版本更新的存档：**拒绝读取**（禁止向前兼容，D-107）。
    ///
    /// 关键点：`from_json` 本身照常解析（忠实反映文件内容），但 `load` 的版本闸门必须拦住它 ——
    /// 旧程序读一半再保存，等于用残缺数据覆盖完整数据。
    #[test]
    fn newer_sidecar_is_refused() {
        let text = r#"{"version": 99, "custom_images": [{"page": 0, "point": [0, 0], "rect": [0, 0, 10, 10], "body": [10, 10], "title": "x", "scale": 3.0}]}"#;
        let parsed = SavedData::from_json(text).expect("解析本身应成功（忠实反映文件）");
        assert_eq!(parsed.version, 99);

        match load(text) {
            Err(LoadError::NewerVersion { found, supported }) => {
                assert_eq!((found, supported), (99, CURRENT_VERSION));
            }
            other => panic!("应当拒绝读取，实际：{other:?}"),
        }
        let message = LoadError::NewerVersion {
            found: 99,
            supported: CURRENT_VERSION,
        }
        .to_string();
        assert!(message.contains("拒绝"), "{message}");
    }

    /// 版本恰好等于当前版本：正常载入，且不算迁移。
    #[test]
    fn current_version_loads_without_migration() {
        let data = SavedData::default();
        let loaded = load(&data.to_json()).expect("当前版本应能载入");
        assert_eq!(loaded.data.version, CURRENT_VERSION);
        assert!(!loaded.migration.changed);
        assert!(loaded.migration.notes.is_empty());
    }

    /// 缺 `version` 的老文件：照常读，升级并置脏（向后兼容）。
    #[test]
    fn missing_version_is_treated_as_v1_and_migrated() {
        let text = r#"{"custom_images": [{"page": 0, "point": [0, 0], "rect": [0, 0, 10, 10], "body": [10, 10], "title": "old"}]}"#;
        let loaded = load(text).expect("无版本号的文件应能载入");
        assert_eq!(loaded.data.version, CURRENT_VERSION);
        assert!(loaded.migration.changed, "应置脏，让下次保存写回当前版本");
        assert_eq!(loaded.data.custom_images[0].scale, DEFAULT_SCALE);
    }

    /// 非法/缺失的 `scale` 回退到 100%（不丢弃条目）。
    #[test]
    fn invalid_scale_falls_back_to_the_default() {
        let data = SavedData::from_json(
            r#"{"version": 2, "custom_images": [
                {"page": 0, "point": [0, 0], "rect": [0, 0, 10, 10], "body": [10, 10], "title": "a", "scale": 0},
                {"page": 0, "point": [0, 0], "rect": [0, 0, 10, 10], "body": [10, 10], "title": "b"}
            ]}"#,
        )
        .expect("应能解析");
        assert_eq!(data.custom_images[0].scale, DEFAULT_SCALE);
        assert_eq!(data.custom_images[1].scale, DEFAULT_SCALE);
    }

    /// 损坏的输入返回 Err 而不是 panic。
    #[test]
    fn malformed_input_is_an_error() {
        assert!(SavedData::from_json("{").is_err());
        assert!(SavedData::from_json("[]").is_err(), "根必须是对象");
        assert!(SavedData::from_json("{\"links\": 3}").is_err());
        assert!(SavedData::from_json("{}").is_ok(), "空对象是合法的最小存档");
    }
}

// ====================================================================== 标注图层（D-118）
//
// 与 `<pdf>.json` **分开**存放：标注几何是"应用层装饰"，注释正文仍在 `<pdf>.json` 的
// `custom_texts` 里（哨兵标题 `#annot:<id>` 引用）。这样删掉本文件就等于清空所有标注，
// 也不会牵动主存档的版本迁移。

/// 标注图层的格式版本（与 `SavedData` 独立）。
///
/// * **v1**：只有文本类标注（高亮 / 下划线 / 删除线），几何是 `quads`。
/// * **v2**（S4）：增加墨迹（`points` + `width`）。v1 文件仍可读；v1 的读取端
///   会把墨迹条目当成"未知种类"静默丢掉，再保存就真丢了 —— 所以必须升版本，
///   让旧程序按既有规则**拒读且不覆盖**。
pub const ANNOT_VERSION: u32 = 2;

/// `<pdf>.annot.json` 里的一条标注。
#[derive(Debug, Clone, PartialEq)]
pub struct SavedAnnotation {
    /// 标注 id（与正文的哨兵标题 `#annot:<id>` 对应）。
    pub id: u64,
    /// 种类：`highlight` / `underline` / `strike_out` / `ink`。
    pub kind: String,
    /// 每行的矩形（**未旋转页面空间**）；墨迹为空。
    pub quads: Vec<pdfcore::prelude::Rect>,
    /// 墨迹的折线顶点（**未旋转页面空间**）；文本类标注为空。
    pub points: Vec<pdfcore::prelude::Point>,
    /// 墨迹的线宽（页面空间）；文本类标注为 `None`。
    pub width: Option<f32>,
    /// RGB。
    pub color: [u8; 3],
    /// 不透明度。
    pub opacity: f32,
    /// 正文引用（哨兵标题）；没有正文时为 `None`。
    pub body_title: Option<String>,
}

/// 整份标注图层。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SavedOverlay {
    /// 格式版本。
    pub version: u32,
    /// 逐页的标注（下标 = 页号）。
    pub pages: Vec<Vec<SavedAnnotation>>,
}

impl SavedOverlay {
    /// 空图层（当前版本）。
    pub fn new() -> Self {
        Self {
            version: ANNOT_VERSION,
            pages: Vec::new(),
        }
    }

    /// 标注总数。
    pub fn len(&self) -> usize {
        self.pages.iter().map(Vec::len).sum()
    }

    /// 是否没有任何标注。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 序列化成 JSON（逐页数组，便于人工检查与 diff）。
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n");
        out.push_str(&format!("  \"version\": {},\n", self.version));
        out.push_str("  \"pages\": [\n");
        for (page, annotations) in self.pages.iter().enumerate() {
            out.push_str("    [");
            for (index, annotation) in annotations.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n     ");
                }
                out.push_str(&format!(
                    "{{\"id\":{}, \"kind\":\"{}\", \"color\":[{},{},{}], \"opacity\":{:.2}, ",
                    annotation.id,
                    json_string(&annotation.kind).trim_matches('"'),
                    annotation.color[0],
                    annotation.color[1],
                    annotation.color[2],
                    annotation.opacity
                ));
                if annotation.points.is_empty() {
                    out.push_str("\"quads\":[");
                    for (quad_index, quad) in annotation.quads.iter().enumerate() {
                        if quad_index > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&format!(
                            "[{:.2},{:.2},{:.2},{:.2}]",
                            quad.x, quad.y, quad.width, quad.height
                        ));
                    }
                    out.push(']');
                } else {
                    out.push_str("\"points\":[");
                    for (point_index, point) in annotation.points.iter().enumerate() {
                        if point_index > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&format!("[{:.2},{:.2}]", point.x, point.y));
                    }
                    out.push(']');
                    out.push_str(&format!(
                        ", \"width\":{:.2}",
                        annotation.width.unwrap_or(2.0)
                    ));
                }
                if let Some(title) = &annotation.body_title {
                    out.push_str(&format!(", \"body\": {}", json_string(title)));
                }
                out.push('}');
            }
            out.push(']');
            if page + 1 < self.pages.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n}\n");
        out
    }

    /// 解析（版本高于 [`ANNOT_VERSION`] 时返回 [`LoadError::NewerVersion`]）。
    pub fn from_json(text: &str) -> std::result::Result<Self, LoadError> {
        let parsed = parse_overlay(text)?;
        if parsed.version > ANNOT_VERSION {
            return Err(LoadError::NewerVersion {
                found: parsed.version,
                supported: ANNOT_VERSION,
            });
        }
        Ok(parsed)
    }
}

/// 标注正文的**哨兵标题**：`#annot:<id>`。
///
/// 正文本身存在主存档的 `custom_texts` 里（不新增字段），靠这个标题与标注对应。
/// 用户可见（不隐藏）；保存时做唯一性检查。
pub fn annotation_body_title(id: pdfcore::annotation::AnnotationId) -> String {
    format!("#annot:{}", id.get())
}

/// `<pdf>.annot.json` 的路径。
pub fn annot_path(pdf: &std::path::Path) -> std::path::PathBuf {
    let mut name = pdf.as_os_str().to_owned();
    name.push(".annot.json");
    std::path::PathBuf::from(name)
}

/// 原子写：先写同目录的临时文件，再 `rename` 覆盖。
///
/// 存档变成"每次 Ctrl+S 都写"的高频操作之后，`fs::write` 中途崩溃会留下截断的 JSON
/// （下一次打开就解析失败）。`rename` 在同一文件系统上是原子的，因此要么是旧内容、
/// 要么是完整的新内容。
pub fn write_atomic(target: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let mut temporary = target.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    std::fs::write(&temporary, contents)?;
    std::fs::rename(&temporary, target)
}

/// 极简解析：只认本文件自己的格式（字段名固定、顺序不限、忽略未知字段）。
fn parse_overlay(text: &str) -> std::result::Result<SavedOverlay, LoadError> {
    use crate::json::Json;
    let root = Json::parse(text).map_err(|error| LoadError::Parse(format!("解析失败：{error}")))?;
    let Json::Object(root) = root else {
        return Err(LoadError::Parse("标注文件顶层不是对象".to_string()));
    };
    let root = root.as_slice();
    let version = root
        .field("version")
        .and_then(|value| value.as_number())
        .map(|value| value.round().max(1.0) as u32)
        .unwrap_or(1);
    let mut pages = Vec::new();
    let page_items = root
        .field("pages")
        .and_then(|value| value.as_array())
        .unwrap_or(&[]);
    for (page, item) in page_items.iter().enumerate() {
        let mut annotations = Vec::new();
        for annotation in item.as_array().unwrap_or(&[]) {
            let Json::Object(annotation) = annotation else {
                continue;
            };
            let annotation = annotation.as_slice();
            let id = annotation
                .field("id")
                .and_then(|value| value.as_number())
                .unwrap_or(0.0) as u64;
            let kind = annotation
                .field("kind")
                .and_then(|value| value.as_str())
                .unwrap_or("highlight")
                .to_string();
            let mut color = [255u8, 214, 0];
            if let Some(channels) = annotation.field("color").and_then(|value| value.as_array()) {
                for (index, channel) in channels.iter().take(3).enumerate() {
                    if let Some(number) = channel.as_number() {
                        color[index] = number.clamp(0.0, 255.0) as u8;
                    }
                }
            }
            let opacity = annotation
                .field("opacity")
                .and_then(|value| value.as_number())
                .unwrap_or(0.35)
                .clamp(0.0, 1.0);
            let mut quads = Vec::new();
            for quad in annotation
                .field("quads")
                .and_then(|value| value.as_array())
                .unwrap_or(&[])
            {
                let numbers: Vec<f32> = quad
                    .as_array()
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|value| value.as_number())
                    .collect();
                if numbers.len() == 4 {
                    quads.push(pdfcore::prelude::Rect::new(
                        numbers[0], numbers[1], numbers[2], numbers[3],
                    ));
                }
            }
            let mut points = Vec::new();
            for point in annotation
                .field("points")
                .and_then(|value| value.as_array())
                .unwrap_or(&[])
            {
                let numbers: Vec<f32> = point
                    .as_array()
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|value| value.as_number())
                    .collect();
                if numbers.len() == 2 {
                    points.push(pdfcore::prelude::Point::new(numbers[0], numbers[1]));
                }
            }
            let width = annotation
                .field("width")
                .and_then(|value| value.as_number());
            if quads.is_empty() && points.is_empty() {
                continue; // 几何缺失的条目直接跳过（不因为一条坏数据整份加载失败）
            }
            let body_title = annotation
                .field("body")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            annotations.push(SavedAnnotation {
                id,
                kind,
                quads,
                points,
                width,
                color,
                opacity,
                body_title,
            });
        }
        pages.push(annotations);
        let _ = page;
    }
    Ok(SavedOverlay { version, pages })
}

/// 标注种类 → 存档里的字符串。
pub fn annotation_kind_name(kind: &pdfcore::annotation::AnnotationKind) -> &'static str {
    use pdfcore::annotation::AnnotationKind;
    match kind {
        AnnotationKind::Highlight { .. } => "highlight",
        AnnotationKind::Underline { .. } => "underline",
        AnnotationKind::StrikeOut { .. } => "strike_out",
        AnnotationKind::Ink { .. } => "ink",
    }
}

/// 存档里的一条 → 标注种类（未知种类按高亮处理，避免一条坏数据毁掉整份图层）。
///
/// **几何是否有效不在这里判断**：交给 `Annotation::new` 的不变量检查，
/// 这样"缺几何 / 单点墨迹 / 零宽矩形"的跳过规则只有一处。
pub fn annotation_kind_from_saved(saved: &SavedAnnotation) -> pdfcore::annotation::AnnotationKind {
    use pdfcore::annotation::AnnotationKind;
    if saved.kind == "ink" {
        return AnnotationKind::Ink {
            points: saved.points.clone(),
            width: saved.width.unwrap_or(2.0),
        };
    }
    match saved.kind.as_str() {
        "underline" => AnnotationKind::Underline {
            quads: saved.quads.clone(),
        },
        "strike_out" => AnnotationKind::StrikeOut {
            quads: saved.quads.clone(),
        },
        _ => AnnotationKind::Highlight {
            quads: saved.quads.clone(),
        },
    }
}

/// 把内存里的标注写成图层文件结构。
pub fn overlay_from_store(store: &pdfcore::annotation::AnnotationStore) -> SavedOverlay {
    let mut pages: Vec<Vec<SavedAnnotation>> = Vec::new();
    for annotation in store.iter() {
        let page = annotation.page.get() as usize;
        if pages.len() <= page {
            pages.resize_with(page + 1, Vec::new);
        }
        let (quads, points, width) = match &annotation.kind {
            pdfcore::annotation::AnnotationKind::Ink { points, width } => {
                (Vec::new(), points.clone(), Some(*width))
            }
            _ => (annotation.kind.quads().to_vec(), Vec::new(), None),
        };
        pages[page].push(SavedAnnotation {
            id: annotation.id.get(),
            kind: annotation_kind_name(&annotation.kind).to_string(),
            quads,
            points,
            width,
            color: annotation.color,
            opacity: annotation.opacity,
            body_title: annotation.body_title.clone(),
        });
    }
    SavedOverlay {
        version: ANNOT_VERSION,
        pages,
    }
}

/// 从图层文件结构恢复标注（保留 id，撤销栈因此仍能对上）。
pub fn store_from_overlay(overlay: &SavedOverlay) -> pdfcore::annotation::AnnotationStore {
    let mut store = pdfcore::annotation::AnnotationStore::new();
    for (page, annotations) in overlay.pages.iter().enumerate() {
        for saved in annotations {
            let kind = annotation_kind_from_saved(saved);
            let mut annotation = match pdfcore::annotation::Annotation::new(
                pdfcore::prelude::PageIndex::new(page as u32),
                kind,
            ) {
                Some(annotation) => annotation,
                None => continue,
            };
            annotation.id = pdfcore::annotation::AnnotationId(saved.id);
            annotation.color = saved.color;
            annotation.opacity = saved.opacity;
            annotation.body_title = saved.body_title.clone();
            store.insert(annotation);
        }
    }
    store
}

#[cfg(test)]
mod annot_tests {
    use super::*;
    use pdfcore::annotation::{AnnotationKind, AnnotationStore};
    use pdfcore::prelude::{PageIndex, Point, Rect};

    fn store_with_one() -> AnnotationStore {
        let mut store = AnnotationStore::new();
        store
            .add(
                PageIndex::FIRST,
                AnnotationKind::Highlight {
                    quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
                },
            )
            .expect("有效几何");
        store
    }

    #[test]
    fn overlay_round_trips_through_json() {
        let mut store = store_with_one();
        let id = store.iter().next().expect("一条").id;
        store.set_body(id, Some("#annot:1".to_string()));

        let overlay = overlay_from_store(&store);
        let text = overlay.to_json();
        let parsed = SavedOverlay::from_json(&text).expect("应当能解析");
        assert_eq!(parsed, overlay, "解析结果必须与写出的结构一致");

        let restored = store_from_overlay(&parsed);
        let item = restored.iter().next().expect("恢复一条");
        assert_eq!(item.id, id, "id 必须保留（撤销栈要对得上）");
        assert_eq!(item.kind.quads()[0], Rect::new(10.0, 20.0, 30.0, 8.0));
        assert_eq!(item.body_title.as_deref(), Some("#annot:1"));
    }

    #[test]
    fn newer_overlay_version_is_refused() {
        let text = "{\"version\": 99, \"pages\": []}";
        match SavedOverlay::from_json(text) {
            Err(LoadError::NewerVersion { found, supported }) => {
                assert_eq!((found, supported), (99, ANNOT_VERSION));
            }
            other => panic!("应当拒绝更新的版本，实际 {other:?}"),
        }
    }

    #[test]
    fn entries_without_geometry_are_skipped_instead_of_failing() {
        let text = r#"{"version":1,"pages":[[
            {"id":1,"kind":"highlight","quads":[]},
            {"id":2,"kind":"underline","quads":[[1,2,3,4]],"color":[1,2,3],"opacity":1.0}
        ]]}"#;
        let overlay = SavedOverlay::from_json(text).expect("坏条目不应让整份失败");
        assert_eq!(overlay.len(), 1, "缺几何的条目应被跳过");
        assert_eq!(overlay.pages[0][0].id, 2);
    }

    #[test]
    fn ink_round_trips_through_json_with_points_and_width() {
        let mut store = AnnotationStore::new();
        store
            .add(
                PageIndex::FIRST,
                AnnotationKind::Ink {
                    points: vec![Point::new(10.0, 20.0), Point::new(60.0, 28.0)],
                    width: 3.5,
                },
            )
            .expect("有效墨迹");

        let overlay = overlay_from_store(&store);
        assert_eq!(overlay.version, ANNOT_VERSION, "新写的图层用当前版本");
        let text = overlay.to_json();
        assert!(text.contains("\"points\""), "墨迹必须写成 points：{text}");
        assert!(text.contains("\"width\":3.50"), "线宽要落盘：{text}");
        assert!(
            !text.contains("\"quads\""),
            "墨迹条目不该再写一个空的 quads：{text}"
        );

        let parsed = SavedOverlay::from_json(&text).expect("应当能解析");
        assert_eq!(parsed, overlay, "解析结果必须与写出的结构一致");

        let restored = store_from_overlay(&parsed);
        let item = restored.iter().next().expect("恢复一条");
        match &item.kind {
            AnnotationKind::Ink { points, width } => {
                assert_eq!(points.len(), 2, "顶点数必须保留");
                assert!((points[0].x - 10.0).abs() < 0.01);
                assert!((points[1].y - 28.0).abs() < 0.01);
                assert!((width - 3.5).abs() < 0.001, "线宽必须保留");
            }
            other => panic!("应当恢复成墨迹，实际 {other:?}"),
        }
        assert_eq!(
            item.bounds().map(|rect| rect.width),
            Some(53.5),
            "恢复后的包围盒仍按半线宽外扩"
        );
    }

    #[test]
    fn a_version_1_overlay_still_loads() {
        let text = r#"{"version":1,"pages":[[
            {"id":1,"kind":"highlight","quads":[[1,2,3,4]],"color":[1,2,3],"opacity":1.0}
        ]]}"#;
        let overlay = SavedOverlay::from_json(text).expect("v1 必须能读");
        assert_eq!(overlay.version, 1, "旧版本号原样保留");
        assert_eq!(
            store_from_overlay(&overlay).len(),
            1,
            "v1 的高亮必须无损升级成内存模型"
        );
    }

    #[test]
    fn ink_with_too_few_points_is_skipped_at_the_model_layer() {
        let text = r#"{"version":2,"pages":[[
            {"id":1,"kind":"ink","points":[[1,2]],"width":3}
        ]]}"#;
        let overlay = SavedOverlay::from_json(text).expect("坏条目不应让整份失败");
        assert_eq!(overlay.len(), 1, "解析层保留原始条目");
        assert!(
            store_from_overlay(&overlay).is_empty(),
            "单点墨迹在模型层被拒（不变量只有一处）"
        );
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("pdf-annot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let target = dir.join("a.annot.json");
        write_atomic(&target, "{\"version\":1,\"pages\":[]}").expect("第一次写");
        write_atomic(&target, "{\"version\":1,\"pages\":[[]]}").expect("覆盖写");
        let text = std::fs::read_to_string(&target).expect("读回");
        assert!(text.contains("[[]]"), "应当是新内容，实际 {text}");
        assert!(
            !dir.join("a.annot.json.tmp").exists(),
            "临时文件必须被 rename 掉"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn annot_path_sits_next_to_the_pdf() {
        let path = annot_path(std::path::Path::new("/tmp/book.pdf"));
        assert_eq!(path, std::path::PathBuf::from("/tmp/book.pdf.annot.json"));
    }
}
