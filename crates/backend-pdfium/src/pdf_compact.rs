//! S8a：整理副本（去掉历史修订）。
//!
//! ## 做什么
//!
//! 把一份"被增量更新过很多次"的 PDF 整理成**单段文件**：
//!
//! * 按 xref 链（经典 xref 表 / xref 流，沿 `/Prev`、`/XRefStm`）找出每个对象号的
//!   **最后一次定义**；
//! * 保留对象号与对象字节（字典、流原样拷贝），因此所有引用都不需要改；
//! * 丢掉被覆盖的旧版本，重建一个 **xref stream**（type 1 / type 2 / free）。
//!
//! 用 xref stream 而不是经典 xref 表，是为了**保住原文件的对象流（ObjStm）**：
//! 经典 xref 只能表达"文件偏移"，ObjStm 里的对象必须用 type 2 的
//! `(容器对象号, 序号)` 寻址。PDFium 的全量保存会把 ObjStm 展开，文件反而 +45%
//! （见 `docs/architecture.md` §4.6）。
//!
//! ## 不做什么
//!
//! 不重压缩流、不回收不可达对象、不做线性化、不处理加密与签名（这两种直接拒绝）。
//! 只写"整理后的字节"，IO 与落盘由调用方负责。

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::pdf_index::{balanced_end, decompress, find_from, skip_space};

/// 输出文件的 PDF 版本头（xref stream 需要 1.5+）。
const HEADER: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";
/// xref 每个字段的字节宽度：type(1) + field2(4) + field3(2)。
const W: [usize; 3] = [1, 4, 2];
/// 单次解析最多跟随多少个 xref 段（防止损坏文件里的环）。
const MAX_SECTIONS: usize = 4096;

/// 整理失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactError {
    /// 文件结构无法解析（缺 `startxref`、xref 对不上、对象越界等）。
    Invalid(String),
    /// 加密文档：不支持整理。
    Encrypted,
    /// 带数字签名：整理会让签名失效，直接拒绝。
    Signed,
    /// 对象号或文件超过当前实现能表达的 32 位偏移范围。
    TooLarge,
}

impl fmt::Display for CompactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(formatter, "PDF 结构无法解析：{reason}"),
            Self::Encrypted => write!(formatter, "加密文档不支持整理副本"),
            Self::Signed => write!(
                formatter,
                "文档带数字签名，整理会使签名失效（原文件未改动）"
            ),
            Self::TooLarge => write!(formatter, "文件过大，超出当前实现支持的范围"),
        }
    }
}

impl std::error::Error for CompactError {}

/// 整理结果。
#[derive(Debug, Clone)]
pub struct CompactOutput {
    /// 整理后的完整 PDF 字节。
    pub bytes: Vec<u8>,
    /// 活对象的数量（不含 xref 流自身）。
    pub objects: u32,
    /// 被丢弃的旧版本条目数（同一对象号在历史修订里的重复定义）。
    pub dropped: u32,
    /// 源文件字节数（方便调用方比较）。
    pub source_bytes: usize,
}

impl CompactOutput {
    /// 整理后字节数。
    pub fn output_bytes(&self) -> usize {
        self.bytes.len()
    }
}

