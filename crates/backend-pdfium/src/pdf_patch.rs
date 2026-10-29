//! P4a：同对象号、同 generation 的正文增量更新。
//!
//! 只做一件事：复制目标注释字典、替换 `/Contents`，把新版本以
//! `<num> <gen> obj … endobj` 追加到文件末尾，再追加经典 xref + trailer `/Prev`。
//! 页对象和 `/Annots` 引用不动；原对象/ObjStm 不动。

use crate::pdf_index::{object_dict, trailer, PdfObjectRef};

/// 追加若干条正文更新；返回新字节。
pub fn append_content_updates(
    pdf: &[u8],
    updates: &[(PdfObjectRef, Option<String>)],
) -> Result<Vec<u8>, String> {
    if updates.is_empty() {
        return Ok(pdf.to_vec());
    }
    let trailer = trailer(pdf)?;
    let mut out = pdf.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }

    let mut entries: Vec<(PdfObjectRef, usize)> = Vec::with_capacity(updates.len());
    for (reference, body) in updates {
        let dict = object_dict(pdf, *reference)?
            .ok_or_else(|| format!("对象 {reference:?} 不在源文件里"))?;
        let dict = replace_contents(&dict, body.as_deref())?;
        let offset = out.len();
        out.extend_from_slice(format!("{} {} obj\n", reference.num, reference.gen).as_bytes());
        out.extend_from_slice(&dict);
        out.extend_from_slice(b"\nendobj\n");
        entries.push((*reference, offset));
    }

    entries.sort_by_key(|(reference, _)| (reference.num, reference.gen));
    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n");
    for (reference, offset) in &entries {
        out.extend_from_slice(
            format!(
                "{} 1\n{:010} {:05} n \n",
                reference.num, offset, reference.gen
            )
            .as_bytes(),
        );
    }
    let max_number = entries
        .iter()
        .map(|(reference, _)| reference.num)
        .max()
        .unwrap_or(0);
    let size = trailer.size.max(max_number + 1);
    out.extend_from_slice(
        format!(
            "trailer\n<</Size {size} /Root {} {} R /Prev {}>>\nstartxref\n{xref_offset}\n%%EOF\n",
            trailer.root.num, trailer.root.gen, trailer.startxref
        )
        .as_bytes(),
    );
    Ok(out)
}

/// 替换/删除注释字典的 `/Contents`；其它键原样保留。
fn replace_contents(dict: &[u8], body: Option<&str>) -> Result<Vec<u8>, String> {
    if !dict.starts_with(b"<<") || !dict.ends_with(b">>") {
        return Err("注释对象不是字典".to_string());
    }
    let mut index = 2;
    let close = dict.len() - 2;
    let mut old_range = None;
    while index < close {
        skip_space(dict, &mut index);
        if index >= close {
            break;
        }
        if dict[index] != b'/' {
            return Err("注释字典里出现非键 token".to_string());
        }
        let key_start = index;
        index += 1;
        while index < close && !is_delimiter(dict[index]) {
            index += 1;
        }
        let key = &dict[key_start..index];
        skip_space(dict, &mut index);
        let value_end = value_end(dict, index)?;
        if key == b"/Contents" {
            old_range = Some((key_start, value_end));
            break;
        }
        index = value_end;
    }

    let mut out = Vec::with_capacity(dict.len() + 64);
    match old_range {
        Some((start, end)) => {
            out.extend_from_slice(&dict[..start]);
            out.extend_from_slice(&dict[end..]);
        }
        None => out.extend_from_slice(dict),
    }
    if let Some(body) = body {
        let insert_at = out.len() - 2;
        let hex = utf16_hex(body);
        out.splice(
            insert_at..insert_at,
            format!(" /Contents <{hex}>").into_bytes(),
        );
    }
    Ok(out)
}

fn utf16_hex(text: &str) -> String {
    let mut out = String::from("FEFF");
    for unit in text.encode_utf16() {
        out.push_str(&format!("{unit:04X}"));
    }
    out
}

fn value_end(bytes: &[u8], mut index: usize) -> Result<usize, String> {
    skip_space(bytes, &mut index);
    match bytes.get(index).copied() {
        Some(b'(') => {
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
            Ok(index)
        }
        Some(b'<') if bytes.get(index + 1) == Some(&b'<') => balanced(bytes, index, b'<', b'>'),
        Some(b'<') => {
            index += 1;
            while index < bytes.len() && bytes[index] != b'>' {
                index += 1;
            }
            Ok((index + 1).min(bytes.len()))
        }
        Some(b'[') => balanced(bytes, index, b'[', b']'),
        Some(b'/') => {
            index += 1;
            while index < bytes.len() && !is_delimiter(bytes[index]) {
                index += 1;
            }
            Ok(index)
        }
        Some(_) => {
            // 先吃掉第一个 token；若后面是 `<num> <num> R`，整个引用一起吃掉。
            let first_end = token_end(bytes, index);
            let mut second = first_end;
            skip_space(bytes, &mut second);
            if bytes.get(second).is_some_and(|byte| byte.is_ascii_digit()) {
                let second_end = token_end(bytes, second);
                let mut third = second_end;
                skip_space(bytes, &mut third);
                if bytes.get(third) == Some(&b'R')
                    && bytes
                        .get(third + 1)
                        .map_or(true, |byte| is_delimiter(*byte))
                {
                    return Ok(third + 1);
                }
            }
            Ok(first_end)
        }
        None => Err("缺少值".to_string()),
    }
}

fn token_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while index < bytes.len() && !is_delimiter(bytes[index]) {
        index += 1;
    }
    index
}

fn balanced(bytes: &[u8], start: usize, open: u8, close: u8) -> Result<usize, String> {
    let mut index = start;
    let mut depth = 0usize;
    while index < bytes.len() {
        if bytes[index] == open {
            depth += 1;
        } else if bytes[index] == close {
            depth -= 1;
            if depth == 0 {
                return Ok(index + 1);
            }
        }
        index += 1;
    }
    Err("括号不配对".to_string())
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

fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}
