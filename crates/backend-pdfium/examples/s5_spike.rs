//! **S5a spike（一次性探针）**：验证写回 PDF 的关键假设。
//!
//! 不是测试、不进产品代码；结论见 `docs/writeback.md`。
//! 跑法：`cargo run -p backend-pdfium --example s5_spike`（需要 PDFium 动态库）。
//! 假设：
//! 1. 墨迹：`CreateAnnot(Ink)` + `AddInkStroke` + `AppendObject(路径)` → 保存后**能渲染**；
//! 2. 高亮：`CreateAnnot(Highlight)` + `SetRect` + `AppendAttachmentPoints`（= /QuadPoints）
//!    → 保存后**能渲染**；
//! 3. `/Contents` 用 UTF-16LE 写入后能原样读回；
//! 4. `FPDF_SaveAsCopy` 的 flags=0 与 `FPDF_NO_INCREMENTAL` 都能产出可重新打开的 PDF。

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::os::raw::c_float;

use backend_pdfium::PdfiumBackend;
use pdfcore::prelude::*;

// ---------------------------------------------------------------- 最小 FFI

type FpdfDocument = *mut c_void;
type FpdfPage = *mut c_void;
type FpdfAnnot = *mut c_void;
type FpdfPageObject = *mut c_void;
type FpdfBool = c_int;

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

/// `/Rect` 的字段顺序是 left、top、right、bottom（**不是** x,y,w,h）。
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
    let slice = std::slice::from_raw_parts(data as *const u8, size as usize);
    writer.data.extend_from_slice(slice);
    1
}

const ANNOT_HIGHLIGHT: c_uint = 9;
const ANNOT_UNDERLINE: c_uint = 10;
const ANNOT_STRIKEOUT: c_uint = 12;
const ANNOT_INK: c_uint = 15;

struct Ffi {
    _library: libloading::Library,
    load_document: unsafe extern "C" fn(*const c_char, *const c_char) -> FpdfDocument,
    close_document: unsafe extern "C" fn(FpdfDocument),
    page_count: unsafe extern "C" fn(FpdfDocument) -> c_int,
    load_page: unsafe extern "C" fn(FpdfDocument, c_int) -> FpdfPage,
    close_page: unsafe extern "C" fn(FpdfPage),
    page_rotation: unsafe extern "C" fn(FpdfPage) -> c_int,
    annot_count: unsafe extern "C" fn(FpdfPage) -> c_int,
    create_annot: unsafe extern "C" fn(FpdfPage, c_uint) -> FpdfAnnot,
    close_annot: unsafe extern "C" fn(FpdfAnnot),
    set_rect: unsafe extern "C" fn(FpdfAnnot, *const FsRectF) -> FpdfBool,
    set_color: unsafe extern "C" fn(FpdfAnnot, c_uint, c_uint, c_uint, c_uint, c_uint) -> FpdfBool,
    set_string: unsafe extern "C" fn(FpdfAnnot, *const c_char, *const u16) -> FpdfBool,
    set_ap: unsafe extern "C" fn(FpdfAnnot, c_uint, *const u16) -> FpdfBool,
    get_string: unsafe extern "C" fn(FpdfAnnot, *const c_char, *mut u16, c_ulong) -> c_ulong,
    add_ink_stroke: unsafe extern "C" fn(FpdfAnnot, *const FsPointF, usize) -> c_int,
    get_subtype: unsafe extern "C" fn(FpdfAnnot) -> c_uint,
    supported_subtype: unsafe extern "C" fn(c_uint) -> FpdfBool,
    ink_count: unsafe extern "C" fn(FpdfAnnot) -> usize,
    append_quad: unsafe extern "C" fn(FpdfAnnot, *const FsQuadPointsF) -> FpdfBool,
    new_path: unsafe extern "C" fn(c_float, c_float) -> FpdfPageObject,
    line_to: unsafe extern "C" fn(FpdfPageObject, c_float, c_float) -> FpdfBool,
    set_draw_mode: unsafe extern "C" fn(FpdfPageObject, c_int, c_int) -> FpdfBool,
    set_stroke_color:
        unsafe extern "C" fn(FpdfPageObject, c_uint, c_uint, c_uint, c_uint) -> FpdfBool,
    set_stroke_width: unsafe extern "C" fn(FpdfPageObject, c_float) -> FpdfBool,
    append_object: unsafe extern "C" fn(FpdfAnnot, FpdfPageObject) -> FpdfBool,
    save_as_copy: unsafe extern "C" fn(FpdfDocument, *mut FpdfFileWrite, c_uint) -> FpdfBool,
}

fn library_path() -> String {
    if let Ok(path) = std::env::var("PDFIUM_PATH") {
        return path;
    }
    let tag = backend_pdfium::library::platform_tag();
    for prefix in ["../..", ".", "../../.."] {
        let candidate = format!("{prefix}/vendor/pdfium-{tag}/lib");
        if std::path::Path::new(&candidate).is_dir() {
            return candidate;
        }
    }
    panic!("找不到 vendor/pdfium-{tag}/lib；先跑 scripts/fetch-pdfium.sh");
}

