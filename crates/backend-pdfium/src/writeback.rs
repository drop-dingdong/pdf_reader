//! **S5 写回 PDF**：把标注写成真正的 PDF 注释（S5b-1）。
//!
//! 背景与决策见 [`docs/decisions/evaluations/s5-writeback.md`](../../../docs/decisions/evaluations/s5-writeback.md)
//! 与 D-123/D-124；FFI 的完整说明（函数表、ABI、排查步骤）见
//! [`docs/current/writeback.md`](../../../docs/current/writeback.md)。
//!
//! ## 为什么有这一层 FFI
//! `pdfium-render 0.9.4` 没有注释 API，也不暴露 document/page 句柄；而 PDFium 的 C API
//! 提供了创建注释与生成外观的能力。这里用 `libloading` 直接绑定那十几个函数。
//!
//! ## 三条硬约定（改这个文件前先读）
//! 1. **只在子进程里调用**：本模块绕过 `pdfium-render` 的全局锁，与后台渲染线程并发会崩。
//!    产品路径是 `app --writeback <job.json>`（见 `app::writeback`）；
//! 2. **调用方必须先初始化 PDFium**（`backend_pdfium::pdfium()`），本模块不 `Init/Destroy`；
//! 3. **不覆盖输入文件**：本模块只写 `spec.output`；临时文件 + rename + 校验由调用方负责。
//!
//! ## 四种标注怎么写
//! | 种类 | 语义字段 | 外观（`/AP`） |
//! | --- | --- | --- |
//! | 墨迹 | `FPDFAnnot_AddInkStroke` → `/InkList` | `SetAP`：每条笔画一个子路径（页面坐标） |
//! | 高亮 | `FPDFAnnot_AppendAttachmentPoints` → `/QuadPoints` | **不写**：写了就是不透明填充，会盖住文字 |
//! | 下划线 | 同上 | `SetAP`：每个 quad 底部一条线 |
//! | 删除线 | 同上 | `SetAP`：每个 quad 中线一条线 |
//!
//! 实测依据（无 AP 时的渲染结果）：墨迹不可见（1 px）、下划线 0 px、删除线只覆盖文字（24 px）、
//! 高亮可被 PDFium 合成（文字仍可读）。详见评估 §12。

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::os::raw::c_float;
use std::path::{Path, PathBuf};

use pdfcore::annotation::{Annotation, AnnotationKind};
use pdfcore::prelude::{PageIndex, Point, Rect};

use crate::pdf_index::{page_annotations, PdfObjectRef};

// ============================================================ 对外类型

/// 一次写回任务。
#[derive(Debug, Clone)]
pub struct WritebackSpec {
    /// 源 PDF（只读，**绝不覆盖**）。
    pub source: PathBuf,
    /// 输出 PDF（通常是 `<原名>.annotated.pdf` 或临时文件）。
    pub output: PathBuf,
    /// 逐页要写入的标注。
    pub pages: Vec<WritebackPage>,
    /// 外部注释正文更新（对象号定位；只改 `/Contents`）。
    pub updates: Vec<WritebackUpdate>,
    /// 外部注释删除（对象号定位；Popup 子对象一并删）。
    pub deletes: Vec<PdfObjectRef>,
}

/// 一条按对象号定位的正文更新。
#[derive(Debug, Clone)]
pub struct WritebackUpdate {
    /// 注释对象号。
    pub reference: PdfObjectRef,
    /// 新正文（空 = 删除 `/Contents`）。
    pub body: Option<String>,
}

/// 某一页要写入的标注。
#[derive(Debug, Clone)]
pub struct WritebackPage {
    /// 页号（0 起）。
    pub page: PageIndex,
    /// 该页的标注（顺序即写入顺序）。
    pub annotations: Vec<WritebackAnnotation>,
}

/// 一条要写进 PDF 的标注。
///
/// 几何与 `pdfcore::annotation` 完全一致（**未旋转页面空间**）；`body` 是注释正文，
/// 写到 PDF 的 `/Contents`。
#[derive(Debug, Clone)]
pub struct WritebackAnnotation {
    /// 标注 id（用于 `/NM` 标记，保证重复写回不会叠两遍）。`None` 表示不打标记。
    pub id: Option<u64>,
    /// 种类与几何。
    pub kind: AnnotationKind,
    /// RGB。
    pub color: [u8; 3],
    /// 不透明度（PDF 侧**无法**通过公开 API 表达，目前仅记录，见模块文档）。
    pub opacity: f32,
    /// 正文（写到 `/Contents`，UTF-16LE）。
    pub body: Option<String>,
}

/// 写回结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritebackReport {
    /// 实际写入的注释条数。
    pub annotations: usize,
    /// 输出文件字节数。
    pub bytes: u64,
}

/// 写回失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritebackError {
    /// 文件读写失败（含"输出与源相同"这类契约错误）。
    Io(String),
    /// 找不到/加载不了 PDFium 动态库。
    Library(String),
    /// 任务本身不合法（页号越界、几何退化、输出路径等于源）。
    Invalid(String),
    /// PDFium 调用失败；`stage` 指出是哪一步，便于按文档排查。
    Pdfium {
        /// 失败发生的位置（例如 `FPDFPage_CreateAnnot`）。
        stage: &'static str,
        /// 细节。
        message: String,
    },
}

impl std::fmt::Display for WritebackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "文件操作失败：{message}"),
            Self::Library(message) => write!(formatter, "PDFium 加载失败：{message}"),
            Self::Invalid(message) => write!(formatter, "写回任务不合法：{message}"),
            Self::Pdfium { stage, message } => {
                write!(formatter, "PDFium 调用失败（{stage}）：{message}")
            }
        }
    }
}

impl std::error::Error for WritebackError {}

/// 本模块的 `Result` 别名。
pub type WritebackResult<T> = std::result::Result<T, WritebackError>;

// ============================================================ 写回入口

/// 把 `spec` 里的标注写进 `spec.output`，并**重开输出文件核对注释数量**。
///
/// ## 前置条件
/// * 本进程已初始化 PDFium（`backend_pdfium::pdfium()`）；
/// * 当前进程里**没有其它线程**在使用 PDFium（子进程模型）；
/// * `spec.output != spec.source`（不允许原地覆盖；原子替换由调用方做）。
///
/// ## 行为
/// * 逐页 `FPDFPage_CreateAnnot` + 语义字段 + `SetAP` 外观（高亮除外）；
/// * `FPDF_SaveAsCopy(..., FPDF_NO_INCREMENTAL)` 全量重写；
/// * 保存后重开输出、逐页核对"原有注释 + 新增"的数量与页数；不一致即报错。
pub fn write(spec: &WritebackSpec) -> WritebackResult<WritebackReport> {
    write_with_mode(spec, false)
}