/// 把 PDF 字节整理成单段文件（不写盘）。
///
/// 解析失败、加密、带签名都会返回错误；调用方拿到字节后自行决定是否落盘
/// （建议：不比原文件小就不要写）。
pub fn compact(pdf: &[u8]) -> Result<CompactOutput, CompactError> {
    if looks_signed(pdf) {
        return Err(CompactError::Signed);
    }
    let parsed = Parsed::parse(pdf)?;
    if parsed.encrypted {
        return Err(CompactError::Encrypted);
    }
    // 先把所有直接对象的字节范围算出来（同时验证 xref 偏移处确实是该对象头）。
    let mut spans: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    for (number, entry) in &parsed.entries {
        if let Entry::InFile { gen, offset } = *entry {
            spans.insert(*number, object_span(pdf, *number, gen, offset)?);
        }
    }
    // 压缩对象的容器必须本身是活的对象流；否则引用会悬空。每个容器只验证一次。
    let mut checked_containers: BTreeMap<u32, bool> = BTreeMap::new();
    for (number, entry) in &parsed.entries {
        let Entry::InStream { container, .. } = *entry else {
            continue;
        };
        let valid = match checked_containers.get(&container) {
            Some(valid) => *valid,
            None => {
                let valid = match spans.get(&container) {
                    Some((start, end)) => is_object_stream(&pdf[*start..*end]),
                    None => false,
                };
                checked_containers.insert(container, valid);
                valid
            }
        };
        if !valid {
            return Err(CompactError::Invalid(format!(
                "对象 {number} 的容器 {container} 不在最新修订里，或不是 /ObjStm"
            )));
        }
    }

    let mut out = Vec::with_capacity(pdf.len());
    out.extend_from_slice(HEADER);
    // 对象号 -> (xref 字段1, 字段2, 类型)
    let mut records: BTreeMap<u32, (u32, u32, u8)> = BTreeMap::new();
    for (number, entry) in &parsed.entries {
        match *entry {
            Entry::Free => {}
            Entry::InFile { gen, .. } => {
                let (start, end) = spans[number];
                // 线性化的提示字典（第一个对象里的 /Linearized）在重写后已经不成立，
                // 留着会让部分阅读器按线性化结构去读；qpdf 也把它删掉。
                if is_linearization_hint(&pdf[start..end]) {
                    continue;
                }
                let here = u32::try_from(out.len()).map_err(|_| CompactError::TooLarge)?;
                records.insert(*number, (here, u32::from(gen), 1));
                out.extend_from_slice(&pdf[start..end]);
                out.push(b'\n');
            }
            Entry::InStream { container, index } => {
                if index > u32::from(u16::MAX) {
                    return Err(CompactError::TooLarge);
                }
                records.insert(*number, (container, index, 2));
            }
        }
    }

    let max_live = records.keys().next_back().copied().unwrap_or(0);
    // 只按"活对象最大号"开表：原文件里可能有大量已删除对象（/Size 很大），
    // 照抄会让我们的 xref 表白占内存。悬空引用本来就指向已删除对象，不受影响。
    let xref_number = max_live.checked_add(1).ok_or(CompactError::TooLarge)?;
    let size = xref_number.checked_add(1).ok_or(CompactError::TooLarge)?;

    // 条目表：0 号永远是 free；其余按 records 填。
    let mut rows = Vec::with_capacity(size as usize * 7);
    for number in 0..size {
        let (kind, field2, field3) = match records.get(&number) {
            Some((first, second, kind)) => (*kind, *first, *second),
            None => (0u8, 0u32, 0u32),
        };
        rows.push(kind);
        rows.extend_from_slice(&field2.to_be_bytes());
        rows.extend_from_slice(&(field3 as u16).to_be_bytes());
    }
    debug_assert_eq!(rows.len(), size as usize * 7);
    let compressed = deflate(&rows)?;

    let xref_offset = u32::try_from(out.len()).map_err(|_| CompactError::TooLarge)?;
    let root = parsed.root.ok_or_else(|| {
        CompactError::Invalid("找不到 /Root（trailer 里没有目录对象）".to_string())
    })?;
    let mut dict = format!(
        "<</Type/XRef/Size {size}/W[{} {} {}]/Root {} {} R",
        W[0], W[1], W[2], root.0, root.1
    )
    .into_bytes();
    if let Some((number, generation)) = parsed.info {
        dict.extend_from_slice(format!("/Info {number} {generation} R").as_bytes());
    }
    if let Some(id) = &parsed.id {
        dict.extend_from_slice(b"/ID ");
        dict.extend_from_slice(id);
    }
    dict.extend_from_slice(format!("/Filter/FlateDecode/Length {}>>", compressed.len()).as_bytes());

    out.extend_from_slice(format!("{xref_number} 0 obj\n").as_bytes());
    out.extend_from_slice(&dict);
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    Ok(CompactOutput {
        bytes: out,
        objects: records.len() as u32,
        dropped: parsed.dropped,
        source_bytes: pdf.len(),
    })
}

/// 签名启发式：签名域一定带 `/ByteRange`（签名覆盖的字节区间），
/// AcroForm 有签名时一般也带 `/SigFlags`。任一出现就按"带签名"处理（宁可拒绝）。
fn looks_signed(pdf: &[u8]) -> bool {
    find_from(pdf, 0, b"/ByteRange").is_some() || find_from(pdf, 0, b"/SigFlags").is_some()
}

fn deflate(data: &[u8]) -> Result<Vec<u8>, CompactError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(data)
        .map_err(|error| CompactError::Invalid(format!("压缩 xref 失败：{error}")))?;
    encoder
        .finish()
        .map_err(|error| CompactError::Invalid(format!("压缩 xref 失败：{error}")))
}

