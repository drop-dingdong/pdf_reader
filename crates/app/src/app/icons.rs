//! 跨平台 SVG 小图标：资源加载、按主题色光栅化与缓存。
//!
//! 规则见 `docs/ui-standards.md`：图标用单色 SVG（占位 `#000000`），按
//! `(Icon, 像素尺寸, 颜色)` 缓存；不依赖系统字体里的生僻 Unicode。

use super::*;

use egui::{Color32, Pos2, Rect, Sense, TextureHandle, Vec2};
use std::collections::HashMap;

/// 图标种类；新增时同时加 `assets/icons/*.svg` 和 `svg()` 映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Icon {
    Close,
    Edit,
    Link,
    Goto,
    More,
    TextPreview,
    ImagePreview,
    ImageRef,
}

impl Icon {
    fn svg(self) -> &'static str {
        match self {
            Self::Close => include_str!("../../assets/icons/close.svg"),
            Self::Edit => include_str!("../../assets/icons/edit.svg"),
            Self::Link => include_str!("../../assets/icons/link.svg"),
            Self::Goto => include_str!("../../assets/icons/goto.svg"),
            Self::More => include_str!("../../assets/icons/more.svg"),
            Self::TextPreview => include_str!("../../assets/icons/text-preview.svg"),
            Self::ImagePreview => include_str!("../../assets/icons/image-preview.svg"),
            Self::ImageRef => include_str!("../../assets/icons/image-ref.svg"),
        }
    }
}

/// 缓存键：同一图标在不同 DPI/主题色下尺寸不同，必须分开缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct IconKey {
    icon: Icon,
    pixels: u32,
    rgb: [u8; 3],
}

/// SVG 图标纹理缓存。
#[derive(Default)]
pub(super) struct IconCache {
    textures: HashMap<IconKey, TextureHandle>,
}

impl IconCache {
    /// 取（必要时生成）一张图标纹理。`logical` 是 egui 逻辑尺寸，实际像素按 DPI 放大。
    pub(super) fn texture(
        &mut self,
        ctx: &egui::Context,
        icon: Icon,
        logical: f32,
        color: Color32,
    ) -> Option<TextureHandle> {
        let pixels = (logical * ctx.pixels_per_point()).round().clamp(8.0, 96.0) as u32;
        let rgb = color.to_array();
        let key = IconKey {
            icon,
            pixels,
            rgb: [rgb[0], rgb[1], rgb[2]],
        };
        if let Some(texture) = self.textures.get(&key) {
            return Some(texture.clone());
        }
        let image = render_svg(icon, pixels, [rgb[0], rgb[1], rgb[2]])?;
        let texture = ctx.load_texture(
            format!(
                "icon-{icon:?}-{pixels}-{:02x}{:02x}{:02x}",
                rgb[0], rgb[1], rgb[2]
            ),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert(key, texture.clone());
        Some(texture)
    }
}

fn render_svg(icon: Icon, pixels: u32, rgb: [u8; 3]) -> Option<egui::ColorImage> {
    let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    let source = icon.svg().replace("#000000", &hex);
    let tree = resvg::usvg::Tree::from_str(&source, &resvg::usvg::Options::default()).ok()?;
    let base = tree.size();
    if !(base.width() > 0.0 && base.height() > 0.0) {
        return None;
    }
    let scale = pixels as f32 / base.width().max(base.height());
    let width = (base.width() * scale).ceil().max(1.0) as u32;
    let height = (base.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
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
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &rgba,
    ))
}

impl PdfReaderApp {
    /// 在 `center` 画一个图标；`size` 是逻辑边长。
    pub(super) fn paint_icon(
        &mut self,
        ui: &egui::Ui,
        icon: Icon,
        center: Pos2,
        size: f32,
        color: Color32,
    ) {
        let Some(texture) = self.icons.texture(ui.ctx(), icon, size, color) else {
            return;
        };
        ui.painter().image(
            texture.id(),
            Rect::from_center_size(center, Vec2::splat(size)),
            Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// 标准图标按钮：固定 24×22 点击区，透明背景，hover 高亮。
    pub(super) fn icon_button(
        &mut self,
        ui: &mut egui::Ui,
        icon: Icon,
        tooltip: &str,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(24.0, 18.0), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(3),
                ui.visuals().widgets.hovered.weak_bg_fill,
            );
        }
        let color = if response.hovered() {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().text_color()
        };
        self.paint_icon(ui, icon, rect.center(), 16.0, color);
        response.on_hover_text(tooltip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_bundled_svgs_render_at_icon_sizes() {
        for icon in [
            Icon::Close,
            Icon::Edit,
            Icon::Link,
            Icon::Goto,
            Icon::More,
            Icon::TextPreview,
            Icon::ImagePreview,
            Icon::ImageRef,
        ] {
            let image = render_svg(icon, 18, [235, 235, 235]).expect("SVG 应能渲染");
            assert!(image.width() > 0 && image.height() > 0, "{icon:?}");
            assert!(
                image.pixels.iter().any(|pixel| pixel.a() > 0),
                "{icon:?} 不应是全透明"
            );
        }
    }
}
