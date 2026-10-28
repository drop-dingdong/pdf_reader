//! `--writeback <job.json>`：导出带标注的 PDF（S5b-1，**子进程入口**）。
//!
//! 这一层只做三件事：读 job JSON → 翻译成 [`backend_pdfium::writeback::WritebackSpec`] →
//! 调 [`backend_pdfium::writeback::write`]。真正的 FFI 与外观生成都在 backend；
//! 为什么必须走子进程、以及 FFI 的完整契约，见 `docs/current/writeback.md` 与 D-124。
//!
//! ## job 格式
//!
//! 路径相对 **job 文件所在目录**；`pdf` 与 `output` 必须不同（覆盖原文件由调用方原子完成）。
//!
//! ```json
//! {
//!   "pdf": "in.pdf",
//!   "output": "out.pdf",
//!   "pages": [
//!     { "page": 0, "annotations": [
//!       { "kind": "ink", "points": [[10, 20], [30, 40]], "width": 2.0,
//!         "color": [25, 25, 25], "opacity": 1.0, "body": "正文" },
//!       { "kind": "highlight", "quads": [[10, 200, 100, 12]],
//!         "color": [255, 214, 0], "opacity": 0.35 }
//!     ] }
//!   ]
//! }
//! ```
//!
//! `kind`：`ink` / `highlight` / `underline` / `strike_out`。几何一律是**未旋转页面空间**
//! （与 `pdfcore::annotation` 相同）；`quads` 是 `[x, y, 宽, 高]`。

use std::path::{Path, PathBuf};

use backend_pdfium::writeback::{
    self as backend, WritebackAnnotation, WritebackPage, WritebackSpec,
};
use pdfcore::annotation::AnnotationKind;
use pdfcore::prelude::{PageIndex, Point, Rect};

use crate::json::{Json, JsonLookup};

/// 执行一个 job 文件，返回给人看的一句话（成功时）。
pub fn run_job(path: &Path) -> Result<String, String> {
    let (spec, incremental) = read_spec_mode(path)?;
    let report = if incremental {
        backend::write_incremental(&spec)
    } else {
        backend::write(&spec)
    }
    .map_err(|error| error.to_string())?;
    Ok(format!(
        "已导出 {} 条标注 → {}（{} 字节）",
        report.annotations,
        spec.output.display(),
        report.bytes
    ))
}

fn read_spec_mode(path: &Path) -> Result<(WritebackSpec, bool), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读 job {} 失败：{error}", path.display()))?;
    let root = Json::parse(&text).map_err(|error| format!("job JSON 解析失败：{error}"))?;
    let root = root
        .as_object()
        .ok_or_else(|| "job 顶层不是对象".to_string())?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));

    let source = resolve(base, string(root, "pdf")?);
    let output = resolve(base, string(root, "output")?);
    let pages_json = root
        .field("pages")
        .ok_or_else(|| "job 缺 pages".to_string())?
        .as_array()
        .ok_or_else(|| "pages 不是数组".to_string())?;

    let mut pages = Vec::with_capacity(pages_json.len());
    for (page_index, item) in pages_json.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("pages[{page_index}] 不是对象"))?;
        let page = number(object, "page")? as u32;
        let annotations_json = object
            .field("annotations")
            .ok_or_else(|| format!("pages[{page_index}] 缺 annotations"))?
            .as_array()
            .ok_or_else(|| format!("pages[{page_index}].annotations 不是数组"))?;
        let mut annotations = Vec::with_capacity(annotations_json.len());
        for (index, annotation) in annotations_json.iter().enumerate() {
            let context = format!("pages[{page_index}].annotations[{index}]");
            annotations.push(parse_annotation(annotation, &context)?);
        }
        pages.push(WritebackPage {
            page: PageIndex::new(page),
            annotations,
        });
    }

    let incremental = root
        .field("incremental")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    Ok((
        WritebackSpec {
            source,
            output,
            pages,
        },
        incremental,
    ))
}

fn parse_annotation(value: &Json, context: &str) -> Result<WritebackAnnotation, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} 不是对象"))?;
    let kind_name = string(object, "kind")?;
    let id = object
        .field("id")
        .and_then(|value| value.as_number())
        .map(|value| value as u64);
    let color = color(object).unwrap_or([255, 214, 0]);
    let opacity = object
        .field("opacity")
        .and_then(|value| value.as_number())
        .unwrap_or(0.35);
    let body = object
        .field("body")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let kind = match kind_name {
        "ink" => {
            let points = points(object, "points", context)?;
            let width = number(object, "width").unwrap_or(2.0);
            AnnotationKind::Ink { points, width }
        }
        "highlight" | "underline" | "strike_out" => {
            let quads = quads(object, "quads", context)?;
            match kind_name {
                "underline" => AnnotationKind::Underline { quads },
                "strike_out" => AnnotationKind::StrikeOut { quads },
                _ => AnnotationKind::Highlight { quads },
            }
        }
        other => return Err(format!("{context}：未知标注种类 {other:?}")),
    };
    Ok(WritebackAnnotation {
        id,
        kind,
        color,
        opacity,
        body,
    })
}

fn points(object: &[(String, Json)], key: &str, context: &str) -> Result<Vec<Point>, String> {
    let list = object
        .field(key)
        .ok_or_else(|| format!("{context} 缺 {key}"))?
        .as_array()
        .ok_or_else(|| format!("{context}.{key} 不是数组"))?;
    let mut points = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let numbers =
            numbers(item).ok_or_else(|| format!("{context}.{key}[{index}] 不是数字数组"))?;
        if numbers.len() != 2 {
            return Err(format!("{context}.{key}[{index}] 必须是 [x,y]"));
        }
        points.push(Point::new(numbers[0], numbers[1]));
    }
    Ok(points)
}

fn quads(object: &[(String, Json)], key: &str, context: &str) -> Result<Vec<Rect>, String> {
    let list = object
        .field(key)
        .ok_or_else(|| format!("{context} 缺 {key}"))?
        .as_array()
        .ok_or_else(|| format!("{context}.{key} 不是数组"))?;
    let mut quads = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let numbers =
            numbers(item).ok_or_else(|| format!("{context}.{key}[{index}] 不是数字数组"))?;
        if numbers.len() != 4 {
            return Err(format!("{context}.{key}[{index}] 必须是 [x,y,宽,高]"));
        }
        quads.push(Rect::new(numbers[0], numbers[1], numbers[2], numbers[3]));
    }
    Ok(quads)
}

fn color(object: &[(String, Json)]) -> Option<[u8; 3]> {
    let numbers = numbers(object.field("color")?)?;
    if numbers.len() != 3 {
        return None;
    }
    Some([
        numbers[0].clamp(0.0, 255.0) as u8,
        numbers[1].clamp(0.0, 255.0) as u8,
        numbers[2].clamp(0.0, 255.0) as u8,
    ])
}

fn numbers(value: &Json) -> Option<Vec<f32>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_number())
        .collect()
}

fn number(object: &[(String, Json)], key: &str) -> Result<f32, String> {
    object
        .field(key)
        .and_then(|value| value.as_number())
        .ok_or_else(|| format!("缺数字字段 {key}"))
}

fn string<'a>(object: &'a [(String, Json)], key: &str) -> Result<&'a str, String> {
    object
        .field(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("缺字符串字段 {key}"))
}

fn resolve(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