/// 线性化提示字典（`/Linearized 1 ...`）：重写后结构已不是线性化，必须去掉。
fn is_linearization_hint(object: &[u8]) -> bool {
    let Some(dict_start) = find_from(object, 0, b"<<") else {
        return false;
    };
    let Some(dict_end) = balanced_end(object, dict_start) else {
        return false;
    };
    find_from(&object[dict_start..dict_end], 0, b"/Linearized").is_some()
}

fn is_object_stream(object: &[u8]) -> bool {
    let dict_end = find_from(object, 0, b"<<").and_then(|start| balanced_end(object, start));
    let Some(dict_end) = dict_end else {
        return false;
    };
    find_from(&object[..dict_end], 0, b"/ObjStm").is_some()
}

/// 一个对象号在最新修订里的定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// 空闲（被删除）。
    Free,
    /// 直接对象：文件偏移 + generation。
    InFile { gen: u16, offset: u32 },
    /// 对象流里的成员：容器对象号 + 序号。
    InStream { container: u32, index: u32 },
}

/// 沿 xref 链解析出来的整份目录。
#[derive(Debug, Default, Clone)]
struct Parsed {
    entries: BTreeMap<u32, Entry>,
    root: Option<(u32, u16)>,
    info: Option<(u32, u16)>,
    id: Option<Vec<u8>>,
    size_hint: u32,
    encrypted: bool,
    dropped: u32,
}

impl Parsed {
    fn parse(pdf: &[u8]) -> Result<Self, CompactError> {
        let start = last_startxref(pdf)
            .ok_or_else(|| CompactError::Invalid("找不到 startxref".to_string()))?;
        let mut parsed = Parsed::default();
        let mut visited = HashSet::new();
        // 当前修订 → 它的 /XRefStm（同一修订，hybrid 文件）→ /Prev（更旧）。
        let mut pending = vec![start];
        while let Some(offset) = pending.pop() {
            if !visited.insert(offset) {
                continue;
            }
            if visited.len() > MAX_SECTIONS {
                return Err(CompactError::Invalid("xref 链过长".to_string()));
            }
            let mut section = parse_section(pdf, offset)?;
            // hybrid-reference：经典表只覆盖未压缩对象，压缩对象在同一修订的
            // xref 流里；两者合并时**非 free 优先**，再一起参与"新修订优先"。
            if let Some(xrefstm) = section.xrefstm {
                if xrefstm != offset {
                    let extra = parse_section(pdf, xrefstm)?;
                    for (number, entry) in extra.entries {
                        match section.entries.iter_mut().find(|(num, _)| *num == number) {
                            Some(slot) if matches!(slot.1, Entry::Free) => slot.1 = entry,
                            Some(_) => {}
                            None => section.entries.push((number, entry)),
                        }
                    }
                }
            }
            if parsed.root.is_none() {
                parsed.root = section.root;
            }
            if parsed.info.is_none() {
                parsed.info = section.info;
            }
            if parsed.id.is_none() {
                parsed.id = section.id;
            }
            parsed.encrypted |= section.encrypt;
            parsed.size_hint = parsed.size_hint.max(section.size);
            let mut dropped_here = 0u32;
            for (number, entry) in section.entries {
                match parsed.entries.entry(number) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(entry);
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {
                        dropped_here = dropped_here.saturating_add(1);
                    }
                }
            }
            // 放在循环外，避免借用 `parsed.entries` 的同时改 `parsed.dropped`。
            parsed.dropped = parsed.dropped.saturating_add(dropped_here);
            if let Some(prev) = section.prev {
                pending.push(prev);
            }
        }
        if parsed.entries.is_empty() {
            return Err(CompactError::Invalid("xref 里没有任何对象".to_string()));
        }
        Ok(parsed)
    }
}

/// 一个 xref 段（经典表或流）解析出来的一次性数据。
#[derive(Debug, Default)]
struct Section {
    entries: Vec<(u32, Entry)>,
    root: Option<(u32, u16)>,
    info: Option<(u32, u16)>,
    id: Option<Vec<u8>>,
    size: u32,
    encrypt: bool,
    prev: Option<usize>,
    xrefstm: Option<usize>,
}