/// 增量保存：把改动追加到原文件之后（保留原有对象/外部注释的原始结构）。
///
/// 原地写入用它 —— PDFium 全量重写会把 Adobe 等阅读器的对象流展开，
/// 有用户实测 Okular 保存时报“文件有问题”；增量保存不动原有对象（D-141）。
pub fn write_incremental(spec: &WritebackSpec) -> WritebackResult<WritebackReport> {
    // PDFium 的增量保存对 generation > 0 的注释会写出新对象（gen 0），
    // 但重新打开时仍解析到旧代（实测：Okular 样本的 highlight `11 1 R`）。
    // 这类对象改用全量保存：PDFium 会把注释统一重写为 gen 0，读回正确。
    // 其它情况仍走增量，保留 Adobe 等对象流/外部结构（D-141）。
    let full = needs_full_save(spec);
    write_with_mode(spec, !full)
}

fn write_with_mode(spec: &WritebackSpec, incremental: bool) -> WritebackResult<WritebackReport> {
    if spec.source == spec.output {
        return Err(WritebackError::Invalid(
            "输出路径与源文件相同；写回必须写到新文件（替换由调用方原子完成）".to_string(),
        ));
    }
    if !spec.source.is_file() {
        return Err(WritebackError::Io(format!(
            "源文件不存在：{}",
            spec.source.display()
        )));
    }
    validate(spec)?;

    let ffi = Ffi::load()?;
    let source_bytes = std::fs::read(&spec.source)
        .map_err(|error| WritebackError::Io(format!("读取源文件失败：{error}")))?;
    let page_entries = page_annotations(&source_bytes)
        .map_err(|error| WritebackError::Invalid(format!("解析注释对象号失败：{error}")))?;
    let mut locations: HashMap<PdfObjectRef, (usize, usize)> = HashMap::new();
    let mut popups: HashMap<PdfObjectRef, PdfObjectRef> = HashMap::new();
    for (page, entries) in page_entries.iter().enumerate() {
        for (index, entry) in entries.iter().enumerate() {
            if let Some(reference) = entry.reference {
                locations.insert(reference, (page, index));
                if let Some(popup) = entry.popup {
                    popups.insert(reference, popup);
                }
            }
        }
    }
    let mut page_deletes: HashMap<usize, Vec<usize>> = HashMap::new();
    for reference in &spec.deletes {
        let Some(&(page, index)) = locations.get(reference) else {
            return Err(WritebackError::Invalid(format!(
                "要删除的注释对象 {reference:?} 不在当前 PDF 中（可能已被外部修改）"
            )));
        };
        page_deletes.entry(page).or_default().push(index);
        if let Some(popup) = popups.get(reference) {
            if let Some(&(popup_page, popup_index)) = locations.get(popup) {
                if popup_page == page {
                    page_deletes.entry(page).or_default().push(popup_index);
                }
            }
        }
    }
    let mut page_updates: HashMap<usize, Vec<&WritebackUpdate>> = HashMap::new();
    for update in &spec.updates {
        let Some(&(page, _)) = locations.get(&update.reference) else {
            return Err(WritebackError::Invalid(format!(
                "要改正文的注释对象 {:?} 不在当前 PDF 中（可能已被外部修改）",
                update.reference
            )));
        };
        page_updates.entry(page).or_default().push(update);
    }
    let source = CString::new(spec.source.to_string_lossy().as_ref())
        .map_err(|_| WritebackError::Invalid("源路径含 NUL 字节".to_string()))?;

    // 期望的逐页注释数（同步之后）；保存后用它核对
    let mut expected: Vec<usize>;
    unsafe {
        let document = (ffi.load_document)(source.as_ptr(), std::ptr::null());
        if document.is_null() {
            return Err(WritebackError::Pdfium {
                stage: "FPDF_LoadDocument",
                message: format!(
                    "打不开 {}（加密文档需要密码；S5b 明确拒绝加密文档）",
                    spec.source.display()
                ),
            });
        }
        // 闸门（用户确认的 S5 策略）：加密 / 有数字签名 → 拒绝。
        // 备注：PDF 的"权限位"只对加密文档存在；既然加密文档整体拒绝，
        // `annotate == false` 这一类也就被这道闸门覆盖了。
        if (ffi.security_revision)(document) >= 0 {
            (ffi.close_document)(document);
            return Err(WritebackError::Invalid(
                "加密文档：写回需要 owner 密码，且权限位可能禁止批注；S5b 拒绝".to_string(),
            ));
        }
        let signatures = (ffi.signature_count)(document);
        if signatures > 0 {
            (ffi.close_document)(document);
            return Err(WritebackError::Invalid(format!(
                "文档带 {signatures} 个数字签名：写回会让签名失效；S5b 拒绝"
            )));
        }

        let page_count = (ffi.page_count)(document).max(0) as usize;

        expected = vec![0usize; page_count];
        // 写之前先确认页号合法，避免写了一半才失败
        for page in &spec.pages {
            let index = page.page.get() as usize;
            if index >= page_count {
                (ffi.close_document)(document);
                return Err(WritebackError::Invalid(format!(
                    "页号 {} 越界（文档共 {page_count} 页）",
                    page.page.display_number()
                )));
            }
        }

        // 读回当前 PDF 里的标记注释，用来做**差分**：
        // * 内容只改 /Contents → 原地更新同一个注释对象（对象号不变，像 Okular 那样）；
        // * 几何/颜色/透明度变了 → 删旧引用 + 新建对象；
        // * 没变 → 完全不碰；
        // * 新增/删除 → 只改页面 /Annots 的引用。
        let existing = crate::readback::read(&spec.source, None).unwrap_or_default();
        let existing_by_id: HashMap<u64, &crate::readback::ReadAnnotation> = existing
            .annotations
            .iter()
            .filter_map(|annotation| annotation.id.map(|id| (id, annotation)))
            .collect();
        let mut desired: HashMap<u32, HashMap<u64, &WritebackAnnotation>> = HashMap::new();
        for page in &spec.pages {
            let entry = desired.entry(page.page.get()).or_default();
            for annotation in &page.annotations {
                if let Some(id) = annotation.id {
                    entry.insert(id, annotation);
                }
            }
        }

        // **同步**（S5c）：以 `/NM` 标记识别"上次写回的注释"。
        // 其它程序加的注释（无标记）一律不碰。
        for (index, expected_slot) in expected.iter_mut().enumerate() {
            let handle = (ffi.load_page)(document, index as c_int);
            if handle.is_null() {
                (ffi.close_document)(document);
                return Err(WritebackError::Pdfium {
                    stage: "FPDF_LoadPage",
                    message: format!("第 {} 页加载失败", index + 1),
                });
            }
            let count = (ffi.annot_count)(handle).max(0) as usize;
            let wanted = desired.get(&(index as u32));
            let mut present: std::collections::HashSet<u64> = std::collections::HashSet::new();
            let mut remove_indices: Vec<c_int> = page_deletes
                .get(&index)
                .map(|indices| indices.iter().map(|index| *index as c_int).collect())
                .unwrap_or_default();
            if let Some(updates) = page_updates.get(&index) {
                for update in updates {
                    let (_, annot_index) = locations[&update.reference];
                    let annot = (ffi.get_annot)(handle, annot_index as c_int);
                    if annot.is_null() {
                        (ffi.close_page)(handle);
                        (ffi.close_document)(document);
                        return Err(WritebackError::Pdfium {
                            stage: "FPDFPage_GetAnnot",
                            message: format!("更新第 {} 页 #{}", index + 1, annot_index),
                        });
                    }
                    let ok = update_contents(&ffi, annot, update.body.as_deref());
                    (ffi.close_annot)(annot);
                    if !ok {
                        (ffi.close_page)(handle);
                        (ffi.close_document)(document);
                        return Err(WritebackError::Pdfium {
                            stage: "FPDFAnnot_SetStringValue",
                            message: format!("更新第 {} 页 #{} 的正文", index + 1, annot_index),
                        });
                    }
                }
            }
            for annot_index in 0..count as c_int {
                let annot = (ffi.get_annot)(handle, annot_index);
                if annot.is_null() {
                    continue;
                }
                if let Some(id) = read_marker(&ffi, annot) {
                    let target = wanted.and_then(|map| map.get(&id));
                    if let Some(target) = target {
                        if let Some(current) = existing_by_id.get(&id) {
                            let same_geometry = same_kind(&current.kind, &target.kind);
                            let same_style = current.color == target.color
                                && (current.opacity - target.opacity).abs() <= 0.01;
                            if same_geometry && same_style {
                                if current.body == target.body {
                                    // 完全没变：不碰
                                    present.insert(id);
                                } else if update_contents(&ffi, annot, target.body.as_deref()) {
                                    // 只改正文：原地更新同一个对象（不重建）
                                    present.insert(id);
                                } else {
                                    remove_indices.push(annot_index);
                                }
                            } else {
                                // 几何/颜色/透明度变了：旧对象无法可靠就地改（SetColor 对墨迹失败）
                                remove_indices.push(annot_index);
                            }
                        } else {
                            remove_indices.push(annot_index);
                        }
                    } else {
                        remove_indices.push(annot_index);
                    }
                }
                (ffi.close_annot)(annot);
            }
            // 合并 marker 删除与对象号删除，按索引降序删；去重（父/子删除可能重叠）
            remove_indices.sort_unstable();
            remove_indices.dedup();
            let mut remaining = count;
            for annot_index in remove_indices.iter().rev() {
                if (ffi.remove_annot)(handle, *annot_index) == 0 {
                    (ffi.close_page)(handle);
                    (ffi.close_document)(document);
                    return Err(WritebackError::Pdfium {
                        stage: "FPDFPage_RemoveAnnot",
                        message: format!("第 {} 页 #{}", index + 1, annot_index),
                    });
                }
                remaining -= 1;
            }

            let mut added = 0usize;
            if let Some(page) = spec
                .pages
                .iter()
                .find(|page| page.page.get() as usize == index)
            {
                for annotation in &page.annotations {
                    if let Some(id) = annotation.id {
                        if present.contains(&id) {
                            continue; // 内容已原地更新或完全没变
                        }
                    }
                    add_annotation(&ffi, handle, annotation)?;
                    added += 1;
                }
            }
            *expected_slot = remaining + added;
            (ffi.close_page)(handle);
        }

        let mut bytes = save(&ffi, document, incremental)?;
        // PDFium 新建的注释是内联字典；Okular 保存这类注释会丢修改（D-142）。
        // 追加一段增量更新，把它们外置成独立对象。
        if let Some(fixed) = crate::pdf_repair::externalize_annotations(&bytes) {
            bytes = fixed;
        }
        (ffi.close_document)(document);

        std::fs::write(&spec.output, &bytes).map_err(|error| {
            WritebackError::Io(format!(
                "写输出文件 {} 失败：{error}",
                spec.output.display()
            ))
        })?;
    }

    verify_counts(spec, &expected, &ffi)?;

    Ok(WritebackReport {
        // 语义：本次 PDF 里"我们的注释"总数（新增 + 更新 + 未变），便于 UI 报"共 N 条"
        annotations: spec.pages.iter().map(|page| page.annotations.len()).sum(),
        bytes: std::fs::metadata(&spec.output)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
    })
}

