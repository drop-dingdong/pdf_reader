//! 把 PDF 里"直接内联"的注释字典外置成独立对象（增量更新）。
//!
//! PDFium 的 `FPDFPage_CreateAnnot` 新建的注释会被保存成 `/Annots [ <<…>> <<…>> ]`
//! —— 直接字典、没有对象号。Okular（Poppler）能读、能编辑，但**保存时会把修改丢掉**：
//! 它的注释对象带着 `Ref`，直接字典没有 Ref。Adobe/Okular 自己写的注释都是独立对象。
//!
//! 这里在 PDFium 输出之后追加一段**经典增量更新**：
//!
//! ```text
//! <新注释对象…>
//! <同一个页对象（/Annots 换成引用）>
//! xref（每个对象一个子段）
//! trailer <</Size … /Root … /Prev <原 startxref>>>
//! startxref …
//! %%EOF
//! ```
//!
//! 只做这一件事；解析失败时调用方保留原字节（宁可少修，不弄坏文件）。

/// 返回 `None`：没有内联注释，无需修改。
pub(crate) fn externalize_annotations(pdf: &[u8]) -> Option<Vec<u8>> {
    let trailer = TrailerInfo::parse(pdf)?;
    let objects = scan_objects(pdf);
    if objects.is_empty() {
        return None;
    }

    // 每个对象号只保留最后一次出现（增量文件里同一对象可能有多版）
    let latest = latest_objects(&objects);
    let mut next_object = trailer
        .size
        .max(latest.keys().copied().max().unwrap_or(0) + 1);
    let mut appended: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut changed_pages = 0;

    for (number, (dict_start, dict_end)) in &latest {
        let dict = &pdf[*dict_start..*dict_end];
        if !is_page_dict(dict) {
            continue;
        }
        let Some(annots) = find_annots(dict) else {
            continue;
        };
        let Some(entries) = split_entries(&dict[annots.array_start + 1..annots.array_end - 1])
        else {
            eprintln!("[repair] split_entries failed");
            return None;
        };
        if !entries
            .iter()
            .any(|entry| matches!(entry, Entry::Inline(_)))
        {
            continue;
        }
        let mut refs: Vec<String> = Vec::new();
        for entry in entries {
            match entry {
                Entry::Ref(text) => refs.push(text),
                Entry::Inline(dict_bytes) => {
                    let object_number = next_object;
                    next_object += 1;
                    appended.push((object_number, dict_bytes));
                    refs.push(format!("{object_number} 0 R"));
                }
            }
        }
        let mut new_dict = Vec::with_capacity(dict.len() + 32);
        new_dict.extend_from_slice(&dict[..annots.array_start]);
        new_dict.push(b'[');
        new_dict.extend_from_slice(refs.join(" ").as_bytes());
        new_dict.push(b']');
        new_dict.extend_from_slice(&dict[annots.array_end..]);
        appended.push((*number, new_dict));
        changed_pages += 1;
    }
    if changed_pages == 0 {
        return None;
    }

    appended.sort_by_key(|(number, _)| *number);
    let mut out = pdf.to_vec();
    let mut offsets: Vec<(usize, usize)> = Vec::new();
    for (number, body) in &appended {
        let offset = out.len();
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
        offsets.push((*number, offset));
    }
    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n");
    for (number, offset) in &offsets {
        out.extend_from_slice(format!("{number} 1\n{offset:010} 00000 n \n").as_bytes());
    }
    let new_size = next_object.max(trailer.size);
    out.extend_from_slice(
        format!(
            "trailer\n<</Size {new_size} /Root {} /Prev {}>>\nstartxref\n{xref_offset}\n%%EOF\n",
            trailer.root, trailer.startxref
        )
        .as_bytes(),
    );
    Some(out)
}

struct TrailerInfo {
    root: String,
    size: usize,
    startxref: usize,
}

impl TrailerInfo {
    fn parse(pdf: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(pdf);
        let startxref = last_number_after(&text, "startxref")?;
        let root = last_ref_after(&text, "/Root")?;
        let size = last_number_after(&text, "/Size").unwrap_or(0);
        Some(Self {
            root,
            size,
            startxref,
        })
    }
}