fn parse_section(pdf: &[u8], offset: usize) -> Result<Section, CompactError> {
    if offset >= pdf.len() {
        return Err(CompactError::Invalid(format!(
            "xref 偏移 {offset} 越界（文件 {} 字节）",
            pdf.len()
        )));
    }
    let mut cursor = offset;
    skip_space(pdf, &mut cursor);
    if pdf[cursor..].starts_with(b"xref") {
        parse_classic(pdf, cursor)
    } else {
        parse_xref_stream(pdf, cursor)
    }
}

fn parse_classic(pdf: &[u8], offset: usize) -> Result<Section, CompactError> {
    let mut cursor = offset + b"xref".len();
    let mut section = Section::default();
    loop {
        skip_space(pdf, &mut cursor);
        if pdf[cursor..].starts_with(b"trailer") {
            cursor += b"trailer".len();
            break;
        }
        let Some(start) = parse_uint(pdf, &mut cursor) else {
            return Err(CompactError::Invalid("xref 子段缺起始对象号".to_string()));
        };
        let Some(count) = parse_uint(pdf, &mut cursor) else {
            return Err(CompactError::Invalid("xref 子段缺条数".to_string()));
        };
        for index in 0..count {
            let Some(field) = parse_uint(pdf, &mut cursor) else {
                return Err(CompactError::Invalid("xref 条目缺偏移".to_string()));
            };
            let Some(gen) = parse_uint(pdf, &mut cursor) else {
                return Err(CompactError::Invalid("xref 条目缺 generation".to_string()));
            };
            skip_space(pdf, &mut cursor);
            let kind = *pdf
                .get(cursor)
                .ok_or_else(|| CompactError::Invalid("xref 条目被截断".to_string()))?;
            cursor += 1;
            let number = start as u32 + index as u32;
            let entry = match kind {
                b'n' => Entry::InFile {
                    gen: u16::try_from(gen).unwrap_or(0),
                    offset: u32::try_from(field).map_err(|_| CompactError::TooLarge)?,
                },
                b'f' => Entry::Free,
                other => {
                    return Err(CompactError::Invalid(format!(
                        "xref 条目类型未知：{}",
                        char::from(other)
                    )))
                }
            };
            section.entries.push((number, entry));
        }
    }
    skip_space(pdf, &mut cursor);
    let dict_start = cursor;
    if pdf[dict_start..].starts_with(b"<<") {
        let dict_end = balanced_end(pdf, dict_start)
            .ok_or_else(|| CompactError::Invalid("trailer 字典不完整".to_string()))?;
        let dict = &pdf[dict_start..dict_end];
        section.root = parse_ref(dict, b"/Root");
        section.info = parse_ref(dict, b"/Info");
        section.id = parse_id(dict);
        section.size = dict_number(dict, b"/Size").unwrap_or(0) as u32;
        section.encrypt = find_from(dict, 0, b"/Encrypt").is_some();
        section.prev = dict_number(dict, b"/Prev").map(|value| value as usize);
        section.xrefstm = dict_number(dict, b"/XRefStm").map(|value| value as usize);
    }
    Ok(section)
}