impl Ffi {
    fn load() -> Self {
        // 先让 pdfium-render 完成进程级初始化（同一个 .so，全局状态共享）
        let _ = backend_pdfium::pdfium().expect("pdfium");
        let directory = library_path();
        let so = if std::path::Path::new(&directory).is_dir() {
            format!("{directory}/libpdfium.so")
        } else {
            directory
        };
        let library = unsafe { libloading::Library::new(&so) }.expect("加载 libpdfium");
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let symbol: libloading::Symbol<$ty> =
                    unsafe { library.get(concat!($name, "\0").as_bytes()) }
                        .unwrap_or_else(|error| panic!("缺符号 {}: {error}", $name));
                *symbol
            }};
        }
        let ffi = Self {
            load_document: sym!(
                "FPDF_LoadDocument",
                unsafe extern "C" fn(*const c_char, *const c_char) -> FpdfDocument
            ),
            close_document: sym!("FPDF_CloseDocument", unsafe extern "C" fn(FpdfDocument)),
            page_count: sym!(
                "FPDF_GetPageCount",
                unsafe extern "C" fn(FpdfDocument) -> c_int
            ),
            load_page: sym!(
                "FPDF_LoadPage",
                unsafe extern "C" fn(FpdfDocument, c_int) -> FpdfPage
            ),
            close_page: sym!("FPDF_ClosePage", unsafe extern "C" fn(FpdfPage)),
            page_rotation: sym!(
                "FPDFPage_GetRotation",
                unsafe extern "C" fn(FpdfPage) -> c_int
            ),
            annot_count: sym!(
                "FPDFPage_GetAnnotCount",
                unsafe extern "C" fn(FpdfPage) -> c_int
            ),
            create_annot: sym!(
                "FPDFPage_CreateAnnot",
                unsafe extern "C" fn(FpdfPage, c_uint) -> FpdfAnnot
            ),
            close_annot: sym!("FPDFPage_CloseAnnot", unsafe extern "C" fn(FpdfAnnot)),
            set_rect: sym!(
                "FPDFAnnot_SetRect",
                unsafe extern "C" fn(FpdfAnnot, *const FsRectF) -> FpdfBool
            ),
            set_color: sym!(
                "FPDFAnnot_SetColor",
                unsafe extern "C" fn(FpdfAnnot, c_uint, c_uint, c_uint, c_uint, c_uint) -> FpdfBool
            ),
            set_string: sym!(
                "FPDFAnnot_SetStringValue",
                unsafe extern "C" fn(FpdfAnnot, *const c_char, *const u16) -> FpdfBool
            ),
            set_ap: sym!(
                "FPDFAnnot_SetAP",
                unsafe extern "C" fn(FpdfAnnot, c_uint, *const u16) -> FpdfBool
            ),
            get_string: sym!(
                "FPDFAnnot_GetStringValue",
                unsafe extern "C" fn(FpdfAnnot, *const c_char, *mut u16, c_ulong) -> c_ulong
            ),
            add_ink_stroke: sym!(
                "FPDFAnnot_AddInkStroke",
                unsafe extern "C" fn(FpdfAnnot, *const FsPointF, usize) -> c_int
            ),
            get_subtype: sym!(
                "FPDFAnnot_GetSubtype",
                unsafe extern "C" fn(FpdfAnnot) -> c_uint
            ),
            supported_subtype: sym!(
                "FPDFAnnot_IsSupportedSubtype",
                unsafe extern "C" fn(c_uint) -> FpdfBool
            ),
            ink_count: sym!(
                "FPDFAnnot_GetInkListCount",
                unsafe extern "C" fn(FpdfAnnot) -> usize
            ),
            append_quad: sym!(
                "FPDFAnnot_AppendAttachmentPoints",
                unsafe extern "C" fn(FpdfAnnot, *const FsQuadPointsF) -> FpdfBool
            ),
            new_path: sym!(
                "FPDFPageObj_CreateNewPath",
                unsafe extern "C" fn(c_float, c_float) -> FpdfPageObject
            ),
            line_to: sym!(
                "FPDFPath_LineTo",
                unsafe extern "C" fn(FpdfPageObject, c_float, c_float) -> FpdfBool
            ),
            set_draw_mode: sym!(
                "FPDFPath_SetDrawMode",
                unsafe extern "C" fn(FpdfPageObject, c_int, c_int) -> FpdfBool
            ),
            set_stroke_color: sym!(
                "FPDFPageObj_SetStrokeColor",
                unsafe extern "C" fn(FpdfPageObject, c_uint, c_uint, c_uint, c_uint) -> FpdfBool
            ),
            set_stroke_width: sym!(
                "FPDFPageObj_SetStrokeWidth",
                unsafe extern "C" fn(FpdfPageObject, c_float) -> FpdfBool
            ),
            append_object: sym!(
                "FPDFAnnot_AppendObject",
                unsafe extern "C" fn(FpdfAnnot, FpdfPageObject) -> FpdfBool
            ),
            save_as_copy: sym!(
                "FPDF_SaveAsCopy",
                unsafe extern "C" fn(FpdfDocument, *mut FpdfFileWrite, c_uint) -> FpdfBool
            ),
            // 放在最后：前面的字段初始化会借用 `library`，最后再移动它
            _library: library,
        };
        ffi
    }
}

