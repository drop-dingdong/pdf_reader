//! S8a：整理副本的端到端测试（需要 PDFium）。
//!
//! 与写回测试一样，本文件的测试用一把互斥锁串行（读注释的裸 FFI 绕过 pdfium-render 的全局锁）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use backend_pdfium::pdf_compact::{compact, CompactError};
use backend_pdfium::writeback::count_annotations;
use backend_pdfium::PdfiumBackend;
use pdfcore::prelude::*;

static LOCK: Mutex<()> = Mutex::new(());

fn backend_available() -> bool {
    backend_pdfium::pdfium().is_ok()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/s6b")
        .join(name)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pdf-compact-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// 逐页渲染（Scale 0.5）后取像素字节，用来对比"内容有没有变"。
fn render_pages(path: &Path, pages: u32) -> Vec<Vec<u8>> {
    let backend = PdfiumBackend::new();
    let opened = backend.open(Source::Path(path), None).expect("打开 PDF");
    (0..pages)
        .map(|index| {
            opened
                .raster
                .render(&RenderRequest::new(
                    PageIndex::new(index),
                    Scale(0.5),
                    Rotation::None,
                ))
                .expect("渲染")
                .data()
                .to_vec()
        })
        .collect()
}

/// 所有真实阅读器样本：整理后页数与逐页注释数必须完全一致，且不比原文件大。
#[test]
fn compaction_keeps_pages_and_annotations() {
    if !backend_available() {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let dir = temp_dir("samples");
    for name in [
        "okular.pdf",
        "okular_without_content.pdf",
        "evince.pdf",
        "firefox.pdf",
        "adobe.pdf",
        "foxit.pdf",
    ] {
        let source = fixture(name);
        let bytes = std::fs::read(&source).expect("读样本");
        let output = compact(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        let path = dir.join(format!("{name}.compact.pdf"));
        std::fs::write(&path, &output.bytes).expect("写整理结果");

        eprintln!(
            "[{name}] {} → {}，objects={} dropped={}",
            bytes.len(),
            output.bytes.len(),
            output.objects,
            output.dropped
        );
        let before = count_annotations(&source).expect("数原文件注释");
        let after = count_annotations(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(before, after, "{name}: 逐页注释数应一致");

        // 内容也必须逐像素一致（字体/内容流/图片都在活对象里）。
        let pages = before.len() as u32;
        let original_pixels = render_pages(&source, pages);
        let compacted_pixels = render_pages(&path, pages);
        for (index, (left, right)) in original_pixels.iter().zip(&compacted_pixels).enumerate() {
            assert!(
                left == right,
                "{name}: 第 {} 页渲染结果应完全一致",
                index + 1
            );
        }
        if output.dropped > 0 {
            assert!(
                output.bytes.len() < bytes.len(),
                "{name}: 有旧修订可丢时应该变小（{} → {}）",
                bytes.len(),
                output.bytes.len()
            );
        } else {
            assert!(
                output.bytes.len() <= bytes.len() + 512,
                "{name}: 没有旧修订时只允许 xref 流的少量开销（{} → {}）",
                bytes.len(),
                output.bytes.len()
            );
        }
        assert!(
            contains(&output.bytes, b"/Type/XRef"),
            "{name}: 应输出 xref 流"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// 对象流（Adobe 的 `/ObjStm`）必须原样保留 —— 这正是"不用 PDFium 全量保存"的原因。
#[test]
fn compaction_keeps_object_streams() {
    let bytes = std::fs::read(fixture("adobe.pdf")).expect("读样本");
    assert!(contains(&bytes, b"/ObjStm"), "样本本身应有对象流");
    let output = compact(&bytes).expect("整理");
    assert!(
        contains(&output.bytes, b"/ObjStm"),
        "整理后对象流必须还在（否则就退化成全量展开了）"
    );
    assert_eq!(
        output.objects, 44,
        "线性化提示对象应被去掉，其余对象保持一致"
    );
    assert!(
        !contains(&output.bytes, b"/Linearized"),
        "重写后不再是线性化结构，/Linearized 必须去掉"
    );
}

/// 整理结果可以再次整理（幂等/可重入），且不会丢对象。
#[test]
fn compaction_is_reentrant() {
    let bytes = std::fs::read(fixture("firefox.pdf")).expect("读样本");
    let once = compact(&bytes).expect("第一次整理");
    let twice = compact(&once.bytes).expect("第二次整理");
    assert_eq!(twice.dropped, 0, "单段文件不应再有旧修订可丢");
    assert_eq!(twice.objects, once.objects);
    assert!(
        twice.bytes.len() <= once.bytes.len() + 16,
        "二次整理不应变大：{} → {}",
        once.bytes.len(),
        twice.bytes.len()
    );
}

/// 带签名的文档直接拒绝（`/ByteRange` 是签名域的强特征）。
#[test]
fn signed_pdf_is_refused() {
    let mut bytes = std::fs::read(fixture("okular.pdf")).expect("读样本");
    bytes.extend_from_slice(b"\n% /ByteRange [0 0 0 0]\n");
    assert_eq!(compact(&bytes).unwrap_err(), CompactError::Signed);
}

/// 带 `/SigFlags` 的 AcroForm 也按"带签名"拒绝。
#[test]
fn sig_flags_is_refused() {
    let mut bytes = std::fs::read(fixture("okular.pdf")).expect("读样本");
    bytes.extend_from_slice(b"\n% /SigFlags 3\n");
    assert_eq!(compact(&bytes).unwrap_err(), CompactError::Signed);
}

/// 加密文档拒绝：把 `/Encrypt` 塞进 trailer。
#[test]
fn encrypted_pdf_is_refused() {
    let bytes = std::fs::read(fixture("okular.pdf")).expect("读样本");
    // 只改**最后**一个 trailer（前面的 trailer 属于旧修订，改了会把整个文件偏移弄乱）。
    let index = bytes
        .windows(b"trailer".len())
        .rposition(|window| window == b"trailer")
        .expect("样本应有 trailer");
    let mut patched = Vec::new();
    patched.extend_from_slice(&bytes[..index]);
    patched.extend_from_slice(b"trailer\n<</Encrypt 99 0 R /Size 999>>\n");
    patched.extend_from_slice(&bytes[index + b"trailer".len()..]);
    assert_eq!(compact(&patched).unwrap_err(), CompactError::Encrypted);
}

/// 空/垃圾输入要给出可读错误，而不是 panic。
#[test]
fn garbage_input_is_rejected() {
    assert!(matches!(
        compact(b"not a pdf"),
        Err(CompactError::Invalid(_))
    ));
    assert!(matches!(compact(b""), Err(CompactError::Invalid(_))));
}

/// 整理后的文件要能被我们自己的写回解析器接受（找对象/注释），
/// 这条是"整理 → 继续写回"的地基。
#[test]
fn compacted_file_still_parses_for_writeback() {
    let bytes = std::fs::read(fixture("adobe.pdf")).expect("读样本");
    let output = compact(&bytes).expect("整理");
    let path = temp_dir("parse").join("out.pdf");
    std::fs::write(&path, &output.bytes).expect("写文件");
    let pages = backend_pdfium::pdf_index::page_annotations(&output.bytes)
        .unwrap_or_else(|error| panic!("整理结果的注释引用应可解析：{error}"));
    assert_eq!(pages.len(), 2, "应仍是 2 页");
    assert!(
        pages
            .iter()
            .any(|page| page.iter().any(|entry| entry.reference.is_some())),
        "页面上应仍有独立对象形式的注释"
    );
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

/// 偏移/对象号校验：xref 指向错位的文件要拒绝，而不是把野字节抄进输出。
#[test]
fn wrong_object_header_is_rejected() {
    let mut bytes = std::fs::read(fixture("okular.pdf")).expect("读样本");
    // 找最后一个 startxref 指向的经典 xref 表；把表里**第一个 n 条目**的偏移 +1。
    let marker = bytes
        .windows(b"startxref".len())
        .rposition(|window| window == b"startxref")
        .expect("应有 startxref");
    let mut cursor = marker + b"startxref".len();
    while bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let mut end = cursor;
    while bytes[end].is_ascii_digit() {
        end += 1;
    }
    let table = std::str::from_utf8(&bytes[cursor..end])
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!(bytes[table..].starts_with(b"xref"));

    // 在表里找第一个 " n" 类型条目（10 位偏移 + 空格 + 5 位 gen + 空格 + 类型）。
    let mut probe = table;
    let entry = loop {
        let found = bytes[probe..]
            .windows(3)
            .position(|window| window == b" n\n" || window == b" n\r")
            .map(|offset| probe + offset);
        let Some(found) = found else {
            panic!("找不到 n 条目")
        };
        // "0000000766 00000 n\r\n"：n 前面的偏移从 found-16 开始
        if found >= 16
            && bytes[found - 16..found - 6]
                .iter()
                .all(|b| b.is_ascii_digit())
        {
            break found - 16;
        }
        probe = found + 1;
    };
    let offset = std::str::from_utf8(&bytes[entry..entry + 10])
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let shifted = format!("{:010}", offset + 1);
    bytes[entry..entry + 10].copy_from_slice(shifted.as_bytes());
    assert!(
        matches!(compact(&bytes), Err(CompactError::Invalid(_))),
        "偏移对不上必须拒绝"
    );
}

#[allow(dead_code)]
fn dump(path: &Path) {
    let _ = std::fs::read(path);
}

/// 造一份最小 PDF：对象 4 是流，按要求给 `/Length`（可以让它重复/错误）。
fn tiny_pdf(lengths: &[usize]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let objects: Vec<(u32, &[u8])> = vec![
        (1, b"<</Type/Catalog/Pages 2 0 R>>"),
        (2, b"<</Type/Pages/Kids[3 0 R]/Count 1>>"),
        (
            3,
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>",
        ),
    ];
    for (number, body) in &objects {
        offsets.push((*number, out.len()));
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let mut dict = String::from("<</");
    for length in lengths {
        dict.push_str(&format!("/Length {length} "));
    }
    dict.push_str("/Filter/FlateDecode>>");
    offsets.push((4, out.len()));
    out.extend_from_slice(b"4 0 obj\n");
    out.extend_from_slice(dict.as_bytes());
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(b"HELLO");
    out.extend_from_slice(b"\nendstream\nendobj\n");
    offsets.push((5, out.len()));
    out.extend_from_slice(b"5 0 obj\n<</Marker(after)>>\nendobj\n");
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    offsets.sort_by_key(|(number, _)| *number);
    for (_, offset) in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<</Size 6 /Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// 重复 `/Length` 必须取最后一个；否则流会越过自己的末尾，把后面的对象吞进来
/// （真实样本 `rotated_2p_okular.pdf` 的对象 26 1 就是这样）。
#[test]
fn duplicate_length_does_not_swallow_following_objects() {
    let pdf = tiny_pdf(&[99, 5]);
    let output = compact(&pdf).expect("应能整理");
    let text = output.bytes;
    let marker = b"\n5 0 obj";
    assert_eq!(
        text.windows(marker.len()).filter(|w| *w == marker).count(),
        1,
        "对象 5 只能出现一次（不能被对象的流吞进去）"
    );
    // 整理结果仍可再整理（说明 xref/对象头一致）。
    compact(&text).expect("二次整理");
}

/// 如果最后一个 `/Length` 也不对，必须**拒绝**，而不是猜一个 endstream。
#[test]
fn inconsistent_length_is_refused() {
    let pdf = tiny_pdf(&[99]);
    assert!(matches!(compact(&pdf), Err(CompactError::Invalid(_))));
}
