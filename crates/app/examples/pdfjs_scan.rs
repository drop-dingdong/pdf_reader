//! 扫描 pdf.js 测试集的注释读写兼容性。
//!
//! 用法（测试集不随仓库分发）：
//! `cargo run -p app --example pdfjs_scan -- pdfjs_test_pdfs /tmp/pdfjs-corpus.tsv`
//!
//! 输出 TSV：path/version/open/pages/readback/skipped/index_ok/mismatch/add_ok/add_seen/error。
//! 这是开发诊断工具，不是门禁；不修改测试集，只在临时目录读写。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use backend_pdfium::pdf_index::{page_annotations, PdfVersion};
use backend_pdfium::pdf_patch::{append_new_annotations, MarkupKind, NewMarkup};
use backend_pdfium::PdfiumBackend;
use pdfcore::prelude::*;

fn collect(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("pdf") {
            out.push(path);
        }
    }
}

fn supported_count(entries: &[backend_pdfium::pdf_index::PageAnnot]) -> usize {
    entries
        .iter()
        .filter(|entry| !entry.hidden && matches!(entry.subtype, Some(1 | 9 | 10 | 12 | 15)))
        .count()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(
        args.first()
            .map(String::as_str)
            .unwrap_or("pdfjs_test_pdfs"),
    );
    let out = PathBuf::from(
        args.get(1)
            .map(String::as_str)
            .unwrap_or("/tmp/pdfjs-corpus.tsv"),
    );
    if backend_pdfium::pdfium().is_err() {
        eprintln!("需要先准备 PDFium：scripts/fetch-pdfium.sh");
        std::process::exit(2);
    }
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let temp = std::env::temp_dir().join(format!("pdfjs-scan-{}", std::process::id()));
    std::fs::create_dir_all(&temp).expect("临时目录");
    let backend = PdfiumBackend::new();
    let mut report = String::from(
        "path\tversion\topen\tpages\treadback\tskipped\tindex_ok\tmismatch\tadd_ok\tadd_seen\terror\n",
    );
    for (index, path) in files.iter().enumerate() {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace(['\n', '\r', '\t'], " ");
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                writeln!(report, "{rel}\t?\t0\t0\t0\t0\t0\t0\t0\t0\trd:{error}").ok();
                continue;
            }
        };
        let version = PdfVersion::document(&bytes)
            .map(|version| version.label())
            .unwrap_or_else(|| "?".into());
        let opened = backend.open(Source::Bytes(&bytes), None);
        let (open, pages, mut error) = match &opened {
            Ok(opened) => (
                1,
                opened
                    .document
                    .page_geometries()
                    .map(|geometries| geometries.len())
                    .unwrap_or(0),
                String::new(),
            ),
            Err(error) => (0, 0, error.to_string().replace(['\n', '\r', '\t'], " ")),
        };
        let readback = backend_pdfium::readback::read(path, None);
        let (count, skipped) = match &readback {
            Ok(readback) => (readback.annotations.len(), readback.skipped),
            Err(e) => {
                error = format!("{error} rb:{e}");
                (0, 0)
            }
        };
        let (index_ok, mismatch) = match (&page_annotations(&bytes), &readback) {
            (Ok(refs), Ok(readback)) => {
                let mut counts = std::collections::BTreeMap::<u32, usize>::new();
                for item in &readback.annotations {
                    *counts.entry(item.page.get()).or_default() += 1;
                }
                let mut mismatch = 0;
                for page in 0..refs.len() as u32 {
                    if supported_count(&refs[page as usize])
                        != counts.get(&page).copied().unwrap_or(0)
                    {
                        mismatch += 1;
                    }
                }
                (1, mismatch)
            }
            (Err(e), _) => {
                error = format!("{error} index:{e}");
                (0, 0)
            }
            _ => (0, 0),
        };
        let mut add_ok = 0;
        let mut add_seen = 0;
        if open == 1 && pages > 0 {
            let quads = vec![pdfcore::prelude::Rect::new(50.0, 50.0, 120.0, 18.0)];
            let markup = NewMarkup {
                page: 0,
                id: Some(9_000_000_000_000_001),
                rect: quads[0],
                quads,
                kind: MarkupKind::Highlight,
                color: [255, 214, 0],
                opacity: 0.35,
                body: Some("corpus".into()),
            };
            match append_new_annotations(&bytes, &[markup], &[], &[], &Default::default()) {
                Ok(patched) => {
                    add_ok = 1;
                    let path = temp.join(format!("{index}.pdf"));
                    if let Err(e) = std::fs::write(&path, &patched) {
                        error = format!("{error} write:{e}");
                    } else if let Ok(readback) = backend_pdfium::readback::read(&path, None) {
                        add_seen = readback
                            .annotations
                            .iter()
                            .filter(|annotation| annotation.id == Some(9_000_000_000_000_001))
                            .count();
                    }
                    let _ = std::fs::remove_file(path);
                }
                Err(e) => error = format!("{error} add:{e}"),
            }
        }
        writeln!(
            report,
            "{rel}\t{version}\t{open}\t{pages}\t{count}\t{skipped}\t{index_ok}\t{mismatch}\t{add_ok}\t{add_seen}\t{error}"
        )
        .ok();
    }
    std::fs::write(&out, report).expect("写报告");
    let _ = std::fs::remove_dir_all(temp);
    println!("扫描 {} 个 PDF，报告：{}", files.len(), out.display());
}
