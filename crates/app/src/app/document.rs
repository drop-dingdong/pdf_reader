//! `app::document`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::persist;
use crate::view::{default_dest_rect, destination_points, find_dest_index, flatten_outline};
use backend_pdfium::PdfiumBackend;
use eframe::egui;
use pdfcore::annotation::{Annotation, AnnotationId};
use std::sync::Arc;

impl PdfReaderApp {
    /// 读回注释的坐标也按 `MediaBox` 给出；PDF 渲染用的是 `MediaBox ∩ CropBox`。
    /// 文本/链接已在后端归一化，注释必须在进入模型前做同样处理，否则会整体偏移。
    pub(super) fn normalize_readback_geometry(
        readback: &mut backend_pdfium::readback::Readback,
        origins: &[Point],
    ) {
        use pdfcore::annotation::AnnotationKind;
        let shift_rect = |rect: &mut Rect, origin: Point| {
            rect.x -= origin.x;
            rect.y -= origin.y;
        };
        for annotation in &mut readback.annotations {
            let origin = origins
                .get(annotation.page.get() as usize)
                .copied()
                .unwrap_or(Point::ORIGIN);
            match &mut annotation.kind {
                AnnotationKind::Highlight { quads }
                | AnnotationKind::Underline { quads }
                | AnnotationKind::StrikeOut { quads } => {
                    for quad in quads {
                        shift_rect(quad, origin);
                    }
                }
                AnnotationKind::Ink { points, .. } => {
                    for point in points {
                        point.x -= origin.x;
                        point.y -= origin.y;
                    }
                }
                AnnotationKind::Note { rect } => shift_rect(rect, origin),
            }
            for stroke in &mut annotation.ink_strokes {
                for point in stroke {
                    point.x -= origin.x;
                    point.y -= origin.y;
                }
            }
        }
    }