/// 增量保存是否会踩到“旧对象版本”的坑。
///
/// 实测两种：
/// * 对象 generation > 0（Okular 样本的 `11 1 R`）：PDFium 增量写出 gen0，
///   重新打开却仍解析旧代；
/// * 同一 `(num, gen)` 在源文件里出现多次（我们之前增量写坏的文件）：
///   PDFium 可能取到较早的那一版。
///
/// 这两种情况改走全量保存；PDFium 会把对象统一重写，读回一致。
/// 其它情况（Adobe 的 ObjStm、单版对象）仍走增量，避免破坏对象流（D-141）。
fn needs_full_save(spec: &WritebackSpec) -> bool {
    if spec.updates.iter().any(|update| update.reference.gen > 0)
        || spec.deletes.iter().any(|reference| reference.gen > 0)
    {
        return true;
    }
    let Ok(bytes) = std::fs::read(&spec.source) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut references: Vec<PdfObjectRef> =
        spec.updates.iter().map(|update| update.reference).collect();
    references.extend(spec.deletes.iter().copied());
    references.iter().any(|reference| {
        let needle = format!("{} {} obj", reference.num, reference.gen);
        text.matches(&needle).count() > 1
    })
}

/// 读回输出文件的逐页注释数（供调用方校验；**不渲染**）。
///
/// 同样要求调用方已初始化 PDFium，且只在子进程/串行测试里调用。
pub fn count_annotations(path: &Path) -> WritebackResult<Vec<usize>> {
    let ffi = Ffi::load()?;
    let c_path = CString::new(path.to_string_lossy().as_ref())
        .map_err(|_| WritebackError::Invalid("路径含 NUL 字节".to_string()))?;
    unsafe {
        let document = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
        if document.is_null() {
            return Err(WritebackError::Pdfium {
                stage: "FPDF_LoadDocument",
                message: format!("打不开 {}", path.display()),
            });
        }
        let page_count = (ffi.page_count)(document).max(0);
        let mut counts = Vec::with_capacity(page_count as usize);
        for index in 0..page_count {
            let page = (ffi.load_page)(document, index);
            if page.is_null() {
                (ffi.close_document)(document);
                return Err(WritebackError::Pdfium {
                    stage: "FPDF_LoadPage",
                    message: format!("第 {} 页加载失败", index + 1),
                });
            }
            counts.push((ffi.annot_count)(page).max(0) as usize);
            (ffi.close_page)(page);
        }
        (ffi.close_document)(document);
        Ok(counts)
    }
}

