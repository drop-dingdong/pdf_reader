//! S6a：读回 PDF 注释（主进程、串行调用）。
//!
//! 只读 `/Annots`，不渲染；调用方必须保证**没有**并发的 PDFium 调用
//! （主进程里 pdfium-render 的全局锁不覆盖这张裸 FFI 表，见 S6 评估 §3.3）。
//!
//! 读回是**增强**：单条坏注释跳过并计数，不影响打开文档。

use std::ffi::{c_uint, c_ulong, c_void, CString};
use std::path::Path;

use pdfcore::annotation::{Annotation, AnnotationKind};
use pdfcore::prelude::{PageIndex, Point, Rect};

use crate::writeback::{
    read_marker, Ffi, FsPointF, FsQuadPointsF, FsRectF, ANNOT_HIGHLIGHT, ANNOT_INK,
    ANNOT_STRIKEOUT, ANNOT_TEXT, ANNOT_UNDERLINE,
};

/// 一条从 PDF 读回的注释（与模型解耦：id 可能缺失，由调用方决定合并策略）。
#[derive(Debug, Clone)]
pub struct ReadAnnotation {
    /// 所在页。
    pub page: PageIndex,
    /// `/NM = pd-fr:<id>` 里的 id；外部注释为 `None`。
    pub id: Option<u64>,
    /// 种类与几何（未旋转页面空间）。
    pub kind: AnnotationKind,
    /// 颜色。
    pub color: [u8; 3],
    /// 透明度（`/CA`；缺省用种类默认值）。
    pub opacity: f32,
    /// `/Contents`（空串视为无正文）。
    pub body: Option<String>,
    /// `/T` 作者（外部注释可能有）。
    pub author: Option<String>,
    /// `/CreationDate`（Unix 秒，UTC）；解析失败为 `None`。
    pub created: Option<i64>,
    /// `/M`（Unix 秒，UTC）。
    pub modified: Option<i64>,
    /// 墨迹的**全部** path（S6b：外部注释可能多条 path；`kind` 里只放第一条）。
    pub ink_strokes: Vec<Vec<Point>>,
}

/// 一次读回的结果。
#[derive(Debug, Default, Clone)]
pub struct Readback {
    /// 读到的注释。
    pub annotations: Vec<ReadAnnotation>,
    /// 无法读取而跳过的注释数（坏 QuadPoints、空 InkList…）。
    pub skipped: usize,
    /// 外部多笔迹注释里的额外 path 总数（信息性；S6b 会把所有 path 都读进 `ink_strokes`）。
    pub extra_ink_strokes: usize,
}

/// 打开 `path`，逐页读回四种注释；`password` 用于加密文档。
pub fn read(path: &Path, password: Option<&str>) -> Result<Readback, String> {
    // 独立调用（测试/工具）时也要先初始化 PDFium；主程序打开文档时已经初始化过。
    crate::library::pdfium().map_err(|error| error.to_string())?;
    let ffi = Ffi::load().map_err(|error| error.to_string())?;
    let c_path =
        CString::new(path.to_string_lossy().as_ref()).map_err(|_| "路径含 NUL 字节".to_string())?;
    let c_password = match password {
        Some(password) => Some(CString::new(password).map_err(|_| "密码含 NUL 字节".to_string())?),
        None => None,
    };
    unsafe {
        let document = (ffi.load_document)(
            c_path.as_ptr(),
            c_password
                .as_ref()
                .map_or(std::ptr::null(), |password| password.as_ptr()),
        );
        if document.is_null() {
            return Err(format!("打不开 {}（密码错误或文件损坏）", path.display()));
        }
        let result = read_document(&ffi, document);
        (ffi.close_document)(document);
        result
    }
}

unsafe fn read_document(ffi: &Ffi, document: *mut c_void) -> Result<Readback, String> {
    let page_count = (ffi.page_count)(document).max(0);
    let mut out = Readback::default();
    for page_index in 0..page_count {
        let page = (ffi.load_page)(document, page_index);
        if page.is_null() {
            out.skipped += 1;
            continue;
        }
        let count = (ffi.annot_count)(page).max(0);
        for annot_index in 0..count {
            let annot = (ffi.get_annot)(page, annot_index);
            if annot.is_null() {
                out.skipped += 1;
                continue;
            }
            if let Some(annotation) =
                read_annotation(ffi, annot, PageIndex::new(page_index as u32), &mut out)
            {
                out.annotations.push(annotation);
            }
            (ffi.close_annot)(annot);
        }
        (ffi.close_page)(page);
    }
    Ok(out)
}

