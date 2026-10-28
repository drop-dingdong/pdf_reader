//! 导出带标注的 PDF（S5b-2）：**主进程侧**的编排。
//!
//! 分工：
//! * 本模块：目标路径、job JSON、起子进程、写后冒烟校验、`temp → 最终名` 的原子改名；
//! * 子进程（`app --writeback`）：真正的 FFI 写回（见 [`docs/current/writeback.md`]）。
//!
//! 为什么写回必须在子进程：FFI 绕过 pdfium-render 的全局锁，与后台渲染线程并发会崩（D-124）。
//! 因此这里只调用"另一个进程"，**绝不**在本进程里调 `writeback::write`。
//!
//! 落盘顺序：`<stem>.annotated.pdf.tmp-<pid>` → 校验 → rename 成 `<stem>.annotated.pdf`。
//! 目标已存在时直接拒绝（用户确认：不覆盖）。

use std::path::{Path, PathBuf};

use backend_pdfium::PdfiumBackend;
use pdfcore::annotation::AnnotationKind;
use pdfcore::prelude::{PageIndex, PdfBackend, RenderRequest, Rotation, Scale, Source};

/// 一条要导出的标注（已从 overlay 与正文表里取好）。
#[derive(Debug, Clone)]
pub struct ExportItem {
    /// 标注 id（写回时用作 `/NM` 标记，保证重复写回幂等）。
    pub id: Option<u64>,
    /// 页号。
    pub page: PageIndex,
    /// 种类与几何（页面空间）。
    pub kind: AnnotationKind,
    /// RGB。
    pub color: [u8; 3],
    /// 不透明度（PDF 侧不表达，仅随 job 传递）。
    pub opacity: f32,
    /// 正文（已按 `#annot:<id>` 从 `custom_texts` 取好）。
    pub body: Option<String>,
}

/// 导出结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    /// 最终文件。
    pub output: PathBuf,
    /// 写入的标注条数。
    pub annotations: usize,
    /// 文件字节数。
    pub bytes: u64,
}

/// `<pdf>` → 同目录 `<stem>.annotated.pdf`（例如 `book.pdf` → `book.annotated.pdf`）。
pub fn default_output_path(pdf: &Path) -> PathBuf {
    let stem = pdf
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".to_string());
    pdf.with_file_name(format!("{stem}.annotated.pdf"))
}

/// 目标文件已存在时必须拒绝（不覆盖用户文件）。
pub fn ensure_destination_free(output: &Path) -> Result<(), String> {
    if output.exists() {
        return Err(format!(
            "已存在同名文件，不覆盖：{}（请先改名或删除它）",
            output.display()
        ));
    }
    Ok(())
}

