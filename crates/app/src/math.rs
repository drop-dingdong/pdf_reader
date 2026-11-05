//! 浮窗 Markdown 里的 LaTeX 数学。
//!
//! 渲染后端通过 [`MathBackend`] 抽象：当前是 `latex-rust` + 透明 PNG；
//! 以后可以换 RaTeX 等实现，而不改动缓存/错误/纹理上传这条链。
//!
//! 失败时回退成原文 `$...$`，不 panic；错误由 [`MathCache::take_error`] 交给状态栏。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};

use egui::{Color32, Sense, TextureHandle, TextureOptions, Vec2};
use latex_rust::{Dim, MathFont, MathStyle, PngOptions};

/// 公式源码上限：超过就不排版，直接回退原文。
const MAX_MATH_TEXT: usize = 4096;
/// 纹理缓存条数上限：超过后整体清空（简单 LRU 的替代；公式通常不多）。
const MAX_MATH_ENTRIES: usize = 64;
/// 单张纹理的像素上限；超宽公式降采样或回退。
const MAX_TEXTURE_PIXELS: f32 = 8_000_000.0;
/// 与浮窗正文一致的字号（egui 逻辑点）。
const LOGICAL_FONT_SIZE: f32 = 13.0;
/// 9.75pt ≈ 13px @96dpi；latex-rust 的 PNG 用 pt。
const PT_PER_EM: (i64, i64) = (39, 4);
/// 纹理超采样倍率（按设备像素再乘 pixels_per_point）。
const SUPERSAMPLE: f32 = 2.0;

/// 公式的逻辑尺寸（em）。
#[derive(Clone, Debug, Default)]
pub struct MathMetrics {
    /// 宽度（em）
    pub width_em: f32,
    /// 基线以上高度（em）
    pub height_em: f32,
    /// 基线以下深度（em）
    pub depth_em: f32,
}

/// 后端产出的 RGBA 光栅（非预乘）。
#[derive(Clone, Debug)]
pub struct MathRaster {
    /// 像素宽
    pub width: u32,
    /// 像素高
    pub height: u32,
    /// RGBA8 像素
    pub rgba: Vec<u8>,
}

/// 公式失败类别；用于状态栏/帮助窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathErrorKind {
    /// 语法错误
    Parse,
    /// 字体/缺字形
    Font,
    /// 后端明确不支持
    Unsupported,
    /// 超过长度或纹理限制
    TooLarge,
    /// 解码/内部错误
    Internal,
}

impl MathErrorKind {
    /// 状态栏用的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Parse => "语法",
            Self::Font => "字形",
            Self::Unsupported => "不支持",
            Self::TooLarge => "过大",
            Self::Internal => "内部错误",
        }
    }
}

/// 一条公式的失败信息。
#[derive(Clone, Debug)]
pub struct MathError {
    /// 类别
    pub kind: MathErrorKind,
    /// 后端原始错误
    pub message: String,
    /// 出错公式（用于定位）
    pub formula: String,
}

impl MathError {
    fn new(kind: MathErrorKind, formula: &str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            formula: formula.to_string(),
        }
    }

    fn internal(formula: &str, message: impl Into<String>) -> Self {
        Self::new(MathErrorKind::Internal, formula, message)
    }

    /// 状态栏的一行提示（截断公式和错误）。
    pub fn status_line(&self) -> String {
        let formula: String = self.formula.chars().take(48).collect();
        let mut message: String = self.message.chars().take(160).collect();
        if message.is_empty() {
            message = self.kind.label().to_string();
        }
        format!(
            "数学公式：{}（{}）",
            message,
            if formula.is_empty() {
                self.kind.label().to_string()
            } else {
                formula
            }
        )
    }
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}（{}）", self.message, self.formula)
    }
}

/// 后端能力；帮助窗口用它说明“支持什么”。
#[derive(Clone, Debug)]
pub struct MathCapabilities {
    /// 后端名称
    pub backend: &'static str,
    /// 是否支持 `\text{中文}` 这类 CJK 文本
    pub cjk_text: bool,
    /// 是否支持化学式（`\ce`/`\pu`）
    pub chemistry: bool,
    /// 是否原生输出矢量 SVG
    pub vector_svg: bool,
}

