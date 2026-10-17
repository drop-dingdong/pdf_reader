//! 自定义数据的序列化（JSON，无第三方依赖）。
//!
//! 只存**可重建图像所需的元数据**：图片像素一律不入档 ——
//! 被引用点与自定义图片 preview 都记录了"页号 + 页面空间矩形"，
//! 载入后按当前缩放重新合成即可（见 `PdfReaderApp::snapshot_for`）。
//!
//! 文件与 PDF 同目录同名，后缀追加 `.json`（`aa/bb.pdf` → `aa/bb.pdf.json`）。

use std::fmt::Write as _;

use pdfcore::prelude::*;

/// 本模块的错误类型（避开 pdfcore prelude 里的 `Result` 别名）。
type Parsed<T> = std::result::Result<T, String>;

/// 一条被引用点的可持久化数据（不含截图）。
#[derive(Debug, Clone, PartialEq)]
pub struct SavedMark {
    /// 页内坐标
    pub point: Point,
    /// 页面空间矩形
    pub rect: Rect,
    /// 浮窗正文尺寸（egui 单位）
    pub body: [f32; 2],
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
}

/// 整个存档。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedData {
    /// 每页的链接（含目的地的类型/页号/坐标/URI）
    pub links: Vec<Vec<Link>>,
    /// 每页的被引用点
    pub dest_marks: Vec<Vec<SavedMark>>,
    /// 自定义图片 preview
    pub custom_images: Vec<SavedCustom>,
    /// 自定义文字 preview
    pub custom_texts: Vec<SavedCustom>,
}

impl SavedData {
    /// 序列化为 JSON 文本。
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n  \"version\": 1,\n");

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
                let _ = write!(out, ", \"body\": [{}, {}]", num(mark.body[0]), num(mark.body[1]));
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
        let mut data = SavedData::default();
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
            ", \"body\": [{}, {}], \"title\": {}",
            num(item.body[0]),
            num(item.body[1]),
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
    let dest = object.field("dest").ok_or("link 缺 dest")?.as_object().ok_or("dest 不是对象")?;
    let kind = dest.field("kind").and_then(|v| v.as_str()).ok_or("dest 缺 kind")?;
    let destination = match kind {
        "page" => {
            let page = dest
                .field("page")
                .and_then(|v| v.as_number())
                .ok_or("dest 缺 page")? as u32;
            let offset = dest.field("offset").map(parse_point).transpose()?;
            Destination::Page { page: PageIndex::new(page), offset }
        }
        "uri" => {
            let uri = dest.field("uri").and_then(|v| v.as_str()).ok_or("dest 缺 uri")?;
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
    })
}

fn parse_custom(value: &Json) -> Parsed<SavedCustom> {
    let object = value.as_object().ok_or("custom 不是对象")?;
    let page = object.field("page").and_then(|v| v.as_number()).ok_or("custom 缺 page")? as u32;
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
        text: object.field("text").and_then(|v| v.as_str()).map(|s| s.to_string()),
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

// ---------- 极简 JSON（只覆盖本模块用到的子集） ----------

#[derive(Debug, Clone, PartialEq)]
enum Json {
    /// 空值
    Null,
    /// 布尔
    Bool(bool),
    /// 数字
    Number(f32),
    /// 字符串
    Str(String),
    /// 数组
    Array(Vec<Json>),
    /// 对象（保持键顺序）
    Object(Vec<(String, Json)>),
}

impl Json {
    fn parse(text: &str) -> Parsed<Json> {
        let mut parser = Parser { bytes: text.as_bytes(), position: 0 };
        let value = parser.value()?;
        parser.skip_whitespace();
        if parser.position != parser.bytes.len() {
            return Err("末尾有多余内容".to_string());
        }
        Ok(value)
    }

    fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(items) => Some(items),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

impl<'a> Json {
    fn get_from<'b>(items: &'b [(String, Json)], key: &str) -> Option<&'b Json> {
        items.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }
}

trait JsonLookup {
    fn field(&self, key: &str) -> Option<&Json>;
}

