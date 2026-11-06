//! `app::save`：从 `app.rs` 拆出的 egui 胶水（Phase 1 纯移动，无行为变化）。

use super::*;

use crate::persist;
use eframe::egui;
use pdfcore::annotation::AnnotationId;
use std::path::PathBuf;

impl PdfReaderApp {
    /// 原地写入（S5c）：暂存 → 关闭会话 → 备份 → 原子替换 → 重开并恢复视图。
    ///
    /// 注释不删 overlay（S6 才能读回 PDF 注释；删了本程序就看不到/改不了它们了）。
    /// 写回本身在子进程里（见 `docs/writeback.md`）。
    /// 有未保存改动（注释或自定义数据）。
    pub(super) fn is_dirty(&self) -> bool {
        self.pdf_dirty || self.data_dirty
    }

    /// 请求打开另一个文件：脏就先弹提示，干净就直接延后打开。
    pub(super) fn request_open(&mut self, path: PathBuf) {
        if self.document_path.as_ref() == Some(&path) {
            return;
        }
        if self.body_edit_dirty() {
            self.pending_edit_action = Some(EditDiscardAction::Open(path));
            return;
        }
        if self.is_dirty() {
            self.pending_action = Some(PendingAction {
                kind: PendingActionKind::Open(path),
            });
            self.pending_action_error = None;
        } else {
            self.pending_open = Some(path);
        }
    }

    /// 原 PDF 是否已被其他程序改过（决定提示里是否禁用原地保存）。
    pub(super) fn pdf_conflict(&self) -> bool {
        let Some(path) = self.document_path.as_ref() else {
            return false;
        };
        match self.pdf_file_stamp {
            Some(opened) => pdf_file_stamp(path)
                .map(|now| now != opened)
                .unwrap_or(true),
            None => false,
        }
    }

    /// 提示框里的"保存"：显式允许/不允许忽略外部修改。
    pub(super) fn save_in_place_now(
        &mut self,
        allow_conflict: bool,
    ) -> std::result::Result<(), String> {
        let Some(pdf) = self.document_path.clone() else {
            return Err("没有打开的文档".to_string());
        };
        let request = WriteRequest {
            target: pdf,
            allow_overwrite: true,
            allow_conflict,
            write_source_data: true,
            write_target_data: true,
        };
        self.write_document(request)
    }

    /// 保存/另存为成功之后，继续挂起的关闭或切换动作。
    pub(super) fn continue_pending_action(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_action.take() else {
            return;
        };
        self.pending_action_error = None;
        match pending.kind {
            PendingActionKind::Quit => {
                self.closing_after_decision = true;
                self.remember_document_view();
                self.flush_settings(Some(ctx));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            PendingActionKind::Open(path) => self.pending_open = Some(path),
        }
    }

    pub(super) fn perform_in_place_writeback(&mut self) {
        let Some(pdf) = self.document_path.clone() else {
            self.message = "先打开一个 PDF 再写入".to_string();
            return;
        };
        if !self.pdf_dirty && !self.data_dirty {
            self.message = "无改动：没有需要写入的内容".to_string();
            return;
        }
        let request = WriteRequest {
            target: pdf,
            allow_overwrite: true,
            allow_conflict: false,
            write_source_data: true,
            write_target_data: true,
        };
        if let Err(error) = self.write_document(request) {
            self.message = error;
        }
    }

    /// 导出带标注的 PDF（S5b-2）：overlay + 正文 → job → 子进程 → 校验 → 原子改名。
    ///
    /// 目标固定为同目录 `<stem>.annotated.pdf`，**不覆盖**已有文件；
    /// 真正的 FFI 写回在子进程里（见 `docs/writeback.md`），这里只做编排与冒烟校验。
    /// 导出注释摘要（Markdown）：同目录 `<stem>.annotations.md`，已存在则递增，不覆盖。
    pub(super) fn export_annotation_summary(&mut self) {
        let Some(pdf) = self.document_path.clone() else {
            self.message = "先打开一个 PDF 再导出".to_string();
            return;
        };
        let rows: Vec<crate::annotations_md::Row> =
            self.annotations
                .iter()
                .map(|annotation| {
                    let (kind, _) = annotation_kind_label(&annotation.kind);
                    let bounds = annotation.bounds();
                    let external = self.external_ids.contains(&annotation.id);
                    crate::annotations_md::Row {
                        page: annotation.page.get(),
                        y: bounds.map(|rect| rect.y).unwrap_or(0.0),
                        x: bounds.map(|rect| rect.x).unwrap_or(0.0),
                        id: annotation.id.get(),
                        kind,
                        author: annotation.author.clone(),
                        created: annotation.created,
                        modified: annotation.modified,
                        body: annotation.body.as_ref().map(|body| {
                            crate::image_store::materialize_markdown(body, &self.images)
                        }),
                        external,
                        readonly: external && annotation.pdf_ref.is_none(),
                    }
                })
                .collect();
        if rows.is_empty() {
            self.message = "没有可导出的注释".to_string();
            return;
        }
        let file_name = pdf
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "文档".to_string());
        let text = crate::annotations_md::render(&file_name, &rows, unix_now());
        let target = annotation_summary_path(&pdf);
        match std::fs::write(&target, text) {
            Ok(()) => {
                self.message = format!("已导出 {} 条注释摘要 → {}", rows.len(), target.display())
            }
            Err(error) => self.error = Some(format!("导出注释摘要失败：{error}")),
        }
    }

