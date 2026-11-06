//! `app::textures`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::view::{
    color_image, compose_region_snapshot, desired_textures, texture_pages, tile_view_rect,
    PageFacts, PageShape,
};
use eframe::egui;
use std::sync::Arc;

impl PdfReaderApp {
    /// 让纹理层与"这一帧该有的纹理"（[`desired_textures`]）对账（D-112）。
    ///
    /// 三个步骤，顺序固定：
    /// 1. **算** `desired`：唯一决定"GPU 里该有哪些键"的地方（纯函数，每帧算，成本几微秒）；
    /// 2. **删**：`present - keep` 立刻删除（不留旧图过夜）；
    /// 3. **传**：`desired.upload` 里"位图已到"的那些传上去。
    ///
    /// 于是"上传集合 ⊆ 保留集合"由构造保证 —— 不会再出现"传上去、下一帧被裁、再传"的循环。
    /// `allow_upload = false`（拖动期间）只做 1、2：**只删不传**，把逐像素转换挪到松手之后。
    pub(super) fn ensure_textures(
        &mut self,
        ctx: &egui::Context,
        session: &mut DocumentSession,
        allow_upload: bool,
    ) -> usize {
        // 纹理单边上限来自后端（显卡能力）。告诉会话：单边超限的页会被强制分块（D-108）。
        let max_side = ctx.input(|input| input.max_texture_side as u32);
        if session.max_texture_side() != max_side {
            session.set_max_texture_side(max_side);
        }
        self.visible.clear();
        session.visible_pages(&mut self.visible);
        let targets = texture_pages(
            &self.visible,
            session.state().current_page,
            session.page_count(),
        );

        // 开场底料（D-110）：目标页如果**一张整页图都没有**（起步就在高倍、或刚跳到没去过的页），
        // 请会话渲一张低分辨率整页图。它只当底料用，不会被当成"当前档已经清楚"。
        for page in &targets {
            if !session.has_whole_page_image(*page) && session.thumbnail_key(*page).is_none() {
                session.request_thumbnail(*page);
            }
        }

        // ---- 1. 算 desired ----
        let facts: Vec<PageFacts> = targets
            .iter()
            .filter_map(|page| {
                let placement = session.placement(*page)?;
                let render_milli =
                    RenderKey::new(*page, session.render_scale(*page), placement.rotation)
                        .scale_milli;
                let shape = match session.tile_grid(*page) {
                    None => PageShape::Whole,
                    Some(_) => match session.visible_tiles(*page) {
                        Some((col0, row0, col1, row1)) => PageShape::Tiles(col0, row0, col1, row1),
                        None => PageShape::TilesHidden,
                    },
                };
                Some(PageFacts {
                    page: *page,
                    rotation: placement.rotation,
                    render_milli,
                    shape,
                    whole_pages: session.whole_page_scales(*page),
                })
            })
            .collect();
        let page_sizes: Vec<(PageIndex, Rotation, Size)> = facts
            .iter()
            .filter_map(|fact| {
                let placement = session.placement(fact.page)?;
                Some((fact.page, fact.rotation, placement.page_size))
            })
            .collect();
        let existing = self.textures.keys_newest_first();
        let desired = desired_textures(&facts, &existing, self.textures.max_bytes(), |key| {
            let Some((_, _, page_size)) = page_sizes
                .iter()
                .find(|(page, rotation, _)| *page == key.page && *rotation == key.rotation)
            else {
                return 0;
            };
            texture_key_bytes(*page_size, key.rotation, max_side, key)
        });

        // ---- 2. 删 ----
        self.textures.keep_only(&desired.keep);

        // ---- 3. 传 ----
        let mut uploaded = 0usize;
        if allow_upload {
            for key in &desired.upload {
                if uploaded >= MAX_TEXTURE_UPLOADS_PER_FRAME {
                    break;
                }
                let Some(image) = session.image_for_key(key) else {
                    continue;
                };
                self.note_uploaded(session, key, &image);
                self.upload(ctx, *key, &image);
                uploaded += 1;
            }
        }

        // 软上限：当前视口自身就超预算时只报告一次（这台机器的显示需求超出预算）
        if desired.over_budget && !self.budget_warned {
            self.budget_warned = true;
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[贴图] 纹理超出预算：当前视口需要 {:.1}MB > 预算 {:.0}MB（不淘汰当前视口，只提示）",
                    desired.must_bytes as f64 / (1024.0 * 1024.0),
                    self.textures.max_bytes() as f64 / (1024.0 * 1024.0)
                ));
            }
        }
        self.last_retain = desired;
        uploaded
    }

    /// `[贴图]` 诊断：记录"这一帧传了哪个键、多大"。
    ///
    /// 这条日志是 D-110 那次"每帧重传"被发现的唯一线索，所以对账循环里也必须保留。
    pub(super) fn note_uploaded(
        &mut self,
        session: &DocumentSession,
        key: &RenderKey,
        image: &Arc<RasterImage>,
    ) {
        if !self.diag.enabled() {
            return;
        }
        let Some(placement) = session.placement(key.page).copied() else {
            return;
        };
        let scroll = session.viewport().scroll;
        match key.tile {
            None => self.diag.line(format!(
                "[贴图] 页={} 整页图={}x{} 渲染比例={:.3} 键={}",
                key.page.display_number(),
                image.width,
                image.height,
                key.scale_milli as f32 / 1000.0,
                key.scale_milli
            )),
            Some(tile) => {
                let Some(grid) = session.tile_grid(key.page) else {
                    return;
                };
                let (page_width, page_height) = grid.render_px();
                let (x, y, width, height) = grid.tile_px(tile.col, tile.row);
                let screen = tile_view_rect(
                    &grid,
                    &placement,
                    tile.col,
                    tile.row,
                    session.render_scale(key.page).0,
                    scroll,
                );
                self.diag.line(format!(
                    "[贴图] 页={} col={} row={} 瓦片图={}x{} 页面px={}x{} 显示px={:.0}x{:.0} 瓦片px=({},{},{},{}) 屏幕=({:.1},{:.1},{:.0},{:.0}) scroll=({:.1},{:.1}) 渲染比例={:.3}",
                    key.page.get(), tile.col, tile.row, image.width, image.height,
                    page_width, page_height,
                    placement.rect.width, placement.rect.height,
                    x, y, width, height,
                    screen.x, screen.y, screen.width, screen.height,
                    scroll.x, scroll.y,
                    key.scale_milli as f32 / 1000.0
                ));
            }
        }
    }

    pub(super) fn upload(&mut self, ctx: &egui::Context, key: RenderKey, image: &Arc<RasterImage>) {
        if self.textures.contains(&key) {
            return;
        }
        // 全项目唯一的像素格式转换点：**一遍**完成（见 view::color_image 的说明）
        let started = std::time::Instant::now();
        let color = color_image(image.format, image.data(), image.width, image.height);
        self.last_convert_ms += started.elapsed().as_secs_f64() * 1000.0;

        let started = std::time::Instant::now();
        let texture = ctx.load_texture(
            format!("page-{}-{}-{:?}", key.page.get(), key.scale_milli, key.tile),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.last_upload_ms += started.elapsed().as_secs_f64() * 1000.0;
        self.textures.insert(key, texture);
    }

    /// 按**记录的分辨率**取得某页某区域的预览图；缺像素就**提交渲染**并返回 `None`（D-101）。
    ///
    /// 流程：
    /// 1. 记录的分辨率 `scale`（v2 存档字段，`1.0` = 100%）→ 吸到波段上
    ///    （`render_scale_for`），这样与主视图同波段时能直接命中已有缓存；
    /// 2. `region_tiles` 已全部就绪 → 在**渲染像素**里 1:1 拼图（输出尺寸 = 区域 × 渲染比例，
    ///    与当前缩放无关）；
    /// 3. 缺块 → `request_region` 提交覆盖该区域的块，本帧返回 `None`（调用方下帧重试）。
    ///
    /// 这样"初次生成"不再依赖"用户是否在这个缩放下走过那块地方"。
    // 8 个参数都是"这次生成预览需要的事实"，打包成结构体只会把调用点变长
    #[allow(clippy::too_many_arguments)]
    pub(super) fn preview_texture(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut DocumentSession,
        page: PageIndex,
        page_rect: Rect,
        scale: f32,
        texture_name: String,
        tag: &str,
        image_ref: Option<crate::image_store::ImageRef>,
    ) -> Option<egui::TextureHandle> {
        let placement = session.placement(page).copied()?;
        if page_rect.width.is_nan()
            || page_rect.height.is_nan()
            || page_rect.width <= 0.0
            || page_rect.height <= 0.0
        {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成失败：{tag} 页={page:?} 区域为空 {page_rect:?}"
                ));
            }
            return None;
        }
        let display_scale = if scale > 0.0 && scale.is_finite() {
            scale
        } else {
            1.0
        };
        // 记录的比例可能让预览图超出纹理单边上限（大区域 + 高倍）：按超采样步长降档（D-108）
        let requested_scale = render_scale_for(display_scale, session.overscan());
        let render_value = crate::view::preview_render_scale(
            page_rect,
            placement.page_size,
            placement.rotation,
            requested_scale,
            session.max_texture_side(),
        );
        let render = Scale::from_zoom(render_value);
        let grid = TileGrid::new(placement.page_size.rotated(placement.rotation), render.0);
        let Some(range) =
            grid.tiles_for_page_rect(page_rect, placement.page_size, placement.rotation)
        else {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成失败：{tag} 页={} 区域 {page_rect:?} 不在页面内",
                    page.display_number()
                ));
            }
            return None;
        };
        let Some(tiles) = session.region_tiles(page, page_rect, render) else {
            let count = session.request_region(page, page_rect, render);
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成：{tag} 页={} 区域pt=({:.1},{:.1},{:.1},{:.1}) 记录分辨率={:.0}% 渲染比例={:.3} 可见块={:?} 提交渲染={} 块（等待就绪）",
                    page.display_number(),
                    page_rect.x,
                    page_rect.y,
                    page_rect.width,
                    page_rect.height,
                    display_scale * 100.0,
                    render.0,
                    range,
                    count
                ));
                // 显式说明"降档"：上面的数字里能推导出来（记录分辨率 vs 渲染比例），
                // 但这里把原因也写清楚（区域像素超过纹理单边上限）。每次请求只打一行。
                if render_value < requested_scale {
                    let (region_w, region_h) = crate::view::region_pixel_size(
                        page_rect,
                        placement.page_size,
                        placement.rotation,
                        requested_scale,
                    )
                    .unwrap_or((0, 0));
                    let steps = (requested_scale / render_value.max(f32::MIN_POSITIVE))
                        .log(crate::view::DEFAULT_PREVIEW_STEP)
                        .round();
                    self.diag.line(format!(
                        "[预览] 降档：{tag} 页={} 记录分辨率={:.0}% 原渲染比例={:.3} → 实际={:.3}（降 {:.0} 档：区域 {}x{} 超过 max_texture_side={}）",
                        page.display_number(),
                        display_scale * 100.0,
                        requested_scale,
                        render_value,
                        steps,
                        region_w,
                        region_h,
                        session.max_texture_side()
                    ));
                }
            }
            return None;
        };
        let borrowed: Vec<(TileSpec, &RasterImage)> = tiles
            .iter()
            .map(|(tile, image)| (*tile, image.as_ref()))
            .collect();
        let Some(image) = compose_region_snapshot(
            page_rect,
            placement.page_size,
            placement.rotation,
            render.0,
            &grid,
            &borrowed,
        ) else {
            if self.diag.enabled() {
                self.diag.line(format!(
                    "[预览] 初次生成失败：{tag} 页={} 区域 {page_rect:?} 的块都为空（块数 {}）",
                    page.display_number(),
                    borrowed.len()
                ));
            }
            return None;
        };
        if self.diag.enabled() {
            self.diag.line(format!(
                "[预览] 初次生成完成：{tag} 页={} 区域pt=({:.1},{:.1},{:.1},{:.1}) 记录分辨率={:.0}% 渲染比例={:.3} 尺寸={}x{}",
                page.display_number(),
                page_rect.x,
                page_rect.y,
                page_rect.width,
                page_rect.height,
                display_scale * 100.0,
                render.0,
                image.size[0],
                image.size[1]
            ));
        }
        if let Some(image_ref) = image_ref {
            if let Some(png) = crate::image_store::encode_png(&image) {
                self.images.insert(&image_ref, png);
                ui.ctx().forget_image(&crate::image_store::image_uri(
                    &image_ref.record_id,
                    image_ref.revision,
                ));
            }
        }
        Some(
            ui.ctx()
                .load_texture(texture_name, image, egui::TextureOptions::LINEAR),
        )
    }
}