/// `startxref` / `/Size` 后面的第一个整数（取最后一次出现）。
fn last_number_after(text: &str, key: &str) -> Option<usize> {
    let index = text.rfind(key)? + key.len();
    let rest = &text[index..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// `/Root 1 0 R` 里的 `1 0 R`（取最后一次出现）。
fn last_ref_after(text: &str, key: &str) -> Option<String> {
    let index = text.rfind(key)? + key.len();
    let rest = text[index..].trim_start();
    let mut parts = rest.split_whitespace();
    let number = parts.next()?.parse::<usize>().ok()?;
    let generation = parts.next()?.parse::<usize>().ok()?;
    if parts.next()? != "R" {
        return None;
    }
    Some(format!("{number} {generation} R"))
}

/// 扫描 `N G obj`，返回（对象号，(字典起, 字典止)）。只认紧跟 `<<` 的对象。
fn scan_objects(pdf: &[u8]) -> Vec<(usize, (usize, usize))> {
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(found) = find_from(pdf, index, b" obj") {
        // 往前找行首的对象号与 generation
        let mut line_start = found;
        while line_start > 0 && pdf[line_start - 1] != b'\n' && pdf[line_start - 1] != b'\r' {
            line_start -= 1;
        }
        let header = String::from_utf8_lossy(&pdf[line_start..found]);
        let tokens: Vec<&str> = header.split_whitespace().collect();
        index = found + 4;
        if tokens.len() < 2 {
            continue;
        }
        let Ok(number) = tokens[0].parse::<usize>() else {
            continue;
        };
        let mut cursor = index;
        while cursor < pdf.len() && pdf[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor + 1 >= pdf.len() || &pdf[cursor..cursor + 2] != b"<<" {
            continue;
        }
        let Some(end) = balanced_end(pdf, cursor) else {
            continue;
        };
        out.push((number, (cursor, end)));
    }
    out
}

fn latest_objects(
    objects: &[(usize, (usize, usize))],
) -> std::collections::BTreeMap<usize, (usize, usize)> {
    let mut map = std::collections::BTreeMap::new();
    for (number, range) in objects {
        map.insert(*number, *range);
    }
    map
}

fn is_page_dict(dict: &[u8]) -> bool {
    // 不能找"第一个 /Type"：Annots 里的注释字典也有 /Type/Annot。
    let Some(index) = find_from(dict, 0, b"/Type/Page") else {
        return false;
    };
    dict.get(index + b"/Type/Page".len()) != Some(&b's')
}

struct Annots {
    /// `/Annots` 数组的左括号位置。
    array_start: usize,
    /// 右括号之后的位置。
    array_end: usize,
}

fn find_annots(dict: &[u8]) -> Option<Annots> {
    let index = find_from(dict, 0, b"/Annots")?;
    let mut cursor = index + b"/Annots".len();
    while cursor < dict.len() && dict[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if dict.get(cursor) != Some(&b'[') {
        return None;
    }
    let end = balanced_end(dict, cursor)?;
    Some(Annots {
        array_start: cursor,
        array_end: end,
    })
}

enum Entry {
    Ref(String),
    Inline(Vec<u8>),
}

/// 拆分 `/Annots [ … ]` 的内容（不含括号）。
fn split_entries(content: &[u8]) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut index = 0;
    while index < content.len() {
        skip_space(content, &mut index);
        if index >= content.len() {
            break;
        }
        if content[index..].starts_with(b"<<") {
            let end = balanced_end(content, index)?;
            entries.push(Entry::Inline(content[index..end].to_vec()));
            index = end;
        } else {
            let start = index;
            let mut tokens = 0;
            while index < content.len() {
                skip_space(content, &mut index);
                if index >= content.len() {
                    break;
                }
                let token_start = index;
                while index < content.len() && !content[index].is_ascii_whitespace() {
                    index += 1;
                }
                tokens += 1;
                if tokens == 3 {
                    if &content[token_start..index] != b"R" {
                        return None;
                    }
                    break;
                }
            }
            entries.push(Entry::Ref(
                String::from_utf8_lossy(&content[start..index])
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ));
        }
    }
    Some(entries)
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
    if from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// 从 `<<` 或 `[` 开始，返回配对闭合之后的位置；处理 `(...)` 字符串与 `<hex>`。
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

    #[test]
    fn splits_inline_and_refs() {
        let content = b" 9 0 R << /Subtype /Highlight >> 12 0 R ";
        let entries = split_entries(content).expect("split");
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], Entry::Ref(text) if text == "9 0 R"));
        assert!(matches!(&entries[1], Entry::Inline(_)));
        assert!(matches!(&entries[2], Entry::Ref(text) if text == "12 0 R"));
    }

    #[test]
    fn balanced_handles_strings_and_nested_dicts() {
        let bytes = b"<< /Contents (a (b) c) /X << /Y [1 2] >> >> tail";
        let end = balanced_end(bytes, 0).expect("end");
        assert_eq!(&bytes[end..], b" tail");
    }
}