// ============================================================ 内部：校验/写入/外观

fn validate(spec: &WritebackSpec) -> WritebackResult<()> {
    // `pages` 允许为空：表示"把 PDF 里我们写过的注释全部删掉"（叠层已清空时写回）。
    for page in &spec.pages {
        for annotation in &page.annotations {
            // 复用模型的不变量：退化几何/NaN 在入口就拒绝，避免写坏文件
            if Annotation::new(page.page, annotation.kind.clone()).is_none() {
                return Err(WritebackError::Invalid(format!(
                    "第 {} 页有一条几何无效的标注（{:?}）",
                    page.page.display_number(),
                    annotation.kind
                )));
            }
        }
    }
    Ok(())
}

/// 创建一条注释并写完语义字段与外观。
unsafe fn add_annotation(
    ffi: &Ffi,
    page: *mut c_void,
    annotation: &WritebackAnnotation,
) -> WritebackResult<()> {
    let subtype = match &annotation.kind {
        AnnotationKind::Highlight { .. } => ANNOT_HIGHLIGHT,
        AnnotationKind::Underline { .. } => ANNOT_UNDERLINE,
        AnnotationKind::StrikeOut { .. } => ANNOT_STRIKEOUT,
        AnnotationKind::Ink { .. } => ANNOT_INK,
    };
    let handle = (ffi.create_annot)(page, subtype);
    if handle.is_null() {
        return Err(WritebackError::Pdfium {
            stage: "FPDFPage_CreateAnnot",
            message: format!("创建子类型 {subtype} 失败"),
        });
    }

    let model = Annotation::new(PageIndex::FIRST, annotation.kind.clone())
        .ok_or_else(|| WritebackError::Invalid("几何无效".to_string()))?;
    let bounds = model
        .bounds()
        .ok_or_else(|| WritebackError::Invalid("几何没有包围盒".to_string()))?;

    let rect = FsRectF {
        left: bounds.x,
        right: bounds.x + bounds.width,
        bottom: bounds.y,
        top: bounds.y + bounds.height,
    };
    if (ffi.set_rect)(handle, &rect) == 0 {
        (ffi.close_annot)(handle);
        return Err(WritebackError::Pdfium {
            stage: "FPDFAnnot_SetRect",
            message: format!("{rect:?}"),
        });
    }
    let [r, g, b] = annotation.color;
    // alpha 同时写进 /CA（透明度）：不写的话读回永远是 1.0，round-trip 不保真（D-134）
    let alpha = (annotation.opacity.clamp(0.0, 1.0) * 255.0).round() as c_uint;
    if (ffi.set_color)(handle, 0, r as c_uint, g as c_uint, b as c_uint, alpha) == 0 {
        (ffi.close_annot)(handle);
        return Err(WritebackError::Pdfium {
            stage: "FPDFAnnot_SetColor",
            message: format!("{r},{g},{b}"),
        });
    }
    // 打印标志（PDF 的 /F 位 3 = Print）。用户打印时注释应该出现。
    let _ = (ffi.set_flags)(handle, ANNOT_FLAG_PRINT);

    if let Some(id) = annotation.id {
        let marker = wide_string(&marker_value(id));
        let key = CString::new("NM").expect("字面量");
        if (ffi.set_string)(handle, key.as_ptr(), marker.as_ptr()) == 0 {
            (ffi.close_annot)(handle);
            return Err(WritebackError::Pdfium {
                stage: "FPDFAnnot_SetStringValue(NM)",
                message: format!("id={id}"),
            });
        }
    }

    if let Some(body) = &annotation.body {
        let wide = wide_string(body);
        let key = CString::new("Contents").expect("字面量");
        if (ffi.set_string)(handle, key.as_ptr(), wide.as_ptr()) == 0 {
            (ffi.close_annot)(handle);
            return Err(WritebackError::Pdfium {
                stage: "FPDFAnnot_SetStringValue(Contents)",
                message: "正文写入失败".to_string(),
            });
        }
    }

    // 线宽写进 /BS（编辑器要知道笔宽；PDFium 不会替我们补）
    if let AnnotationKind::Ink { width, .. } = &annotation.kind {
        let _ = (ffi.set_border)(handle, 0.0, 0.0, *width);
    }

    // 语义字段：墨迹写 /InkList；文本类写 /QuadPoints
    match &annotation.kind {
        AnnotationKind::Ink { points, .. } => {
            if points.len() < 2 {
                (ffi.close_annot)(handle);
                return Err(WritebackError::Invalid("墨迹至少需要两个点".to_string()));
            }
            let stroke: Vec<FsPointF> = points
                .iter()
                .map(|point| FsPointF {
                    x: point.x,
                    y: point.y,
                })
                .collect();
            if (ffi.add_ink_stroke)(handle, stroke.as_ptr(), stroke.len()) < 0 {
                (ffi.close_annot)(handle);
                return Err(WritebackError::Pdfium {
                    stage: "FPDFAnnot_AddInkStroke",
                    message: format!("{} 个点", stroke.len()),
                });
            }
        }
        _ => {
            for quad in annotation.kind.quads() {
                let points = quad_points(*quad);
                if (ffi.append_quad)(handle, &points) == 0 {
                    (ffi.close_annot)(handle);
                    return Err(WritebackError::Pdfium {
                        stage: "FPDFAnnot_AppendAttachmentPoints",
                        message: format!("{quad:?}"),
                    });
                }
            }
        }
    }

    // 外观：
    // * 墨迹用 `AppendObject`（PDFium 原生的 Form XObject，带 /Resources 与 /Matrix；
    //   Okular 保存时对只有 /BBox 的 Form 会报错，见 D-132）；
    // * 下划线/删除线只能 `SetAP`（AppendObject 文档明确只支持 ink/stamp）；
    // * 高亮不写（见模块文档）。
    match &annotation.kind {
        AnnotationKind::Ink { points, width } => {
            append_ink_object(ffi, handle, points, *width, annotation.color)?;
        }
        _ => {
            if let Some(content) =
                appearance_content(&annotation.kind, annotation.color, annotation.opacity)
            {
                let wide = wide_string(&content);
                if (ffi.set_ap)(handle, 0, wide.as_ptr()) == 0 {
                    (ffi.close_annot)(handle);
                    return Err(WritebackError::Pdfium {
                        stage: "FPDFAnnot_SetAP",
                        message: "外观写入失败".to_string(),
                    });
                }
            }
        }
    }

    (ffi.close_annot)(handle);
    Ok(())
}