unsafe fn read_annotation(
    ffi: &Ffi,
    annot: *mut c_void,
    page: PageIndex,
    out: &mut Readback,
) -> Option<ReadAnnotation> {
    // 隐藏 / 不显示的不读（位 2 = Hidden，位 6 = NoView）
    let flags = (ffi.get_flags)(annot);
    if flags & 0b10 != 0 || flags & 0b100000 != 0 {
        return None;
    }
    let subtype = (ffi.get_subtype)(annot) as c_uint;
    if !matches!(
        subtype,
        ANNOT_HIGHLIGHT | ANNOT_UNDERLINE | ANNOT_STRIKEOUT | ANNOT_INK | ANNOT_TEXT
    ) {
        return None; // 链接、Widget、Popup、Squiggly 等：不读
    }

    let mut ink_strokes: Vec<Vec<Point>> = Vec::new();
    let kind = match subtype {
        ANNOT_HIGHLIGHT | ANNOT_UNDERLINE | ANNOT_STRIKEOUT => {
            let quads = read_quads(ffi, annot);
            if quads.is_empty() {
                out.skipped += 1;
                return None;
            }
            match subtype {
                ANNOT_HIGHLIGHT => AnnotationKind::Highlight { quads },
                ANNOT_UNDERLINE => AnnotationKind::Underline { quads },
                _ => AnnotationKind::StrikeOut { quads },
            }
        }
        ANNOT_TEXT => {
            let mut rect = FsRectF::default();
            if (ffi.get_rect)(annot, &mut rect) == 0 {
                out.skipped += 1;
                return None;
            }
            let left = rect.left.min(rect.right);
            let right = rect.left.max(rect.right);
            let bottom = rect.bottom.min(rect.top);
            let top = rect.bottom.max(rect.top);
            if !(right > left && top > bottom) {
                out.skipped += 1;
                return None;
            }
            AnnotationKind::Note {
                rect: Rect::new(left, bottom, right - left, top - bottom),
            }
        }
        _ => {
            let strokes = read_ink(ffi, annot);
            ink_strokes = strokes.clone();
            let Some(points) = strokes.first().cloned() else {
                out.skipped += 1;
                return None;
            };
            if strokes.len() > 1 {
                out.extra_ink_strokes += strokes.len() - 1;
            }
            let mut horizontal = 0.0;
            let mut vertical = 0.0;
            let mut width = 0.0;
            let width = if (ffi.get_border)(annot, &mut horizontal, &mut vertical, &mut width) != 0
                && width.is_finite()
                && width > 0.0
            {
                width
            } else {
                2.0 // 外部注释常常没有 /BS；用我们的默认笔宽
            };
            AnnotationKind::Ink { points, width }
        }
    };

    // 用模型的构造函数做一次几何校验；顺手拿到种类默认色/透明度作回退
    let model = Annotation::new(page, kind.clone())?;
    let (color, opacity) = read_color_and_opacity(ffi, annot, &model);

    Some(ReadAnnotation {
        author: read_string(ffi, annot, "T"),
        created: read_string(ffi, annot, "CreationDate").and_then(|text| parse_pdf_date(&text)),
        modified: read_string(ffi, annot, "M").and_then(|text| parse_pdf_date(&text)),
        page,
        id: read_marker(ffi, annot),
        kind,
        color,
        opacity,
        body: read_string(ffi, annot, "Contents").filter(|text| !text.is_empty()),
        ink_strokes,
    })
}

