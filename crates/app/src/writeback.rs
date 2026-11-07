//! `--writeback <job.json>`：导出带标注的 PDF（S5b-1，**子进程入口**）。
//!
//! 这一层只做三件事：读 job JSON → 翻译成 [`backend_pdfium::writeback::WritebackSpec`] →
//! 调 [`backend_pdfium::writeback::write`]。真正的 FFI 与外观生成都在 backend；
//! 为什么必须走子进程、以及 FFI 的完整契约，见 `docs/writeback.md`。
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

use backend_pdfium::pdf_index::PdfObjectRef;
use backend_pdfium::writeback::{
    self as backend, WritebackAnnotation, WritebackPage, WritebackSpec, WritebackUpdate,
};
use pdfcore::annotation::AnnotationKind;
use pdfcore::prelude::{PageIndex, Point, Rect};

use crate::json::{Json, JsonLookup};

/// 执行一个 job 文件，返回给人看的一句话（成功时）。
pub fn run_job(path: &Path) -> Result<String, String> {
    let (spec, incremental, inline, meta) = read_spec_mode(path)?;
    let report = if incremental {
        backend::write_incremental_with_meta(&spec, &inline, &meta)
    } else {
        backend::write_with_inline(&spec, &inline)
    }
    .map_err(|error| error.to_string())?;
    Ok(format!(
        "已导出 {} 条标注 → {}（{} 字节）",
        report.annotations,
        spec.output.display(),
        report.bytes
    ))
}

fn read_spec_mode(
    path: &Path,
) -> Result<
    (
        WritebackSpec,
        bool,
        backend_pdfium::pdf_patch::InlineChanges,
        backend_pdfium::writeback::AnnotationMetadata,
    ),
    String,
> {
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
        let origin = object
            .field("origin")
            .and_then(|value| value.as_array())
            .and_then(|coords| {
                let x = coords.first()?.as_number()?;
                let y = coords.get(1)?.as_number()?;
                Some(Point::new(x, y))
            })
            .unwrap_or(Point::ORIGIN);
        pages.push(WritebackPage {
            page: PageIndex::new(page),
            origin,
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
            updates: parse_updates(root)?,
            deletes: parse_deletes(root)?,
        },
        incremental,
        parse_inline(root)?,
        parse_meta(root),
    ))
}

fn parse_ref(value: &Json) -> Result<PdfObjectRef, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "对象号不是对象".to_string())?;
    Ok(PdfObjectRef {
        num: number(object, "num")? as u32,
        gen: number(object, "gen")? as u16,
    })
}

fn parse_updates(root: &[(String, Json)]) -> Result<Vec<WritebackUpdate>, String> {
    let Some(items) = root.field("updates") else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| "updates 不是数组".to_string())?;
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("updates[{index}] 不是对象"))?;
        let reference = parse_ref(item)?;
        let body = object
            .field("body")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        out.push(WritebackUpdate { reference, body });
    }
    Ok(out)
}

fn parse_deletes(root: &[(String, Json)]) -> Result<Vec<PdfObjectRef>, String> {
    let Some(items) = root.field("deletes") else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| "deletes 不是数组".to_string())?;
    items.iter().map(parse_ref).collect()
}

fn parse_inline(
    root: &[(String, Json)],
) -> Result<backend_pdfium::pdf_patch::InlineChanges, String> {
    let mut changes = backend_pdfium::pdf_patch::InlineChanges::default();
    if let Some(items) = root.field("inline_updates") {
        let items = items
            .as_array()
            .ok_or_else(|| "inline_updates 不是数组".to_string())?;
        for (index, item) in items.iter().enumerate() {
            let object = item
                .as_object()
                .ok_or_else(|| format!("inline_updates[{index}] 不是对象"))?;
            changes
                .updates
                .push(backend_pdfium::pdf_patch::InlineUpdate {
                    page: number(object, "page")? as u32,
                    position: number(object, "position")? as usize,
                    body: object
                        .field("body")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                });
        }
    }
    if let Some(items) = root.field("inline_deletes") {
        let items = items
            .as_array()
            .ok_or_else(|| "inline_deletes 不是数组".to_string())?;
        for (index, item) in items.iter().enumerate() {
            let object = item
                .as_object()
                .ok_or_else(|| format!("inline_deletes[{index}] 不是对象"))?;
            changes
                .deletes
                .push(backend_pdfium::pdf_patch::InlineDelete {
                    page: number(object, "page")? as u32,
                    position: number(object, "position")? as usize,
                });
        }
    }
    Ok(changes)
}