/// LaTeX 数学渲染后端。
///
/// 只负责“测量 + 光栅化”，不碰 egui 纹理；纹理缓存/DPI/错误提示由
/// [`MathCache`] 统一处理。这样测试可以替换成假后端，未来也能接 RaTeX。
pub trait MathBackend {
    /// 后端名称
    fn name(&self) -> &'static str;
    /// 后端能力
    fn capabilities(&self) -> MathCapabilities;
    /// 排版并返回 em 尺寸；失败返回可展示的错误
    fn measure(&mut self, tex: &str, display: bool) -> Result<MathMetrics, MathError>;
    /// 按 `dpi` 光栅化；返回透明 RGBA
    fn rasterize(&mut self, tex: &str, display: bool, dpi: f32) -> Result<MathRaster, MathError>;
}

/// 公式后端的光栅来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RasterMode {
    /// 旧 PNG 后端（保留作 fallback/对照）
    Png,
    /// 矢量 SVG → 按目标像素光栅化
    Svg,
}

/// `latex-rust` 后端（内嵌 STIX Two Math）。
struct LatexRustBackend {
    font: Option<MathFont>,
    mode: RasterMode,
}

impl LatexRustBackend {
    fn new() -> Self {
        // 对照用：PDF_READER_MATH_BACKEND=png 可临时回退旧 PNG 路径。
        // 默认 SVG：按目标像素缩放矢量，缩放/高 DPI 下更清晰。
        let mode = if std::env::var("PDF_READER_MATH_BACKEND")
            .is_ok_and(|value| value.eq_ignore_ascii_case("png"))
        {
            RasterMode::Png
        } else {
            RasterMode::Svg
        };
        Self {
            font: MathFont::stix_two_math().ok(),
            mode,
        }
    }

    #[allow(dead_code)]
    fn new_png() -> Self {
        Self {
            font: MathFont::stix_two_math().ok(),
            mode: RasterMode::Png,
        }
    }
}

/// `latex-rust` 把 `~` 解析成字面量 `~`，而 LaTeX 语义是不断行空格。
/// 这里在进入后端前把未转义的 `~` 换成 `\ `；`\~`（重音）保留。
fn normalize_tilde(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let mut backslashes = 0usize;
    for ch in tex.chars() {
        if ch == '~' {
            if backslashes.is_multiple_of(2) {
                out.push_str("\\ ");
            } else {
                out.push('~');
            }
            backslashes = 0;
        } else {
            if ch == '\\' {
                backslashes += 1;
            } else {
                backslashes = 0;
            }
            out.push(ch);
        }
    }
    out
}

impl MathBackend for LatexRustBackend {
    fn name(&self) -> &'static str {
        "latex-rust 3.0.1"
    }

    fn capabilities(&self) -> MathCapabilities {
        MathCapabilities {
            backend: self.name(),
            cjk_text: false,
            chemistry: false,
            vector_svg: self.mode == RasterMode::Svg,
        }
    }

    fn measure(&mut self, tex: &str, display: bool) -> Result<MathMetrics, MathError> {
        let font = self
            .font
            .as_ref()
            .ok_or_else(|| MathError::internal(tex, "STIX Two Math 未加载"))?;
        let style = if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        };
        let normalized = normalize_tilde(tex);
        let ast = latex_rust::parse(&normalized)
            .map_err(|error| MathError::new(MathErrorKind::Parse, tex, error.to_string()))?;
        let boxed =
            latex_rust::layout(&ast, font, style).map_err(|error| map_latex_error(tex, error))?;
        Ok(MathMetrics {
            width_em: dim_f32(boxed.width.clone()),
            height_em: dim_f32(boxed.height.clone()),
            depth_em: dim_f32(boxed.depth.clone()),
        })
    }

    fn rasterize(&mut self, tex: &str, display: bool, dpi: f32) -> Result<MathRaster, MathError> {
        match self.mode {
            RasterMode::Png => self.rasterize_png(tex, display, dpi),
            RasterMode::Svg => self.rasterize_svg(tex, display, dpi),
        }
    }
}

impl LatexRustBackend {
    fn rasterize_png(&self, tex: &str, display: bool, dpi: f32) -> Result<MathRaster, MathError> {
        let font = self
            .font
            .as_ref()
            .ok_or_else(|| MathError::internal(tex, "STIX Two Math 未加载"))?;
        let mut options = PngOptions::new();
        options.font_size_pt = Dim::ratio(PT_PER_EM.0, PT_PER_EM.1);
        options.dpi = Dim::from_i64(dpi.round().clamp(96.0, 2400.0) as i64);
        options.color = latex_rust::Color::rgb(225, 225, 225);
        options.display = display;
        let normalized = normalize_tilde(tex);
        let png = latex_rust::latex_to_png(&normalized, font, &options)
            .map_err(|error| map_latex_error(tex, error))?;
        let decoded = image::load_from_memory(&png)
            .map_err(|error| MathError::internal(tex, format!("PNG 解码失败：{error}")))?
            .into_rgba8();
        let (width, height) = decoded.dimensions();
        if width == 0 || height == 0 {
            return Err(MathError::internal(tex, "PNG 尺寸为 0"));
        }
        Ok(MathRaster {
            width,
            height,
            rgba: decoded.into_raw(),
        })
    }

