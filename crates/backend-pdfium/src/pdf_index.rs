//! P2 spike：从 PDF 字节里解析页面顺序与每页注释的对象号 `(num, gen)`。
//!
//! 只做“定位”：
//! * 扫直接对象 + ObjStm（FlateDecode）里的对象；
//! * 从 trailer `/Root` 沿 `/Pages` → `/Kids` 走页面树，得到页序；
//! * 读每页 `/Annots`，能拿到的引用返回 `Some((num, gen))`，内联字典返回 `None`。
//!
//! 不依赖 PDFium；解析失败返回错误，调用方决定是否退回“无对象号”。

use std::collections::HashMap;

use flate2::read::ZlibDecoder;
use std::io::Read;

/// 注释的对象号 + generation。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PdfObjectRef {
    /// 对象号。
    pub num: u32,
    /// generation。
    pub gen: u16,
}

/// 一个页对象（用于删除时重写 `/Annots`）。
#[derive(Debug, Clone)]
pub(crate) struct PageObject {
    pub reference: PdfObjectRef,
    pub dict: Vec<u8>,
    pub annots: Vec<PageAnnot>,
}

/// 页面 `/Annots` 里的一条（包括 Popup 等不支持的子类型）。
#[derive(Debug, Clone, Copy)]
pub struct PageAnnot {
    /// 独立对象引用；内联字典为 `None`。
    pub reference: Option<PdfObjectRef>,
    /// PDF 子类型编号（Highlight=9、Underline=10、StrikeOut=12、Ink=15、Popup=16）；
    /// 未知/无 `/Subtype` 为 `None`。
    pub subtype: Option<u32>,
    /// `/Popup` 子对象引用。
    pub popup: Option<PdfObjectRef>,
    /// Hidden/NoView 等不显示标志。
    pub hidden: bool,
}

struct RawObject {
    offset: usize,
    dict: Vec<u8>,
    stream: Option<Vec<u8>>,
}

/// 每页的 `/Annots` 条目：`Some(ref)` 是独立对象，`None` 是内联字典。
pub fn page_annotation_refs(pdf: &[u8]) -> Result<Vec<Vec<Option<PdfObjectRef>>>, String> {
    Ok(page_annotations(pdf)?
        .into_iter()
        .map(|page| page.into_iter().map(|entry| entry.reference).collect())
        .collect())
}

/// 每页的 `/Annots` 条目（含 subtype/Popup/隐藏标志）。
pub fn page_annotations(pdf: &[u8]) -> Result<Vec<Vec<PageAnnot>>, String> {
    Ok(page_objects(pdf)?
        .into_iter()
        .map(|page| page.annots)
        .collect())
}

/// 页对象（按页序），供删除时复制页 dict。
pub(crate) fn page_objects(pdf: &[u8]) -> Result<Vec<PageObject>, String> {
    let mut objects: HashMap<(u32, u16), RawObject> = HashMap::new();
    let mut arrays: HashMap<(u32, u16), Vec<u8>> = HashMap::new();
    scan_objects(pdf, &mut objects, &mut arrays);
    scan_object_streams(pdf, &mut objects, &mut arrays);
    if objects.is_empty() {
        return Err("没有解析到任何对象".to_string());
    }
    let root = last_ref_after(pdf, b"/Root").ok_or_else(|| "找不到 /Root".to_string())?;
    let pages_ref =
        get_ref(&objects, root, b"/Pages").ok_or_else(|| "找不到 /Pages".to_string())?;
    let mut out = Vec::new();
    walk_page_objects(&objects, &arrays, pages_ref, 0, &mut out)?;
    Ok(out)
}

fn walk_page_objects(
    objects: &HashMap<(u32, u16), RawObject>,
    arrays: &HashMap<(u32, u16), Vec<u8>>,
    reference: PdfObjectRef,
    depth: usize,
    out: &mut Vec<PageObject>,
) -> Result<(), String> {
    if depth > 64 {
        return Err("页面树太深".to_string());
    }
    let object =
        get_object(objects, reference).ok_or_else(|| format!("页面对象 {reference:?} 不存在"))?;
    if is_name(&object.dict, b"/Type", b"/Pages") {
        for kid in
            refs_for(&object.dict, b"/Kids", arrays).ok_or_else(|| "Pages 缺 /Kids".to_string())?
        {
            walk_page_objects(objects, arrays, kid, depth + 1, out)?;
        }
        return Ok(());
    }
    if !is_name(&object.dict, b"/Type", b"/Page") {
        return Err(format!("对象 {reference:?} 既不是 /Pages 也不是 /Page"));
    }
    out.push(PageObject {
        reference,
        dict: object.dict.clone(),
        annots: annots_of(&object.dict, arrays, objects),
    });
    Ok(())
}