    /// 把 PDF 读回的注释并进内存集合，并把对象号配对好。
    pub(super) fn merge_readback(
        &mut self,
        readback: &backend_pdfium::readback::Readback,
        ref_pages: &[Vec<backend_pdfium::pdf_index::PageAnnot>],
    ) -> usize {
        self.external_ids.clear();
        self.opened_pdf_refs.clear();
        self.opened_pdf_bodies.clear();
        self.opened_id_by_ref.clear();
        self.inline_refs.clear();
        self.opened_inline_bodies.clear();
        let mut by_page: std::collections::BTreeMap<
            u32,
            Vec<&backend_pdfium::readback::ReadAnnotation>,
        > = std::collections::BTreeMap::new();
        for item in &readback.annotations {
            by_page.entry(item.page.get()).or_default().push(item);
        }

        // 先把读回结果和对象号配对；**先导入带标记的注释，再给外部注释分配 id**，
        // 否则外部 id 可能先占用了后面 pd-fr 标记要用的 id，把注释覆盖掉。
        let mut entries: Vec<ReadAnnotationEntry<'_>> = Vec::new();
        for (page, items) in by_page {
            let page_entries: Vec<&backend_pdfium::pdf_index::PageAnnot> = ref_pages
                .get(page as usize)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|entry| {
                            !entry.hidden && matches!(entry.subtype, Some(1 | 9 | 10 | 12 | 15))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let refs_ok = page_entries.len() == items.len();
            for (position, (item, entry)) in items.into_iter().zip(page_entries).enumerate() {
                let reference = refs_ok
                    .then_some(entry.reference)
                    .flatten()
                    .map(|reference| pdfcore::annotation::PdfObjectRef {
                        num: reference.num,
                        gen: reference.gen,
                    });
                let inline =
                    (refs_ok && reference.is_none()).then_some((PageIndex::new(page), position));
                if let Some(inline) = inline {
                    self.opened_inline_bodies.insert(inline, item.body.clone());
                }
                if let Some(reference) = reference {
                    self.opened_pdf_refs.insert(reference);
                    self.opened_pdf_bodies.insert(reference, item.body.clone());
                    if let Some(raw_id) = item.id {
                        self.opened_id_by_ref
                            .insert(reference, AnnotationId(raw_id));
                    }
                }
                entries.push((item, reference, inline));
            }
        }

        let mut restored = 0;
        let mut seen_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
        // 第一遍：带 pd-fr 标记的注释（id 固定，先占住 id 空间）
        for (item, reference, _) in &entries {
            let Some(raw_id) = item.id else {
                continue;
            };
            let id = AnnotationId(raw_id);
            if let Some(existing) = self.annotations.get_mut(id) {
                existing.pdf_ref = *reference;
                if seen_ids.contains(&raw_id) {
                    // 同一 id 在页面 /Annots 里出现多次（历史写入/其它阅读器重复保存）：
                    // 数组顺序就是追加顺序，**以最后一个为准**，否则会拿旧正文覆盖新修改。
                    existing.kind = item.kind.clone();
                    existing.color = item.color;
                    existing.opacity = item.opacity;
                    existing.body = item.body.clone();
                    existing.author = item.author.clone();
                    existing.created = item.created;
                    existing.modified = item.modified;
                }
            } else {
                let Some(mut annotation) = Annotation::new(item.page, item.kind.clone()) else {
                    continue;
                };
                annotation.id = id;
                annotation.color = item.color;
                annotation.opacity = item.opacity;
                annotation.body = item.body.clone();
                annotation.author = item.author.clone();
                annotation.created = item.created;
                annotation.modified = item.modified;
                annotation.pdf_ref = *reference;
                self.annotations.insert(annotation);
                restored += 1;
            }
            seen_ids.insert(raw_id);
        }
        // 第二遍：无标记的外部注释
        for (item, reference, inline) in &entries {
            if item.id.is_some() {
                continue;
            }
            let reused = reference
                .and_then(|reference| self.pdf_ref_ids.get(&reference).copied())
                .filter(|id| self.annotations.get(*id).is_none());
            let id = if let Some(id) = reused {
                id
            } else {
                let Some(id) = self.annotations.add(item.page, item.kind.clone()) else {
                    continue;
                };
                id
            };
            if self.annotations.get(id).is_none() {
                let Some(mut annotation) = Annotation::new(item.page, item.kind.clone()) else {
                    continue;
                };
                annotation.id = id;
                self.annotations.insert(annotation);
            }
            if let Some(annotation) = self.annotations.get_mut(id) {
                annotation.color = item.color;
                annotation.opacity = item.opacity;
                annotation.body = item.body.clone();
                annotation.author = item.author.clone();
                annotation.created = item.created;
                annotation.modified = item.modified;
                annotation.pdf_ref = *reference;
            }
            if let Some(reference) = reference {
                self.pdf_ref_ids.insert(*reference, id);
            }
            if let Some(position) = inline {
                self.inline_refs.insert(id, *position);
            }
            self.external_ids.insert(id);
        }
        restored
    }

    /// 写回重开后，把撤销栈里注释快照的 `pdf_ref` 重新绑到新 PDF 对象号。
    pub(super) fn reconcile_history_refs(&mut self) {
        let refs: std::collections::HashMap<AnnotationId, pdfcore::annotation::PdfObjectRef> = self
            .opened_id_by_ref
            .iter()
            .map(|(reference, id)| (*id, *reference))
            .collect();
        for command in self.history.commands_mut() {
            match command {
                AppEdit::AddAnnotation { annotation }
                | AppEdit::DeleteAnnotation { annotation } => {
                    // 写回重开后 id → 新对象号；不在文件里的（例如刚删掉的）清空，
                    // 撤销恢复时才会按"新注释"重建，而不是拿着过期对象号什么都不写。
                    annotation.pdf_ref = refs.get(&annotation.id).copied();
                }
                AppEdit::RestyleAnnotation { before, after }
                | AppEdit::MoveAnnotation { before, after } => {
                    // 样式/几何编辑写回时旧对象已删、对象号会变；两个快照都按"重建"处理，
                    // 撤销/重做才不会带着过期 ref 变成"什么都没写"（移动的墨迹会消失）。
                    before.pdf_ref = None;
                    after.pdf_ref = None;
                }
                _ => {}
            }
        }
    }

    pub(super) fn open(&mut self, path: &std::path::Path, password: Option<&str>) {
        // 每次加载（包括写回后的重开）都换 token：外部注释 id 可能被重新分配。
        self.document_load_token = super::model::new_document_load_token();
        self.pending_external_link = None;
        let preserve_history = self.reopening_after_write;
        self.reopening_after_write = false;
        if !preserve_history && self.document_path.is_some() {
            // 换文档前先把"上次看到哪"记下来（写盘在指针空闲的帧里做）。
            self.remember_document_view();
        }
        if !preserve_history {
            self.history.clear();
            self.pdf_ref_ids.clear();
            // 换文档：旧文档的数据脏位不能带过来；迁移/升级会在加载时重新置脏。
            self.data_dirty = false;
        }
        let backend = PdfiumBackend::new();
        match backend.open(Source::Path(path), password) {
            Ok(opened) => {
                // S6a：读回 PDF 注释。裸 FFI 不经过 pdfium-render 的全局锁，
                // 所以**必须先放掉旧会话**（它的渲染 worker 可能还在跑）——D-134 选择的方案。
                self.session = None;
                let origins: Vec<Point> = opened
                    .document
                    .page_geometries()
                    .map(|geometries| geometries.iter().map(|page| page.origin).collect())
                    .unwrap_or_default();
                let mut readback = match backend_pdfium::readback::read(path, password) {
                    Ok(readback) => readback,
                    Err(error) => {
                        eprintln!("[readback] {error}");
                        backend_pdfium::readback::Readback::default()
                    }
                };
                Self::normalize_readback_geometry(&mut readback, &origins);
                self.pdf_marked_annotations = readback
                    .annotations
                    .iter()
                    .filter(|annotation| annotation.id.is_some())
                    .count();
                let raster = opened.raster.clone();
                let concurrency = opened.capabilities.max_concurrency as usize;
                let cache = Arc::new(LruCache::with_default_capacity());
                let scheduler = Arc::new(WorkerScheduler::new(raster, concurrency));

                match DocumentSession::new(
                    opened,
                    cache,
                    scheduler,
                    Box::new(VerticalLayout),
                    Box::new(RangeSelection::new()),
                ) {
                    Ok(session) => {
                        // **先**清空上一份文档的自定义状态（含浮窗），再由存档按需恢复。
                        // 顺序很关键：清理若放在恢复之后，会把刚从存档读到的数据又抹掉。
                        self.custom_previews.clear();
                        self.custom_pin = None;
                        self.custom_dialog = None;
                        self.custom_hover = None;
                        self.editing_title = None;
                        self.editing_body = None;
                        self.click_highlight = None;
                        self.hover_tip = None;
                        self.floats.clear();
                        self.markdown.caches.clear();
                        self.markdown.viewports.clear();
                        self.markdown.prepared.clear();
                        if !preserve_history {
                            self.markdown.images.clear();
                        }
                        self.data_dirty = false;
                        self.sidecar_migrated = false;
                        self.sidecar_locked = None;
                        self.document_path = Some(path.to_path_buf());
                        self.pdf_file_stamp = pdf_file_stamp(path).ok();
                        self.annotations.clear();
                        self.pdf_dirty = false;
                        // 同目录若有存档（<pdf>.json），载入并**替换**链接与被引用点/自定义数据。
                        // 旧版本就地升级并置脏（下次保存写回当前版本）；**更新版本一律拒绝**（D-107）。
                        let saved = match load_sidecar(path) {
                            Sidecar::Missing => None,
                            Sidecar::Ready(loaded) => {
                                for note in &loaded.migration.notes {
                                    eprintln!("[存档] {note}");
                                }
                                self.sidecar_migrated = loaded.migration.changed;
                                Some(loaded.data)
                            }
                            Sidecar::Locked { found, supported } => {
                                self.sidecar_locked = Some((found, supported));
                                None
                            }
                        };
                        self.navigation = session.navigation().ok();
                        // `NavigationIndex` 在会话里是共享只读的，因此这里**替换整个索引**
                        // （克隆后换成存档里的链接），而不是原地改字段。
                        if let (Some(saved), Some(navigation)) = (&saved, self.navigation.clone()) {
                            if saved.links.len() == navigation.links_by_page.len() {
                                let mut updated = (*navigation).clone();
                                updated.links_by_page = saved.links.clone();
                                self.navigation = Some(std::sync::Arc::new(updated));
                            }
                        }
                        // 摊平一次即可（虚拟化行渲染需要行列表）
                        self.outline_rows = self
                            .navigation
                            .as_ref()
                            .map(|navigation| flatten_outline(&navigation.outline))
                            .unwrap_or_default();
                        // 被引用的点（链接目的地）也只算一次：按目标页分组、按位置去重；
                        // 每个点附一个"点周围的小矩形"，截图留待用户发起动作后再构建。
                        self.dest_marks = self
                            .navigation
                            .as_ref()
                            .map(|navigation| {
                                destination_points(
                                    &navigation.links_by_page,
                                    session.page_count() as usize,
                                )
                                .into_iter()
                                .map(|points| {
                                    points
                                        .into_iter()
                                        .map(|point| {
                                            let rect = default_dest_rect(point);
                                            // 初始尺寸：该 rect 按 100% 缩放比（页面单位==像素）
                                            DestMark {
                                                point,
                                                rect,
                                                snapshot: None,
                                                body_size: egui::vec2(rect.width, rect.height),
                                                // 默认 rect 是按 pt 给的（相当于 100%）：分辨率记 1.0
                                                scale: 1.0,
                                                image_id: new_image_id("dest"),
                                                image_revision: 0,
                                            }
                                        })
                                        .collect()
                                })
                                .collect()
                            })
                            .unwrap_or_default();
                        // 链接 → 被引用点：建表时解析一次并**记住索引**。
                        // 之后悬停预览与"跳转到矩形中心"都用它，不再按位置临时查询。
                        self.link_marks = self
                            .navigation
                            .as_ref()
                            .map(|navigation| {
                                navigation
                                    .links_by_page
                                    .iter()
                                    .map(|links| {
                                        links
                                            .iter()
                                            .map(|link| {
                                                let Destination::Page {
                                                    page,
                                                    offset: Some(point),
                                                } = &link.destination
                                                else {
                                                    return None;
                                                };
                                                let marks =
                                                    self.dest_marks.get(page.get() as usize)?;
                                                let positions: Vec<Point> =
                                                    marks.iter().map(|mark| mark.point).collect();
                                                find_dest_index(&positions, *point)
                                                    .map(|index| (*page, index))
                                            })
                                            .collect()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        // 存档里的被引用点与自定义 preview 数据 → 覆盖/追加
                        if let Some(saved) = &saved {
                            for (page_index, page_marks) in saved.dest_marks.iter().enumerate() {
                                let Some(marks) = self.dest_marks.get_mut(page_index) else {
                                    continue;
                                };
                                for (index, saved_mark) in page_marks.iter().enumerate() {
                                    if let Some(mark) = marks.get_mut(index) {
                                        mark.rect = saved_mark.rect;
                                        mark.body_size =
                                            egui::vec2(saved_mark.body[0], saved_mark.body[1]);
                                        mark.scale = saved_mark.scale;
                                        if !saved_mark.image_id.is_empty() {
                                            mark.image_id = saved_mark.image_id.clone();
                                        }
                                        mark.image_revision = saved_mark.image_revision;
                                    }
                                }
                            }
                            self.custom_previews.clear();
                            // v3 是单一有序列表；v2 及更早拆成 images/texts。
                            let ordered: Vec<(CustomKind, &persist::SavedCustom)> = if !saved
                                .custom_previews
                                .is_empty()
                            {
                                saved
                                    .custom_previews
                                    .iter()
                                    .map(|preview| {
                                        let kind = match preview.kind {
                                            persist::SavedCustomKind::Image => CustomKind::Image,
                                            persist::SavedCustomKind::Text => CustomKind::Text,
                                        };
                                        (kind, &preview.item)
                                    })
                                    .collect()
                            } else {
                                let mut old = Vec::new();
                                for item in &saved.custom_images {
                                    old.push((CustomKind::Image, item));
                                }
                                for item in &saved.custom_texts {
                                    old.push((CustomKind::Text, item));
                                }
                                old
                            };
                            let mut generated_preview_ids = 0usize;
                            for (kind, item) in ordered {
                                let image_id = if item.image_id.is_empty() {
                                    generated_preview_ids += 1;
                                    new_image_id(if kind == CustomKind::Image {
                                        "custom"
                                    } else {
                                        "preview"
                                    })
                                } else {
                                    item.image_id.clone()
                                };
                                self.custom_previews.push(CustomPreview {
                                    id: 0,
                                    page: item.page,
                                    point: item.point,
                                    rect: item.rect,
                                    kind,
                                    title: item.title.clone(),
                                    text: item.text.clone(),
                                    snapshot: None,
                                    body_size: egui::vec2(item.body[0], item.body[1]),
                                    scale: item.scale,
                                    image_id,
                                    image_revision: item.image_revision,
                                });
                            }
                            if generated_preview_ids > 0 {
                                // 文字 preview 复用了 v4 的 image_id 字段作为链接记录 id；
                                // 生成后必须让文件变脏，保存才会把 id 写回。
                                self.sidecar_migrated = true;
                                eprintln!(
                                    "[存档] 为 {generated_preview_ids} 条 preview 生成了稳定记录 id（保存后写回）"
                                );
                            }
                        }
                        self.next_preview_id = vec![0; session.page_count() as usize];
                        for preview in &mut self.custom_previews {
                            let page = preview.page.get() as usize;
                            if page >= self.next_preview_id.len() {
                                self.next_preview_id.resize(page + 1, 0);
                            }
                            preview.id = self.next_preview_id[page];
                            self.next_preview_id[page] += 1;
                        }
                        let ref_pages = std::fs::read(path)
                            .ok()
                            .and_then(|bytes| {
                                backend_pdfium::pdf_index::page_annotations(&bytes).ok()
                            })
                            .unwrap_or_default();
                        let restored = self.merge_readback(&readback, &ref_pages);
                        if preserve_history {
                            self.reconcile_history_refs();
                        }
                        self.next_float_id = 1;
                        self.capture = None;
                        let title = session.opened().document.info().to_string();
                        let page_count = session.page_count();
                        self.session = Some(session);
                        if !preserve_history {
                            if let Some(state) =
                                self.settings.document(&settings_key(path)).cloned()
                            {
                                let page =
                                    PageIndex::new(state.page.min(page_count.saturating_sub(1)));
                                let scroll =
                                    state.anchor.map(|anchor| Vec2::new(anchor[0], anchor[1]));
                                self.pending_view_restore = Some((page, state.zoom, scroll, 2));
                                self.horizontal_layout = state.horizontal;
                                self.pending_layout = Some(state.horizontal);
                            }
                        }
                        self.textures.clear();
                        // D-96 的记账按页号索引：换文档后必须清空，否则旧文档的
                        // "第一块就绪时刻"会让新文档的同一页号一上来就判定超时（提前补白）。
                        self.band_since.clear();
                        self.last_plan.clear();
                        self.error = None;
                        if let Some((found, supported)) = self.sidecar_locked {
                            self.message = format!(
                                "已打开：{title}（同目录存档是 v{found}，本程序只支持到 v{supported}：不加载、也不会覆盖它）"
                            );
                        } else if self.sidecar_migrated {
                            // 旧版本存档被升级过：置脏，让"保存自定义数据"写回当前版本
                            self.data_dirty = true;
                            self.message = format!(
                                "已打开：{title}（存档已升级到 v{}，请保存）",
                                persist::CURRENT_VERSION
                            );
                        } else {
                            self.message = format!("已打开：{title}");
                        }
                        if restored > 0 {
                            self.message
                                .push_str(&format!("（已从 PDF 恢复 {restored} 条标注）"));
                        }
                        self.settle_dirty_after_open();
                        self.sidecar_migrated = false;
                        self.password_for = None;
                        self.password_input.clear();
                        self.hits.clear();
                        self.active_hit = None;
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            // 加密文档：不是故障，弹密码框后重试
            Err(CoreError::PasswordRequired) => {
                self.password_for = Some(path.to_path_buf());
                self.password_input.clear();
            }
            Err(CoreError::IncorrectPassword) => {
                self.password_for = Some(path.to_path_buf());
                self.error = Some("密码错误，请重试".to_string());
            }
            Err(error) => self.error = Some(format!("{}：{error}", path.display())),
        }
    }
}