    fn rasterize_svg(&self, tex: &str, display: bool, dpi: f32) -> Result<MathRaster, MathError> {
        let font = self
            .font
            .as_ref()
            .ok_or_else(|| MathError::internal(tex, "STIX Two Math 未加载"))?;
        let mut options = latex_rust::SvgOptions::new();
        options.font_size_pt = Dim::ratio(PT_PER_EM.0, PT_PER_EM.1);
        options.color = latex_rust::Color::rgb(225, 225, 225);
        options.display = display;
        let normalized = normalize_tilde(tex);
        let svg = latex_rust::latex_to_svg(&normalized, font, &options)
            .map_err(|error| map_latex_error(tex, error))?;
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
            .map_err(|error| MathError::internal(tex, format!("SVG 解析失败：{error}")))?;
        let base = tree.size();
        if !(base.width() > 0.0 && base.height() > 0.0) {
            return Err(MathError::internal(tex, "SVG 尺寸为 0"));
        }
        // SVG 以 pt 生成；resvg 在 96dpi 下把 1pt 当作 1.333px。
        // 这里直接用 dpi/96 缩放矢量，得到目标像素尺寸。
        let scale = (dpi / 96.0).max(f32::MIN_POSITIVE);
        let width = (base.width() * scale).ceil().max(1.0) as u32;
        let height = (base.height() * scale).ceil().max(1.0) as u32;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| MathError::new(MathErrorKind::TooLarge, tex, "SVG 纹理超过像素上限"))?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for pixel in pixmap.pixels() {
            let color = pixel.demultiply();
            rgba.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
        }
        Ok(MathRaster {
            width,
            height,
            rgba,
        })
    }
}

fn map_latex_error(tex: &str, error: latex_rust::Error) -> MathError {
    let kind = match &error {
        latex_rust::Error::Parse(_) | latex_rust::Error::Malformed { .. } => MathErrorKind::Parse,
        latex_rust::Error::Font(_) => MathErrorKind::Font,
        latex_rust::Error::Unsupported { .. } => MathErrorKind::Unsupported,
        latex_rust::Error::InvalidOption { .. } => MathErrorKind::Internal,
    };
    MathError::new(kind, tex, error.to_string())
}

fn dim_f32(dim: Dim) -> f32 {
    f32::from_bits(dim.to_ieee32_bits())
}