/// 生成 `/AP` 的内容流（**页面坐标**的 PDF 操作符）。
///
/// `FPDFAnnot_SetAP` 会让 PDFium 建出 `/BBox` = `/Rect` 的 Form XObject，所以这里
/// 直接写页面坐标即可；数字一律保留 2 位小数，避免科学计数法。
fn appearance_content(kind: &AnnotationKind, color: [u8; 3], opacity: f32) -> Option<String> {
    let stroke = format!(
        "{} {} {} RG",
        channel(color[0]),
        channel(color[1]),
        channel(color[2])
    );
    match kind {
        // 高亮不写 AP：写了就是不透明填充，会盖住文字；让阅读器按 /QuadPoints 合成
        AnnotationKind::Highlight { .. } => None,
        AnnotationKind::Ink { .. } => None,
        AnnotationKind::Underline { quads } => Some(lines_at(quads, false, &stroke, opacity)),
        AnnotationKind::StrikeOut { quads } => Some(lines_at(quads, true, &stroke, opacity)),
    }
}

/// 用 PDFium 的路径对象生成墨迹外观，并 `AppendObject` 到注释上。
///
/// PDFium 会生成标准的 Form XObject（含 `/Resources`、`/Matrix`）——别的编辑器
/// （例如 Okular）保存时对缺少这些键的 Form 会报错（D-132）。
/// 失败时由这里负责 `Destroy` 未被接管的路径对象。
unsafe fn append_ink_object(
    ffi: &Ffi,
    annot: *mut c_void,
    points: &[Point],
    width: f32,
    color: [u8; 3],
) -> WritebackResult<()> {
    let Some(first) = points.first() else {
        return Err(WritebackError::Invalid("墨迹没有顶点".to_string()));
    };
    let object = (ffi.new_path)(first.x, first.y);
    if object.is_null() {
        return Err(WritebackError::Pdfium {
            stage: "FPDFPageObj_CreateNewPath",
            message: String::new(),
        });
    }
    let mut ok = true;
    for point in &points[1..] {
        if (ffi.path_line_to)(object, point.x, point.y) == 0 {
            ok = false;
            break;
        }
    }
    let [r, g, b] = color;
    if ok {
        ok = (ffi.page_obj_set_stroke_color)(object, r as c_uint, g as c_uint, b as c_uint, 255)
            != 0;
    }
    if ok {
        ok = (ffi.page_obj_set_stroke_width)(object, width) != 0;
    }
    // 只描边、不填充
    if ok {
        ok = (ffi.path_set_draw_mode)(object, 0, 1) != 0;
    }
    if !ok {
        (ffi.page_obj_destroy)(object);
        return Err(WritebackError::Pdfium {
            stage: "FPDFPath/PageObj",
            message: "构造墨迹路径失败".to_string(),
        });
    }
    // 成功后对象所有权归注释
    if (ffi.append_object)(annot, object) == 0 {
        (ffi.page_obj_destroy)(object);
        return Err(WritebackError::Pdfium {
            stage: "FPDFAnnot_AppendObject",
            message: "外观追加失败".to_string(),
        });
    }
    Ok(())
}

/// 每个 quad 一条线：`middle == false` 画底部（下划线），`true` 画中线（删除线）。
fn lines_at(quads: &[Rect], middle: bool, stroke: &str, opacity: f32) -> String {
    // 把透明度也写进内容流（`CA` 描边 / `ca` 非描边）：`FPDFAnnot_GetColor` 对带 AP 的注释
    // 会失败，读回时只能从这里解析（D-134）
    let alpha = number(opacity.clamp(0.0, 1.0));
    let mut content = format!(
        "q {stroke} {} w {alpha} CA {alpha} ca",
        number(TEXT_MARK_LINE_WIDTH)
    );
    for quad in quads {
        let y = if middle {
            quad.y + quad.height * 0.5
        } else {
            quad.y
        };
        content.push_str(&format!(
            " {} {} m {} {} l",
            number(quad.x),
            number(y),
            number(quad.x + quad.width),
            number(y)
        ));
    }
    content.push_str(" S Q");
    content
}

fn quad_points(quad: Rect) -> FsQuadPointsF {
    // PDF /QuadPoints 顺序：左上、右上、左下、右下（默认坐标系，y 向上）
    FsQuadPointsF {
        x1: quad.x,
        y1: quad.y + quad.height,
        x2: quad.x + quad.width,
        y2: quad.y + quad.height,
        x3: quad.x,
        y3: quad.y,
        x4: quad.x + quad.width,
        y4: quad.y,
    }
}

fn channel(value: u8) -> String {
    number(value as f32 / 255.0)
}

fn number(value: f32) -> String {
    if !value.is_finite() {
        return "0.00".to_string();
    }
    let rounded = (value * 100.0).round() / 100.0;
    // 避免把 -0.0 写成 "-0.00"（内容流里没必要出现负零）
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    format!("{rounded:.2}")
}