// ---------------------------------------------------------------- 测试夹具

fn stream_object(content: &str) -> String {
    format!(
        "<< /Length {} >>\nstream\n{}endstream",
        content.len(),
        content
    )
}

fn build_pdf(width: u32, height: u32, rotate: u32, content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
             /Rotate {rotate} /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
        ),
        stream_object(content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }
    let xref_offset = out.len();
    let size = objects.len() + 1;
    out.push_str(&format!("xref\n0 {size}\n"));
    out.push_str("0000000000 65535 f \n");
    for offset in &offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    ));
    out.into_bytes()
}

fn count_pixels(image: &RasterImage, predicate: impl Fn(&[u8]) -> bool) -> usize {
    image
        .data()
        .chunks_exact(4)
        .filter(|pixel| predicate(pixel))
        .count()
}

fn is_red(pixel: &[u8]) -> bool {
    pixel[2] > 180 && pixel[1] < 100 && pixel[0] < 100
}

/// 命中像素的包围盒（左、上、右、下，**位图坐标**）。
fn bbox(image: &RasterImage, predicate: impl Fn(&[u8]) -> bool) -> Option<(u32, u32, u32, u32)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0, 0);
    for (index, pixel) in image.data().chunks_exact(4).enumerate() {
        if !predicate(pixel) {
            continue;
        }
        // `chunks_exact(4)` 的下标已经按像素计
        let x = index as u32 % image.width;
        let y = index as u32 / image.width;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    (min_x != u32::MAX).then_some((min_x, min_y, max_x, max_y))
}

fn is_yellow(pixel: &[u8]) -> bool {
    pixel[2] > 200 && pixel[1] > 200 && pixel[0] < 120
}

fn render(bytes: &[u8]) -> RasterImage {
    let backend = PdfiumBackend::new();
    let opened = backend.open(Source::Bytes(bytes), None).expect("打开");
    let image = opened
        .raster
        .render(&RenderRequest::new(
            PageIndex::FIRST,
            Scale::IDENTITY,
            Rotation::None,
        ))
        .expect("渲染");
    (*image).clone()
}

// ---------------------------------------------------------------- spike

