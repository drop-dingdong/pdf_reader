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

/// 删除若干注释：复制页对象，从 `/Annots` 去掉引用（含 Popup），追加页对象 + xref。
pub fn append_deletes(pdf: &[u8], deletes: &[PdfObjectRef]) -> Result<Vec<u8>, String> {
    use std::collections::{HashMap, HashSet};

    if deletes.is_empty() {
        return Ok(pdf.to_vec());
    }
    let pages = crate::pdf_index::page_objects(pdf)?;
    let mut locations: HashMap<PdfObjectRef, usize> = HashMap::new();
    let mut deleted: HashSet<PdfObjectRef> = deletes.iter().copied().collect();
    for (page, object) in pages.iter().enumerate() {
        for entry in &object.annots {
            if let Some(reference) = entry.reference {
                locations.insert(reference, page);
                if deleted.contains(&reference) {
                    if let Some(popup) = entry.popup {
                        deleted.insert(popup);
                    }
                }
            }
        }
    }
    for reference in deletes {
        if !locations.contains_key(reference) {
            return Err(format!("要删除的注释对象 {reference:?} 不在当前 PDF 中"));
        }
    }

    let trailer = trailer(pdf)?;
    let mut out = pdf.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut entries: Vec<(PdfObjectRef, usize)> = Vec::new();
    for object in &pages {
        if !object
            .annots
            .iter()
            .filter_map(|entry| entry.reference)
            .any(|reference| deleted.contains(&reference))
        {
            continue;
        }
        let dict = rewrite_annots(pdf, &object.dict, &deleted)?;
        let offset = out.len();
        out.extend_from_slice(
            format!("{} {} obj\n", object.reference.num, object.reference.gen).as_bytes(),
        );
        out.extend_from_slice(&dict);
        out.extend_from_slice(b"\nendobj\n");
        entries.push((object.reference, offset));
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

fn rewrite_annots(
    pdf: &[u8],
    dict: &[u8],
    deleted: &std::collections::HashSet<PdfObjectRef>,
) -> Result<Vec<u8>, String> {
    let key = b"/Annots";
    let index = find_from(dict, 0, key).ok_or_else(|| "页对象缺 /Annots".to_string())?;
    let mut cursor = index + key.len();
    skip_space(dict, &mut cursor);
    let value_start = cursor;
    let value_end = value_end(dict, cursor)?;
    let (array, replace_start, replace_end) = if dict.get(value_start) == Some(&b'[') {
        let end = balanced(dict, value_start, b'[', b']')?;
        (dict[value_start + 1..end - 1].to_vec(), value_start, end)
    } else {
        let mut ref_cursor = value_start;
        let reference = parse_ref_at(dict, &mut ref_cursor)
            .ok_or_else(|| "页 /Annots 不是数组也不是引用".to_string())?;
        let array = crate::pdf_index::array_object(pdf, reference)?
            .ok_or_else(|| format!("数组对象 {reference:?} 不存在"))?;
        (array, value_start, value_end)
    };

    let mut kept: Vec<Vec<u8>> = Vec::new();
    for entry in split_entries(&array)? {
        if let Some(reference) = entry_ref(&entry) {
            if deleted.contains(&reference) {
                continue;
            }
        }
        kept.push(entry);
    }
    let mut new_array = Vec::with_capacity(array.len());
    new_array.push(b'[');
    for (index, entry) in kept.iter().enumerate() {
        if index > 0 {
            new_array.push(b' ');
        }
        new_array.extend_from_slice(entry);
    }
    new_array.push(b']');

    let mut out = Vec::with_capacity(dict.len() + new_array.len());
    out.extend_from_slice(&dict[..replace_start]);
    out.extend_from_slice(&new_array);
    out.extend_from_slice(&dict[replace_end..]);
    Ok(out)
}

fn split_entries(array: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut entries = Vec::new();
    let mut index = 0;
    while index < array.len() {
        skip_space(array, &mut index);
        if index >= array.len() {
            break;
        }
        if array[index..].starts_with(b"<<") {
            let end = balanced(array, index, b'<', b'>')?;
            entries.push(array[index..end].to_vec());
            index = end;
        } else {
            let start = index;
            let mut reference_cursor = index;
            if parse_ref_at(array, &mut reference_cursor).is_none() {
                return Err("Annots 数组里有非法条目".to_string());
            }
            index = reference_cursor;
            entries.push(array[start..index].to_vec());
        }
    }
    Ok(entries)
}

fn entry_ref(entry: &[u8]) -> Option<PdfObjectRef> {
    let mut cursor = 0;
    parse_ref_at(entry, &mut cursor)
}

fn parse_ref_at(bytes: &[u8], cursor: &mut usize) -> Option<PdfObjectRef> {
    let start = *cursor;
    let mut tokens: [&[u8]; 3] = [&[]; 3];
    for token in &mut tokens {
        skip_space(bytes, cursor);
        let token_start = *cursor;
        while *cursor < bytes.len() && !is_delimiter(bytes[*cursor]) {
            *cursor += 1;
        }
        if token_start == *cursor {
            *cursor = start;
            return None;
        }
        *token = &bytes[token_start..*cursor];
    }
    if tokens[2] != b"R" {
        *cursor = start;
        return None;
    }
    Some(PdfObjectRef {
        num: std::str::from_utf8(tokens[0]).ok()?.parse().ok()?,
        gen: std::str::from_utf8(tokens[1]).ok()?.parse().ok()?,
    })
}

fn find_from(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// 一条待写入的新高亮。
pub struct NewHighlight {
    /// 页号（0 起）。
    pub page: u32,
    /// 我们的注释 id（写 `/NM = pd-fr:<id>`）。
    pub id: Option<u64>,
    /// 包围盒。
    pub rect: pdfcore::prelude::Rect,
    /// 逐行矩形。
    pub quads: Vec<pdfcore::prelude::Rect>,
    /// RGB。
    pub color: [u8; 3],
    /// 不透明度。
    pub opacity: f32,
    /// 正文。
    pub body: Option<String>,
}

/// 追加新高亮：分配对象号，写注释对象，复制页对象把 ref 加到 `/Annots`。
pub fn append_new_highlights(pdf: &[u8], highlights: &[NewHighlight]) -> Result<Vec<u8>, String> {
    if highlights.is_empty() {
        return Ok(pdf.to_vec());
    }
    let pages = crate::pdf_index::page_objects(pdf)?;
    let trailer = trailer(pdf)?;
    let next_object = trailer.size.max(
        pages
            .iter()
            .map(|page| page.reference.num)
            .max()
            .unwrap_or(0)
            + 1,
    );
    let mut out = pdf.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut entries: Vec<(PdfObjectRef, usize)> = Vec::new();
    let mut by_page: std::collections::BTreeMap<usize, Vec<PdfObjectRef>> =
        std::collections::BTreeMap::new();

    for (index, highlight) in highlights.iter().enumerate() {
        let page = pages
            .get(highlight.page as usize)
            .ok_or_else(|| format!("页号 {} 越界", highlight.page))?;
        let number = next_object + index as u32;
        let offset = out.len();
        let mut dict = format!(
            "<</Type/Annot/Subtype/Highlight/Rect[{:.2} {:.2} {:.2} {:.2}]/QuadPoints[",
            number_f32(highlight.rect.x),
            number_f32(highlight.rect.y),
            number_f32(highlight.rect.x + highlight.rect.width),
            number_f32(highlight.rect.y + highlight.rect.height),
        );
        for quad in &highlight.quads {
            dict.push_str(&format!(
                "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                number_f32(quad.x),
                number_f32(quad.y + quad.height),
                number_f32(quad.x + quad.width),
                number_f32(quad.y + quad.height),
                number_f32(quad.x),
                number_f32(quad.y),
                number_f32(quad.x + quad.width),
                number_f32(quad.y),
            ));
        }
        dict.push_str(&format!(
            "]/C[{:.3} {:.3} {:.3}]/CA {:.3}/F 4",
            highlight.color[0] as f32 / 255.0,
            highlight.color[1] as f32 / 255.0,
            highlight.color[2] as f32 / 255.0,
            highlight.opacity.clamp(0.0, 1.0),
        ));
        if let Some(id) = highlight.id {
            dict.push_str(&format!("/NM(pd-fr:{id})"));
        }
        if let Some(body) = &highlight.body {
            dict.push_str(&format!("/Contents <{}>", utf16_hex(body)));
        }
        dict.push_str(&format!(
            "/P {} {} R>>",
            page.reference.num, page.reference.gen
        ));
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(dict.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
        let reference = PdfObjectRef {
            num: number,
            gen: 0,
        };
        entries.push((reference, offset));
        by_page
            .entry(highlight.page as usize)
            .or_default()
            .push(reference);
    }

    for (page_index, refs) in by_page {
        let page = &pages[page_index];
        let new_dict = append_annots(pdf, &page.dict, &refs)?;
        let offset = out.len();
        out.extend_from_slice(
            format!("{} {} obj\n", page.reference.num, page.reference.gen).as_bytes(),
        );
        out.extend_from_slice(&new_dict);
        out.extend_from_slice(b"\nendobj\n");
        entries.push((page.reference, offset));
    }

    append_xref(pdf, trailer, &mut out, entries)
}

fn append_annots(pdf: &[u8], dict: &[u8], new_refs: &[PdfObjectRef]) -> Result<Vec<u8>, String> {
    let key = b"/Annots";
    let Some(index) = find_from(dict, 0, key) else {
        let insert_at = dict.len().saturating_sub(2);
        let mut out = dict.to_vec();
        let mut array = String::from(" /Annots [");
        for (i, reference) in new_refs.iter().enumerate() {
            if i > 0 {
                array.push(' ');
            }
            array.push_str(&format!("{} {} R", reference.num, reference.gen));
        }
        array.push(']');
        out.splice(insert_at..insert_at, array.into_bytes());
        return Ok(out);
    };
    let mut cursor = index + key.len();
    skip_space(dict, &mut cursor);
    let value_start = cursor;
    let value_end = value_end(dict, cursor)?;
    let (array, replace_start, replace_end) = if dict.get(value_start) == Some(&b'[') {
        let end = balanced(dict, value_start, b'[', b']')?;
        (dict[value_start + 1..end - 1].to_vec(), value_start, end)
    } else {
        let mut ref_cursor = value_start;
        let reference = parse_ref_at(dict, &mut ref_cursor)
            .ok_or_else(|| "页 /Annots 不是数组也不是引用".to_string())?;
        let array = crate::pdf_index::array_object(pdf, reference)?
            .ok_or_else(|| format!("数组对象 {reference:?} 不存在"))?;
        (array, value_start, value_end)
    };
    let mut entries = split_entries(&array)?;
    for reference in new_refs {
        entries.push(format!("{} {} R", reference.num, reference.gen).into_bytes());
    }
    let mut new_array = Vec::new();
    new_array.push(b'[');
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            new_array.push(b' ');
        }
        new_array.extend_from_slice(entry);
    }
    new_array.push(b']');
    let mut out = Vec::with_capacity(dict.len() + new_array.len());
    out.extend_from_slice(&dict[..replace_start]);
    out.extend_from_slice(&new_array);
    out.extend_from_slice(&dict[replace_end..]);
    Ok(out)
}

fn append_xref(
    _pdf: &[u8],
    trailer: crate::pdf_index::Trailer,
    out: &mut Vec<u8>,
    mut entries: Vec<(PdfObjectRef, usize)>,
) -> Result<Vec<u8>, String> {
    entries.sort_by_key(|(reference, _)| (reference.num, reference.gen));
    entries.dedup_by_key(|(reference, _)| *reference);
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
    Ok(std::mem::take(out))
}

fn number_f32(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

/// 文本类标记的种类（新注释对象化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupKind {
    /// 高亮。
    Highlight,
    /// 下划线。
    Underline,
    /// 删除线。
    StrikeOut,
}

/// 一条待写入的新文本标记（高亮/下划线/删除线）。
pub struct NewMarkup {
    /// 页号（0 起）。
    pub page: u32,
    /// 我们的注释 id。
    pub id: Option<u64>,
    /// 包围盒。
    pub rect: pdfcore::prelude::Rect,
    /// 逐行矩形。
    pub quads: Vec<pdfcore::prelude::Rect>,
    /// 种类。
    pub kind: MarkupKind,
    /// RGB。
    pub color: [u8; 3],
    /// 不透明度。
    pub opacity: f32,
    /// 正文。
    pub body: Option<String>,
}

/// 追加新高亮/下划线/删除线：注释对象 + 可选 AP Form + 页 /Annots + xref。
pub fn append_new_markups(pdf: &[u8], markups: &[NewMarkup]) -> Result<Vec<u8>, String> {
    if markups.is_empty() {
        return Ok(pdf.to_vec());
    }
    let pages = crate::pdf_index::page_objects(pdf)?;
    let trailer = trailer(pdf)?;
    let mut next_object = trailer.size.max(
        pages
            .iter()
            .map(|page| page.reference.num)
            .max()
            .unwrap_or(0)
            + 1,
    );
    let mut out = pdf.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut entries: Vec<(PdfObjectRef, usize)> = Vec::new();
    let mut by_page: std::collections::BTreeMap<usize, Vec<PdfObjectRef>> =
        std::collections::BTreeMap::new();

    for markup in markups {
        let page = pages
            .get(markup.page as usize)
            .ok_or_else(|| format!("页号 {} 越界", markup.page))?;
        let annotation_number = next_object;
        next_object += 1;
        let ap_number = if matches!(markup.kind, MarkupKind::Highlight) {
            None
        } else {
            let number = next_object;
            next_object += 1;
            Some(number)
        };

        let display_rect = if markup.kind == MarkupKind::Underline {
            let gap = pdfcore::annotation::UNDERLINE_GAP
                + pdfcore::annotation::TEXT_MARK_LINE_WIDTH * 0.5;
            pdfcore::prelude::Rect::new(
                markup.rect.x,
                markup.rect.y - gap,
                markup.rect.width,
                markup.rect.height + gap,
            )
        } else {
            markup.rect
        };
        if let Some(ap_number) = ap_number {
            let content = if markup.kind == MarkupKind::Underline {
                lines_content(&markup.quads, false, markup.color, markup.opacity)
            } else {
                lines_content(&markup.quads, true, markup.color, markup.opacity)
            };
            let ap = format!(
                "<</Type/XObject/Subtype/Form/FormType 1/BBox[{:.2} {:.2} {:.2} {:.2}]/Matrix[1 0 0 1 0 0]/Resources<<>>/Length {}>>\nstream\n{}\nendstream\nendobj\n",
                number_f32(display_rect.x),
                number_f32(display_rect.y),
                number_f32(display_rect.x + display_rect.width),
                number_f32(display_rect.y + display_rect.height),
                content.len(),
                content
            );
            let offset = out.len();
            out.extend_from_slice(format!("{ap_number} 0 obj\n").as_bytes());
            out.extend_from_slice(ap.as_bytes());
            entries.push((
                PdfObjectRef {
                    num: ap_number,
                    gen: 0,
                },
                offset,
            ));
        }

        let subtype = match markup.kind {
            MarkupKind::Highlight => "Highlight",
            MarkupKind::Underline => "Underline",
            MarkupKind::StrikeOut => "StrikeOut",
        };
        let mut dict = format!(
            "<</Type/Annot/Subtype/{subtype}/Rect[{:.2} {:.2} {:.2} {:.2}]/QuadPoints[",
            number_f32(display_rect.x),
            number_f32(display_rect.y),
            number_f32(display_rect.x + display_rect.width),
            number_f32(display_rect.y + display_rect.height),
        );
        for quad in &markup.quads {
            dict.push_str(&format!(
                "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                number_f32(quad.x),
                number_f32(quad.y + quad.height),
                number_f32(quad.x + quad.width),
                number_f32(quad.y + quad.height),
                number_f32(quad.x),
                number_f32(quad.y),
                number_f32(quad.x + quad.width),
                number_f32(quad.y),
            ));
        }
        dict.push_str(&format!(
            "]/C[{:.3} {:.3} {:.3}]/CA {:.3}/F 4",
            markup.color[0] as f32 / 255.0,
            markup.color[1] as f32 / 255.0,
            markup.color[2] as f32 / 255.0,
            markup.opacity.clamp(0.0, 1.0),
        ));
        if let Some(id) = markup.id {
            dict.push_str(&format!("/NM(pd-fr:{id})"));
        }
        if let Some(body) = &markup.body {
            dict.push_str(&format!("/Contents <{}>", utf16_hex(body)));
        }
        if let Some(ap_number) = ap_number {
            dict.push_str(&format!("/AP<</N {ap_number} 0 R>>"));
        }
        dict.push_str(&format!(
            "/P {} {} R>>",
            page.reference.num, page.reference.gen
        ));
        let offset = out.len();
        out.extend_from_slice(format!("{annotation_number} 0 obj\n").as_bytes());
        out.extend_from_slice(dict.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
        let reference = PdfObjectRef {
            num: annotation_number,
            gen: 0,
        };
        entries.push((reference, offset));
        by_page
            .entry(markup.page as usize)
            .or_default()
            .push(reference);
    }

    for (page_index, refs) in by_page {
        let page = &pages[page_index];
        let new_dict = append_annots(pdf, &page.dict, &refs)?;
        let offset = out.len();
        out.extend_from_slice(
            format!("{} {} obj\n", page.reference.num, page.reference.gen).as_bytes(),
        );
        out.extend_from_slice(&new_dict);
        out.extend_from_slice(b"\nendobj\n");
        entries.push((page.reference, offset));
    }

    append_xref(pdf, trailer, &mut out, entries)
}

fn lines_content(
    quads: &[pdfcore::prelude::Rect],
    middle: bool,
    color: [u8; 3],
    opacity: f32,
) -> String {
    let alpha = opacity.clamp(0.0, 1.0);
    let mut content = format!(
        "q {:.3} {:.3} {:.3} RG 1 w {:.3} CA {:.3} ca",
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        alpha,
        alpha
    );
    for quad in quads {
        let y = if middle {
            quad.y + quad.height * pdfcore::annotation::STRIKE_FRACTION
        } else {
            quad.y - pdfcore::annotation::UNDERLINE_GAP
        };
        content.push_str(&format!(
            " {:.2} {:.2} m {:.2} {:.2} l",
            number_f32(quad.x),
            number_f32(y),
            number_f32(quad.x + quad.width),
            number_f32(y)
        ));
    }
    content.push_str(" S Q");
    content
}

/// 一条待写入的新墨迹。
pub struct NewInk {
    /// 页号。
    pub page: u32,
    /// 我们的注释 id。
    pub id: Option<u64>,
    /// 包围盒。
    pub rect: pdfcore::prelude::Rect,
    /// 折线点。
    pub points: Vec<pdfcore::prelude::Point>,
    /// 线宽。
    pub width: f32,
    /// RGB。
    pub color: [u8; 3],
    /// 不透明度。
    pub opacity: f32,
    /// 正文。
    pub body: Option<String>,
}

/// 追加新墨迹：注释对象 + Form AP（路径描边）+ 页 /Annots + xref。
pub fn append_new_inks(pdf: &[u8], inks: &[NewInk]) -> Result<Vec<u8>, String> {
    if inks.is_empty() {
        return Ok(pdf.to_vec());
    }
    let pages = crate::pdf_index::page_objects(pdf)?;
    let trailer = trailer(pdf)?;
    let mut next_object = trailer.size.max(
        pages
            .iter()
            .map(|page| page.reference.num)
            .max()
            .unwrap_or(0)
            + 1,
    );
    let mut out = pdf.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut entries: Vec<(PdfObjectRef, usize)> = Vec::new();
    let mut by_page: std::collections::BTreeMap<usize, Vec<PdfObjectRef>> =
        std::collections::BTreeMap::new();

    for ink in inks {
        let page = pages
            .get(ink.page as usize)
            .ok_or_else(|| format!("页号 {} 越界", ink.page))?;
        let annotation_number = next_object;
        next_object += 1;
        let ap_number = next_object;
        next_object += 1;
        let half = ink.width.max(0.0) * 0.5;
        let bbox = pdfcore::prelude::Rect::new(
            ink.rect.x - half,
            ink.rect.y - half,
            ink.rect.width + ink.width,
            ink.rect.height + ink.width,
        );
        let alpha = ink.opacity.clamp(0.0, 1.0);
        let mut content = format!(
            "q {:.3} {:.3} {:.3} RG {:.2} w 1 J 1 j {:.3} CA {:.3} ca",
            ink.color[0] as f32 / 255.0,
            ink.color[1] as f32 / 255.0,
            ink.color[2] as f32 / 255.0,
            ink.width.max(0.1),
            alpha,
            alpha
        );
        if let Some(first) = ink.points.first() {
            content.push_str(&format!(
                " {:.2} {:.2} m",
                number_f32(first.x),
                number_f32(first.y)
            ));
            for point in &ink.points[1..] {
                content.push_str(&format!(
                    " {:.2} {:.2} l",
                    number_f32(point.x),
                    number_f32(point.y)
                ));
            }
            content.push_str(" S Q");
        } else {
            content = "q Q".to_string();
        }
        let ap = format!(
            "<</Type/XObject/Subtype/Form/FormType 1/BBox[{:.2} {:.2} {:.2} {:.2}]/Matrix[1 0 0 1 0 0]/Resources<</ExtGState<</GS<</Type/ExtGState/CA {:.3}/ca {:.3}>>>>>>/Length {}>>\nstream\n{}\nendstream\nendobj\n",
            number_f32(bbox.x),
            number_f32(bbox.y),
            number_f32(bbox.x + bbox.width),
            number_f32(bbox.y + bbox.height),
            alpha,
            alpha,
            content.len(),
            content
        );
        let offset = out.len();
        out.extend_from_slice(format!("{ap_number} 0 obj\n").as_bytes());
        out.extend_from_slice(ap.as_bytes());
        entries.push((
            PdfObjectRef {
                num: ap_number,
                gen: 0,
            },
            offset,
        ));

        let mut ink_list = String::from("[");
        for (i, point) in ink.points.iter().enumerate() {
            if i > 0 {
                ink_list.push(' ');
            }
            ink_list.push_str(&format!(
                "{:.2} {:.2}",
                number_f32(point.x),
                number_f32(point.y)
            ));
        }
        ink_list.push(']');
        let mut dict = format!(
            "<</Type/Annot/Subtype/Ink/Rect[{:.2} {:.2} {:.2} {:.2}]/InkList[{}]/C[{:.3} {:.3} {:.3}]/CA {:.3}/Border[0 0 {:.2}]/F 4/AP<</N {ap_number} 0 R>>",
            number_f32(ink.rect.x),
            number_f32(ink.rect.y),
            number_f32(ink.rect.x + ink.rect.width),
            number_f32(ink.rect.y + ink.rect.height),
            ink_list,
            ink.color[0] as f32 / 255.0,
            ink.color[1] as f32 / 255.0,
            ink.color[2] as f32 / 255.0,
            alpha,
            ink.width.max(0.1)
        );
        if let Some(id) = ink.id {
            dict.push_str(&format!("/NM(pd-fr:{id})"));
        }
        if let Some(body) = &ink.body {
            dict.push_str(&format!("/Contents <{}>", utf16_hex(body)));
        }
        dict.push_str(&format!(
            "/P {} {} R>>",
            page.reference.num, page.reference.gen
        ));
        let offset = out.len();
        out.extend_from_slice(format!("{annotation_number} 0 obj\n").as_bytes());
        out.extend_from_slice(dict.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
        let reference = PdfObjectRef {
            num: annotation_number,
            gen: 0,
        };
        entries.push((reference, offset));
        by_page
            .entry(ink.page as usize)
            .or_default()
            .push(reference);
    }

    for (page_index, refs) in by_page {
        let page = &pages[page_index];
        let new_dict = append_annots(pdf, &page.dict, &refs)?;
        let offset = out.len();
        out.extend_from_slice(
            format!("{} {} obj\n", page.reference.num, page.reference.gen).as_bytes(),
        );
        out.extend_from_slice(&new_dict);
        out.extend_from_slice(b"\nendobj\n");
        entries.push((page.reference, offset));
    }
    append_xref(pdf, trailer, &mut out, entries)
}