fn wide_string(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn save(ffi: &Ffi, document: *mut c_void, incremental: bool) -> WritebackResult<Vec<u8>> {
    let mut writer = Writer {
        base: FpdfFileWrite {
            version: 1,
            write_block: Some(write_block),
        },
        data: Vec::new(),
    };
    // 全量重写（而非增量）：避免在一个可能被其它工具改过的文件上叠增量段
    if (ffi.save_as_copy)(
        document,
        &mut writer.base as *mut FpdfFileWrite,
        if incremental {
            FPDF_INCREMENTAL
        } else {
            FPDF_NO_INCREMENTAL
        },
    ) == 0
    {
        return Err(WritebackError::Pdfium {
            stage: "FPDF_SaveAsCopy",
            message: "保存失败（加密文档需要 owner 密码；S5b 拒绝加密文档）".to_string(),
        });
    }
    Ok(writer.data)
}

fn verify_counts(spec: &WritebackSpec, expected: &[usize], ffi: &Ffi) -> WritebackResult<()> {
    let counts = {
        let c_path = CString::new(spec.output.to_string_lossy().as_ref())
            .map_err(|_| WritebackError::Invalid("输出路径含 NUL 字节".to_string()))?;
        unsafe {
            let document = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            if document.is_null() {
                return Err(WritebackError::Pdfium {
                    stage: "verify/FPDF_LoadDocument",
                    message: "保存结果打不开".to_string(),
                });
            }
            let page_count = (ffi.page_count)(document).max(0) as usize;
            let mut counts = Vec::with_capacity(page_count);
            for index in 0..page_count as c_int {
                let page = (ffi.load_page)(document, index);
                if page.is_null() {
                    (ffi.close_document)(document);
                    return Err(WritebackError::Pdfium {
                        stage: "verify/FPDF_LoadPage",
                        message: format!("第 {} 页", index + 1),
                    });
                }
                counts.push((ffi.annot_count)(page).max(0) as usize);
                (ffi.close_page)(page);
            }
            (ffi.close_document)(document);
            counts
        }
    };

    if counts.len() != expected.len() {
        return Err(WritebackError::Pdfium {
            stage: "verify",
            message: format!("页数变了：{} → {}", expected.len(), counts.len()),
        });
    }
    for (index, want) in expected.iter().enumerate() {
        if counts[index] != *want {
            return Err(WritebackError::Pdfium {
                stage: "verify",
                message: format!(
                    "第 {} 页注释数 {}，期望 {}（同步后的计数）",
                    index + 1,
                    counts[index],
                    want
                ),
            });
        }
    }
    Ok(())
}

// ============================================================ FFI（详见 docs/current/writeback.md）

/// 写回标记的前缀：`/NM` = `pd-fr:<id>`。
///
/// 用途：区分"我们写回的注释"与用户/其它程序加的注释，使**原地重复写回幂等**
/// （已有则跳过、overlay 里没了则删掉；其它注释不碰）。见 `write()` 的同步段。
const MARKER_PREFIX: &str = "pd-fr:";

fn marker_value(id: u64) -> String {
    format!("{MARKER_PREFIX}{id}")
}

pub(crate) fn marker_id(value: &str) -> Option<u64> {
    value.strip_prefix(MARKER_PREFIX)?.parse().ok()
}

/// 读一条注释的 `/NM`（UTF-16LE），解析出我们的 id。
pub(crate) unsafe fn read_marker(ffi: &Ffi, annot: *mut c_void) -> Option<u64> {
    let key = CString::new("NM").ok()?;
    let mut buffer = vec![0u16; 64];
    let length = (ffi.get_string)(
        annot,
        key.as_ptr(),
        buffer.as_mut_ptr(),
        (buffer.len() * 2) as c_ulong,
    );
    if length == 0 {
        return None;
    }
    let units = (length as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..units]);
    marker_id(text.trim_end_matches('\0'))
}

/// 内容（`/Contents`）只是正文；几何/颜色/透明度相同才允许原地更新。
fn same_kind(left: &AnnotationKind, right: &AnnotationKind) -> bool {
    const EPS: f32 = 0.01;
    match (left, right) {
        (
            AnnotationKind::Ink {
                points: a,
                width: wa,
            },
            AnnotationKind::Ink {
                points: b,
                width: wb,
            },
        ) => {
            (wa - wb).abs() <= EPS
                && a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| (x.x - y.x).abs() <= EPS && (x.y - y.y).abs() <= EPS)
        }
        _ => {
            let (AnnotationKind::Highlight { quads: a }
            | AnnotationKind::Underline { quads: a }
            | AnnotationKind::StrikeOut { quads: a }) = left
            else {
                return false;
            };
            let (AnnotationKind::Highlight { quads: b }
            | AnnotationKind::Underline { quads: b }
            | AnnotationKind::StrikeOut { quads: b }) = right
            else {
                return false;
            };
            std::mem::discriminant(left) == std::mem::discriminant(right)
                && a.len() == b.len()
                && a.iter().zip(b.iter()).all(|(x, y)| {
                    (x.x - y.x).abs() <= EPS
                        && (x.y - y.y).abs() <= EPS
                        && (x.width - y.width).abs() <= EPS
                        && (x.height - y.height).abs() <= EPS
                })
        }
    }
}

/// 只改 `/Contents`：原地更新已有注释对象（对象号不变）。
unsafe fn update_contents(ffi: &Ffi, annot: *mut c_void, body: Option<&str>) -> bool {
    let key = CString::new("Contents").expect("字面量");
    let wide = wide_string(body.unwrap_or(""));
    (ffi.set_string)(annot, key.as_ptr(), wide.as_ptr()) != 0
}

pub(crate) const ANNOT_HIGHLIGHT: c_uint = 9;
pub(crate) const ANNOT_UNDERLINE: c_uint = 10;
pub(crate) const ANNOT_STRIKEOUT: c_uint = 12;
pub(crate) const ANNOT_INK: c_uint = 15;
const ANNOT_FLAG_PRINT: c_int = 4;
const FPDF_NO_INCREMENTAL: c_uint = 2;
const FPDF_INCREMENTAL: c_uint = 1;
/// 文本类标注的导出线宽（**页面空间** pt）。
///
/// 界面里的下划线/删除线画的是 2 **屏幕**像素（与缩放无关），导出到 PDF 只能选一个
/// 页面空间值；这里取 2pt（100% 下与界面一致）。差异记录在 `docs/current/writeback.md`。
const TEXT_MARK_LINE_WIDTH: f32 = 2.0;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FsPointF {
    pub(crate) x: c_float,
    pub(crate) y: c_float,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FsQuadPointsF {
    pub(crate) x1: c_float,
    pub(crate) y1: c_float,
    pub(crate) x2: c_float,
    pub(crate) y2: c_float,
    pub(crate) x3: c_float,
    pub(crate) y3: c_float,
    pub(crate) x4: c_float,
    pub(crate) y4: c_float,
}

/// `/Rect` 的字段顺序是 left、top、right、bottom。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FsRectF {
    left: c_float,
    top: c_float,
    right: c_float,
    bottom: c_float,
}

#[repr(C)]
pub(crate) struct FpdfFileWrite {
    version: c_int,
    write_block: Option<unsafe extern "C" fn(*mut FpdfFileWrite, *const c_void, c_ulong) -> c_int>,
}

#[repr(C)]
struct Writer {
    base: FpdfFileWrite,
    data: Vec<u8>,
}

unsafe extern "C" fn write_block(
    this: *mut FpdfFileWrite,
    data: *const c_void,
    size: c_ulong,
) -> c_int {
    let writer = &mut *(this as *mut Writer);
    if size == 0 {
        return 1;
    }
    let slice = std::slice::from_raw_parts(data as *const u8, size as usize);
    writer.data.extend_from_slice(slice);
    1
}