/// 颜色/透明度：颜色先 `GetColor`（带 AP 时它必失败），失败再从外观流解析 `RG/rg/G/g`；
/// 透明度**优先读显式的 `/CA`**，缺失或 =1 时按种类默认值（高亮 0.35，其它 1.0）。
///
/// 为什么不能直接用 `GetColor` 的 alpha：外部阅读器的高亮大多没有 `/CA`（或写成 1），
/// 但 Okular 等按"高亮默认半透明"绘制；统一成我们自己的 0.35 才能和本软件的高亮一致。
unsafe fn read_color_and_opacity(
    ffi: &Ffi,
    annot: *mut c_void,
    model: &Annotation,
) -> ([u8; 3], f32) {
    let mut red = 0u32;
    let mut green = 0u32;
    let mut blue = 0u32;
    let mut alpha = 0u32;
    let color = if (ffi.get_color)(annot, 0, &mut red, &mut green, &mut blue, &mut alpha) != 0 {
        [clamp_byte(red), clamp_byte(green), clamp_byte(blue)]
    } else if let Some(content) = read_ap(ffi, annot) {
        parse_appearance(&content)
            .map(|(color, _)| color)
            .unwrap_or(model.color)
    } else {
        model.color
    };

    let explicit = read_number(ffi, annot, "CA");
    let fallback = match model.kind {
        AnnotationKind::Highlight { .. } => 0.35,
        _ => 1.0,
    };
    let opacity = match explicit {
        Some(value) if value < 0.999 => value.clamp(0.0, 1.0),
        _ => fallback,
    };
    (color, opacity)
}

unsafe fn read_number(ffi: &Ffi, annot: *mut c_void, key: &str) -> Option<f32> {
    let key = CString::new(key).ok()?;
    if (ffi.has_key)(annot, key.as_ptr()) == 0 {
        return None;
    }
    let mut value = 0.0f32;
    if (ffi.get_number_value)(annot, key.as_ptr(), &mut value) == 0 {
        return None;
    }
    value.is_finite().then_some(value)
}

/// 读 AP 内容流：**UTF-16LE**（`FPDFAnnot_GetAP` 的文档语义，不是字节流）。
unsafe fn read_ap(ffi: &Ffi, annot: *mut c_void) -> Option<String> {
    let bytes = (ffi.get_ap)(annot, 0, std::ptr::null_mut(), 0);
    // 2 = "没有这个 appearance mode"（只有 UTF-16 的 BOM/NUL），视为无 AP
    if bytes <= 2 {
        return None;
    }
    let units = bytes as usize / 2 + 1;
    let mut buffer = vec![0u16; units];
    let written = (ffi.get_ap)(
        annot,
        0,
        buffer.as_mut_ptr() as *mut c_void,
        (buffer.len() * 2) as c_ulong,
    );
    if written == 0 {
        return None;
    }
    let units = (written as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..units]);
    let text = text.trim_end_matches('\0').to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// 取**最后一个**颜色算子（描边优先）；alpha 由 `read_number("CA")` 负责。
fn parse_appearance(content: &str) -> Option<([u8; 3], Option<f32>)> {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let mut color: Option<[u8; 3]> = None;
    let mut opacity: Option<f32> = None;
    for (index, token) in tokens.iter().enumerate() {
        match *token {
            "RG" | "rg" if index >= 3 => {
                if let (Some(r), Some(g), Some(b)) = (
                    unit(tokens[index - 3]),
                    unit(tokens[index - 2]),
                    unit(tokens[index - 1]),
                ) {
                    color = Some([r, g, b]);
                }
            }
            "G" | "g" if index >= 1 => {
                if let Some(gray) = unit(tokens[index - 1]) {
                    color = Some([gray, gray, gray]);
                }
            }
            "CA" | "ca" if index >= 1 => {
                if let Some(value) = number(tokens[index - 1]) {
                    opacity = Some(value.clamp(0.0, 1.0));
                }
            }
            _ => {}
        }
    }
    color.map(|color| (color, opacity))
}

fn number(token: &str) -> Option<f32> {
    token.parse::<f32>().ok().filter(|value| value.is_finite())
}

