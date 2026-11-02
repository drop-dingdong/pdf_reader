//! 浮窗 Markdown 里的 LaTeX 数学（P1.5：`latex-rust` + PNG 纹理）。
//!
//! 只做“公式 → 透明 PNG → egui 纹理”，不引入 SVG/浏览器。解析或排版失败时
//! 回退成原文 `$...$`，不 panic；纹理有数量上限，避免连续编辑把显存吃满。

use std::cell::RefCell;
use std::collections::HashMap;
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
/// 9.75pt ≈ 13px @96dpi；latex-rust 的 SVG/PNG 用 pt。
const PT_PER_EM: (i64, i64) = (39, 4);
/// 纹理超采样倍率（按设备像素再乘 pixels_per_point）。
const SUPERSAMPLE: f32 = 2.0;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MathKey {
    tex: String,
    inline: bool,
    pixels_per_point_milli: u32,
}

/// 一张已上传的数学纹理及其在 egui 中的逻辑尺寸。
#[derive(Clone)]
pub struct MathImage {
    /// 纹理
    pub texture: TextureHandle,
    /// 逻辑尺寸（egui 点）
    pub size: Vec2,
}

/// 数学纹理缓存 + 字体。整个应用共用一份。
pub struct MathCache {
    font: Option<MathFont>,
    entries: HashMap<MathKey, Option<MathImage>>,
    next_id: u64,
}

impl MathCache {
    /// 加载内嵌的 STIX Two Math（失败则所有公式回退原文）。
    pub fn new() -> Self {
        Self {
            font: MathFont::stix_two_math().ok(),
            entries: HashMap::new(),
            next_id: 1,
        }
    }

    /// 当前缓存的公式条数（含失败占位），主要用于测试/诊断。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否还没有缓存任何公式。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 取缓存或渲染一张公式纹理。失败返回 `None`（调用方回退原文）。
    pub fn render(&mut self, ctx: &egui::Context, tex: &str, inline: bool) -> Option<MathImage> {
        if tex.is_empty() || tex.len() > MAX_MATH_TEXT {
            return None;
        }
        let key = MathKey {
            tex: tex.to_string(),
            inline,
            pixels_per_point_milli: (ctx.pixels_per_point() * 1000.0).round() as u32,
        };
        if let Some(entry) = self.entries.get(&key) {
            return entry.clone();
        }
        let image = self.render_uncached(ctx, tex, inline);
        if self.entries.len() >= MAX_MATH_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert(key, image.clone());
        image
    }

    fn render_uncached(
        &mut self,
        ctx: &egui::Context,
        tex: &str,
        inline: bool,
    ) -> Option<MathImage> {
        let font = self.font.as_ref()?;
        let style = if inline {
            MathStyle::Text
        } else {
            MathStyle::Display
        };
        let ast = latex_rust::parse(tex).ok()?;
        let boxed = latex_rust::layout(&ast, font, style).ok()?;
        let width_em = dim_f32(boxed.width.clone());
        let height_em = dim_f32(boxed.height.clone() + boxed.depth.clone());
        if !(width_em > 0.0 && height_em > 0.0) {
            return None;
        }
        let size = egui::vec2(
            (width_em * LOGICAL_FONT_SIZE).max(1.0),
            (height_em * LOGICAL_FONT_SIZE).max(1.0),
        );

        let ppp = ctx.pixels_per_point().max(0.1);
        let mut scale = SUPERSAMPLE;
        let mut pixels = size.x * size.y * ppp * ppp * scale * scale;
        if pixels > MAX_TEXTURE_PIXELS {
            scale = 1.0;
            pixels = size.x * size.y * ppp * ppp;
        }
        if pixels > MAX_TEXTURE_PIXELS {
            return None;
        }
        let dpi = (96.0 * ppp * scale).round().clamp(96.0, 2400.0) as i64;
        let mut options = PngOptions::new();
        options.font_size_pt = Dim::ratio(PT_PER_EM.0, PT_PER_EM.1);
        options.dpi = Dim::from_i64(dpi);
        options.color = latex_rust::Color::rgb(225, 225, 225);
        options.display = !inline;
        let png = latex_rust::latex_to_png(tex, font, &options).ok()?;
        let decoded = image::load_from_memory(&png).ok()?.into_rgba8();
        let (pixel_width, pixel_height) = decoded.dimensions();
        if pixel_width == 0 || pixel_height == 0 {
            return None;
        }
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [pixel_width as usize, pixel_height as usize],
            decoded.as_raw(),
        );
        let texture = ctx.load_texture(
            format!("latex-{}-{}", self.next_id, key_hash(tex, inline, dpi)),
            color_image,
            TextureOptions::LINEAR,
        );
        self.next_id = self.next_id.wrapping_add(1).max(1);
        Some(MathImage { texture, size })
    }
}

impl Default for MathCache {
    fn default() -> Self {
        Self::new()
    }
}

fn dim_f32(dim: Dim) -> f32 {
    f32::from_bits(dim.to_ieee32_bits())
}

fn key_hash(tex: &str, inline: bool, dpi: i64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tex.hash(&mut hasher);
    inline.hash(&mut hasher);
    dpi.hash(&mut hasher);
    hasher.finish()
}

/// 在 `egui_commonmark` 的 math 回调里绘制公式。
pub fn paint(ui: &mut egui::Ui, cache: &RefCell<MathCache>, tex: &str, inline: bool) {
    let image = cache.borrow_mut().render(ui.ctx(), tex, inline);
    let Some(image) = image else {
        // 解析失败/缺字形（例如 `\text{中文}` 在 STIX 里没有 CJK）：
        // 保留原文，用户仍能看懂；不 panic。
        let fallback = if inline {
            format!("${tex}$")
        } else {
            format!("$${tex}$$")
        };
        ui.label(egui::RichText::new(fallback).monospace());
        return;
    };
    // 行内公式按图片尺寸占位；块级公式占满当前可用宽度并在其中居中。
    let width = if inline {
        image.size.x
    } else {
        ui.available_width().max(image.size.x)
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, image.size.y), Sense::hover());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        ctx
    }

    #[test]
    fn invalid_formula_falls_back_without_a_texture() {
        let ctx = ctx();
        let mut cache = MathCache::new();
        assert!(cache.render(&ctx, r"\frac{1}{", true).is_none());
    }

    #[test]
    fn cjk_text_mode_is_reported_as_missing_glyphs() {
        let ctx = ctx();
        let mut cache = MathCache::new();
        assert!(cache.render(&ctx, r"\text{中文}", true).is_none());
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
                image = cache.render(ui.ctx(), r"x^2 + y^2", true);
            },
        );
        output.drop_without_applying_deltas();
        let image = image.expect("简单公式应能渲染");
        assert!(image.size.x > 0.0 && image.size.y > 0.0);
        assert_eq!(cache.entries.len(), 1);
        let again = cache.render(&ctx, r"x^2 + y^2", true);
        assert!(again.is_some());
        assert_eq!(cache.entries.len(), 1, "相同公式应命中缓存");
    }
}