    /// 打开「另存为」对话框，默认同目录 `<stem>-另存为.pdf`。
    pub(super) fn open_save_as_dialog(&mut self) {
        let Some(pdf) = self.document_path.as_ref() else {
            return;
        };
        let stem = pdf
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "document".to_string());
        let mut index = 1u32;
        loop {
            let suffix = if index == 1 {
                "另存为".to_string()
            } else {
                format!("另存为-{index}")
            };
            let candidate = pdf.with_file_name(format!("{stem}-{suffix}.pdf"));
            if !candidate.exists() {
                self.save_as = Some(candidate.to_string_lossy().to_string());
                break;
            }
            index += 1;
        }
        self.save_as_error = None;
        self.save_as_write_original = true;
        self.save_as_write_target = true;
        self.save_as_overwrite = false;
    }

    /// 用指定目标打开「另存为」（覆盖保存时预填原路径，并默认勾选覆盖）。
    pub(super) fn open_save_as_dialog_at(&mut self, target: PathBuf) {
        self.save_as_overwrite = target.exists();
        self.save_as = Some(target.to_string_lossy().to_string());
        self.save_as_error = None;
        self.save_as_write_original = true;
        self.save_as_write_target = true;
    }

    /// 把当前 PDF + 当前注释写到新文件；成功后切换文档。
    /// 把当前状态写到 `request.target`。保存/另存为/覆盖保存共用这一条路径，
    /// 差别只在目标地址与选项（是否写数据文件、是否允许覆盖/忽略外部修改）。
    pub(super) fn write_document(
        &mut self,
        request: WriteRequest,
    ) -> std::result::Result<(), String> {
        let Some(source) = self.document_path.clone() else {
            return Err("没有打开的文档".to_string());
        };
        let target = request.target;
        let in_place = target == source;
        if !in_place && target.exists() && !request.allow_overwrite {
            return Err(format!("目标已存在，未覆盖：{}", target.display()));
        }
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                return Err(format!("目标目录不存在：{}", parent.display()));
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.opened().document.info().encrypted)
        {
            return Err("加密文档暂不支持写入".to_string());
        }
        if request.write_source_data {
            if let Some((found, supported)) = self.sidecar_locked {
                return Err(format!(
                    "原 PDF 的数据文件版本是 v{found}（支持到 v{supported}），不能覆盖"
                ));
            }
        }
        if !in_place && request.write_target_data {
            let sidecar = persist::sidecar_path(&target);
            if sidecar.exists() && !request.allow_overwrite {
                return Err(format!(
                    "目标数据文件已存在，未覆盖：{}（确认覆盖后可一并写入）",
                    sidecar.display()
                ));
            }
        }
        // 只改数据文件时不碰 PDF，也就没有"丢弃外部修改"的风险，跳过冲突检查。
        if in_place && self.pdf_dirty && !request.allow_conflict {
            if let Some(opened) = self.pdf_file_stamp {
                match pdf_file_stamp(&source) {
                    Ok(current) if current != opened => {
                        return Err(PDF_CONFLICT_MESSAGE.to_string());
                    }
                    Ok(_) => {}
                    Err(error) => {
                        return Err(format!("无法检查 PDF 文件状态，已取消写入：{error}"));
                    }
                }
            }
        }
        let Some((page, zoom, scroll, pages)) = self.session.as_ref().map(|session| {
            (
                session.state().current_page,
                session.state().zoom,
                session.viewport().scroll,
                session.page_count(),
            )
        }) else {
            return Err("没有打开的文档".to_string());
        };
        let (ref_updates, ref_deletes) = self.pdf_ref_changes();
        let (inline_updates, inline_deletes) = self.inline_changes();
        let inline = backend_pdfium::pdf_patch::InlineChanges {
            updates: inline_updates,
            deletes: inline_deletes,
        };
        let items: Vec<crate::export::ExportItem> = self
            .annotations
            .iter()
            .filter(|annotation| {
                !self.external_ids.contains(&annotation.id) && annotation.pdf_ref.is_none()
            })
            .map(|annotation| crate::export::ExportItem {
                id: Some(annotation.id.get()),
                page: annotation.page,
                origin: self
                    .session
                    .as_ref()
                    .and_then(|session| session.page_geometry(annotation.page))
                    .map(|geometry| geometry.origin)
                    .unwrap_or(Point::ORIGIN),
                kind: annotation.kind.clone(),
                color: annotation.color,
                opacity: annotation.opacity,
                body: annotation.body.clone().filter(|text| !text.is_empty()),
                author: annotation.author.clone(),
                created: annotation.created,
                modified: annotation.modified,
            })
            .collect();
        let new_count = items.len();
        let updated = ref_updates.len() + inline.updates.len();
        let deleted = ref_deletes.len() + inline.deletes.len();
        // 数据快照必须在关闭会话前取（preview/引用点与视图无关，但保持与旧路径一致）。
        let data = self.custom_data();
        let had_custom_data =
            self.data_dirty || !self.custom_previews.is_empty() || self.sidecar_migrated;
        let saved_previews = self.custom_previews.clone();
        let saved_dest_marks = self.dest_marks.clone();
        let saved_next_preview_id = self.next_preview_id.clone();

        let mut backup: Option<PathBuf> = None;
        let data_written = in_place && self.data_dirty;
        if in_place {
            let sidecar = persist::sidecar_path(&source);
            if self.data_dirty {
                let sidecar = persist::sidecar_path(&source);
                self.write_custom_data_to(&sidecar, &data)?;
                self.data_dirty = false;
                self.data_clean_hash = Some(self.data_fingerprint());
            }
            if !self.pdf_dirty {
                self.mark_pdf_saved();
                self.message = format!("已保存自定义数据：{}", sidecar.display());
                return Ok(());
            }
            let temporary = crate::export::stage_in_place_with_refs_and_inline(
                &source,
                &items,
                &ref_updates,
                &ref_deletes,
                &inline,
                pages,
            )?;
            if !request.allow_conflict {
                if let Some(opened) = self.pdf_file_stamp {
                    let changed = match pdf_file_stamp(&source) {
                        Ok(current) => current != opened,
                        Err(error) => {
                            let _ = std::fs::remove_file(&temporary);
                            return Err(format!("无法复查 PDF 文件状态，已取消写入：{error}"));
                        }
                    };
                    if changed {
                        let _ = std::fs::remove_file(&temporary);
                        return Err(PDF_CONFLICT_MESSAGE.to_string());
                    }
                }
            }
            if self.backup_before_inplace {
                backup = Some(crate::export::backup_path(&source));
            }
            // 关闭会话：释放文件句柄（Windows 上不放开无法替换原文件）。
            self.session = None;
            self.textures.clear();
            if let Some(path) = &backup {
                if let Err(error) = std::fs::copy(&source, path) {
                    let _ = std::fs::remove_file(&temporary);
                    self.open(&source, None);
                    return Err(format!("备份失败，已取消（原文件未改动）：{error}"));
                }
            }
            if let Err(error) = replace_file(&temporary, &source) {
                let _ = std::fs::remove_file(&temporary);
                self.open(&source, None);
                return Err(error);
            }
        } else {
            let temporary = crate::export::staging_path_for(&target);
            let _ = std::fs::remove_file(&temporary);
            let written = if items.is_empty()
                && ref_updates.is_empty()
                && ref_deletes.is_empty()
                && inline.is_empty()
            {
                std::fs::copy(&source, &temporary)
                    .map(|_| ())
                    .map_err(|error| format!("复制 PDF 失败：{error}"))
            } else {
                crate::export::export_with_refs_and_inline(
                    &source,
                    &temporary,
                    &items,
                    &ref_updates,
                    &ref_deletes,
                    &inline,
                    pages,
                )
                .map(|_| ())
            };
            if let Err(error) = written {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
            self.session = None;
            self.textures.clear();
            if let Err(error) = replace_file(&temporary, &target) {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
            if request.write_source_data {
                let sidecar = persist::sidecar_path(&source);
                self.write_custom_data_to(&sidecar, &data)?;
            }
            if request.write_target_data {
                let sidecar = persist::sidecar_path(&target);
                self.write_custom_data_to(&sidecar, &data)?;
            }
        }

        self.reopening_after_write = true;
        self.open(&target, None);
        if self.session.is_none() {
            return Err(format!("写入完成，但重新打开失败：{}", target.display()));
        }
        self.custom_previews = saved_previews;
        self.dest_marks = saved_dest_marks;
        self.next_preview_id = saved_next_preview_id;
        if in_place || request.write_target_data {
            self.data_dirty = false;
            self.data_clean_hash = Some(self.data_fingerprint());
        } else if had_custom_data {
            self.data_dirty = true;
            self.data_clean_hash = None;
        }
        self.mark_pdf_saved();
        self.pending_view_restore = Some((page, zoom, Some(scroll), 2));
        if in_place {
            let backup_note = backup
                .map(|path| format!("（备份：{}）", path.display()))
                .unwrap_or_default();
            if data_written {
                let sidecar = persist::sidecar_path(&target);
                self.message = format!(
                    "已保存：注释 → PDF（新增 {new_count}、更新 {updated}、删除 {deleted}；{backup_note}），自定义数据 → {}",
                    sidecar.display()
                );
            } else {
                self.message = format!(
                    "已写入 PDF：新增 {new_count}、更新 {updated}、删除 {deleted} 条注释：{}{backup_note}",
                    target.display()
                );
            }
        } else {
            self.message = format!("已另存为并切换到：{}（原文件未改动）", target.display());
        }
        Ok(())
    }

    /// 另存为 / 覆盖保存：统一走 `write_document`。
    pub(super) fn save_as_to(
        &mut self,
        target: PathBuf,
        write_original: bool,
        write_target: bool,
        overwrite: bool,
    ) -> std::result::Result<(), String> {
        let Some(source) = self.document_path.clone() else {
            return Err("没有打开的文档".to_string());
        };
        if target == source && !overwrite {
            return Err("目标与原文件相同；确认覆盖后才能写回原文件".to_string());
        }
        let request = WriteRequest {
            target,
            allow_overwrite: overwrite,
            allow_conflict: overwrite,
            write_source_data: write_original,
            write_target_data: write_target,
        };
        self.write_document(request)
    }

    pub(super) fn save_all(&mut self) {
        // 保存 = 写到当前文件的地址：PDF 脏就写回 PDF，数据脏就写 `.json`。
        self.perform_in_place_writeback();
    }

    /// 注释状态指纹（不含 `pdf_ref`：写回重开时对象号会变）。
    /// 自定义数据指纹（preview 的文本/题目/几何 + 被引用点）。
    pub(super) fn data_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for preview in &self.custom_previews {
            preview.page.get().hash(&mut hasher);
            preview.title.hash(&mut hasher);
            preview.text.hash(&mut hasher);
            preview.body_size.x.to_bits().hash(&mut hasher);
            preview.body_size.y.to_bits().hash(&mut hasher);
            preview.scale.to_bits().hash(&mut hasher);
            format!("{:?}", preview.kind).hash(&mut hasher);
        }
        for page in &self.dest_marks {
            for mark in page {
                mark.point.x.to_bits().hash(&mut hasher);
                mark.point.y.to_bits().hash(&mut hasher);
                mark.rect.x.to_bits().hash(&mut hasher);
                mark.rect.y.to_bits().hash(&mut hasher);
                mark.rect.width.to_bits().hash(&mut hasher);
                mark.rect.height.to_bits().hash(&mut hasher);
                mark.scale.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// 按“当前状态 vs 上次保存状态”重算两个脏位；撤销回到保存点会自然清零。
    /// 打开/写回重开收尾：确定洁净基线再算脏位。
    ///
    /// 顺序很关键：先设置 `data_clean_hash`（迁移中则为 `None`）再 `refresh_dirty`；
    /// 反过来会在 clean hash 仍为 `None` 时把 `data_dirty` 置真，导致干净文件一打开就显示脏。
    pub(super) fn settle_dirty_after_open(&mut self) {
        self.saved_pdf_revision = self.history.current_pdf_revision();
        self.data_clean_hash = if self.data_dirty {
            None
        } else {
            Some(self.data_fingerprint())
        };
        self.refresh_dirty();
    }

    /// 保存成功后把当前修订号记成保存点。
    pub(super) fn mark_pdf_saved(&mut self) {
        self.saved_pdf_revision = self.history.current_pdf_revision();
        self.refresh_dirty();
    }

    pub(super) fn refresh_dirty(&mut self) {
        // 用保存点的修订号，而不是内容指纹：撤销/重做回到保存点就是干净，
        // 也不会因为写回后坐标被四舍五入而误报脏。
        self.pdf_dirty = self.history.current_pdf_revision() != self.saved_pdf_revision;
        if let Some(clean) = self.data_clean_hash {
            self.data_dirty = self.data_fingerprint() != clean;
        } else if self.document_path.is_some() || !self.history.is_empty() {
            self.data_dirty = true;
        }
    }

    /// 对象号级别的待写变更：外部正文更新 + 已从 PDF 删除的对象。
    pub(super) fn pdf_ref_changes(
        &self,
    ) -> (
        Vec<crate::export::RefUpdate>,
        Vec<pdfcore::annotation::PdfObjectRef>,
    ) {
        let current: std::collections::BTreeSet<pdfcore::annotation::PdfObjectRef> = self
            .annotations
            .iter()
            .filter_map(|annotation| annotation.pdf_ref)
            .collect();
        let deletes = self
            .opened_pdf_refs
            .iter()
            .filter(|reference| match self.opened_id_by_ref.get(reference) {
                // 标记注释按 id 判断：撤销/重做后的快照可能暂时没有 pdf_ref，
                // 但它仍在当前集合里，不能当“已删除”。
                Some(id) => match self.annotations.get(*id) {
                    // 同一条注释换了对象（移动/重建后 pdf_ref = None）→ 旧对象要删。
                    Some(annotation) => annotation.pdf_ref != Some(**reference),
                    None => true,
                },
                None => !current.contains(reference),
            })
            .cloned()
            .collect();
        let updates = self
            .annotations
            .iter()
            .filter_map(|annotation| {
                let reference = annotation.pdf_ref?;
                let before = self.opened_pdf_bodies.get(&reference)?;
                (before != &annotation.body).then(|| crate::export::RefUpdate {
                    reference,
                    body: annotation.body.clone(),
                    modified: annotation.modified,
                })
            })
            .collect();
        (updates, deletes)
    }

    /// 保存自定义数据到 `<pdf>.json`（不含任何图片像素）。
    /// 当前自定义数据的快照（不含截图像素）。
    pub(super) fn custom_data(&self) -> persist::SavedData {
        let links = self
            .navigation
            .as_ref()
            .map(|navigation| navigation.links_by_page.clone())
            .unwrap_or_default();
        let dest_marks = self
            .dest_marks
            .iter()
            .map(|marks| {
                marks
                    .iter()
                    .map(|mark| persist::SavedMark {
                        point: mark.point,
                        rect: mark.rect,
                        body: [mark.body_size.x, mark.body_size.y],
                        scale: mark.scale,
                        image_id: mark.image_id.clone(),
                        image_revision: mark.image_revision,
                    })
                    .collect()
            })
            .collect();
        let custom_previews = self
            .custom_previews
            .iter()
            .map(|preview| persist::SavedPreview {
                kind: match preview.kind {
                    CustomKind::Image => persist::SavedCustomKind::Image,
                    CustomKind::Text => persist::SavedCustomKind::Text,
                },
                item: persist::SavedCustom {
                    page: preview.page,
                    point: preview.point,
                    rect: preview.rect,
                    body: [preview.body_size.x, preview.body_size.y],
                    title: preview.title.clone(),
                    text: preview.text.clone(),
                    scale: preview.scale,
                    image_id: preview.image_id.clone(),
                    image_revision: preview.image_revision,
                },
            })
            .collect();
        persist::SavedData {
            version: persist::CURRENT_VERSION,
            links,
            dest_marks,
            custom_images: Vec::new(),
            custom_texts: Vec::new(),
            custom_previews,
        }
    }

    /// 内联外部注释的正文更新/删除（按页 + 可见支持注释位置）。
    pub(super) fn inline_changes(
        &self,
    ) -> (
        Vec<backend_pdfium::pdf_patch::InlineUpdate>,
        Vec<backend_pdfium::pdf_patch::InlineDelete>,
    ) {
        let current: std::collections::HashSet<AnnotationId> = self
            .annotations
            .iter()
            .map(|annotation| annotation.id)
            .collect();
        let mut updates = Vec::new();
        let mut deletes = Vec::new();
        for (id, (page, position)) in &self.inline_refs {
            if !current.contains(id) {
                deletes.push(backend_pdfium::pdf_patch::InlineDelete {
                    page: page.get(),
                    position: *position,
                });
                continue;
            }
            let Some(annotation) = self.annotations.get(*id) else {
                continue;
            };
            if self
                .opened_inline_bodies
                .get(&(*page, *position))
                .is_some_and(|before| before != &annotation.body)
            {
                updates.push(backend_pdfium::pdf_patch::InlineUpdate {
                    page: page.get(),
                    position: *position,
                    body: annotation.body.clone(),
                });
            }
        }
        (updates, deletes)
    }

    pub(super) fn write_custom_data_to(
        &self,
        target: &std::path::Path,
        data: &persist::SavedData,
    ) -> std::result::Result<(), String> {
        persist::write_atomic(target, &data.to_json())
            .map_err(|error| format!("写入数据文件 {} 失败：{error}", target.display()))
    }
}
