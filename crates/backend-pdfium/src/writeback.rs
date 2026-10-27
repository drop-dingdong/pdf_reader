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

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::os::raw::c_float;
use std::path::{Path, PathBuf};

use pdfcore::annotation::{Annotation, AnnotationKind};
use pdfcore::prelude::{PageIndex, Rect};

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
    let source = CString::new(spec.source.to_string_lossy().as_ref())
        .map_err(|_| WritebackError::Invalid("源路径含 NUL 字节".to_string()))?;

    // 写入前每页原有的注释数；保存后用它核对（在 unsafe 块外声明，校验阶段要用）
    let mut before: Vec<usize>;
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

        before = vec![0usize; page_count];
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

        for page in &spec.pages {
            let index = page.page.get() as c_int;
            let handle = (ffi.load_page)(document, index);
            if handle.is_null() {
                (ffi.close_document)(document);
                return Err(WritebackError::Pdfium {
                    stage: "FPDF_LoadPage",
                    message: format!("第 {} 页加载失败", page.page.display_number()),
                });
            }
            before[index as usize] = (ffi.annot_count)(handle).max(0) as usize;
            for annotation in &page.annotations {
                add_annotation(&ffi, handle, annotation)?;
            }
            (ffi.close_page)(handle);
        }

        let bytes = save(&ffi, document)?;
        (ffi.close_document)(document);

        std::fs::write(&spec.output, &bytes).map_err(|error| {
            WritebackError::Io(format!(
                "写输出文件 {} 失败：{error}",
                spec.output.display()
            ))
        })?;
    }

    verify_counts(spec, &before, &ffi)?;

    Ok(WritebackReport {
        annotations: spec.pages.iter().map(|page| page.annotations.len()).sum(),
        bytes: std::fs::metadata(&spec.output)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
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
    if spec.pages.is_empty() {
        return Err(WritebackError::Invalid("没有要写入的标注".to_string()));
    }
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
    if (ffi.set_color)(handle, 0, r as c_uint, g as c_uint, b as c_uint, 255) == 0 {
        (ffi.close_annot)(handle);
        return Err(WritebackError::Pdfium {
            stage: "FPDFAnnot_SetColor",
            message: format!("{r},{g},{b}"),
        });
    }
    // 打印标志（PDF 的 /F 位 3 = Print）。用户打印时注释应该出现。
    let _ = (ffi.set_flags)(handle, ANNOT_FLAG_PRINT);

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

    // 外观：高亮不写（见模块文档），其余写一段页面坐标的内容流
    if let Some(content) = appearance_content(&annotation.kind, annotation.color) {
        let wide = wide_string(&content);
        if (ffi.set_ap)(handle, 0, wide.as_ptr()) == 0 {
            (ffi.close_annot)(handle);
            return Err(WritebackError::Pdfium {
                stage: "FPDFAnnot_SetAP",
                message: "外观写入失败".to_string(),
            });
        }
    }

    (ffi.close_annot)(handle);
    Ok(())
}

/// 生成 `/AP` 的内容流（**页面坐标**的 PDF 操作符）。
///
/// `FPDFAnnot_SetAP` 会让 PDFium 建出 `/BBox` = `/Rect` 的 Form XObject，所以这里
/// 直接写页面坐标即可；数字一律保留 2 位小数，避免科学计数法。
fn appearance_content(kind: &AnnotationKind, color: [u8; 3]) -> Option<String> {
    let stroke = format!(
        "{} {} {} RG",
        channel(color[0]),
        channel(color[1]),
        channel(color[2])
    );
    match kind {
        // 高亮不写 AP：写了就是不透明填充，会盖住文字；让阅读器按 /QuadPoints 合成
        AnnotationKind::Highlight { .. } => None,
        AnnotationKind::Ink { points, width } => {
            let mut content = format!("q {stroke} {} w", number(*width));
            for pair in points.windows(2) {
                content.push_str(&format!(
                    " {} {} m {} {} l",
                    number(pair[0].x),
                    number(pair[0].y),
                    number(pair[1].x),
                    number(pair[1].y)
                ));
            }
            content.push_str(" S Q");
            Some(content)
        }
        AnnotationKind::Underline { quads } => Some(lines_at(quads, false, &stroke)),
        AnnotationKind::StrikeOut { quads } => Some(lines_at(quads, true, &stroke)),
    }
}

/// 每个 quad 一条线：`middle == false` 画底部（下划线），`true` 画中线（删除线）。
fn lines_at(quads: &[Rect], middle: bool, stroke: &str) -> String {
    let mut content = format!("q {stroke} {} w", number(TEXT_MARK_LINE_WIDTH));
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

unsafe fn save(ffi: &Ffi, document: *mut c_void) -> WritebackResult<Vec<u8>> {
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
        FPDF_NO_INCREMENTAL,
    ) == 0
    {
        return Err(WritebackError::Pdfium {
            stage: "FPDF_SaveAsCopy",
            message: "保存失败（加密文档需要 owner 密码；S5b 拒绝加密文档）".to_string(),
        });
    }
    Ok(writer.data)
}

fn verify_counts(spec: &WritebackSpec, before: &[usize], ffi: &Ffi) -> WritebackResult<()> {
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

    if counts.len() != before.len() {
        return Err(WritebackError::Pdfium {
            stage: "verify",
            message: format!("页数变了：{} → {}", before.len(), counts.len()),
        });
    }
    for (index, page) in spec.pages.iter().enumerate() {
        let target = page.page.get() as usize;
        let expected = before[target] + page.annotations.len();
        if counts[target] != expected {
            return Err(WritebackError::Pdfium {
                stage: "verify",
                message: format!(
                    "第 {} 页注释数 {}，期望 {}（原有 {} + 新增 {}）",
                    index + 1,
                    counts[target],
                    expected,
                    before[target],
                    page.annotations.len()
                ),
            });
        }
    }
    Ok(())
}

// ============================================================ FFI（详见 docs/current/writeback.md）

const ANNOT_HIGHLIGHT: c_uint = 9;
const ANNOT_UNDERLINE: c_uint = 10;
const ANNOT_STRIKEOUT: c_uint = 12;
const ANNOT_INK: c_uint = 15;
const ANNOT_FLAG_PRINT: c_int = 4;
const FPDF_NO_INCREMENTAL: c_uint = 2;
/// 文本类标注的导出线宽（**页面空间** pt）。
///
/// 界面里的下划线/删除线画的是 2 **屏幕**像素（与缩放无关），导出到 PDF 只能选一个
/// 页面空间值；这里取 2pt（100% 下与界面一致）。差异记录在 `docs/current/writeback.md`。
const TEXT_MARK_LINE_WIDTH: f32 = 2.0;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FsPointF {
    x: c_float,
    y: c_float,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FsQuadPointsF {
    x1: c_float,
    y1: c_float,
    x2: c_float,
    y2: c_float,
    x3: c_float,
    y3: c_float,
    x4: c_float,
    y4: c_float,
}

/// `/Rect` 的字段顺序是 left、top、right、bottom。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FsRectF {
    left: c_float,
    top: c_float,
    right: c_float,
    bottom: c_float,
}

#[repr(C)]
struct FpdfFileWrite {
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

struct Ffi {
    load_document: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    close_document: unsafe extern "C" fn(*mut c_void),
    page_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    load_page: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    close_page: unsafe extern "C" fn(*mut c_void),
    annot_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    create_annot: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    close_annot: unsafe extern "C" fn(*mut c_void),
    set_rect: unsafe extern "C" fn(*mut c_void, *const FsRectF) -> c_int,
    set_color: unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint, c_uint) -> c_int,
    set_flags: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    set_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const u16) -> c_int,
    add_ink_stroke: unsafe extern "C" fn(*mut c_void, *const FsPointF, usize) -> c_int,
    append_quad: unsafe extern "C" fn(*mut c_void, *const FsQuadPointsF) -> c_int,
    set_ap: unsafe extern "C" fn(*mut c_void, c_uint, *const u16) -> c_int,
    save_as_copy: unsafe extern "C" fn(*mut c_void, *mut FpdfFileWrite, c_uint) -> c_int,
    security_revision: unsafe extern "C" fn(*mut c_void) -> c_int,
    signature_count: unsafe extern "C" fn(*mut c_void) -> c_int,
    _library: libloading::Library,
}