fn key_hash(tex: &str, display: bool, dpi: i64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tex.hash(&mut hasher);
    display.hash(&mut hasher);
    dpi.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MathKey {
    tex: String,
    inline: bool,
    pixels_per_point_milli: u32,
}

/// 一张已上传的数学纹理及其逻辑尺寸。
#[derive(Clone)]
pub struct MathImage {
    /// 纹理
    pub texture: TextureHandle,
    /// 逻辑尺寸（egui 点）
    pub size: Vec2,
}

/// 数学纹理缓存 + 后端。整个应用共用一份。
pub struct MathCache {
    backend: Box<dyn MathBackend>,
    entries: HashMap<MathKey, Result<MathImage, MathError>>,
    last_error: Option<MathError>,
    early_reported: HashSet<u64>,
    next_id: u64,
}

impl MathCache {
    /// 使用当前默认后端（`latex-rust`）。
    pub fn new() -> Self {
        Self::with_backend(Box::new(LatexRustBackend::new()))
    }

    fn with_backend(backend: Box<dyn MathBackend>) -> Self {
        Self {
            backend,
            entries: HashMap::new(),
            last_error: None,
            early_reported: HashSet::new(),
            next_id: 1,
        }
    }

    /// 当前后端名称。
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// 当前后端能力。
    pub fn capabilities(&self) -> MathCapabilities {
        self.backend.capabilities()
    }

    /// 当前缓存的公式条数（含失败占位），主要用于测试/诊断。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否还没有缓存任何公式。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 取走最近一次“首次失败”的错误，供状态栏短暂提示。
    pub fn take_error(&mut self) -> Option<MathError> {
        self.last_error.take()
    }

    /// 清空纹理与错误状态；正文被编辑后调用，让公式重新检查/重新提示。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.last_error = None;
        self.early_reported.clear();
    }

    /// 取缓存或渲染一张公式纹理。失败返回错误（调用方回退原文）。
    pub fn render(
        &mut self,
        ctx: &egui::Context,
        tex: &str,
        inline: bool,
    ) -> Result<MathImage, MathError> {
        if tex.is_empty() {
            return Err(MathError::new(MathErrorKind::Parse, tex, "空公式"));
        }
        if tex.len() > MAX_MATH_TEXT {
            let error = MathError::new(
                MathErrorKind::TooLarge,
                tex,
                format!("公式超过 {MAX_MATH_TEXT} 字节"),
            );
            // 长公式不进 entries（避免缓存超长源码）；同一源码只提示一次。
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            tex.hash(&mut hasher);
            let hash = hasher.finish();
            if self.early_reported.len() >= 64 {
                self.early_reported.clear();
            }
            if self.early_reported.insert(hash) {
                self.last_error = Some(error.clone());
            }
            return Err(error);
        }
        let key = MathKey {
            tex: tex.to_string(),
            inline,
            pixels_per_point_milli: (ctx.pixels_per_point() * 1000.0).round() as u32,
        };
        if let Some(entry) = self.entries.get(&key) {
            return entry.clone();
        }
        let result = self.render_uncached(ctx, tex, inline);
        if let Err(error) = &result {
            // 只在首次失败时提示；后续命中缓存错误不反复刷状态栏。
            self.last_error = Some(error.clone());
        }
        if self.entries.len() >= MAX_MATH_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert(key, result.clone());
        result
    }

    fn render_uncached(
        &mut self,
        ctx: &egui::Context,
        tex: &str,
        inline: bool,
    ) -> Result<MathImage, MathError> {
        let display = !inline;
        let metrics = self.backend.measure(tex, display)?;
        if !(metrics.width_em > 0.0 && metrics.height_em + metrics.depth_em > 0.0) {
            return Err(MathError::internal(tex, "排版尺寸为 0"));
        }
        let size = egui::vec2(
            (metrics.width_em * LOGICAL_FONT_SIZE).max(1.0),
            ((metrics.height_em + metrics.depth_em) * LOGICAL_FONT_SIZE).max(1.0),
        );
        let ppp = ctx.pixels_per_point().max(0.1);
        let mut scale = SUPERSAMPLE;
        let mut pixels = size.x * size.y * ppp * ppp * scale * scale;
        if pixels > MAX_TEXTURE_PIXELS {
            scale = 1.0;
            pixels = size.x * size.y * ppp * ppp;
        }
        if pixels > MAX_TEXTURE_PIXELS {
            return Err(MathError::new(
                MathErrorKind::TooLarge,
                tex,
                "公式纹理超过像素上限",
            ));
        }
        let dpi = (96.0 * ppp * scale).round().clamp(96.0, 2400.0);
        let raster = self.backend.rasterize(tex, display, dpi)?;
        if u64::from(raster.width) * u64::from(raster.height) > MAX_TEXTURE_PIXELS as u64 {
            return Err(MathError::new(
                MathErrorKind::TooLarge,
                tex,
                "公式 PNG 超过像素上限",
            ));
        }
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [raster.width as usize, raster.height as usize],
            &raster.rgba,
        );
        let texture = ctx.load_texture(
            format!(
                "latex-{}-{}",
                self.next_id,
                key_hash(tex, display, dpi.round() as i64)
            ),
            color_image,
            TextureOptions::LINEAR,
        );
        self.next_id = self.next_id.wrapping_add(1).max(1);
        Ok(MathImage { texture, size })
    }
}