fn unit(token: &str) -> Option<u8> {
    number(token).map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

unsafe fn read_quads(ffi: &Ffi, annot: *mut c_void) -> Vec<Rect> {
    let count = (ffi.count_attachment_points)(annot);
    let mut quads = Vec::new();
    for index in 0..count {
        let mut quad = FsQuadPointsF::default();
        if (ffi.get_attachment_points)(annot, index, &mut quad) == 0 {
            continue;
        }
        let xs = [quad.x1, quad.x2, quad.x3, quad.x4];
        let ys = [quad.y1, quad.y2, quad.y3, quad.y4];
        if !xs.iter().chain(ys.iter()).all(|value| value.is_finite()) {
            continue;
        }
        let left = xs.iter().copied().fold(f32::INFINITY, f32::min);
        let right = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let bottom = ys.iter().copied().fold(f32::INFINITY, f32::min);
        let top = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if right > left && top > bottom {
            quads.push(Rect::new(left, bottom, right - left, top - bottom));
        }
    }
    quads
}

/// 读出所有 path（S6b：外部墨迹可能一条注释多条 path）。
unsafe fn read_ink(ffi: &Ffi, annot: *mut c_void) -> Vec<Vec<Point>> {
    let paths = (ffi.ink_list_count)(annot);
    let mut strokes = Vec::new();
    for path_index in 0..paths {
        let count = (ffi.ink_list_path)(annot, path_index, std::ptr::null_mut(), 0);
        if count < 2 {
            continue;
        }
        let mut buffer = vec![FsPointF { x: 0.0, y: 0.0 }; count as usize];
        let written = (ffi.ink_list_path)(annot, path_index, buffer.as_mut_ptr(), count);
        buffer.truncate(written as usize);
        let points: Vec<Point> = buffer
            .iter()
            .filter(|point| point.x.is_finite() && point.y.is_finite())
            .map(|point| Point::new(point.x, point.y))
            .collect();
        if points.len() >= 2 {
            strokes.push(points);
        }
    }
    strokes
}

unsafe fn read_string(ffi: &Ffi, annot: *mut c_void, key: &str) -> Option<String> {
    let key = CString::new(key).ok()?;
    let bytes = (ffi.get_string)(annot, key.as_ptr(), std::ptr::null_mut(), 0);
    if bytes == 0 {
        return None;
    }
    let units = bytes as usize / 2 + 1;
    let mut buffer = vec![0u16; units];
    let written = (ffi.get_string)(
        annot,
        key.as_ptr(),
        buffer.as_mut_ptr(),
        (buffer.len() * 2) as c_ulong,
    );
    if written == 0 {
        return None;
    }
    let units = (written as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..units]);
    let text = text.trim_end_matches('\0').to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// 解析 PDF 日期字符串（`D:YYYYMMDDHHmmSS+HH'mm'` / `D:...Z` / 允许 `-` 分隔与缺字段）。
///
/// 返回 Unix 秒（UTC）。时区偏移会折进去，所以不同来源的注释可以放在同一条时间轴上。
pub fn parse_pdf_date(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    let body = trimmed.strip_prefix("D:").unwrap_or(trimmed);
    let bytes = body.as_bytes();
    let mut date_end = body.len();
    let mut offset_minutes = 0i64;
    if let Some(position) = body.find(['Z', 'z']) {
        date_end = position;
    } else {
        // 从第 8 个字符之后找 '+'/'-'：避免把 2026-10-31 里的连字符当成时区。
        for index in 8..bytes.len() {
            if bytes[index] == b'+' || bytes[index] == b'-' {
                date_end = index;
                let sign = if bytes[index] == b'+' { 1 } else { -1 };
                let tail = &body[index + 1..];
                let digits: String = tail.chars().filter(char::is_ascii_digit).collect();
                if digits.len() >= 2 {
                    let hours: i64 = digits[0..2].parse().ok()?;
                    let minutes: i64 = if digits.len() >= 4 {
                        digits[2..4].parse().ok()?
                    } else {
                        0
                    };
                    offset_minutes = sign * (hours * 60 + minutes);
                }
                break;
            }
        }
    }
    let digits: String = body[..date_end]
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let number = |start: usize, len: usize| -> Option<i64> {
        if digits.len() < start + len {
            None
        } else {
            digits[start..start + len].parse().ok()
        }
    };
    let year = number(0, 4)?;
    let month = number(4, 2).unwrap_or(1);
    let day = number(6, 2).unwrap_or(1);
    let hour = number(8, 2).unwrap_or(0);
    let minute = number(10, 2).unwrap_or(0);
    let second = number(12, 2).unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(
        days_from_civil(year, month as u32, day as u32) * 86_400
            + hour * 3_600
            + minute * 60
            + second
            - offset_minutes * 60,
    )
}

/// 公历日期 → 距 1970-01-01 的天数（Howard Hinnant 的 `days_from_civil`）。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn clamp_byte(value: c_uint) -> u8 {
    value.min(255) as u8
}