/// 生成子进程的 job JSON：按页分组，路径用绝对路径。
///
/// 手写序列化（与 `persist` 一样的极简风格），正文按 JSON 规则转义。
pub fn job_json(pdf: &Path, output: &Path, items: &[ExportItem], incremental: bool) -> String {
    let mut pages: Vec<(u32, Vec<&ExportItem>)> = Vec::new();
    for item in items {
        let index = item.page.get();
        match pages.iter_mut().find(|(page, _)| *page == index) {
            Some((_, group)) => group.push(item),
            None => pages.push((index, vec![item])),
        }
    }

    let mut out = String::from("{\n");
    out.push_str(&format!("  \"pdf\": {},\n", quote(&pdf.to_string_lossy())));
    out.push_str(&format!(
        "  \"output\": {},\n",
        quote(&output.to_string_lossy())
    ));
    if incremental {
        out.push_str("  \"incremental\": true,\n");
    }
    out.push_str("  \"pages\": [\n");
    for (page_index, (page, group)) in pages.iter().enumerate() {
        out.push_str(&format!("    {{ \"page\": {page}, \"annotations\": [\n"));
        for (item_index, item) in group.iter().enumerate() {
            out.push_str("      ");
            out.push_str(&annotation_json(item));
            if item_index + 1 < group.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("    ] }");
        if page_index + 1 < pages.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn annotation_json(item: &ExportItem) -> String {
    let mut fields: Vec<String> = Vec::new();
    if let Some(id) = item.id {
        fields.push(format!("\"id\": {id}"));
    }
    match &item.kind {
        AnnotationKind::Highlight { quads } => {
            fields.push("\"kind\": \"highlight\"".to_string());
            fields.push(format!("\"quads\": {}", quads_json(quads)));
        }
        AnnotationKind::Underline { quads } => {
            fields.push("\"kind\": \"underline\"".to_string());
            fields.push(format!("\"quads\": {}", quads_json(quads)));
        }
        AnnotationKind::StrikeOut { quads } => {
            fields.push("\"kind\": \"strike_out\"".to_string());
            fields.push(format!("\"quads\": {}", quads_json(quads)));
        }
        AnnotationKind::Ink { points, width } => {
            fields.push("\"kind\": \"ink\"".to_string());
            let points: Vec<String> = points
                .iter()
                .map(|point| format!("[{:.2}, {:.2}]", point.x, point.y))
                .collect();
            fields.push(format!("\"points\": [{}]", points.join(", ")));
            fields.push(format!("\"width\": {width:.2}"));
        }
    }
    fields.push(format!(
        "\"color\": [{}, {}, {}]",
        item.color[0], item.color[1], item.color[2]
    ));
    fields.push(format!("\"opacity\": {:.2}", item.opacity));
    if let Some(body) = &item.body {
        fields.push(format!("\"body\": {}", quote(body)));
    }
    format!("{{ {} }}", fields.join(", "))
}

fn quads_json(quads: &[pdfcore::prelude::Rect]) -> String {
    let quads: Vec<String> = quads
        .iter()
        .map(|quad| {
            format!(
                "[{:.2}, {:.2}, {:.2}, {:.2}]",
                quad.x, quad.y, quad.width, quad.height
            )
        })
        .collect();
    format!("[{}]", quads.join(", "))
}

fn quote(value: &str) -> String {
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

/// 导出：写 job → 起子进程（`--writeback`）→ 校验 → 原子改名。
///
/// * `expected_pages` 用来确认写回没有改变页数；
/// * 校验只做"能重开 + 页数一致 + 能渲染一页"的冒烟检查 —— **不比较像素**：
///   PDFium 的整页渲染在旋转页上对注释有已知的位置偏差（`docs/current/writeback.md` L3）；
/// * 失败时清理临时文件；目标已存在时不做任何事。
pub fn export(
    pdf: &Path,
    output: &Path,
    items: &[ExportItem],
    expected_pages: u32,
) -> Result<ExportReport, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("找不到自身可执行文件：{error}"))?;
    export_with(&executable, pdf, output, items, expected_pages)
}

/// [`export`] 的实现：`executable` 是"子进程要跑的程序"（产品是自身 exe，测试可替换）。
fn export_with(
    executable: &Path,
    pdf: &Path,
    output: &Path,
    items: &[ExportItem],
    expected_pages: u32,
) -> Result<ExportReport, String> {
    // 子进程按"相对 job 文件所在目录"解析路径，而 job 写在系统临时目录里 ——
    // 所以父进程必须先把路径转成绝对路径（用户实测：相对路径被解析成 /tmp/scripts/...）。
    let pdf = absolute_path(pdf)?;
    let output = absolute_path(output)?;
    let (pdf, output) = (pdf.as_path(), output.as_path());
    ensure_destination_free(output)?;
    if !pdf.is_file() {
        return Err(format!("源文件不存在：{}", pdf.display()));
    }

    // 临时文件放在目标同目录：rename 必须同文件系统才是原子的
    let temporary = temporary_path(output);
    let _ = std::fs::remove_file(&temporary);

    // 导出副本也用增量：保留源文件的对象流/外部注释结构。
    // 用户实测：全量重写 Adobe 的 PDF 后，Okular 另存会报“文件有问题”。
    if let Err(error) = run_child(executable, pdf, &temporary, items, true) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }

    let report = finalize(&temporary, output, expected_pages, items);
    if report.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    report
}

/// 起子进程，把 `pdf` 的标注写到 `temporary`（已绝对化的路径）。
fn run_child(
    executable: &Path,
    pdf: &Path,
    temporary: &Path,
    items: &[ExportItem],
    incremental: bool,
) -> Result<(), String> {
    let job_path = std::env::temp_dir().join(format!(
        "pdf-export-{}-{}.json",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::write(&job_path, job_json(pdf, temporary, items, incremental))
        .map_err(|error| format!("写 job {} 失败：{error}", job_path.display()))?;
    let child = std::process::Command::new(executable)
        .arg("--writeback")
        .arg(&job_path)
        .output();
    let _ = std::fs::remove_file(&job_path);
    let child = child.map_err(|error| format!("启动写回子进程失败：{error}"))?;
    if child.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&child.stderr);
    let stdout = String::from_utf8_lossy(&child.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };
    Err(format!("写回子进程失败：{detail}"))
}

/// **原地写回的“暂存”阶段**：把标注写成源文件同目录的临时文件（不替换、不删源）。
///
/// 返回临时文件路径；调用方负责：关闭文档 → 备份 → `rename` → 重开。
/// 失败时临时文件已被清理。
pub fn stage_in_place(
    pdf: &Path,
    items: &[ExportItem],
    expected_pages: u32,
) -> Result<PathBuf, String> {
    let pdf = absolute_path(pdf)?;
    if !pdf.is_file() {
        return Err(format!("源文件不存在：{}", pdf.display()));
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("找不到自身可执行文件：{error}"))?;
    let temporary = temporary_path(&pdf);
    let _ = std::fs::remove_file(&temporary);
    if let Err(error) = run_child(&executable, &pdf, &temporary, items, true) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = smoke_check(&temporary, expected_pages, items) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(temporary)
}

/// 原地写回前的备份路径：`<pdf>.bak-<nanos>`（不覆盖旧备份）。
pub fn backup_path(pdf: &Path) -> PathBuf {
    let name = pdf
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "document.pdf".to_string());
    pdf.with_file_name(format!("{name}.bak-{}", unique_suffix()))
}

/// 校验临时文件并原子改名（失败时**不**删除临时文件，由调用方决定）。
fn finalize(
    temporary: &Path,
    output: &Path,
    expected_pages: u32,
    items: &[ExportItem],
) -> Result<ExportReport, String> {
    smoke_check(temporary, expected_pages, items)?;
    std::fs::rename(temporary, output)
        .map_err(|error| format!("改名到 {} 失败：{error}", output.display()))?;
    let bytes = std::fs::metadata(output)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    Ok(ExportReport {
        output: output.to_path_buf(),
        annotations: items.len(),
        bytes,
    })
}

/// 主进程侧的冒烟校验：能重开、页数一致、能渲染一页。
fn smoke_check(path: &Path, expected_pages: u32, items: &[ExportItem]) -> Result<(), String> {
    let backend = PdfiumBackend::new();
    let opened = backend
        .open(Source::Path(path), None)
        .map_err(|error| format!("导出的文件打不开：{error}"))?;
    let pages = opened.document.info().page_count;
    if pages != expected_pages {
        return Err(format!("页数变了：{expected_pages} → {pages}"));
    }
    // 只渲染一页做冒烟检查；缩放 0.5 控制开销
    let page = items
        .first()
        .map(|item| item.page)
        .unwrap_or(PageIndex::FIRST);
    opened
        .raster
        .render(&RenderRequest::new(page, Scale(0.5), Rotation::None))
        .map_err(|error| format!("导出的文件渲染失败：{error}"))?;
    Ok(())
}

/// 转成绝对路径（只用 `current_dir`，不访问文件系统、不要求存在）。
///
/// 不用 `std::path::absolute`：它 1.79 才稳定，而本项目的 MSRV 是 1.75
/// （clippy 的 `incompatible_msrv` 会拦）。
fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| format!("当前目录不可用（无法绝对化 {}）：{error}", path.display()))
}

