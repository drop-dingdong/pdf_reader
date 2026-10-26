//! 极简 JSON（只覆盖本项目用到的子集）。
//!
//! 两个用途：持久化自定义数据（`persist`）与回放脚本/日志（`replay`）。
//! 之所以不引第三方 crate：`app` 只需要"读几个对象/数组/数字/字符串"，
//! 手写一份 200 行的解析器比引 `serde_json` 更省（也避免把依赖带进发布体积）。

/// 本模块的错误类型（避开 pdfcore prelude 里的 `Result` 别名）。
pub(crate) type Parsed<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
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
    pub(crate) fn parse(text: &str) -> Parsed<Json> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            position: 0,
        };
        let value = parser.value()?;
        parser.skip_whitespace();
        if parser.position != parser.bytes.len() {
            return Err("末尾有多余内容".to_string());
        }
        Ok(value)
    }

    pub(crate) fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(items) => Some(items),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

impl Json {
    fn get_from<'b>(items: &'b [(String, Json)], key: &str) -> Option<&'b Json> {
        items
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
}

pub(crate) trait JsonLookup {
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
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn as_number(&self) -> Option<f32> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
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
        for (text, value) in [
            ("true", Json::Bool(true)),
            ("false", Json::Bool(false)),
            ("null", Json::Null),
        ] {
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
            if byte.is_ascii_digit()
                || byte == b'.'
                || byte == b'e'
                || byte == b'E'
                || byte == b'+'
                || byte == b'-'
            {
                self.position += 1;
            } else {
                break;
            }
        }
        let text =
            std::str::from_utf8(&self.bytes[start..self.position]).map_err(|e| e.to_string())?;
        text.parse::<f32>()
            .map(Json::Number)
            .map_err(|e| e.to_string())
    }
}