fn parse_xref_stream(pdf: &[u8], offset: usize) -> Result<Section, CompactError> {
    let mut cursor = offset;
    let _number = parse_uint(pdf, &mut cursor)
        .ok_or_else(|| CompactError::Invalid("xref 流缺对象号".to_string()))?;
    let _gen = parse_uint(pdf, &mut cursor)
        .ok_or_else(|| CompactError::Invalid("xref 流缺 generation".to_string()))?;
    skip_space(pdf, &mut cursor);
    if !pdf[cursor..].starts_with(b"obj") {
        return Err(CompactError::Invalid(format!(
            "偏移 {offset} 既不是 xref 表也不是 xref 流"
        )));
    }
    cursor += 3;
    skip_space(pdf, &mut cursor);
    if !pdf[cursor..].starts_with(b"<<") {
        return Err(CompactError::Invalid("xref 流字典不完整".to_string()));
    }
    let dict_end = balanced_end(pdf, cursor)
        .ok_or_else(|| CompactError::Invalid("xref 流字典不完整".to_string()))?;
    let dict = pdf[cursor..dict_end].to_vec();

    let widths = parse_int_array(&dict, b"/W")
        .ok_or_else(|| CompactError::Invalid("xref 流缺 /W".to_string()))?;
    if widths.len() < 3 || widths[..3].iter().any(|w| *w > 8) {
        return Err(CompactError::Invalid("/W 不合法".to_string()));
    }
    let size = dict_number(&dict, b"/Size")
        .ok_or_else(|| CompactError::Invalid("xref 流缺 /Size".to_string()))? as u32;
    let indexes = match parse_int_array(&dict, b"/Index") {
        Some(values) if values.len() >= 2 => values,
        _ => vec![0, size as usize],
    };
    if indexes.len() % 2 != 0 {
        return Err(CompactError::Invalid("/Index 长度不是偶数".to_string()));
    }

    if let Some(filter) = find_from(&dict, 0, b"/Filter") {
        if find_from(&dict[filter..], 0, b"/FlateDecode").is_none() {
            return Err(CompactError::Invalid(
                "xref 流用了不支持的过滤器（只支持 FlateDecode）".to_string(),
            ));
        }
    }

    // 流数据：优先用 /Length；间接 /Length 就退回 endstream。
    let stream_start = find_from(pdf, dict_end, b"stream")
        .ok_or_else(|| CompactError::Invalid("xref 流缺 stream 关键字".to_string()))?
        + b"stream".len();
    let data_start = skip_eol(pdf, stream_start);
    let length = match dict_number(&dict, b"/Length") {
        Some(length) => length as usize,
        None => find_from(pdf, data_start, b"endstream")
            .map(|end| end.saturating_sub(data_start))
            .ok_or_else(|| CompactError::Invalid("xref 流缺 /Length 与 endstream".to_string()))?,
    };
    let data_end = data_start
        .checked_add(length)
        .filter(|end| *end <= pdf.len())
        .ok_or_else(|| CompactError::Invalid("xref 流数据被截断".to_string()))?;
    let raw = &pdf[data_start..data_end];
    let row_width: usize = widths[..3].iter().sum();
    let data = decode_xref_data(&dict, raw, row_width)?;
    let mut section = Section {
        root: parse_ref(&dict, b"/Root"),
        info: parse_ref(&dict, b"/Info"),
        id: parse_id(&dict),
        size,
        encrypt: find_from(&dict, 0, b"/Encrypt").is_some(),
        prev: dict_number(&dict, b"/Prev").map(|value| value as usize),
        ..Default::default()
    };
    let mut cursor = 0usize;
    for chunk in indexes.chunks(2) {
        let start = chunk[0];
        let count = chunk[1];
        for index in 0..count {
            if cursor + row_width > data.len() {
                return Err(CompactError::Invalid("xref 流条目不足".to_string()));
            }
            let kind = if widths[0] == 0 {
                1u8
            } else {
                read_field(&data[cursor..], widths[0]) as u8
            };
            let field2 = read_field(&data[cursor + widths[0]..], widths[1]);
            let field3 = read_field(&data[cursor + widths[0] + widths[1]..], widths[2]);
            cursor += row_width;
            let number = start + index;
            let number = u32::try_from(number).map_err(|_| CompactError::TooLarge)?;
            let entry = match kind {
                0 => Entry::Free,
                1 => Entry::InFile {
                    gen: u16::try_from(field3).unwrap_or(0),
                    offset: u32::try_from(field2).map_err(|_| CompactError::TooLarge)?,
                },
                2 => Entry::InStream {
                    container: u32::try_from(field2).map_err(|_| CompactError::TooLarge)?,
                    index: u32::try_from(field3).map_err(|_| CompactError::TooLarge)?,
                },
                other => {
                    return Err(CompactError::Invalid(format!(
                        "xref 流条目类型未知：{other}"
                    )))
                }
            };
            section.entries.push((number, entry));
        }
    }
    Ok(section)
}

/// 解出 xref 流的原始字节：FlateDecode + 可选的 PNG/TIFF 预测器。
///
/// Adobe 的 xref 流常用 `/DecodeParms<</Columns 4/Predictor 12>>`（PNG Up），
/// 不处理预测器会把压缩后的增量字节当成条目读，得到"类型 135"这种鬼结果。
fn decode_xref_data(dict: &[u8], raw: &[u8], row_width: usize) -> Result<Vec<u8>, CompactError> {
    let data = if find_from(dict, 0, b"/FlateDecode").is_some() {
        decompress(raw).ok_or_else(|| CompactError::Invalid("xref 流解压失败".to_string()))?
    } else {
        raw.to_vec()
    };
    let Some(parms) = parse_decode_parms(dict)? else {
        return Ok(data);
    };
    let predictor = dict_number(&parms, b"/Predictor").unwrap_or(1);
    let columns = dict_number(&parms, b"/Columns")
        .map(|value| value as usize)
        .unwrap_or(row_width);
    if columns != row_width {
        return Err(CompactError::Invalid(format!(
            "xref 流的 /Columns（{columns}）与 /W 之和（{row_width}）不一致"
        )));
    }
    match predictor {
        0 | 1 => Ok(data),
        2 => Err(CompactError::Invalid(
            "xref 流用了 TIFF 预测器，暂不支持".to_string(),
        )),
        10..=15 => png_unpredict(&data, columns),
        other => Err(CompactError::Invalid(format!(
            "xref 流的预测器 {other} 未知"
        ))),
    }
}