pub(crate) struct Ffi {
    pub(crate) load_document: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    pub(crate) close_document: unsafe extern "C" fn(*mut c_void),
    pub(crate) page_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) load_page: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    pub(crate) close_page: unsafe extern "C" fn(*mut c_void),
    pub(crate) annot_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) get_annot: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    pub(crate) remove_annot: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    pub(crate) create_annot: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    pub(crate) close_annot: unsafe extern "C" fn(*mut c_void),
    pub(crate) set_rect: unsafe extern "C" fn(*mut c_void, *const FsRectF) -> c_int,
    pub(crate) set_color:
        unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint, c_uint) -> c_int,
    pub(crate) set_flags: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    pub(crate) set_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const u16) -> c_int,
    pub(crate) get_string:
        unsafe extern "C" fn(*mut c_void, *const c_char, *mut u16, c_ulong) -> c_ulong,
    pub(crate) add_ink_stroke: unsafe extern "C" fn(*mut c_void, *const FsPointF, usize) -> c_int,
    pub(crate) append_quad: unsafe extern "C" fn(*mut c_void, *const FsQuadPointsF) -> c_int,
    pub(crate) set_ap: unsafe extern "C" fn(*mut c_void, c_uint, *const u16) -> c_int,
    pub(crate) set_border: unsafe extern "C" fn(*mut c_void, c_float, c_float, c_float) -> c_int,
    pub(crate) new_path: unsafe extern "C" fn(c_float, c_float) -> *mut c_void,
    pub(crate) path_line_to: unsafe extern "C" fn(*mut c_void, c_float, c_float) -> c_int,
    pub(crate) path_set_draw_mode: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
    pub(crate) page_obj_set_stroke_color:
        unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint) -> c_int,
    pub(crate) page_obj_set_stroke_width: unsafe extern "C" fn(*mut c_void, c_float) -> c_int,
    pub(crate) append_object: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
    pub(crate) page_obj_destroy: unsafe extern "C" fn(*mut c_void),
    pub(crate) get_subtype: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) get_ap: unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, c_ulong) -> c_ulong,
    pub(crate) has_key: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
    pub(crate) get_number_value:
        unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_float) -> c_int,
    pub(crate) get_color: unsafe extern "C" fn(
        *mut c_void,
        c_int,
        *mut c_uint,
        *mut c_uint,
        *mut c_uint,
        *mut c_uint,
    ) -> c_int,
    pub(crate) get_border:
        unsafe extern "C" fn(*mut c_void, *mut c_float, *mut c_float, *mut c_float) -> c_int,
    pub(crate) get_flags: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) count_attachment_points: unsafe extern "C" fn(*mut c_void) -> usize,
    pub(crate) get_attachment_points:
        unsafe extern "C" fn(*mut c_void, usize, *mut FsQuadPointsF) -> c_int,
    pub(crate) ink_list_count: unsafe extern "C" fn(*mut c_void) -> c_ulong,
    pub(crate) ink_list_path:
        unsafe extern "C" fn(*mut c_void, c_ulong, *mut FsPointF, c_ulong) -> c_ulong,
    pub(crate) save_as_copy: unsafe extern "C" fn(*mut c_void, *mut FpdfFileWrite, c_uint) -> c_int,
    pub(crate) security_revision: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) signature_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(crate) _library: libloading::Library,
}