fn temporary_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "export.pdf".to_string());
    output.with_file_name(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        unique_suffix()
    ))
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::JsonLookup;

    fn highlight() -> ExportItem {
        ExportItem {
            id: Some(1),
            page: PageIndex::FIRST,
            kind: AnnotationKind::Highlight {
                quads: vec![pdfcore::prelude::Rect::new(10.0, 20.0, 100.0, 12.0)],
            },
            color: [255, 214, 0],
            opacity: 0.35,
            body: Some("第一行\n第二行 \"引号\"".to_string()),
        }
    }

    #[test]
    fn output_path_replaces_the_extension() {
        assert_eq!(
            default_output_path(Path::new("/tmp/book.pdf")),
            PathBuf::from("/tmp/book.annotated.pdf")
        );
        assert_eq!(
            default_output_path(Path::new("no-extension")),
            PathBuf::from("no-extension.annotated.pdf"),
        );
    }

    #[test]
    fn job_json_is_parseable_and_keeps_geometry_and_body() {
        let job = job_json(
            Path::new("/tmp/in.pdf"),
            Path::new("/tmp/out.pdf"),
            &[highlight()],
            false,
        );
        let root = crate::json::Json::parse(&job).expect("job 必须是合法 JSON");
        let root = root.as_object().expect("顶层对象");
        assert_eq!(
            root.field("pdf").and_then(|value| value.as_str()),
            Some("/tmp/in.pdf")
        );
        let pages = root
            .field("pages")
            .and_then(|value| value.as_array())
            .expect("pages");
        assert_eq!(pages.len(), 1);
        let annotations = pages[0]
            .as_object()
            .and_then(|page| page.field("annotations"))
            .and_then(|value| value.as_array())
            .expect("annotations");
        assert_eq!(annotations.len(), 1);
        let annotation = annotations[0].as_object().expect("annotation");
        assert_eq!(
            annotation.field("kind").and_then(|value| value.as_str()),
            Some("highlight")
        );
        assert_eq!(
            annotation.field("body").and_then(|value| value.as_str()),
            Some("第一行\n第二行 \"引号\""),
            "正文必须原样（含换行与引号）"
        );
    }

    #[test]
    fn job_json_groups_multiple_annotations_by_page() {
        let ink = ExportItem {
            id: Some(2),
            page: PageIndex::new(2),
            kind: AnnotationKind::Ink {
                points: vec![
                    pdfcore::prelude::Point::new(1.0, 2.0),
                    pdfcore::prelude::Point::new(3.0, 4.0),
                ],
                width: 2.0,
            },
            color: [25, 25, 25],
            opacity: 1.0,
            body: None,
        };
        let job = job_json(
            Path::new("/tmp/a.pdf"),
            Path::new("/tmp/b.pdf"),
            &[highlight(), ink],
            false,
        );
        let root = crate::json::Json::parse(&job).expect("合法 JSON");
        let pages = root
            .as_object()
            .expect("顶层对象")
            .field("pages")
            .and_then(|value| value.as_array())
            .expect("pages");
        assert_eq!(pages.len(), 2, "两个不同的页应当分成两组");
        assert_eq!(
            pages[1]
                .as_object()
                .and_then(|page| page.field("page"))
                .and_then(|value| value.as_number()),
            Some(2.0)
        );
    }

    #[test]
    fn an_existing_destination_is_refused() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-export-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let target = dir.join("book.annotated.pdf");
        std::fs::write(&target, b"existing").expect("写占位");
        let error = ensure_destination_free(&target).expect_err("已存在必须拒绝");
        assert!(error.contains("不覆盖"), "实际：{error}");
        assert_eq!(
            std::fs::read(&target).expect("读回"),
            b"existing",
            "不能动已有文件"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finalize_checks_and_moves_a_real_written_file_into_place() {
        if backend_pdfium::pdfium().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "pdf-export-finalize-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let source = dir.join("in.pdf");
        std::fs::write(&source, backend_pdfium::fixtures::two_page_pdf()).expect("写样本");
        let temporary = dir.join("book.annotated.pdf.tmp-1");
        let spec = backend_pdfium::writeback::WritebackSpec {
            source,
            output: temporary.clone(),
            pages: vec![backend_pdfium::writeback::WritebackPage {
                page: PageIndex::FIRST,
                annotations: vec![backend_pdfium::writeback::WritebackAnnotation {
                    id: Some(1),
                    kind: AnnotationKind::Highlight {
                        quads: vec![pdfcore::prelude::Rect::new(50.0, 660.0, 200.0, 14.0)],
                    },
                    color: [255, 255, 0],
                    opacity: 0.35,
                    body: Some("导出校验".to_string()),
                }],
            }],
        };
        backend_pdfium::writeback::write(&spec).expect("先写一份真实 PDF");

        let items = [ExportItem {
            id: Some(1),
            page: PageIndex::FIRST,
            kind: AnnotationKind::Highlight {
                quads: vec![pdfcore::prelude::Rect::new(50.0, 660.0, 200.0, 14.0)],
            },
            color: [255, 255, 0],
            opacity: 0.35,
            body: Some("导出校验".to_string()),
        }];
        let output = dir.join("book.annotated.pdf");
        let report = finalize(&temporary, &output, 2, &items).expect("校验并改名");
        assert_eq!(report.annotations, 1);
        assert!(output.is_file(), "最终文件必须存在");
        assert!(!temporary.exists(), "临时文件必须被 rename 掉");
        assert!(report.bytes > 0);

        // 页数不符时必须失败，且**不改名**
        let error =
            finalize(&output, &dir.join("bad.pdf"), 99, &items).expect_err("页数不符必须失败");
        assert!(error.contains("页数变了"), "实际：{error}");
        assert!(!dir.join("bad.pdf").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 子进程失败时：错误里带它的 stderr、临时文件被清理、目标不产生。
    #[cfg(unix)]
    #[test]
    fn a_failing_child_propagates_stderr_and_leaves_no_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "pdf-export-child-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let pdf = dir.join("in.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").expect("写占位");
        let fake_child = dir.join("fake-child.sh");
        std::fs::write(
            &fake_child,
            "#!/bin/sh\necho '假子进程：加密文档被拒绝' >&2\nexit 3\n",
        )
        .expect("写脚本");
        std::fs::set_permissions(&fake_child, std::fs::Permissions::from_mode(0o755))
            .expect("加执行位");

        let output = dir.join("book.annotated.pdf");
        let error = export_with(&fake_child, &pdf, &output, &[highlight()], 1)
            .expect_err("子进程失败必须冒泡");
        assert!(
            error.contains("假子进程：加密文档被拒绝"),
            "错误必须带子进程 stderr：{error}"
        );
        assert!(!output.exists(), "失败不能留下最终文件");
        // 临时文件：扫同目录（名字以目标名开头）
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("读目录")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("book.annotated.pdf.tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "临时文件必须被清理：{leftovers:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回归：父进程传给子进程的 job 里，路径**必须是绝对路径**。
    ///
    /// 用户实测事故：`cargo run -p app -- scripts/replay/x.pdf` 打开文档后导出，
    /// job 里写的是相对路径，子进程按 job 所在目录（/tmp）解析 → `/tmp/scripts/...` 找不到。
    #[cfg(unix)]
    #[test]
    fn job_paths_are_absolutized_before_the_child_resolves_them() {
        use std::os::unix::fs::PermissionsExt;

        let cwd = std::env::current_dir().expect("当前目录");
        // 用一个**相对路径**的已存在文件当源（内容不重要：假子进程不解析 PDF）
        let relative_pdf = Path::new("Cargo.toml");
        assert!(
            relative_pdf.is_file(),
            "测试前提：crate 目录下应有 Cargo.toml"
        );
        let expected_pdf = cwd.join(relative_pdf);

        let dir = std::env::temp_dir().join(format!(
            "pdf-export-absolute-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let fake_child = dir.join("dump-job.sh");
        // 把 job（$2）原样打到 stderr，然后失败退出
        std::fs::write(&fake_child, "#!/bin/sh\ncat \"$2\" >&2\nexit 1\n").expect("写脚本");
        std::fs::set_permissions(&fake_child, std::fs::Permissions::from_mode(0o755))
            .expect("加执行位");

        let error = export_with(
            &fake_child,
            relative_pdf,
            &dir.join("out.pdf"),
            &[highlight()],
            1,
        )
        .expect_err("假子进程必然失败");

        let expected = format!("\"pdf\": \"{}\"", expected_pdf.display());
        assert!(
            error.contains(&expected),
            "job 里的 pdf 必须是绝对路径 {expected:?}，实际错误：{error}"
        );
        assert!(
            !error.contains("\"pdf\": \"Cargo.toml\""),
            "不得把相对路径原样写进 job：{error}"
        );
        assert!(
            error.contains("\"incremental\": true"),
            "导出副本也应走增量保存：{error}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn empty_export_is_allowed_and_reaches_the_child() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "pdf-export-empty-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let pdf = dir.join("in.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").expect("写占位");
        let child = dir.join("fail.sh");
        std::fs::write(&child, "#!/bin/sh\nexit 3\n").expect("写脚本");
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755)).expect("加执行位");
        // 空集合不再被"没有标注"提前拒绝：会走到子进程（然后由假子进程失败）
        let error =
            export_with(&child, &pdf, &dir.join("out.pdf"), &[], 1).expect_err("假子进程必然失败");
        assert!(error.contains("写回子进程失败"), "实际：{error}");
        assert!(!dir.join("out.pdf").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