/// 读取一个数组对象（间接 `/Annots`）。
pub(crate) fn array_object(pdf: &[u8], reference: PdfObjectRef) -> Result<Option<Vec<u8>>, String> {
    let mut objects: HashMap<(u32, u16), RawObject> = HashMap::new();
    let mut arrays: HashMap<(u32, u16), Vec<u8>> = HashMap::new();
    scan_objects(pdf, &mut objects, &mut arrays);
    scan_object_streams(pdf, &mut objects, &mut arrays);
    Ok(arrays.get(&(reference.num, reference.gen)).cloned())
}

/// 读取一个对象的字典字节（直接对象或 ObjStm 内的对象）。
pub(crate) fn object_dict(pdf: &[u8], reference: PdfObjectRef) -> Result<Option<Vec<u8>>, String> {
    let mut objects: HashMap<(u32, u16), RawObject> = HashMap::new();
    let mut arrays: HashMap<(u32, u16), Vec<u8>> = HashMap::new();
    scan_objects(pdf, &mut objects, &mut arrays);
    scan_object_streams(pdf, &mut objects, &mut arrays);
    Ok(objects
        .get(&(reference.num, reference.gen))
        .map(|object| object.dict.clone()))
}

/// PDF trailer 的关键字段（用于追加增量更新）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Trailer {
    pub root: PdfObjectRef,
    pub size: u32,
    pub startxref: usize,
}

/// 解析最后一个 trailer 的 `/Root`、`/Size`、`startxref`。
pub(crate) fn trailer(pdf: &[u8]) -> Result<Trailer, String> {
    let text = String::from_utf8_lossy(pdf);
    let root = last_ref_after(pdf, b"/Root").ok_or_else(|| "找不到 /Root".to_string())?;
    let size = last_number_after(&text, "/Size").unwrap_or(0) as u32;
    let startxref =
        last_number_after(&text, "startxref").ok_or_else(|| "找不到 startxref".to_string())?;
    Ok(Trailer {
        root,
        size,
        startxref,
    })
}