impl JsonLookup for Vec<(String, Json)> {
    fn field(&self, key: &str) -> Option<&Json> {
        Json::get_from(self, key)
    }
}

impl JsonLookup for [(String, Json)] {
    fn field(&self, key: &str) -> Option<&Json> {
        Json::get_from(self, key)
    }
}

impl Json {
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<f32> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }
}

struct Parser<'a> {
    /// 输入字节
    bytes: &'a [u8],
    /// 当前位置
    position: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.bytes.get(self.position) {
            if byte.is_ascii_whitespace() {
                self.position += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn value(&mut self) -> Parsed<Json> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') | Some(b'f') | Some(b'n') => self.literal(),
            Some(_) => self.number(),
            None => Err("意外的结尾".to_string()),
        }
    }

    fn object(&mut self) -> Parsed<Json> {
        self.position += 1; // '{'
        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Json::Object(items));
                }
                Some(b'"') => {
                    let key = self.string()?;
                    self.skip_whitespace();
                    if self.peek() != Some(b':') {
                        return Err("对象缺少冒号".to_string());
                    }
                    self.position += 1;
                    let value = self.value()?;
                    items.push((key, value));
                    self.skip_whitespace();
                    match self.peek() {
                        Some(b',') => self.position += 1,
                        Some(b'}') => {}
                        _ => return Err("对象缺少逗号或右括号".to_string()),
                    }
                }
                _ => return Err("对象键不合法".to_string()),
            }
        }
    }

    fn array(&mut self) -> Parsed<Json> {
        self.position += 1; // '['
        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b']') {
                self.position += 1;
                return Ok(Json::Array(items));
            }
            items.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {}
                _ => return Err("数组缺少逗号或右括号".to_string()),
            }
        }
    }

    fn string(&mut self) -> Parsed<String> {
        self.position += 1; // '"'
        let mut out = String::new();
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.position += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            // 只处理 BMP 之外的简单情形：跳过 4 位十六进制
                            self.position += 1;
                            let start = self.position;
                            let end = (start + 4).min(self.bytes.len());
                            let hex = std::str::from_utf8(&self.bytes[start..end]).unwrap_or("");
                            let code = u32::from_str_radix(hex, 16).unwrap_or(0xFFFD);
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            self.position = end;
                            continue;
                        }
                        _ => return Err("非法转义".to_string()),
                    }
                    self.position += 1;
                }
                Some(_) => {
                    let rest = &self.bytes[self.position..];
                    let text = std::str::from_utf8(rest).map_err(|e| e.to_string())?;
                    let ch = text.chars().next().ok_or("字符串结尾")?;
                    out.push(ch);
                    self.position += ch.len_utf8();
                }
                None => return Err("字符串未闭合".to_string()),
            }
        }
    }

    fn literal(&mut self) -> Parsed<Json> {
        for (text, value) in [("true", Json::Bool(true)), ("false", Json::Bool(false)), ("null", Json::Null)] {
            if self.bytes[self.position..].starts_with(text.as_bytes()) {
                self.position += text.len();
                return Ok(value);
            }
        }
        Err("非法字面量".to_string())
    }

    fn number(&mut self) -> Parsed<Json> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() || byte == b'.' || byte == b'e' || byte == b'E' || byte == b'+' || byte == b'-' {
                self.position += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).map_err(|e| e.to_string())?;
        text.parse::<f32>().map(Json::Number).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SavedData {
        SavedData {
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
            }],
            custom_texts: vec![SavedCustom {
                page: PageIndex::new(2),
                point: Point::new(5.0, 6.0),
                rect: Rect::new(5.0, 6.0, 0.0, 0.0),
                body: [300.0, 170.0],
                title: "题\n目".into(),
                text: Some("内容\\带转义".into()),
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

    /// 损坏的输入返回 Err 而不是 panic。
    #[test]
    fn malformed_input_is_an_error() {
        assert!(SavedData::from_json("{").is_err());
        assert!(SavedData::from_json("[]").is_err(), "根必须是对象");
        assert!(SavedData::from_json("{\"links\": 3}").is_err());
        assert!(SavedData::from_json("{}").is_ok(), "空对象是合法的最小存档");
    }
}