impl Ffi {
    pub(crate) fn load() -> WritebackResult<Self> {
        let path = super::library::pdfium_library_path().ok_or_else(|| {
            WritebackError::Library(
                "找不到 PDFium 动态库（scripts/fetch-pdfium.sh 或 PDFIUM_PATH）".to_string(),
            )
        })?;
        let library = unsafe { libloading::Library::new(&path) }.map_err(|error| {
            WritebackError::Library(format!("加载 {} 失败：{error}", path.display()))
        })?;
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let symbol: libloading::Symbol<$ty> =
                    unsafe { library.get(concat!($name, "\0").as_bytes()) }.map_err(|error| {
                        WritebackError::Library(format!("缺符号 {}：{error}", $name))
                    })?;
                *symbol
            }};
        }
        let ffi = Self {
            load_document: sym!(
                "FPDF_LoadDocument",
                unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void
            ),
            close_document: sym!("FPDF_CloseDocument", unsafe extern "C" fn(*mut c_void)),
            page_count: sym!(
                "FPDF_GetPageCount",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            load_page: sym!(
                "FPDF_LoadPage",
                unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void
            ),
            close_page: sym!("FPDF_ClosePage", unsafe extern "C" fn(*mut c_void)),
            annot_count: sym!(
                "FPDFPage_GetAnnotCount",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            get_annot: sym!(
                "FPDFPage_GetAnnot",
                unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void
            ),
            remove_annot: sym!(
                "FPDFPage_RemoveAnnot",
                unsafe extern "C" fn(*mut c_void, c_int) -> c_int
            ),
            create_annot: sym!(
                "FPDFPage_CreateAnnot",
                unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void
            ),
            close_annot: sym!("FPDFPage_CloseAnnot", unsafe extern "C" fn(*mut c_void)),
            set_rect: sym!(
                "FPDFAnnot_SetRect",
                unsafe extern "C" fn(*mut c_void, *const FsRectF) -> c_int
            ),
            set_color: sym!(
                "FPDFAnnot_SetColor",
                unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint, c_uint) -> c_int
            ),
            set_flags: sym!(
                "FPDFAnnot_SetFlags",
                unsafe extern "C" fn(*mut c_void, c_int) -> c_int
            ),
            set_string: sym!(
                "FPDFAnnot_SetStringValue",
                unsafe extern "C" fn(*mut c_void, *const c_char, *const u16) -> c_int
            ),
            get_string: sym!(
                "FPDFAnnot_GetStringValue",
                unsafe extern "C" fn(*mut c_void, *const c_char, *mut u16, c_ulong) -> c_ulong
            ),
            add_ink_stroke: sym!(
                "FPDFAnnot_AddInkStroke",
                unsafe extern "C" fn(*mut c_void, *const FsPointF, usize) -> c_int
            ),
            append_quad: sym!(
                "FPDFAnnot_AppendAttachmentPoints",
                unsafe extern "C" fn(*mut c_void, *const FsQuadPointsF) -> c_int
            ),
            set_ap: sym!(
                "FPDFAnnot_SetAP",
                unsafe extern "C" fn(*mut c_void, c_uint, *const u16) -> c_int
            ),
            set_border: sym!(
                "FPDFAnnot_SetBorder",
                unsafe extern "C" fn(*mut c_void, c_float, c_float, c_float) -> c_int
            ),
            new_path: sym!(
                "FPDFPageObj_CreateNewPath",
                unsafe extern "C" fn(c_float, c_float) -> *mut c_void
            ),
            path_line_to: sym!(
                "FPDFPath_LineTo",
                unsafe extern "C" fn(*mut c_void, c_float, c_float) -> c_int
            ),
            path_set_draw_mode: sym!(
                "FPDFPath_SetDrawMode",
                unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int
            ),
            page_obj_set_stroke_color: sym!(
                "FPDFPageObj_SetStrokeColor",
                unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint) -> c_int
            ),
            page_obj_set_stroke_width: sym!(
                "FPDFPageObj_SetStrokeWidth",
                unsafe extern "C" fn(*mut c_void, c_float) -> c_int
            ),
            append_object: sym!(
                "FPDFAnnot_AppendObject",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int
            ),
            page_obj_destroy: sym!("FPDFPageObj_Destroy", unsafe extern "C" fn(*mut c_void)),
            get_subtype: sym!(
                "FPDFAnnot_GetSubtype",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            get_ap: sym!(
                "FPDFAnnot_GetAP",
                unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, c_ulong) -> c_ulong
            ),
            has_key: sym!(
                "FPDFAnnot_HasKey",
                unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int
            ),
            get_number_value: sym!(
                "FPDFAnnot_GetNumberValue",
                unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_float) -> c_int
            ),
            get_color: sym!(
                "FPDFAnnot_GetColor",
                unsafe extern "C" fn(
                    *mut c_void,
                    c_int,
                    *mut c_uint,
                    *mut c_uint,
                    *mut c_uint,
                    *mut c_uint,
                ) -> c_int
            ),
            get_border: sym!(
                "FPDFAnnot_GetBorder",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_float,
                    *mut c_float,
                    *mut c_float,
                ) -> c_int
            ),
            get_flags: sym!(
                "FPDFAnnot_GetFlags",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            count_attachment_points: sym!(
                "FPDFAnnot_CountAttachmentPoints",
                unsafe extern "C" fn(*mut c_void) -> usize
            ),
            get_attachment_points: sym!(
                "FPDFAnnot_GetAttachmentPoints",
                unsafe extern "C" fn(*mut c_void, usize, *mut FsQuadPointsF) -> c_int
            ),
            ink_list_count: sym!(
                "FPDFAnnot_GetInkListCount",
                unsafe extern "C" fn(*mut c_void) -> c_ulong
            ),
            ink_list_path: sym!(
                "FPDFAnnot_GetInkListPath",
                unsafe extern "C" fn(*mut c_void, c_ulong, *mut FsPointF, c_ulong) -> c_ulong
            ),
            save_as_copy: sym!(
                "FPDF_SaveAsCopy",
                unsafe extern "C" fn(*mut c_void, *mut FpdfFileWrite, c_uint) -> c_int
            ),
            security_revision: sym!(
                "FPDF_GetSecurityHandlerRevision",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            signature_count: sym!(
                "FPDF_GetSignatureCount",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            // 放在最后：前面的字段初始化借用 `library`，最后再移动
            _library: library,
        };
        Ok(ffi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcore::prelude::Point;

    #[test]
    fn ink_uses_pdfium_native_appearance_not_a_handwritten_stream() {
        // 墨迹的外观由 `AppendObject` 生成（PDFium 会补 /Resources、/Matrix，
        // 别的编辑器保存时才不会报错，D-132）；不再走手写内容流。
        let kind = AnnotationKind::Ink {
            points: vec![
                Point::new(10.0, 20.0),
                Point::new(30.0, 40.0),
                Point::new(50.0, 20.0),
            ],
            width: 2.0,
        };
        assert!(appearance_content(&kind, [255, 0, 0], 1.0).is_none());
    }

    #[test]
    fn highlight_has_no_appearance_but_underline_and_strikeout_do() {
        let quad = Rect::new(10.0, 20.0, 100.0, 12.0);
        assert!(
            appearance_content(
                &AnnotationKind::Highlight { quads: vec![quad] },
                [255, 255, 0],
                0.35
            )
            .is_none(),
            "高亮不写 AP（否则不透明填充会盖住文字）"
        );
        let underline = appearance_content(
            &AnnotationKind::Underline { quads: vec![quad] },
            [255, 0, 0],
            0.8,
        )
        .expect("下划线必须写 AP");
        assert!(
            underline.contains("10.00 20.00 m 110.00 20.00 l"),
            "底线在 y=20：{underline}"
        );
        assert!(
            underline.contains("0.80 CA 0.80 ca"),
            "透明度要写进 AP（读回靠它）：{underline}"
        );
        let strike = appearance_content(
            &AnnotationKind::StrikeOut { quads: vec![quad] },
            [255, 0, 0],
            0.6,
        )
        .expect("删除线必须写 AP");
        assert!(
            strike.contains("10.00 26.00 m 110.00 26.00 l"),
            "中线在 y=26：{strike}"
        );
    }

    #[test]
    fn quad_points_use_pdf_order_upper_left_first() {
        let points = quad_points(Rect::new(10.0, 20.0, 100.0, 12.0));
        // 左上 (10,32) → 右上 (110,32) → 左下 (10,20) → 右下 (110,20)
        assert_eq!(
            (points.x1, points.y1, points.x2, points.y2),
            (10.0, 32.0, 110.0, 32.0)
        );
        assert_eq!(
            (points.x3, points.y3, points.x4, points.y4),
            (10.0, 20.0, 110.0, 20.0)
        );
    }

    #[test]
    fn numbers_are_finite_and_fixed_precision() {
        assert_eq!(number(12.345), "12.35");
        assert_eq!(number(-0.001), "0.00");
        assert_eq!(number(f32::NAN), "0.00", "NaN 绝不能进内容流");
        assert_eq!(number(f32::INFINITY), "0.00");
    }

    #[test]
    fn source_and_output_must_differ() {
        let spec = WritebackSpec {
            updates: Vec::new(),
            deletes: Vec::new(),
            source: PathBuf::from("/tmp/a.pdf"),
            output: PathBuf::from("/tmp/a.pdf"),
            pages: vec![],
        };
        assert!(matches!(write(&spec), Err(WritebackError::Invalid(_))));
    }
}

#[cfg(test)]
mod full_save_tests {
    use super::*;

    #[test]
    fn duplicate_object_versions_force_full_save() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-full-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("in.pdf");
        std::fs::write(&source, b"11 0 obj\n<<>>\nendobj\n11 0 obj\n<<>>\nendobj\n").unwrap();
        let spec = WritebackSpec {
            source,
            output: dir.join("out.pdf"),
            pages: Vec::new(),
            updates: vec![WritebackUpdate {
                reference: PdfObjectRef { num: 11, gen: 0 },
                body: Some("x".to_string()),
            }],
            deletes: Vec::new(),
        };
        assert!(needs_full_save(&spec), "同一对象出现多版时必须全量保存");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