/// `/DecodeParms<</Columns 4/Predictor 12>>`；间接引用不支持。
fn parse_decode_parms(dict: &[u8]) -> Result<Option<Vec<u8>>, CompactError> {
    let Some(index) = find_from(dict, 0, b"/DecodeParms") else {
        return Ok(None);
    };
    let mut cursor = index + b"/DecodeParms".len();
    skip_space(dict, &mut cursor);
    if !dict[cursor..].starts_with(b"<<") {
        return Err(CompactError::Invalid(
            "/DecodeParms 是间接引用或不完整".to_string(),
        ));
    }
    let end = balanced_end(dict, cursor)
        .ok_or_else(|| CompactError::Invalid("/DecodeParms 字典不完整".to_string()))?;
    Ok(Some(dict[cursor..end].to_vec()))
}

/// PNG 预测器反解（PDF 1.7 §7.4.4.4）：每行 1 字节滤波类型 + `columns` 字节数据。
fn png_unpredict(data: &[u8], columns: usize) -> Result<Vec<u8>, CompactError> {
    if columns == 0 {
        return Err(CompactError::Invalid("/Columns 为 0".to_string()));
    }
    let stride = columns + 1;
    if data.len() % stride != 0 {
        return Err(CompactError::Invalid(
            "xref 流长度不是 (Columns+1) 的整数倍".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(data.len() / stride * columns);
    let mut previous = vec![0u8; columns];
    for row in data.chunks_exact(stride) {
        let filter = row[0];
        let mut current = row[1..].to_vec();
        for index in 0..columns {
            let left = if index > 0 { current[index - 1] } else { 0 };
            let up = previous[index];
            let upleft = if index > 0 { previous[index - 1] } else { 0 };
            current[index] = match filter {
                0 => current[index],
                1 => current[index].wrapping_add(left),
                2 => current[index].wrapping_add(up),
                3 => current[index].wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => current[index].wrapping_add(paeth(left, up, upleft)),
                other => {
                    return Err(CompactError::Invalid(format!(
                        "xref 流的 PNG 行滤波类型 {other} 未知"
                    )))
                }
            };
        }
        out.extend_from_slice(&current);
        previous = current;
    }
    Ok(out)
}

fn paeth(left: u8, up: u8, upleft: u8) -> u8 {
    let p = i32::from(left) + i32::from(up) - i32::from(upleft);
    let pa = (p - i32::from(left)).abs();
    let pb = (p - i32::from(up)).abs();
    let pc = (p - i32::from(upleft)).abs();
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        up
    } else {
        upleft
    }
}

/// 大端读取 `width` 字节（最多 8 字节）。
fn read_field(bytes: &[u8], width: usize) -> u64 {
    let mut value = 0u64;
    for byte in bytes.iter().take(width) {
        value = (value << 8) | u64::from(*byte);
    }
    value
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

fn last_startxref(pdf: &[u8]) -> Option<usize> {
    let mut found = None;
    let mut from = 0;
    while let Some(index) = find_from(pdf, from, b"startxref") {
        found = Some(index + b"startxref".len());
        from = index + b"startxref".len();
    }
    let mut cursor = found?;
    skip_space(pdf, &mut cursor);
    parse_uint(pdf, &mut cursor)
}

fn parse_uint(pdf: &[u8], cursor: &mut usize) -> Option<usize> {
    skip_space(pdf, cursor);
    let start = *cursor;
    while *cursor < pdf.len() && pdf[*cursor].is_ascii_digit() {
        *cursor += 1;
    }
    if start == *cursor {
        return None;
    }
    std::str::from_utf8(&pdf[start..*cursor]).ok()?.parse().ok()
}

/// 字典里某个键的全部出现位置（要求键后面不是字母/数字，避免 `/Length` 命中 `/Length1`）。
fn key_positions(dict: &[u8], key: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = find_from(dict, from, key) {
        let after = found + key.len();
        let boundary = match dict.get(after) {
            Some(byte) => !byte.is_ascii_alphanumeric(),
            None => true,
        };
        if boundary {
            out.push(after);
        }
        from = found + key.len();
    }
    out
}

/// 字典里的数字：只接受直接数字（`/Length 5 0 R` 这种引用返回 `None`）。
///
/// 重复键按 PDF 的通行做法**取最后一个**（qpdf 的警告也是这个语义）：有的阅读器
/// （如 Okular）会写出 `/Length 6912 /Length 3945`，取第一个会把流截到下一个
/// `endstream`，把中间的对象一起吞进对象体里。
fn dict_number(dict: &[u8], key: &[u8]) -> Option<u64> {
    for after_key in key_positions(dict, key).into_iter().rev() {
        let mut cursor = after_key;
        let Some(value) = parse_uint(dict, &mut cursor) else {
            continue;
        };
        // `/Length 5 0 R` 是引用，不是数字。
        let mut check = cursor;
        if parse_uint(dict, &mut check).is_some() {
            let mut after = check;
            skip_space(dict, &mut after);
            if dict[after..].starts_with(b"R") {
                return None;
            }
        }
        return Some(value as u64);
    }
    None
}

/// 字典里的 `/Root 12 0 R`（重复键同样取最后一个）。
fn parse_ref(dict: &[u8], key: &[u8]) -> Option<(u32, u16)> {
    for after_key in key_positions(dict, key).into_iter().rev() {
        let mut cursor = after_key;
        let Some(number) = parse_uint(dict, &mut cursor) else {
            continue;
        };
        let Some(generation) = parse_uint(dict, &mut cursor) else {
            continue;
        };
        skip_space(dict, &mut cursor);
        if !dict[cursor..].starts_with(b"R") {
            continue;
        }
        return Some((u32::try_from(number).ok()?, u16::try_from(generation).ok()?));
    }
    None
}

/// 字典里的 `/ID [<..><..>]`（原样保留字节）。
fn parse_id(dict: &[u8]) -> Option<Vec<u8>> {
    let index = find_from(dict, 0, b"/ID")? + b"/ID".len();
    let mut cursor = index;
    skip_space(dict, &mut cursor);
    if dict.get(cursor) != Some(&b'[') {
        return None;
    }
    let end = balanced_end(dict, cursor)?;
    Some(dict[cursor..end].to_vec())
}

/// 字典里的整数数组（`/W [1 4 2]`、`/Index [0 5 7 2]`）。
fn parse_int_array(dict: &[u8], key: &[u8]) -> Option<Vec<usize>> {
    let index = find_from(dict, 0, key)? + key.len();
    let mut cursor = index;
    skip_space(dict, &mut cursor);
    if dict.get(cursor) != Some(&b'[') {
        return None;
    }
    let end = balanced_end(dict, cursor)?;
    let mut values = Vec::new();
    let mut inner = cursor + 1;
    while inner + 1 < end {
        skip_space(dict, &mut inner);
        if dict.get(inner) == Some(&b']') {
            break;
        }
        let value = parse_uint(dict, &mut inner)?;
        values.push(value);
    }
    Some(values)
}

/// 从对象起点到 `endobj` 结束的字节范围（含对象头与 `endobj`）。
fn object_span(
    pdf: &[u8],
    number: u32,
    generation: u16,
    offset: u32,
) -> Result<(usize, usize), CompactError> {
    let start = offset as usize;
    if start >= pdf.len() {
        return Err(CompactError::Invalid(format!(
            "对象 {number} 的偏移 {offset} 越界"
        )));
    }
    let mut cursor = start;
    skip_space(pdf, &mut cursor);
    let parsed_number = parse_uint(pdf, &mut cursor)
        .ok_or_else(|| CompactError::Invalid(format!("对象 {number} 缺对象号")))?;
    let parsed_gen = parse_uint(pdf, &mut cursor)
        .ok_or_else(|| CompactError::Invalid(format!("对象 {number} 缺 generation")))?;
    skip_space(pdf, &mut cursor);
    if !pdf[cursor..].starts_with(b"obj") {
        return Err(CompactError::Invalid(format!(
            "偏移 {offset} 处不是对象头（对象 {number}）"
        )));
    }
    if parsed_number != number as usize || parsed_gen != generation as usize {
        return Err(CompactError::Invalid(format!(
            "偏移 {offset} 处是 {parsed_number} {parsed_gen} obj，不是 {number} {generation} obj"
        )));
    }
    cursor += 3;
    skip_space(pdf, &mut cursor);
    let body_end = if pdf[cursor..].starts_with(b"<<") || pdf.get(cursor) == Some(&b'[') {
        let dict_end = balanced_end(pdf, cursor)
            .ok_or_else(|| CompactError::Invalid(format!("对象 {number} 的字典不完整")))?;
        let mut after = dict_end;
        skip_space(pdf, &mut after);
        if pdf[after..].starts_with(b"stream") {
            let dict = &pdf[cursor..dict_end];
            let data_start = skip_eol(pdf, after + b"stream".len());
            let length = match dict_number(dict, b"/Length") {
                Some(length) => length as usize,
                None => resolve_length(pdf, dict).ok_or_else(|| {
                    CompactError::Invalid(format!("对象 {number} 的 /Length 无法解析"))
                })?,
            };
            let data_end = data_start
                .checked_add(length)
                .filter(|end| *end <= pdf.len())
                .ok_or_else(|| CompactError::Invalid(format!("对象 {number} 的流被截断")))?;
            // `/Length` 必须正好落在 `endstream` 前（中间只允许空白）。对不上说明长度
            // 不可信 —— 继续往后找 endstream 会把中间的对象一起抄进对象体（输出里出现
            // 两份同号对象、注释看起来"丢了"）。这里宁可拒绝整理。
            let mut endstream = data_end;
            while endstream < pdf.len() && pdf[endstream].is_ascii_whitespace() {
                endstream += 1;
            }
            if !pdf[endstream..].starts_with(b"endstream") {
                return Err(CompactError::Invalid(format!(
                    "对象 {number} 的 /Length（{length}）与 endstream 位置不符"
                )));
            }
            endstream + b"endstream".len()
        } else {
            dict_end
        }
    } else {
        // 标量对象（数字/名字/字符串）：直接找 endobj。
        find_from(pdf, cursor, b"endobj")
            .ok_or_else(|| CompactError::Invalid(format!("对象 {number} 缺 endobj")))?
    };
    let endobj = find_from(pdf, body_end, b"endobj")
        .ok_or_else(|| CompactError::Invalid(format!("对象 {number} 缺 endobj")))?;
    Ok((start, endobj + b"endobj".len()))
}

/// 间接 `/Length`：字典里是 `5 0 R`，去对象 5 的内容里读整数。
fn resolve_length(pdf: &[u8], dict: &[u8]) -> Option<usize> {
    let (number, generation) = parse_ref(dict, b"/Length")?;
    let _ = number;
    // 兜底：在文件里找该对象头，再读它的数字内容（不做完整解析，够用即可）。
    let header = format!("{number} {generation} obj");
    let index = find_from(pdf, 0, header.as_bytes())? + header.len();
    let mut cursor = index;
    parse_uint(pdf, &mut cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_number_takes_last_duplicate() {
        // Okular 会写出重复 /Length；PDF 的通行语义是后者生效（qpdf 也是这么警告的）。
        let dict = b"<</Length 6912 /Length 3945 /Filter /FlateDecode>>";
        assert_eq!(dict_number(dict, b"/Length"), Some(3945));
        // 引用不是数字。
        assert_eq!(dict_number(b"<</Length 5 0 R>>", b"/Length"), None);
        // 键边界：/Length1 不能被当成 /Length。
        assert_eq!(dict_number(b"<</Length1 77>>", b"/Length"), None);
    }

    #[test]
    fn object_span_stops_at_its_own_endstream() {
        // 重复 /Length：第一个是错的（999），最后一个是 4。
        let pdf = b"26 1 obj\n<</Length 999 /Length 4>>\nstream\nABCD\nendstream\nendobj\n3 0 obj\n<<>>\nendobj\n";
        let (start, end) = object_span(pdf, 26, 1, 0).expect("应能定位");
        assert_eq!(
            &pdf[start..end],
            b"26 1 obj\n<</Length 999 /Length 4>>\nstream\nABCD\nendstream\nendobj"
        );
        assert!(
            !pdf[start..end].windows(5).any(|w| w == b"3 0 o"),
            "不能把后面的对象吞进来"
        );
    }

    #[test]
    fn mismatched_length_is_rejected() {
        let pdf = b"26 1 obj\n<</Length 99>>\nstream\nABCD\nendstream\nendobj\n";
        assert!(matches!(
            object_span(pdf, 26, 1, 0),
            Err(CompactError::Invalid(_))
        ));
    }
}
