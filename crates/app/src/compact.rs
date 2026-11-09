//! S8a：整理副本（压缩增量）的入口与后置校验。
//!
//! 真正的字节处理在 [`backend_pdfium::pdf_compact`]；这里负责：
//! 读文件 → 整理 → **不比原文件小就不落盘** → 写临时文件 → 校验 → 原子改名。
//!
//! 校验口径与导出的冒烟检查一致，另外加两条更强断言：
//! 逐页注释数一致、首页渲染结果逐像素一致（内容流/字体丢了会立刻暴露）。

use std::path::{Path, PathBuf};

use backend_pdfium::pdf_compact::compact;
use backend_pdfium::writeback::count_annotations;
use backend_pdfium::PdfiumBackend;
use pdfcore::prelude::*;

/// 一次整理的结果摘要（给人看 / 给 UI 用）。
#[derive(Debug, Clone)]
pub struct CompactReport {
    /// 源文件字节数。
    pub source_bytes: usize,
    /// 整理后字节数。
    pub output_bytes: usize,
    /// 活对象数。
    pub objects: u32,
    /// 丢弃的旧版本条目数。
    pub dropped: u32,
}

impl CompactReport {
    /// 一行中文摘要。
    pub fn summary(&self, output: &Path) -> String {
        let saved = self.source_bytes.saturating_sub(self.output_bytes);
        let percent = if self.source_bytes == 0 {
            0.0
        } else {
            saved as f64 * 100.0 / self.source_bytes as f64
        };
        format!(
            "已整理：{} → {} 字节（-{percent:.1}%，丢弃 {} 个旧对象）→ {}",
            self.source_bytes,
            self.output_bytes,
            self.dropped,
            output.display()
        )
    }
}

/// 命令行入口：把 `input` 整理成 `output`。
///
/// 目标已存在时拒绝（不覆盖）；整理后不比原文件小就**不写盘**，返回"已经很紧凑"的说明。
pub fn run(input: &Path, output: &Path) -> std::result::Result<String, String> {
    // `count_annotations` 走裸 FFI，调它之前必须已 FPDF_InitLibrary（测试里由 backend() 初始化）。
    backend_pdfium::pdfium().map_err(|error| format!("加载 PDFium 失败：{error}"))?;
    let bytes =
        std::fs::read(input).map_err(|error| format!("读取 {} 失败：{error}", input.display()))?;
    let compacted = compact(&bytes).map_err(|error| error.to_string())?;
    if compacted.bytes.len() >= bytes.len() {
        return Ok(format!(
            "已经很紧凑：{}（{} 字节），未生成副本",
            input.display(),
            bytes.len()
        ));
    }
    if output.exists() {
        return Err(format!("目标已存在，不覆盖：{}", output.display()));
    }
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(format!("目标目录不存在：{}", parent.display()));
        }
    }

    let temporary = temporary_path(output);
    let _ = std::fs::remove_file(&temporary);
    std::fs::write(&temporary, &compacted.bytes)
        .map_err(|error| format!("写临时文件 {} 失败：{error}", temporary.display()))?;
    if let Err(error) = verify(input, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    std::fs::rename(&temporary, output)
        .map_err(|error| format!("改名到 {} 失败：{error}", output.display()))?;

    let report = CompactReport {
        source_bytes: bytes.len(),
        output_bytes: compacted.bytes.len(),
        objects: compacted.objects,
        dropped: compacted.dropped,
    };
    Ok(report.summary(output))
}