impl Default for MathCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 在 `egui_commonmark` 的 math 回调里绘制公式。
pub fn paint(ui: &mut egui::Ui, cache: &RefCell<MathCache>, tex: &str, inline: bool) {
    match cache.borrow_mut().render(ui.ctx(), tex, inline) {
        Ok(image) => {
            // 行内公式按图片尺寸占位；块级公式占满当前可用宽度并居中。
            let width = if inline {
                image.size.x
            } else {
                ui.available_width().max(image.size.x)
            };
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, image.size.y), Sense::hover());
            if inline {
                // CommonMark 的 left_to_right 行只在行首由图片撑高；在行中图片
                // 高度不会更新行高，导致同样的公式在行首/行中行距不同。这里
                // 显式把当前行高设为“正文行高与公式高度取大”，两种情况一致。
                let row_height = ui
                    .text_style_height(&egui::TextStyle::Body)
                    .max(image.size.y);
                ui.set_row_height(row_height);
            }
            let target = if inline {
                rect
            } else {
                egui::Rect::from_center_size(rect.center(), image.size)
            };
            ui.painter().image(
                image.texture.id(),
                target,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        Err(_) => {
            let fallback = if inline {
                format!("${tex}$")
            } else {
                format!("$${tex}$$")
            };
            ui.label(egui::RichText::new(fallback).monospace());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        ctx
    }

    struct FakeBackend;

    impl MathBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn capabilities(&self) -> MathCapabilities {
            MathCapabilities {
                backend: self.name(),
                cjk_text: true,
                chemistry: false,
                vector_svg: false,
            }
        }

        fn measure(&mut self, tex: &str, _display: bool) -> Result<MathMetrics, MathError> {
            if tex == "bad" {
                return Err(MathError::new(
                    MathErrorKind::Unsupported,
                    tex,
                    "假后端的失败",
                ));
            }
            Ok(MathMetrics {
                width_em: 1.0,
                height_em: 1.0,
                depth_em: 0.25,
            })
        }

        fn rasterize(
            &mut self,
            _tex: &str,
            _display: bool,
            _dpi: f32,
        ) -> Result<MathRaster, MathError> {
            Ok(MathRaster {
                width: 2,
                height: 2,
                rgba: vec![255; 16],
            })
        }
    }

    #[test]
    fn tilde_becomes_a_space_but_escaped_tilde_is_kept() {
        assert_eq!(normalize_tilde("~1~"), "\\ 1\\ ");
        assert_eq!(normalize_tilde(r"\~n"), r"\~n");
        assert_eq!(normalize_tilde(r"a~~b"), r"a\ \ b");
    }

    #[test]
    fn invalid_formula_reports_an_error_without_a_texture() {
        let ctx = ctx();
        let mut cache = MathCache::new();
        assert!(cache.render(&ctx, r"\frac{1}{", true).is_err());
        let error = cache.take_error().expect("首次失败应留下错误");
        assert_eq!(error.kind, MathErrorKind::Parse);
        assert!(error.status_line().contains("数学公式"));
    }

    #[test]
    fn too_long_formula_is_reported_once() {
        let ctx = ctx();
        let mut cache = MathCache::new();
        let long = "x".repeat(MAX_MATH_TEXT + 1);
        assert!(cache.render(&ctx, &long, true).is_err());
        assert!(cache.take_error().is_some());
        assert!(cache.render(&ctx, &long, true).is_err());
        assert!(cache.take_error().is_none(), "同一超长公式只提示一次");
    }

    #[test]
    fn cjk_text_mode_is_reported_as_missing_glyphs() {
        let ctx = ctx();
        let mut cache = MathCache::new();
        let error = cache
            .render(&ctx, r"\text{中文}", true)
            .err()
            .expect("STIX 没有 CJK 字形");
        assert_eq!(error.kind, MathErrorKind::Font);
    }

    #[test]
    fn svg_backend_reports_vector_and_renders_a_texture() {
        let mut cache = MathCache::new();
        assert!(cache.capabilities().vector_svg);
        let image = cache
            .render(&ctx(), "\\frac{1}{2}", true)
            .expect("SVG 公式应能渲染");
        assert!(image.size.x > 0.0 && image.size.y > 0.0);
    }

    #[test]
    fn a_simple_formula_renders_and_is_cached() {
        let ctx = ctx();
        let mut cache = MathCache::new();
        let mut image = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                image = cache.render(ui.ctx(), r"x^2 + y^2", true).ok();
            },
        );
        output.drop_without_applying_deltas();
        let image = image.expect("简单公式应能渲染");
        assert!(image.size.x > 0.0 && image.size.y > 0.0);
        assert_eq!(cache.len(), 1);
        assert!(cache.take_error().is_none());
        let again = cache.render(&ctx, r"x^2 + y^2", true);
        assert!(again.is_ok());
        assert_eq!(cache.len(), 1, "相同公式应命中缓存");
    }

    #[test]
    fn a_custom_backend_can_be_plugged_in() {
        let ctx = ctx();
        let mut cache = MathCache::with_backend(Box::new(FakeBackend));
        let image = cache.render(&ctx, r"\frac{1}{2}", true).expect("假后端");
        assert!(image.size.x > 0.0);
        assert_eq!(cache.backend_name(), "fake");
        assert!(cache.capabilities().cjk_text);
        assert!(cache.render(&ctx, "bad", true).is_err());
        assert_eq!(
            cache.take_error().map(|error| error.kind),
            Some(MathErrorKind::Unsupported)
        );
    }
}