/// 从 job 里读出作者/时间（写在每条 annotation / update 上，缺省即不写）。
fn parse_meta(root: &[(String, Json)]) -> backend_pdfium::writeback::AnnotationMetadata {
    use backend_pdfium::pdf_patch::AnnotationMeta;
    let mut meta = backend_pdfium::writeback::AnnotationMetadata::default();
    if let Some(pages) = root.field("pages").and_then(|value| value.as_array()) {
        for page in pages {
            let Some(object) = page.as_object() else {
                continue;
            };
            let Some(annotations) = object
                .field("annotations")
                .and_then(|value| value.as_array())
            else {
                continue;
            };
            for annotation in annotations {
                let Some(annotation) = annotation.as_object() else {
                    continue;
                };
                let Some(id) = annotation.field("id").and_then(|value| value.as_i64()) else {
                    continue;
                };
                let info = AnnotationMeta {
                    author: annotation
                        .field("author")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    created: annotation.field("created").and_then(|value| value.as_i64()),
                    modified: annotation
                        .field("modified")
                        .and_then(|value| value.as_i64()),
                };
                if info != AnnotationMeta::default() {
                    meta.by_id.insert(id as u64, info);
                }
            }
        }
    }
    if let Some(updates) = root.field("updates").and_then(|value| value.as_array()) {
        for update in updates {
            let Ok(reference) = parse_ref(update) else {
                continue;
            };
            let modified = update
                .as_object()
                .and_then(|object| object.field("modified"))
                .and_then(|value| value.as_i64());
            if let Some(modified) = modified {
                meta.by_ref.insert(
                    reference,
                    AnnotationMeta {
                        modified: Some(modified),
                        ..Default::default()
                    },
                );
            }
        }
    }
    meta
}

fn parse_annotation(value: &Json, context: &str) -> Result<WritebackAnnotation, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} 不是对象"))?;
    let kind_name = string(object, "kind")?;
    // id 是 u64（时间戳量级），不能用 as_number（内部是 f32，会把相邻 id 舍入成同一个）。
    let id = object
        .field("id")
        .and_then(|value| value.as_i64())
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
        "note" => {
            let rect = rect(object, "rect", context)?;
            AnnotationKind::Note { rect }
        }
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

fn rect(object: &[(String, Json)], key: &str, context: &str) -> Result<Rect, String> {
    let numbers = object
        .field(key)
        .and_then(numbers)
        .ok_or_else(|| format!("{context} 缺 {key} 或不是数字数组"))?;
    if numbers.len() != 4 {
        return Err(format!("{context}.{key} 必须是 [x,y,宽,高]"));
    }
    Ok(Rect::new(numbers[0], numbers[1], numbers[2], numbers[3]))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_updates_and_deletes_by_object_ref() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-job-refs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let job = dir.join("job.json");
        std::fs::write(
            &job,
            r#"{"pdf":"in.pdf","output":"out.pdf","pages":[],
                "updates":[{"num":16,"gen":0,"body":"新正文"}],
                "deletes":[{"num":18,"gen":1}]}"#,
        )
        .expect("写 job");
        let (spec, incremental, _, _) = read_spec_mode(&job).expect("解析");
        assert!(!incremental);
        assert_eq!(spec.updates.len(), 1);
        assert_eq!(spec.updates[0].reference.num, 16);
        assert_eq!(spec.updates[0].body.as_deref(), Some("新正文"));
        assert_eq!(spec.deletes.len(), 1);
        assert_eq!((spec.deletes[0].num, spec.deletes[0].gen), (18, 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_author_and_times_from_job() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-job-meta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let job = dir.join("job.json");
        std::fs::write(
            &job,
            r#"{"pdf":"in.pdf","output":"out.pdf",
                "pages":[{"page":0,"annotations":[
                  {"id":3,"kind":"highlight","quads":[[1,2,3,4]],
                   "color":[255,214,0],"opacity":0.35,"body":"正文",
                   "author":"张三","created":1700000000,"modified":1700000600}]}],
                "updates":[{"num":16,"gen":0,"body":"新正文","modified":1700500000}]}"#,
        )
        .expect("写 job");
        let (_, _, _, meta) = read_spec_mode(&job).expect("解析");
        let info = meta.by_id.get(&3).expect("应解析出 id=3 的元数据");
        assert_eq!(info.author.as_deref(), Some("张三"));
        assert_eq!(info.created, Some(1_700_000_000));
        assert_eq!(info.modified, Some(1_700_000_600));
        let reference = backend_pdfium::pdf_index::PdfObjectRef { num: 16, gen: 0 };
        assert_eq!(
            meta.by_ref.get(&reference).and_then(|info| info.modified),
            Some(1_700_500_000)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