/// 后置校验：页数、逐页注释数、首页渲染像素都要一致。
fn verify(source: &Path, output: &Path) -> std::result::Result<(), String> {
    let before = count_annotations(source).map_err(|error| format!("原文件读注释失败：{error}"))?;
    let after =
        count_annotations(output).map_err(|error| format!("整理结果读注释失败：{error}"))?;
    if before != after {
        return Err(format!(
            "逐页注释数变了：{before:?} → {after:?}（已放弃输出）"
        ));
    }

    let backend = PdfiumBackend::new();
    let original = backend
        .open(Source::Path(source), None)
        .map_err(|error| format!("原文件打不开：{error}"))?;
    let compacted = backend
        .open(Source::Path(output), None)
        .map_err(|error| format!("整理结果打不开：{error}"))?;
    let original_pages = original.document.info().page_count;
    let compacted_pages = compacted.document.info().page_count;
    if original_pages != compacted_pages {
        return Err(format!(
            "页数变了：{original_pages} → {compacted_pages}（已放弃输出）"
        ));
    }
    if original_pages > 0 {
        let request = RenderRequest::new(PageIndex::FIRST, Scale(0.5), Rotation::None);
        let left = original
            .raster
            .render(&request)
            .map_err(|error| format!("原文件渲染失败：{error}"))?;
        let right = compacted
            .raster
            .render(&request)
            .map_err(|error| format!("整理结果渲染失败：{error}"))?;
        if left.data() != right.data() {
            return Err("整理结果的第一页与原文件不一致（已放弃输出）".to_string());
        }
    }
    Ok(())
}

fn temporary_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "compact.pdf".to_string());
    output.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

/// 「整理副本」的默认目标：同目录 `<stem>.compact.pdf`（只替换最后一个扩展名）。
pub fn default_path(pdf: &Path) -> PathBuf {
    let stem = pdf
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".to_string());
    pdf.with_file_name(format!("{stem}.compact.pdf"))
}

/// 交互层（对话框）的目标预检：立刻给出可读错误，不必等子进程。
pub fn validate_target(source: &Path, target: &Path) -> std::result::Result<(), String> {
    if target == source {
        return Err("目标路径与原文件相同；整理副本必须写新文件".to_string());
    }
    if target.exists() {
        return Err(format!("目标已存在，不覆盖：{}", target.display()));
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(format!("目标目录不存在：{}", parent.display()));
        }
    }
    Ok(())
}

/// 在**子进程**里跑 `--compact`，返回它打印的一行摘要。
///
/// 为什么必须子进程：整理后的校验用 `count_annotations`（裸 FFI），而 GUI 主进程有渲染线程
/// （见 `docs/writeback.md`）；与写回/导出同一条隔离策略。
pub fn run_child(input: &Path, output: &Path) -> std::result::Result<String, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("找不到自身可执行文件：{error}"))?;
    let child = std::process::Command::new(executable)
        .arg("--compact")
        .arg(input)
        .arg(output)
        .output()
        .map_err(|error| format!("启动整理子进程失败：{error}"))?;
    let stdout = String::from_utf8_lossy(&child.stdout).trim().to_string();
    if !child.status.success() {
        let stderr = String::from_utf8_lossy(&child.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("整理失败（退出码 {:?}）", child.status.code())
        } else {
            stderr
        });
    }
    if stdout.is_empty() {
        return Err("整理子进程没有输出结果".to_string());
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_target_rejects_same_and_existing() {
        let source = Path::new("/tmp/book.pdf");
        assert!(validate_target(source, source).is_err());
        let existing =
            std::env::temp_dir().join(format!("compact-exists-{}.pdf", std::process::id()));
        std::fs::write(&existing, b"%PDF").unwrap();
        assert!(validate_target(source, &existing)
            .unwrap_err()
            .contains("不覆盖"));
        let missing_dir = std::env::temp_dir()
            .join(format!("compact-missing-{}", std::process::id()))
            .join("out.pdf");
        assert!(validate_target(source, &missing_dir)
            .unwrap_err()
            .contains("目录不存在"));
        std::fs::remove_file(&existing).ok();
    }

    #[test]
    fn default_path_keeps_multi_dot_stems() {
        assert_eq!(
            default_path(Path::new("/tmp/a.b.pdf")),
            PathBuf::from("/tmp/a.b.compact.pdf")
        );
        assert_eq!(
            default_path(Path::new("/tmp/book.pdf")),
            PathBuf::from("/tmp/book.compact.pdf")
        );
    }
}