impl Ffi {
    fn load() -> WritebackResult<Self> {
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
    fn ink_content_lists_every_segment_in_page_space() {
        let kind = AnnotationKind::Ink {
            points: vec![Point::new(10.0, 20.0), Point::new(30.0, 40.0)],
            width: 2.0,
        };
        let content = appearance_content(&kind, [255, 0, 0]).expect("墨迹必须写 AP");
        assert!(
            content.starts_with("q 1.00 0.00 0.00 RG 2.00 w "),
            "实际：{content}"
        );
        assert!(content.contains("10.00 20.00 m 30.00 40.00 l"));
        assert!(content.ends_with(" S Q"));
    }

    #[test]
    fn highlight_has_no_appearance_but_underline_and_strikeout_do() {
        let quad = Rect::new(10.0, 20.0, 100.0, 12.0);
        assert!(
            appearance_content(
                &AnnotationKind::Highlight { quads: vec![quad] },
                [255, 255, 0]
            )
            .is_none(),
            "高亮不写 AP（否则不透明填充会盖住文字）"
        );
        let underline = appearance_content(
            &AnnotationKind::Underline { quads: vec![quad] },
            [255, 0, 0],
        )
        .expect("下划线必须写 AP");
        assert!(
            underline.contains("10.00 20.00 m 110.00 20.00 l"),
            "底线在 y=20：{underline}"
        );
        let strike = appearance_content(
            &AnnotationKind::StrikeOut { quads: vec![quad] },
            [255, 0, 0],
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
            source: PathBuf::from("/tmp/a.pdf"),
            output: PathBuf::from("/tmp/a.pdf"),
            pages: vec![],
        };
        assert!(matches!(write(&spec), Err(WritebackError::Invalid(_))));
    }
}