fn last_number_after(text: &str, key: &str) -> Option<usize> {
    let index = text.rfind(key)? + key.len();
    let digits: String = text[index..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn get_object(
    objects: &HashMap<(u32, u16), RawObject>,
    reference: PdfObjectRef,
) -> Option<&RawObject> {
    objects.get(&(reference.num, reference.gen))
}

fn annots_of(
    dict: &[u8],
    arrays: &HashMap<(u32, u16), Vec<u8>>,
    objects: &HashMap<(u32, u16), RawObject>,
) -> Vec<PageAnnot> {
    let Some(array) = array_value(dict, b"/Annots", arrays) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = 0;
    let content = &array;
    while cursor < content.len() {
        skip_space(content, &mut cursor);
        if cursor >= content.len() {
            break;
        }
        let (reference, entry_dict) = if content[cursor..].starts_with(b"<<") {
            let Some(end) = balanced_end(content, cursor) else {
                break;
            };
            let entry = content[cursor..end].to_vec();
            cursor = end;
            (None, entry)
        } else if let Some(reference) = parse_ref_at(content, &mut cursor) {
            let Some(object) = get_object(objects, reference) else {
                out.push(PageAnnot {
                    reference: Some(reference),
                    subtype: None,
                    popup: None,
                    hidden: false,
                });
                continue;
            };
            (Some(reference), object.dict.clone())
        } else {
            break;
        };
        let subtype = subtype_of(&entry_dict);
        let mut popup_cursor = find_from(&entry_dict, 0, b"/Popup").unwrap_or(entry_dict.len());
        let popup = if popup_cursor < entry_dict.len() {
            popup_cursor += b"/Popup".len();
            parse_ref_at(&entry_dict, &mut popup_cursor)
        } else {
            None
        };
        out.push(PageAnnot {
            reference,
            subtype,
            popup,
            hidden: is_hidden(&entry_dict),
        });
    }
    out
}

fn subtype_of(dict: &[u8]) -> Option<u32> {
    let index = find_from(dict, 0, b"/Subtype")?;
    let mut cursor = index + b"/Subtype".len();
    skip_space(dict, &mut cursor);
    let start = cursor;
    if dict.get(cursor) == Some(&b'/') {
        cursor += 1;
    }
    while cursor < dict.len() && !is_delimiter(dict[cursor]) {
        cursor += 1;
    }
    match &dict[start..cursor] {
        b"/Highlight" => Some(9),
        b"/Underline" => Some(10),
        b"/StrikeOut" => Some(12),
        b"/Ink" => Some(15),
        b"/Popup" => Some(16),
        _ => None,
    }
}

fn is_hidden(dict: &[u8]) -> bool {
    let Some(index) = find_from(dict, 0, b"/F") else {
        return false;
    };
    let mut cursor = index + b"/F".len();
    skip_space(dict, &mut cursor);
    let start = cursor;
    while cursor < dict.len() && dict[cursor].is_ascii_digit() {
        cursor += 1;
    }
    let flags: u32 = std::str::from_utf8(&dict[start..cursor])
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    flags & (2 | 32) != 0
}

fn parse_ref_at(bytes: &[u8], cursor: &mut usize) -> Option<PdfObjectRef> {
    let start = *cursor;
    let mut tokens = Vec::new();
    while *cursor < bytes.len() && tokens.len() < 3 {
        skip_space(bytes, cursor);
        let token_start = *cursor;
        while *cursor < bytes.len() && !is_delimiter(bytes[*cursor]) {
            *cursor += 1;
        }
        if token_start == *cursor {
            return None;
        }
        tokens.push(&bytes[token_start..*cursor]);
    }
    if tokens.len() != 3 || tokens[2] != b"R" {
        *cursor = start;
        return None;
    }
    Some(PdfObjectRef {
        num: std::str::from_utf8(tokens[0]).ok()?.parse().ok()?,
        gen: std::str::from_utf8(tokens[1]).ok()?.parse().ok()?,
    })
}

fn refs_for(
    dict: &[u8],
    key: &[u8],
    arrays: &HashMap<(u32, u16), Vec<u8>>,
) -> Option<Vec<PdfObjectRef>> {
    let array = array_value(dict, key, arrays)?;
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < array.len() {
        skip_space(&array, &mut cursor);
        if cursor >= array.len() {
            break;
        }
        out.push(parse_ref_at(&array, &mut cursor)?);
    }
    Some(out)
}

fn get_ref(
    objects: &HashMap<(u32, u16), RawObject>,
    reference: PdfObjectRef,
    key: &[u8],
) -> Option<PdfObjectRef> {
    let dict = &get_object(objects, reference)?.dict;
    let index = find_from(dict, 0, key)?;
    let mut cursor = index + key.len();
    parse_ref_at(dict, &mut cursor)
}

fn is_name(dict: &[u8], key: &[u8], name: &[u8]) -> bool {
    let Some(index) = find_from(dict, 0, key) else {
        return false;
    };
    let mut cursor = index + key.len();
    while cursor < dict.len() && dict[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    dict[cursor..].starts_with(name) && dict.get(cursor + name.len()) != Some(&b's')
}

fn array_value(dict: &[u8], key: &[u8], arrays: &HashMap<(u32, u16), Vec<u8>>) -> Option<Vec<u8>> {
    let index = find_from(dict, 0, key)?;
    let mut cursor = index + key.len();
    skip_space(dict, &mut cursor);
    if dict.get(cursor) == Some(&b'[') {
        let end = balanced_end(dict, cursor)?;
        return Some(dict[cursor + 1..end - 1].to_vec());
    }
    // `/Annots 16 0 R`：数组是独立对象
    let reference = parse_ref_at(dict, &mut cursor)?;
    arrays.get(&(reference.num, reference.gen)).cloned()
}

fn scan_objects(
    pdf: &[u8],
    objects: &mut HashMap<(u32, u16), RawObject>,
    arrays: &mut HashMap<(u32, u16), Vec<u8>>,
) {
    let mut cursor = 0;
    while let Some(found) = find_from(pdf, cursor, b" obj") {
        let mut line_start = found;
        while line_start > 0 && pdf[line_start - 1] != b'\n' && pdf[line_start - 1] != b'\r' {
            line_start -= 1;
        }
        let header = String::from_utf8_lossy(&pdf[line_start..found]);
        let tokens: Vec<&str> = header.split_whitespace().collect();
        cursor = found + 4;
        if tokens.len() < 2 {
            continue;
        }
        let (Ok(num), Ok(gen)) = (tokens[0].parse::<u32>(), tokens[1].parse::<u16>()) else {
            continue;
        };
        let mut dict_start = cursor;
        while dict_start < pdf.len() && pdf[dict_start].is_ascii_whitespace() {
            dict_start += 1;
        }
        if dict_start < pdf.len() && pdf[dict_start] == b'[' {
            if let Some(end) = balanced_end(pdf, dict_start) {
                arrays.insert((num, gen), pdf[dict_start + 1..end - 1].to_vec());
            }
            continue;
        }
        if dict_start + 1 >= pdf.len() || &pdf[dict_start..dict_start + 2] != b"<<" {
            continue;
        }
        let Some(dict_end) = balanced_end(pdf, dict_start) else {
            continue;
        };
        let mut after = dict_end;
        while after < pdf.len() && pdf[after].is_ascii_whitespace() {
            after += 1;
        }
        let mut stream = None;
        if pdf[after..].starts_with(b"stream") {
            let data_start = after + b"stream".len();
            let data_start = skip_eol(pdf, data_start);
            let data_end = if let Some(length) = dict_length(&pdf[dict_start..dict_end]) {
                (data_start + length).min(pdf.len())
            } else {
                find_from(pdf, data_start, b"endstream").unwrap_or(pdf.len())
            };
            let raw = &pdf[data_start..data_end];
            let decoded = if is_flate(&pdf[dict_start..dict_end]) {
                decompress(raw).unwrap_or_else(|| raw.to_vec())
            } else {
                raw.to_vec()
            };
            stream = Some(decoded);
        }
        objects.insert(
            (num, gen),
            RawObject {
                offset: line_start,
                dict: pdf[dict_start..dict_end].to_vec(),
                stream,
            },
        );
    }
}

fn dict_length(dict: &[u8]) -> Option<usize> {
    let index = find_from(dict, 0, b"/Length")?;
    let mut cursor = index + b"/Length".len();
    skip_space(dict, &mut cursor);
    let start = cursor;
    while cursor < dict.len() && dict[cursor].is_ascii_digit() {
        cursor += 1;
    }
    std::str::from_utf8(&dict[start..cursor]).ok()?.parse().ok()
}

fn is_flate(dict: &[u8]) -> bool {
    find_from(dict, 0, b"/FlateDecode").is_some()
}

fn decompress(raw: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(raw);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok().map(|_| out)
}

fn skip_eol(bytes: &[u8], mut index: usize) -> usize {
    if bytes.get(index) == Some(&b'\r') {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\n') {
        index += 1;
    }
    index
}

/// 解析 ObjStm：把其中的对象按对象号放进同一个表。
fn scan_object_streams(
    _pdf: &[u8],
    objects: &mut HashMap<(u32, u16), RawObject>,
    arrays: &mut HashMap<(u32, u16), Vec<u8>>,
) {
    let streams: Vec<PdfObjectRef> = objects
        .iter()
        .filter(|(_, object)| is_name(&object.dict, b"/Type", b"/ObjStm"))
        .map(|((num, gen), _)| PdfObjectRef {
            num: *num,
            gen: *gen,
        })
        .collect();
    for reference in streams {
        let Some((offset, data, count, first)) = (|| {
            let object = get_object(objects, reference)?;
            let data = object.stream.clone()?;
            let count = number_value(&object.dict, b"/N")?;
            let first = number_value(&object.dict, b"/First")?;
            Some((object.offset, data, count, first))
        })() else {
            continue;
        };
        let mut header_cursor = 0;
        let mut entries = Vec::new();
        for _ in 0..count {
            skip_space(&data, &mut header_cursor);
            let num_start = header_cursor;
            while header_cursor < data.len() && data[header_cursor].is_ascii_digit() {
                header_cursor += 1;
            }
            let num: u32 = std::str::from_utf8(&data[num_start..header_cursor])
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(0);
            skip_space(&data, &mut header_cursor);
            let offset_start = header_cursor;
            while header_cursor < data.len() && data[header_cursor].is_ascii_digit() {
                header_cursor += 1;
            }
            let offset: usize = std::str::from_utf8(&data[offset_start..header_cursor])
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(0);
            entries.push((num, first + offset));
        }
        for (index, (num, start)) in entries.iter().enumerate() {
            let end = entries
                .get(index + 1)
                .map(|(_, next)| *next)
                .unwrap_or(data.len());
            if *start >= end || end > data.len() {
                continue;
            }
            let dict = data[*start..end].to_vec();
            if dict.first() == Some(&b'[') {
                if let Some(close) = balanced_end(&dict, 0) {
                    arrays.insert((*num, 0), dict[1..close - 1].to_vec());
                }
                continue;
            }
            objects.insert(
                (*num, 0),
                RawObject {
                    offset: offset + index,
                    dict,
                    stream: None,
                },
            );
        }
    }
}

fn number_value(dict: &[u8], key: &[u8]) -> Option<usize> {
    let index = find_from(dict, 0, key)?;
    let mut cursor = index + key.len();
    skip_space(dict, &mut cursor);
    let start = cursor;
    while cursor < dict.len() && dict[cursor].is_ascii_digit() {
        cursor += 1;
    }
    std::str::from_utf8(&dict[start..cursor]).ok()?.parse().ok()
}

fn last_ref_after(bytes: &[u8], key: &[u8]) -> Option<PdfObjectRef> {
    let text = String::from_utf8_lossy(bytes);
    let index = text.rfind(std::str::from_utf8(key).ok()?)? + key.len();
    let mut cursor = index;
    let rest = text.as_bytes();
    parse_ref_at(rest, &mut cursor)
}

fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn skip_space(bytes: &[u8], index: &mut usize) {
    while *index < bytes.len() {
        if bytes[*index].is_ascii_whitespace() {
            *index += 1;
        } else if bytes[*index] == b'%' {
            while *index < bytes.len() && bytes[*index] != b'\n' {
                *index += 1;
            }
        } else {
            break;
        }
    }
}

fn find_from(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    let mut dict_depth = 0i32;
    let mut array_depth = 0i32;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'(' => {
                index += 1;
                let mut depth = 1;
                while index < bytes.len() && depth > 0 {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'(' => {
                            depth += 1;
                            index += 1;
                        }
                        b')' => {
                            depth -= 1;
                            index += 1;
                        }
                        _ => index += 1,
                    }
                }
            }
            b'<' if bytes.get(index + 1) == Some(&b'<') => {
                dict_depth += 1;
                index += 2;
            }
            b'>' if bytes.get(index + 1) == Some(&b'>') => {
                dict_depth -= 1;
                index += 2;
            }
            b'<' => {
                while index < bytes.len() && bytes[index] != b'>' {
                    index += 1;
                }
                index += 1;
            }
            b'[' => {
                array_depth += 1;
                index += 1;
            }
            b']' => {
                array_depth -= 1;
                index += 1;
            }
            _ => index += 1,
        }
        if dict_depth == 0 && array_depth == 0 && index > start {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/s6b")
            .join(name)
    }

    #[test]
    fn page_annotation_refs_for_real_samples() {
        for name in [
            "okular.pdf",
            "okular_without_content.pdf",
            "evince.pdf",
            "firefox.pdf",
            "adobe.pdf",
            "foxit.pdf",
        ] {
            let bytes = std::fs::read(fixture(name)).expect("读样本");
            let pages =
                page_annotation_refs(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
            let counts: Vec<usize> = pages
                .iter()
                .map(|refs| refs.iter().filter(|r| r.is_some()).count())
                .collect();
            let inline: Vec<usize> = pages
                .iter()
                .map(|refs| refs.iter().filter(|r| r.is_none()).count())
                .collect();
            eprintln!(
                "[{name}] pages={} refs={counts:?} inline={inline:?}",
                pages.len()
            );
            assert_eq!(pages.len(), 2, "{name}: 应有 2 页");
        }
    }
}