fn main() {
    if backend_pdfium::pdfium().is_err() {
        eprintln!("跳过：没有 PDFium（scripts/fetch-pdfium.sh）");
        return;
    }
    let ffi = Ffi::load();

    // ---------------- 1) 墨迹：CreateAnnot + AddInkStroke + AppendObject 生成 AP
    {
        let source = build_pdf(300, 300, 0, "");
        let path = std::env::temp_dir().join(format!("s5a-ink-{}.pdf", std::process::id()));
        std::fs::write(&path, &source).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        unsafe {
            let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            assert!(!doc.is_null(), "LoadDocument 失败");
            assert_eq!((ffi.page_count)(doc), 1);
            let page = (ffi.load_page)(doc, 0);
            assert!(!page.is_null());

            let annot = (ffi.create_annot)(page, ANNOT_INK);
            assert!(!annot.is_null(), "CreateAnnot(Ink) 失败");
            let rect = FsRectF {
                left: 40.0,
                top: 250.0,
                right: 260.0,
                bottom: 40.0,
            };
            assert_eq!((ffi.set_rect)(annot, &rect), 1, "SetRect");
            assert_eq!((ffi.set_color)(annot, 0, 255, 0, 0, 255), 1, "SetColor");

            eprintln!(
                "[S5a/1] subtype={}，支持创建={}，ink 前数量={}",
                (ffi.get_subtype)(annot),
                (ffi.supported_subtype)(ANNOT_INK),
                (ffi.ink_count)(annot)
            );
            let stroke = [
                FsPointF { x: 40.0, y: 40.0 },
                FsPointF { x: 260.0, y: 40.0 },
            ];
            let index = (ffi.add_ink_stroke)(annot, stroke.as_ptr(), stroke.len());
            eprintln!(
                "[S5a/1] AddInkStroke(2 点) → {index}，ink 数量={}",
                (ffi.ink_count)(annot)
            );
            if index < 0 {
                let stroke3 = [
                    FsPointF { x: 40.0, y: 40.0 },
                    FsPointF { x: 150.0, y: 60.0 },
                    FsPointF { x: 260.0, y: 40.0 },
                ];
                let index3 = (ffi.add_ink_stroke)(annot, stroke3.as_ptr(), stroke3.len());
                eprintln!("[S5a/1] AddInkStroke(3 点) → {index3}");
                assert!(index3 >= 0, "AddInkStroke 两种点数都失败");
            }

            // 用路径对象画 /AP：一条红线
            let object = (ffi.new_path)(40.0, 40.0);
            assert!(!object.is_null(), "CreateNewPath 失败");
            assert_eq!((ffi.line_to)(object, 260.0, 40.0), 1, "LineTo");
            assert_eq!((ffi.set_stroke_color)(object, 255, 0, 0, 255), 1);
            assert_eq!((ffi.set_stroke_width)(object, 2.0), 1);
            assert_eq!((ffi.set_draw_mode)(object, 0, 1), 1, "stroke only");
            assert_eq!((ffi.append_object)(annot, object), 1, "AppendObject 失败");

            let mut writer = Writer {
                base: FpdfFileWrite {
                    version: 1,
                    write_block: Some(write_block),
                },
                data: Vec::new(),
            };
            assert_eq!(
                (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 0),
                1,
                "SaveAsCopy 失败"
            );
            let saved = writer.data.clone();
            let ink_path =
                std::env::temp_dir().join(format!("s5a-ink-out-{}.pdf", std::process::id()));
            std::fs::write(&ink_path, &saved).unwrap();

            (ffi.close_annot)(annot);
            (ffi.close_page)(page);
            (ffi.close_document)(doc);

            let image = render(&saved);
            let red = count_pixels(&image, is_red);
            let red_bbox = bbox(&image, is_red).expect("红色像素");
            // 页面空间 y=40 → 位图行 300-40=260；x 40..260
            eprintln!(
                "[S5a/1] 墨迹：保存 {} 字节，红色像素 {red} 个，包围盒 {red_bbox:?}（期望 x≈40..260，y≈260）",
                saved.len()
            );
            assert!(red > 50, "墨迹 AP 没渲染出来（red={red}）");
            assert!(
                (red_bbox.0 as i32 - 40).abs() <= 3
                    && (red_bbox.2 as i32 - 260).abs() <= 3
                    && (red_bbox.1 as i32 - 260).abs() <= 3
                    && (red_bbox.3 as i32 - 260).abs() <= 3,
                "墨迹位置不对：{red_bbox:?}（页面空间直写应当落在 x 40..260、行 260）"
            );

            // 重新打开保存结果，数注释
            let doc2 = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            assert!(!doc2.is_null());
            let _ = doc2;
            let reopened = (ffi.load_document)(
                CString::new(ink_path.to_str().unwrap()).unwrap().as_ptr(),
                std::ptr::null(),
            );
            assert!(!reopened.is_null(), "保存结果打不开");
            let page2 = (ffi.load_page)(reopened, 0);
            let count = (ffi.annot_count)(page2);
            eprintln!("[S5a/1] 重开后注释数 = {count}");
            assert_eq!(count, 1);
            (ffi.close_page)(page2);
            (ffi.close_document)(reopened);
        }
    }

    // ---------------- 2) 高亮：CreateAnnot(Highlight) + AppendAttachmentPoints
    // ---------------- 3) 正文 UTF-16LE 往返；4) flags 0 与 NO_INCREMENTAL
    {
        let source = build_pdf(300, 300, 0, "BT /F1 24 Tf 60 45 Td (Hello) Tj ET");
        let path = std::env::temp_dir().join(format!("s5a-hl-{}.pdf", std::process::id()));
        std::fs::write(&path, &source).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        unsafe {
            let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            assert!(!doc.is_null());
            let page = (ffi.load_page)(doc, 0);
            let annot = (ffi.create_annot)(page, ANNOT_HIGHLIGHT);
            assert!(!annot.is_null(), "CreateAnnot(Highlight) 失败");
            let rect = FsRectF {
                left: 40.0,
                top: 60.0,
                right: 260.0,
                bottom: 40.0,
            };
            assert_eq!((ffi.set_rect)(annot, &rect), 1);
            assert_eq!((ffi.set_color)(annot, 0, 255, 255, 0, 255), 1);
            // /QuadPoints 顺序：左上、右上、左下、右下（PDF 默认坐标系）
            let quad = FsQuadPointsF {
                x1: 40.0,
                y1: 60.0,
                x2: 260.0,
                y2: 60.0,
                x3: 40.0,
                y3: 40.0,
                x4: 260.0,
                y4: 40.0,
            };
            assert_eq!(
                (ffi.append_quad)(annot, &quad),
                1,
                "AppendAttachmentPoints 失败"
            );

            // /Contents：UTF-16LE（带结尾 NUL）
            let text = "中文正文测试";
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let key = CString::new("Contents").unwrap();
            assert_eq!(
                (ffi.set_string)(annot, key.as_ptr(), utf16.as_ptr()),
                1,
                "SetStringValue 失败"
            );
            let mut buffer = vec![0u16; 256];
            let length = (ffi.get_string)(annot, key.as_ptr(), buffer.as_mut_ptr(), 512);
            let decoded = {
                let units: Vec<u16> = buffer[..(length as usize / 2).saturating_sub(1)]
                    .iter()
                    .copied()
                    .take_while(|unit| *unit != 0)
                    .collect();
                String::from_utf16_lossy(&units)
            };
            eprintln!("[S5a/3] /Contents 往返：{decoded:?}（期望 {text:?}）");
            assert_eq!(decoded, text);

            let save = |flags: c_uint| -> Vec<u8> {
                let mut writer = Writer {
                    base: FpdfFileWrite {
                        version: 1,
                        write_block: Some(write_block),
                    },
                    data: Vec::new(),
                };
                assert_eq!(
                    (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, flags),
                    1
                );
                writer.data
            };
            let with_default = save(0);
            let with_no_incremental = save(2);
            eprintln!(
                "[S5a/4] SaveAsCopy: flags=0 → {} 字节；FPDF_NO_INCREMENTAL → {} 字节",
                with_default.len(),
                with_no_incremental.len()
            );
            for (label, bytes) in [
                ("flags=0", &with_default),
                ("NO_INCREMENTAL", &with_no_incremental),
            ] {
                let saved_path = std::env::temp_dir().join(format!(
                    "s5a-flags-{}-{}.pdf",
                    std::process::id(),
                    label.replace('=', "")
                ));
                std::fs::write(&saved_path, bytes).unwrap();
                let probe = (ffi.load_document)(
                    CString::new(saved_path.to_str().unwrap()).unwrap().as_ptr(),
                    std::ptr::null(),
                );
                assert!(!probe.is_null(), "{label} 保存结果打不开");
                let page_probe = (ffi.load_page)(probe, 0);
                eprintln!(
                    "[S5a/4] {label}: 重开成功，注释数 = {}",
                    (ffi.annot_count)(page_probe)
                );
                (ffi.close_page)(page_probe);
                (ffi.close_document)(probe);
            }

            (ffi.close_annot)(annot);
            (ffi.close_page)(page);
            (ffi.close_document)(doc);

            let image = render(&with_no_incremental);
            let yellow = count_pixels(&image, is_yellow);
            let yellow_bbox = bbox(&image, is_yellow).expect("黄色像素");
            // 页面空间 quad y 40..60 → 位图行 240..260；x 40..260
            eprintln!("[S5a/2] 高亮包围盒 {yellow_bbox:?}（期望 x≈40..260，y≈240..260）");
            assert!(
                (yellow_bbox.0 as i32 - 40).abs() <= 3
                    && (yellow_bbox.2 as i32 - 260).abs() <= 3
                    && (yellow_bbox.1 as i32 - 240).abs() <= 3
                    && (yellow_bbox.3 as i32 - 260).abs() <= 3,
                "高亮位置不对：{yellow_bbox:?}（QuadPoints 应为页面空间直写）"
            );
            let dark = count_pixels(&image, |pixel| {
                pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80
            });
            eprintln!(
                "[S5a/2] 高亮：黄色像素 {yellow} 个，深色（文字）像素 {dark} 个（页面 300×300）"
            );
            assert!(yellow > 500, "高亮没渲染出来（yellow={yellow}）");
        }
    }

    // ---------------- 6) 下划线与删除线：同一套 QuadPoints 路径，确认都能渲染
    {
        for (_, subtype) in [("下划线", ANNOT_UNDERLINE), ("删除线", ANNOT_STRIKEOUT)] {
            let source = build_pdf(300, 300, 0, "BT /F1 24 Tf 60 45 Td (Hello) Tj ET");
            let path =
                std::env::temp_dir().join(format!("s5a-{}-{}.pdf", subtype, std::process::id()));
            std::fs::write(&path, &source).unwrap();
            let c_path = CString::new(path.to_str().unwrap()).unwrap();
            unsafe {
                let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
                let page = (ffi.load_page)(doc, 0);
                let annot = (ffi.create_annot)(page, subtype);
                assert!(!annot.is_null(), "CreateAnnot 失败");
                let rect = FsRectF {
                    left: 40.0,
                    top: 60.0,
                    right: 260.0,
                    bottom: 40.0,
                };
                (ffi.set_rect)(annot, &rect);
                (ffi.set_color)(annot, 0, 255, 0, 0, 255);
                let quad = FsQuadPointsF {
                    x1: 40.0,
                    y1: 60.0,
                    x2: 260.0,
                    y2: 60.0,
                    x3: 40.0,
                    y3: 40.0,
                    x4: 260.0,
                    y4: 40.0,
                };
                assert_eq!((ffi.append_quad)(annot, &quad), 1, "AppendAttachmentPoints");
                let mut writer = Writer {
                    base: FpdfFileWrite {
                        version: 1,
                        write_block: Some(write_block),
                    },
                    data: Vec::new(),
                };
                assert_eq!(
                    (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2),
                    1
                );
                let saved = writer.data;
                (ffi.close_annot)(annot);
                (ffi.close_page)(page);
                (ffi.close_document)(doc);
                let image = render(&saved);
                let red = count_pixels(&image, is_red);
                let red_bbox = bbox(&image, is_red);
                eprintln!("[S5a/6] subtype {subtype}（无 AP，仅 QuadPoints）：红色像素 {red} 个，包围盒 {red_bbox:?}");
            }
        }
    }

    // ---------------- 6b) 对照：原始 PDF 里直接放 /Underline、/StrikeOut（无 AP）
    {
        for (_, subtype) in [("下划线", "Underline"), ("删除线", "StrikeOut")] {
            let objects = [
                "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Rotate 0                  /Contents 4 0 R /Annots [5 0 R] /Resources << /Font << /F1 6 0 R >> >> >>"
                    .to_string(),
                stream_object("BT /F1 24 Tf 60 45 Td (Hello) Tj ET"),
                format!(
                    "<< /Type /Annot /Subtype /{subtype} /Rect [40 40 260 60]                      /QuadPoints [40 60 260 60 40 40 260 40] /C [1 0 0] /F 4 >>"
                ),
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            ];
            let mut out = String::from("%PDF-1.4\n");
            let mut offsets = Vec::with_capacity(objects.len());
            for (index, body) in objects.iter().enumerate() {
                offsets.push(out.len());
                out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
            }
            let xref_offset = out.len();
            let size = objects.len() + 1;
            out.push_str(&format!("xref\n0 {size}\n"));
            out.push_str("0000000000 65535 f \n");
            for offset in &offsets {
                out.push_str(&format!("{offset:010} 00000 n \n"));
            }
            out.push_str(&format!(
                "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
            ));
            let image = render(out.as_bytes());
            let red = count_pixels(&image, is_red);
            let red_bbox = bbox(&image, is_red);
            eprintln!("[S5a/6b] 原始 /{subtype}（无 AP）：红色像素 {red} 个，包围盒 {red_bbox:?}");
        }
    }

    // ---------------- 6c) 下划线 + AppendObject（文档说只支持 ink/stamp，试试看）
    {
        let source = build_pdf(300, 300, 0, "BT /F1 24 Tf 60 45 Td (Hello) Tj ET");
        let path = std::env::temp_dir().join(format!("s5a-ul-ap-{}.pdf", std::process::id()));
        std::fs::write(&path, &source).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        unsafe {
            let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            let page = (ffi.load_page)(doc, 0);
            let annot = (ffi.create_annot)(page, ANNOT_UNDERLINE);
            let rect = FsRectF {
                left: 40.0,
                top: 60.0,
                right: 260.0,
                bottom: 40.0,
            };
            (ffi.set_rect)(annot, &rect);
            (ffi.set_color)(annot, 0, 255, 0, 0, 255);
            let quad = FsQuadPointsF {
                x1: 40.0,
                y1: 60.0,
                x2: 260.0,
                y2: 60.0,
                x3: 40.0,
                y3: 40.0,
                x4: 260.0,
                y4: 40.0,
            };
            (ffi.append_quad)(annot, &quad);
            let object = (ffi.new_path)(40.0, 41.0);
            (ffi.line_to)(object, 260.0, 41.0);
            (ffi.set_stroke_color)(object, 255, 0, 0, 255);
            (ffi.set_stroke_width)(object, 2.0);
            (ffi.set_draw_mode)(object, 0, 1);
            let appended = (ffi.append_object)(annot, object);
            let mut writer = Writer {
                base: FpdfFileWrite {
                    version: 1,
                    write_block: Some(write_block),
                },
                data: Vec::new(),
            };
            let saved_ok = (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2);
            let saved = writer.data;
            (ffi.close_annot)(annot);
            (ffi.close_page)(page);
            (ffi.close_document)(doc);
            let image = render(&saved);
            let red = count_pixels(&image, is_red);
            eprintln!(
                "[S5a/6c] 下划线 + AppendObject：返回值 {appended}，保存 {saved_ok}，红色像素 {red} 个，包围盒 {:?}",
                bbox(&image, is_red)
            );
        }
    }

    // ---------------- 6d) SetAP：直接装一个外观流字符串
    {
        let source = build_pdf(300, 300, 0, "BT /F1 24 Tf 60 45 Td (Hello) Tj ET");
        let path = std::env::temp_dir().join(format!("s5a-setap-{}.pdf", std::process::id()));
        std::fs::write(&path, &source).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        unsafe {
            let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            let page = (ffi.load_page)(doc, 0);
            let annot = (ffi.create_annot)(page, ANNOT_UNDERLINE);
            let rect = FsRectF {
                left: 40.0,
                top: 60.0,
                right: 260.0,
                bottom: 40.0,
            };
            (ffi.set_rect)(annot, &rect);
            (ffi.set_color)(annot, 0, 255, 0, 0, 255);
            let quad = FsQuadPointsF {
                x1: 40.0,
                y1: 60.0,
                x2: 260.0,
                y2: 60.0,
                x3: 40.0,
                y3: 40.0,
                x4: 260.0,
                y4: 40.0,
            };
            (ffi.append_quad)(annot, &quad);
            let content = "q 1 0 0 RG 2 w 40 41 m 260 41 l S Q";
            let utf16: Vec<u16> = content.encode_utf16().chain(std::iter::once(0)).collect();
            let set = (ffi.set_ap)(annot, 0, utf16.as_ptr());
            let mut writer = Writer {
                base: FpdfFileWrite {
                    version: 1,
                    write_block: Some(write_block),
                },
                data: Vec::new(),
            };
            let saved_ok = (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2);
            let saved = writer.data;
            (ffi.close_annot)(annot);
            (ffi.close_page)(page);
            (ffi.close_document)(doc);
            let out =
                std::env::temp_dir().join(format!("s5a-setap-out-{}.pdf", std::process::id()));
            std::fs::write(&out, &saved).unwrap();
            let image = render(&saved);
            let red = count_pixels(&image, is_red);
            eprintln!(
                "[S5a/6d] SetAP：返回 {set}，保存 {saved_ok}，红色像素 {red} 个，包围盒 {:?}（文件 {}）",
                bbox(&image, is_red), out.display()
            );
        }
    }

    // ---------------- 6e) 空页上的 /StrikeOut 与 /Underline（确认是否依赖文字层）
    {
        for (_, subtype, annot_type) in [
            ("下划线", "Underline", ANNOT_UNDERLINE),
            ("删除线", "StrikeOut", ANNOT_STRIKEOUT),
        ] {
            let source = build_pdf(300, 300, 0, "");
            let path = std::env::temp_dir().join(format!(
                "s5a-empty-{}-{}.pdf",
                subtype,
                std::process::id()
            ));
            std::fs::write(&path, &source).unwrap();
            let c_path = CString::new(path.to_str().unwrap()).unwrap();
            unsafe {
                let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
                let page = (ffi.load_page)(doc, 0);
                let annot = (ffi.create_annot)(page, annot_type);
                let rect = FsRectF {
                    left: 40.0,
                    top: 60.0,
                    right: 260.0,
                    bottom: 40.0,
                };
                (ffi.set_rect)(annot, &rect);
                (ffi.set_color)(annot, 0, 255, 0, 0, 255);
                let quad = FsQuadPointsF {
                    x1: 40.0,
                    y1: 60.0,
                    x2: 260.0,
                    y2: 60.0,
                    x3: 40.0,
                    y3: 40.0,
                    x4: 260.0,
                    y4: 40.0,
                };
                (ffi.append_quad)(annot, &quad);
                let mut writer = Writer {
                    base: FpdfFileWrite {
                        version: 1,
                        write_block: Some(write_block),
                    },
                    data: Vec::new(),
                };
                (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2);
                let saved = writer.data;
                (ffi.close_annot)(annot);
                (ffi.close_page)(page);
                (ffi.close_document)(doc);
                let image = render(&saved);
                eprintln!(
                    "[S5a/6e] 空页 /{subtype}：红色像素 {} 个，包围盒 {:?}",
                    count_pixels(&image, is_red),
                    bbox(&image, is_red)
                );
            }
        }
    }

    // ---------------- 6f) 墨迹 + SetAP（两条子路径，验证通用外观方案）
    {
        let source = build_pdf(300, 300, 0, "");
        let path = std::env::temp_dir().join(format!("s5a-ink-ap-{}.pdf", std::process::id()));
        std::fs::write(&path, &source).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        unsafe {
            let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            let page = (ffi.load_page)(doc, 0);
            let annot = (ffi.create_annot)(page, ANNOT_INK);
            let rect = FsRectF {
                left: 40.0,
                top: 120.0,
                right: 260.0,
                bottom: 40.0,
            };
            (ffi.set_rect)(annot, &rect);
            (ffi.set_color)(annot, 0, 255, 0, 0, 255);
            let stroke_a = [
                FsPointF { x: 40.0, y: 60.0 },
                FsPointF { x: 260.0, y: 60.0 },
            ];
            let stroke_b = [
                FsPointF { x: 40.0, y: 100.0 },
                FsPointF { x: 140.0, y: 120.0 },
            ];
            (ffi.add_ink_stroke)(annot, stroke_a.as_ptr(), stroke_a.len());
            (ffi.add_ink_stroke)(annot, stroke_b.as_ptr(), stroke_b.len());
            let content = "q 1 0 0 RG 2 w 40 60 m 260 60 l S 40 100 m 140 120 l S Q";
            let utf16: Vec<u16> = content.encode_utf16().chain(std::iter::once(0)).collect();
            let set = (ffi.set_ap)(annot, 0, utf16.as_ptr());
            let mut writer = Writer {
                base: FpdfFileWrite {
                    version: 1,
                    write_block: Some(write_block),
                },
                data: Vec::new(),
            };
            (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2);
            let saved = writer.data;
            (ffi.close_annot)(annot);
            (ffi.close_page)(page);
            (ffi.close_document)(doc);
            let image = render(&saved);
            eprintln!(
                "[S5a/6f] 墨迹 + SetAP（2 条子路径）：返回 {set}，红色像素 {} 个，包围盒 {:?}",
                count_pixels(&image, is_red),
                bbox(&image, is_red)
            );
        }
    }

    // ---------------- 7) 已有内容不被破坏：用带链接的真实样本
    {
        // 从工作区根运行与从 crate 目录运行都要能找到
        let sample = [
            "scripts/replay/rotated_2p.pdf",
            "../../scripts/replay/rotated_2p.pdf",
        ]
        .iter()
        .map(std::path::Path::new)
        .find(|path| path.exists());
        if let Some(sample) = sample {
            let c_path = CString::new(sample.to_str().unwrap()).unwrap();
            unsafe {
                let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
                assert!(!doc.is_null(), "样本打不开");
                let pages = (ffi.page_count)(doc);
                let page = (ffi.load_page)(doc, 0);
                let annot = (ffi.create_annot)(page, ANNOT_INK);
                let stroke = [
                    FsPointF { x: 40.0, y: 40.0 },
                    FsPointF { x: 260.0, y: 40.0 },
                ];
                assert_eq!(
                    (ffi.add_ink_stroke)(annot, stroke.as_ptr(), stroke.len()),
                    0
                );
                let object = (ffi.new_path)(40.0, 40.0);
                (ffi.line_to)(object, 260.0, 40.0);
                (ffi.set_stroke_color)(object, 255, 0, 0, 255);
                (ffi.set_stroke_width)(object, 2.0);
                (ffi.set_draw_mode)(object, 0, 1);
                (ffi.append_object)(annot, object);
                let mut writer = Writer {
                    base: FpdfFileWrite {
                        version: 1,
                        write_block: Some(write_block),
                    },
                    data: Vec::new(),
                };
                assert_eq!(
                    (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2),
                    1
                );
                let saved = writer.data;
                (ffi.close_annot)(annot);
                (ffi.close_page)(page);
                (ffi.close_document)(doc);

                let backend = PdfiumBackend::new();
                let opened = backend.open(Source::Bytes(&saved), None).expect("重开");
                assert_eq!(
                    opened.document.info().page_count,
                    pages as u32,
                    "页数必须不变"
                );
                let navigation = opened.document.navigation().expect("导航");
                let links = navigation.links_on(PageIndex::FIRST).len();
                eprintln!(
                    "[S5a/7] 真实样本：页数 {}，第 1 页链接 {} 条，保存 {} 字节",
                    pages,
                    links,
                    saved.len()
                );
                assert_eq!(links, 1, "已有链接不能被写回破坏");
            }
        } else {
            eprintln!(
                "[S5a/7] 跳过：找不到 rotated_2p.pdf（先跑 cargo run -p app -- --samples scripts/replay）"
            );
        }
    }

    // ---------------- 5) 旋转页（/Rotate 90）：只为确认"不崩、能保存、能渲染"
    {
        let source = build_pdf(400, 200, 90, "");
        let path = std::env::temp_dir().join(format!("s5a-rot-{}.pdf", std::process::id()));
        std::fs::write(&path, &source).unwrap();
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        unsafe {
            let doc = (ffi.load_document)(c_path.as_ptr(), std::ptr::null());
            assert!(!doc.is_null());
            let page = (ffi.load_page)(doc, 0);
            eprintln!("[S5a/5] 旋转页 /Rotate = {}", (ffi.page_rotation)(page));
            let annot = (ffi.create_annot)(page, ANNOT_HIGHLIGHT);
            let rect = FsRectF {
                left: 50.0,
                top: 150.0,
                right: 250.0,
                bottom: 100.0,
            };
            assert_eq!((ffi.set_rect)(annot, &rect), 1);
            assert_eq!((ffi.set_color)(annot, 0, 255, 255, 0, 255), 1);
            let quad = FsQuadPointsF {
                x1: 50.0,
                y1: 150.0,
                x2: 250.0,
                y2: 150.0,
                x3: 50.0,
                y3: 100.0,
                x4: 250.0,
                y4: 100.0,
            };
            assert_eq!((ffi.append_quad)(annot, &quad), 1);
            let mut writer = Writer {
                base: FpdfFileWrite {
                    version: 1,
                    write_block: Some(write_block),
                },
                data: Vec::new(),
            };
            assert_eq!(
                (ffi.save_as_copy)(doc, &mut writer.base as *mut FpdfFileWrite, 2),
                1
            );
            let saved = writer.data;
            (ffi.close_annot)(annot);
            (ffi.close_page)(page);
            (ffi.close_document)(doc);
            let image = render(&saved);
            let yellow = count_pixels(&image, is_yellow);
            eprintln!(
                "[S5a/5] 旋转页保存后渲染：位图 {}×{}，黄色像素 {yellow} 个",
                image.width, image.height
            );
            assert!(yellow > 500, "旋转页高亮没渲染出来");
        }
    }
}
