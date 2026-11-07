//! `app::tests`：`app.rs` 的内联测试（Phase 1 纯移动，测试名不变）。

#[cfg(test)]
mod edit_describe_tests {
    use super::super::*;
    use pdfcore::annotation::AnnotationKind;

    fn ink() -> pdfcore::annotation::Annotation {
        pdfcore::annotation::Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)],
                width: 2.0,
            },
        )
        .expect("有效墨迹")
    }

    #[test]
    fn undo_list_calls_an_ink_stroke_ink_not_a_zero_row_highlight() {
        let edit = AppEdit::AddAnnotation { annotation: ink() };
        let description = edit.describe();
        assert!(
            description.starts_with("墨迹"),
            "历史列表必须说人话：实际 {description}"
        );
        assert!(
            !description.contains("0 行"),
            "墨迹没有行矩形，不能按行数描述：实际 {description}"
        );
    }

    #[test]
    fn undo_list_keeps_the_row_count_for_text_marks() {
        let annotation = pdfcore::annotation::Annotation::new(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![
                    Rect::new(0.0, 0.0, 10.0, 2.0),
                    Rect::new(0.0, 4.0, 10.0, 2.0),
                ],
            },
        )
        .expect("有效高亮");
        let edit = AppEdit::AddAnnotation { annotation };
        assert_eq!(edit.describe(), "高亮（第 1 页，2 行）");
    }
}

#[cfg(test)]
mod annotation_body_tests {
    use super::super::*;
    use crate::view::{body_size_of, FLOAT_TITLE_HEIGHT};
    use egui::Id;
    use pdfcore::annotation::INK_OPACITY;
    use pdfcore::annotation::{Annotation, AnnotationKind};

    fn app() -> PdfReaderApp {
        PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        )
    }

    #[test]
    fn editor_applies_requested_initial_cursor() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        app.open_body_editor(
            FloatOwner::Annotation(id),
            "hello world".to_string(),
            Some(5),
            &ctx,
        );
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(700.0, 500.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            let output = ctx.run_ui(input(), |ui| {
                let _ = ui;
                app.draw_body_editor(&ctx);
            });
            output.drop_without_applying_deltas();
        }
        let text_id = Id::unique(("edit-body-text", app.body_edit_session));
        let state = egui::text_edit::TextEditState::load(&ctx, text_id).expect("编辑器状态");
        let range = state.cursor.char_range().expect("光标范围");
        assert_eq!(usize::from(range.primary.index), 5);
    }

    #[test]
    fn body_cursor_mapping_moves_from_start_to_end() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            let body = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(300.0, 120.0));
            let start = body_cursor_from_pointer(
                ui.ctx(),
                "hello\nworld",
                body,
                body.left_top(),
                false,
                egui::Vec2::ZERO,
                None,
            );
            let end = body_cursor_from_pointer(
                ui.ctx(),
                "hello\nworld",
                body,
                body.right_bottom(),
                false,
                egui::Vec2::ZERO,
                None,
            );
            assert_eq!(start, 0);
            assert!(end > start, "底端应映射到后面的字符");
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn markdown_cursor_mapping_accounts_for_scroll_offset() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            let body = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(300.0, 100.0));
            let source = "## 标题\n\n第一段\n\n第二段\n\n第三段\n\n第四段";
            let top = body_cursor_from_pointer(
                ui.ctx(),
                source,
                body,
                body.left_top(),
                true,
                egui::Vec2::ZERO,
                Some(egui::vec2(300.0, 400.0)),
            );
            let scrolled = body_cursor_from_pointer(
                ui.ctx(),
                source,
                body,
                body.left_top(),
                true,
                egui::vec2(0.0, 200.0),
                Some(egui::vec2(300.0, 400.0)),
            );
            assert!(scrolled > top, "滚动后同一屏幕位置应映射到更后面的源码");
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn internal_image_inserts_at_editor_cursor() {
        let ctx = egui::Context::default();
        let id = Id::unique("cursor-insert-test");
        let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(5),
            )));
        state.store(&ctx, id);
        let mut text = "hello world".to_string();
        insert_at_text_cursor(&ctx, id, &mut text, "![图](pd-img://x?rev=1)");
        assert_eq!(text, "hello![图](pd-img://x?rev=1) world");
        let state = egui::text_edit::TextEditState::load(&ctx, id).expect("state");
        let cursor = state.cursor.char_range().expect("cursor");
        assert_eq!(
            usize::from(cursor.primary.index),
            5 + "![图](pd-img://x?rev=1)".chars().count()
        );
    }

    #[test]
    fn hover_preview_can_render_markdown_when_enabled() {
        let mut app = app();
        app.markdown_floats = true;
        app.hover_preview_markdown = true;
        app.custom_previews.push(CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "hover".to_string(),
            text: Some("## 标题\n\n[跳转](#h)\n\n## 标题 {#h}".to_string()),
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        });
        app.custom_hover = Some((
            0,
            egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(160.0, 20.0)),
        ));
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_sidebar_hover_float(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert!(app.markdown.rendered, "开启后 hover 临时预览应走 Markdown");
    }

    #[test]
    fn image_choices_include_previews_and_dest_marks() {
        let mut app = app();
        app.custom_previews.push(CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            kind: CustomKind::Image,
            title: "截图".to_string(),
            text: None,
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: "custom-1".to_string(),
            image_revision: 2,
        });
        app.dest_marks = vec![vec![DestMark {
            point: Point::new(10.0, 10.0),
            rect: Rect::new(5.0, 5.0, 10.0, 10.0),
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: "dest-1".to_string(),
            image_revision: 3,
        }]];
        let choices = app.image_choices();
        assert!(choices
            .iter()
            .any(|(_, uri)| uri.contains("custom-1?rev=2")));
        assert!(choices.iter().any(|(_, uri)| uri.contains("dest-1?rev=3")));
    }

    #[test]
    fn image_ref_button_does_not_use_custom_index_for_float_lookup() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let texture = ctx.load_texture(
            "test-image",
            egui::ColorImage::new([2, 2], vec![egui::Color32::RED; 4]),
            egui::TextureOptions::LINEAR,
        );
        let mut app = app();
        for index in 0..5 {
            app.custom_previews.push(CustomPreview {
                id: index as u64,
                page: PageIndex::FIRST,
                point: Point::new(0.0, 0.0),
                rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                kind: CustomKind::Text,
                title: format!("p{index}"),
                text: Some(String::new()),
                snapshot: None,
                body_size: DEFAULT_CUSTOM_TEXT_BODY,
                scale: 1.0,
                image_id: format!("custom-{index}"),
                image_revision: 0,
            });
        }
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "图片".to_string(),
            content: FloatContent::Image(texture),
            rect,
            owner: FloatOwner::Custom(4),
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn markdown_float_renders_inside_a_scroll_area() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "Markdown 测试".to_string(),
            content: FloatContent::Text("# 标题\n\n- a\n- b\n\n正文".to_string()),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert!(app.markdown.rendered, "文字浮窗应走 Markdown 渲染分支");
    }

    #[test]
    fn empty_text_float_still_has_an_edit_affordance() {
        assert!(text_float_is_editable(
            FloatOwner::Custom(0),
            &FloatContent::Text(String::new())
        ));
        assert!(text_float_is_editable(
            FloatOwner::Annotation(AnnotationId(1)),
            &FloatContent::Text(String::new())
        ));
        assert!(!text_float_is_editable(
            FloatOwner::None,
            &FloatContent::Text(String::new())
        ));
    }

    #[test]
    fn readback_geometry_is_normalized_to_the_rendered_box() {
        use backend_pdfium::readback::{ReadAnnotation, Readback};
        use pdfcore::annotation::AnnotationKind;
        let mut readback = Readback {
            annotations: vec![ReadAnnotation {
                page: PageIndex::FIRST,
                id: Some(1),
                kind: AnnotationKind::Highlight {
                    quads: vec![Rect::new(179.0, 572.0, 173.0, 14.0)],
                },
                color: [255, 255, 0],
                opacity: 0.35,
                body: None,
                author: None,
                created: None,
                modified: None,
                ink_strokes: Vec::new(),
            }],
            skipped: 0,
            extra_ink_strokes: 0,
        };
        PdfReaderApp::normalize_readback_geometry(&mut readback, &[Point::new(36.0, 36.0)]);
        let quad = readback.annotations[0].kind.quads()[0];
        assert!((quad.x - 143.0).abs() < 0.01);
        assert!((quad.y - 536.0).abs() < 0.01);
        assert!((quad.width - 173.0).abs() < 0.01);
    }

    #[test]
    fn markdown_preprocessing_is_cached_by_source() {
        let mut app = app();
        app.markdown_floats = true;
        let source = "第一行\n\n$$\n\n\\frac{1}{2}\n$$\n\n![图](pd-img://x?rev=1)";
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "缓存".to_string(),
            content: FloatContent::Text(source.to_string()),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        let prepared = app.markdown.prepared.get(&1).expect("应缓存预处理结果");
        assert!(
            !prepared.normalized.contains("\n\n\\frac"),
            "块内空行应已规范化"
        );
        assert_eq!(prepared.image_refs.len(), 1);
    }

    #[test]
    fn markdown_heading_cache_is_isolated_per_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 600.0));
        for (id, x) in [(1u64, 20.0f32), (2, 480.0)] {
            let rect = egui::Rect::from_min_size(egui::pos2(x, 30.0), egui::vec2(320.0, 240.0));
            app.floats.push(Float {
                id,
                title: format!("浮窗 {id}"),
                content: FloatContent::Text(format!("## 标题 {id} {{#h{id}}}\n\n[跳转](#h{id})")),
                rect,
                owner: FloatOwner::None,
                body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
                origin: None,
            });
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert_eq!(app.markdown.caches.len(), 2, "每个浮窗必须有独立缓存");
    }

    #[test]
    fn markdown_table_stays_inside_the_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "表格".to_string(),
            content: FloatContent::Text("|a|b|c|\n|:---|:--|:--|\n|1|1|2|".to_string()),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        let mut found = false;
        let mut max_x = f32::NEG_INFINITY;
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                found = true;
                max_x = max_x.max(text.pos.x + text.galley.size().x);
            }
        }
        output.drop_without_applying_deltas();
        assert!(found, "表格应至少绘制出一个文字 mesh");
        assert!(
            max_x <= rect.right() + 2.0,
            "表格不应被推到浮窗右侧：max_x={max_x}, rect_right={}",
            rect.right()
        );
    }

    #[test]
    fn body_editor_commit_writes_annotation_and_is_undoable() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        app.commit_body_edit(FloatOwner::Annotation(id), "正式窗口内容".to_string());
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("正式窗口内容")
        );
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            None
        );
    }

    #[test]
    fn dirty_body_editor_blocks_switching_files_until_decided() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        app.editing_body = Some((FloatOwner::Annotation(id), "未保存".to_string()));
        assert!(app.body_edit_dirty());
        app.request_open(PathBuf::from("/tmp/another.pdf"));
        assert!(
            matches!(
                app.save_flow.pending_edit_action,
                Some(EditDiscardAction::Open(_))
            ),
            "有未保存正文时应先挂起切换动作"
        );
    }

    #[test]
    fn markdown_math_renders_into_the_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.floats.push(Float {
            id: 1,
            title: "数学".to_string(),
            content: FloatContent::Text(
                "行内 $x^2 + y^2$ 与块级：\n\n$$\\frac{1}{2}$$".to_string(),
            ),
            rect,
            owner: FloatOwner::None,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let input = egui::RawInput {
            screen_rect: Some(canvas),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.draw_floats(ui, canvas);
        });
        output.drop_without_applying_deltas();
        assert!(
            !app.markdown.math_cache.borrow().is_empty(),
            "公式应生成至少一张纹理或失败占位"
        );
    }

    #[test]
    fn body_edit_is_undoable_and_does_not_create_previews() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        assert!(app.custom_previews.is_empty(), "注释正文不再创建 preview");
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: None,
                after: Some("第一版".to_string()),
            },
            true,
        );
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("第一版")
        );
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            None
        );
        app.redo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("第一版")
        );
        assert!(app.custom_previews.is_empty());
    }

    #[test]
    fn recoloring_new_annotation_is_undoable() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert!(app.recolor_annotation(id, [235, 70, 70]));
        let recolor_id = app.annotations.iter().next().unwrap().id;
        assert_eq!(
            app.annotations.get(recolor_id).unwrap().color,
            [235, 70, 70]
        );
        app.undo_edit();
        assert_eq!(app.annotations.get(id).unwrap().color, [255, 214, 0]);
        app.redo_edit();
        assert_eq!(
            app.annotations.get(recolor_id).unwrap().color,
            [235, 70, 70]
        );
    }

    /// 合成读回结果 + 页对象条目：便签必须按对象号配对，外部便签不能被判成"内联只读"；
    /// 同一 id 出现多次时以最后一版为准。
    #[test]
    fn notes_pair_with_objects_and_duplicates_take_the_last() {
        let mut app = app();
        let note = |page: PageIndex, id: Option<u64>, body: &str| {
            backend_pdfium::readback::ReadAnnotation {
                page,
                id,
                kind: AnnotationKind::Note {
                    rect: Rect::new(10.0, 20.0, 18.0, 18.0),
                },
                color: [255, 214, 0],
                opacity: 1.0,
                body: Some(body.to_string()),
                author: None,
                created: None,
                modified: None,
                ink_strokes: Vec::new(),
            }
        };
        let entry = |num: u32| backend_pdfium::pdf_index::PageAnnot {
            reference: Some(backend_pdfium::pdf_index::PdfObjectRef { num, gen: 0 }),
            subtype: Some(1),
            popup: None,
            hidden: false,
        };
        let readback = backend_pdfium::readback::Readback {
            annotations: vec![
                note(PageIndex::FIRST, Some(5), "旧正文"),
                note(PageIndex::FIRST, Some(5), "新正文"),
                note(PageIndex::new(1), None, "外部便签"),
            ],
            skipped: 0,
            extra_ink_strokes: 0,
        };
        let refs = vec![vec![entry(50), entry(60)], vec![entry(70)]];
        let _ = app.merge_readback(&readback, &refs);

        let own = app
            .annotations
            .iter()
            .find(|annotation| annotation.id == AnnotationId(5))
            .expect("自有便签");
        assert_eq!(own.body.as_deref(), Some("新正文"), "重复 id 取最后一版");
        assert_eq!(
            own.pdf_ref,
            Some(pdfcore::annotation::PdfObjectRef { num: 60, gen: 0 }),
            "应配对到最新的对象"
        );

        let external = app
            .annotations
            .iter()
            .find(|annotation| annotation.id != AnnotationId(5))
            .expect("外部便签");
        assert_eq!(
            external.pdf_ref,
            Some(pdfcore::annotation::PdfObjectRef { num: 70, gen: 0 }),
            "外部便签也要拿到对象号"
        );
        assert!(
            !(app.external_ids.contains(&external.id) && external.pdf_ref.is_none()),
            "外部便签不能被判成内联只读"
        );
    }

    #[test]
    fn note_mode_and_markup_mode_are_exclusive() {
        let mut app = app();
        app.note_pin = true;
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert!(!app.note_pin, "进入标记模式应清掉便签选点");
        assert_eq!(app.annotation_tool, Some(MarkupTool::Highlight));

        app.add_note_annotation(PageIndex::FIRST, Rect::new(0.0, 0.0, 18.0, 18.0), "x");
        app.enter_note_mode();
        assert!(app.note_pin);
        assert_eq!(app.annotation_tool, None, "进入便签模式应退掉标记模式");
    }

    #[test]
    fn annotation_modes_keep_the_original_reading_mode() {
        let mut app = app();
        app.switch_tool(Tool::Pan);
        // 阅读模式（平移）→ 高亮：记录 A=平移
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool, Tool::Select);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        // 高亮 → 墨迹：A 不变
        app.enter_ink_mode();
        assert_eq!(app.tool, Tool::Ink);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        // 墨迹 → 删除线：A 仍不变
        app.toggle_markup_tool(MarkupTool::StrikeOut);
        assert_eq!(app.tool, Tool::Select);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        // Esc 退出标记：回到最初的平移
        app.exit_markup_tool();
        assert_eq!(app.tool, Tool::Pan);
        assert_eq!(app.tool_before_annotation, None);

        // 便签同样不改变阅读模式，Esc 回原模式
        app.switch_tool(Tool::Select);
        app.enter_note_mode();
        assert_eq!(app.tool, Tool::Select, "便签不切换阅读模式");
        assert_eq!(app.tool_before_annotation, Some(Tool::Select));
        app.exit_note_mode();
        assert_eq!(app.tool, Tool::Select);

        // 从标记模式进便签：保留更早的 A（平移）
        app.switch_tool(Tool::Pan);
        app.toggle_markup_tool(MarkupTool::Underline);
        app.enter_note_mode();
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        app.exit_note_mode();
        assert_eq!(app.tool, Tool::Pan);

        // 便签 → 标记：也不能把 A 改写成选字，Esc 仍回平移
        app.switch_tool(Tool::Pan);
        app.enter_note_mode();
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool_before_annotation, Some(Tool::Pan));
        app.exit_markup_tool();
        assert_eq!(app.tool, Tool::Pan);
    }

    #[test]
    fn undo_after_writeback_rebuilds_moved_ink() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let old_ref = pdfcore::annotation::PdfObjectRef { num: 7, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(old_ref);
        }
        app.opened_pdf_refs.insert(old_ref);

        let before = app.annotations.get(id).cloned().unwrap();
        let mut after = before.clone();
        after.kind = AnnotationKind::Ink {
            points: vec![Point::new(50.0, 60.0), Point::new(70.0, 70.0)],
            width: 2.0,
        };
        after.pdf_ref = None;
        app.apply_edit(AppEdit::MoveAnnotation { before, after }, true);

        // 模拟"写入成功并重开"：该 id 在文件里的对象号变成 new_ref。
        let new_ref = pdfcore::annotation::PdfObjectRef { num: 99, gen: 0 };
        app.opened_pdf_refs.insert(new_ref);
        app.opened_id_by_ref.insert(new_ref, id);
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(new_ref);
        }
        app.reconcile_history_refs();

        // 撤销：几何回原位，且 pdf_ref 为空（下次保存按重建写）。
        app.undo_edit();
        let restored = app.annotations.get(id).expect("撤销后仍在");
        assert_eq!(restored.kind.ink().unwrap().0[0], Point::new(10.0, 10.0));
        assert!(restored.pdf_ref.is_none(), "撤销后不能带着已删除的旧对象号");
        let (_, deletes) = app.pdf_ref_changes();
        assert!(deletes.contains(&new_ref), "当前对象应进 deletes 等待重建");
    }

    #[test]
    fn external_change_is_detected_for_conflict_banner() {
        let path = std::env::temp_dir().join(format!(
            "pdf-reader-conflict-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, b"pdf").expect("写");
        let stamp = pdf_file_stamp(&path).expect("stamp");
        let mut app = app();
        app.document_path = Some(path.clone());
        app.pdf_file_stamp = Some(stamp);
        assert!(!app.pdf_conflict(), "未改动时不应报冲突");
        std::fs::write(&path, b"pdf changed externally").expect("改");
        assert!(app.pdf_conflict(), "外部修改后提示框应能提前发现冲突");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn opening_a_clean_sidecar_stays_clean() {
        let mut app = app();
        app.document_path = Some(PathBuf::from("/tmp/with-sidecar.pdf"));
        app.data_dirty = false;
        app.data_clean_hash = None; // 打开时还没建立基线
        app.settle_dirty_after_open();
        assert!(!app.data_dirty, "带 .json 的干净文件打开后不应变脏");

        // 迁移/升级场景：显式置脏后必须保留脏位。
        app.data_dirty = true;
        app.data_clean_hash = None;
        app.settle_dirty_after_open();
        assert!(app.data_dirty, "迁移中的数据仍应标记为脏");
    }

    #[test]
    fn preview_edits_do_not_dirty_the_pdf() {
        let mut app = app();
        app.document_path = Some(PathBuf::from("/tmp/preview-only.pdf"));
        app.mark_pdf_saved();
        app.apply_edit(
            AppEdit::MovePreview {
                key: PreviewKey {
                    page: PageIndex::FIRST,
                    id: 0,
                },
                from: 0,
                to: 1,
            },
            true,
        );
        assert!(!app.pdf_dirty, "preview 编辑只写 .json，不应弄脏 PDF");
        let annotation = Annotation::new(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(1.0, 1.0), Point::new(2.0, 2.0)],
                width: 2.0,
            },
        )
        .unwrap();
        app.apply_edit(AppEdit::AddAnnotation { annotation }, true);
        assert!(app.pdf_dirty, "注释编辑应弄脏 PDF");
    }

    #[test]
    fn redo_back_to_the_save_point_is_clean() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        app.mark_pdf_saved();
        assert!(!app.pdf_dirty, "保存点之后不应脏");
        app.undo_edit();
        assert!(app.pdf_dirty, "撤销离开保存点应变脏");
        app.redo_edit();
        assert!(!app.pdf_dirty, "重做回到保存点应变干净");
    }

    #[test]
    fn moving_ink_is_undoable_and_drops_the_old_object() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 7, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
        }
        app.opened_pdf_refs.insert(reference);
        app.opened_id_by_ref.insert(reference, id);

        let before = app.annotations.get(id).cloned().unwrap();
        let mut after = before.clone();
        after.kind = AnnotationKind::Ink {
            points: vec![Point::new(50.0, 60.0), Point::new(70.0, 70.0)],
            width: 2.0,
        };
        after.pdf_ref = None;
        app.apply_edit(AppEdit::MoveAnnotation { before, after }, true);

        let moved = app
            .annotations
            .get(id)
            .expect("同一条注释")
            .kind
            .ink()
            .unwrap();
        assert_eq!(moved.0[0], Point::new(50.0, 60.0));
        assert!(app.annotations.get(id).unwrap().pdf_ref.is_none());

        // 旧对象要进 deletes（写回时删掉、再按新几何重建）。
        let (_, deletes) = app.pdf_ref_changes();
        assert!(deletes.contains(&reference), "旧对象应被删除");

        app.undo_edit();
        let restored = app
            .annotations
            .get(id)
            .expect("撤销后仍在")
            .kind
            .ink()
            .unwrap();
        assert_eq!(restored.0[0], Point::new(10.0, 10.0));
    }

    #[test]
    fn adding_a_note_is_one_undo_step() {
        let mut app = app();
        let rect = Rect::new(10.0, 20.0, 18.0, 18.0);
        assert!(app.add_note_annotation(PageIndex::FIRST, rect, "便签正文"));
        let note = app.annotations.iter().next().expect("一条便签");
        assert!(matches!(note.kind, AnnotationKind::Note { .. }));
        assert_eq!(note.body.as_deref(), Some("便签正文"));
        // 作者来自环境变量/系统用户；测试环境可能没有，只要求时间已填。
        assert!(note.modified.is_some());
        assert!(note.created.is_some());
        app.undo_edit();
        assert!(
            app.annotations.iter().next().is_none(),
            "一条撤销应删掉整条便签"
        );
    }

    #[test]
    fn restyling_opacity_and_ink_width_is_undoable() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert!(app.restyle_annotation(id, None, Some(0.5), None));
        let new_id = app.annotations.iter().next().unwrap().id;
        assert!((app.annotations.get(new_id).unwrap().opacity - 0.5).abs() < 1e-6);
        app.undo_edit();
        assert!((app.annotations.get(id).unwrap().opacity - 0.35).abs() < 1e-6);

        // 墨迹线宽是 kind 的一部分，改完也要能撤销回 2.0。
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Ink {
                points: vec![Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
                width: 2.0,
            },
        ));
        let ink_id = app
            .annotations
            .iter()
            .find(|annotation| matches!(annotation.kind, AnnotationKind::Ink { .. }))
            .unwrap()
            .id;
        assert!(app.restyle_annotation(ink_id, None, None, Some(4.0)));
        let width = app
            .annotations
            .iter()
            .find(|annotation| matches!(annotation.kind, AnnotationKind::Ink { .. }))
            .and_then(|annotation| annotation.kind.ink())
            .map(|(_, width)| width)
            .unwrap();
        assert!((width - 4.0).abs() < 1e-6);
        app.undo_edit();
        let width = app
            .annotations
            .get(ink_id)
            .and_then(|annotation| annotation.kind.ink())
            .map(|(_, width)| width)
            .unwrap();
        assert!((width - 2.0).abs() < 1e-6);
    }

    #[test]
    fn recoloring_saved_annotation_replaces_the_old_object() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Underline {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let old_id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 42, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(old_id) {
            annotation.pdf_ref = Some(reference);
        }
        app.opened_pdf_refs.insert(reference);
        assert!(app.recolor_annotation(old_id, [70, 180, 90]));
        let current = app.annotations.iter().next().expect("新注释");
        assert_eq!(current.color, [70, 180, 90]);
        assert_eq!(current.pdf_ref, None, "改色必须重新生成对象");
        let (_, deletes) = app.pdf_ref_changes();
        assert!(deletes.contains(&reference), "旧对象应进入 deletes");
    }

    #[test]
    fn external_annotation_cannot_be_recolored() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        app.external_ids.insert(id);
        assert!(!app.recolor_annotation(id, [235, 70, 70]));
        assert_eq!(app.annotations.get(id).unwrap().color, [255, 214, 0]);
    }

    #[test]
    fn annotation_summary_path_does_not_overwrite() {
        let dir = std::env::temp_dir().join(format!(
            "pdf-summary-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("a.b.pdf");
        let first = annotation_summary_path(&pdf);
        assert_eq!(first, dir.join("a.b.annotations.md"));
        std::fs::write(&first, "x").unwrap();
        let second = annotation_summary_path(&pdf);
        assert_eq!(second, dir.join("a.b.annotations-2.md"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn annotation_search_matches_body_and_author() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(0.0, 0.0, 10.0, 10.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.body = Some("研究 笔记".to_string());
            annotation.author = Some("张三".to_string());
        }
        let annotation = app.annotations.get(id).unwrap();
        assert!(annotation_matches_query(annotation, "笔记"));
        assert!(annotation_matches_query(annotation, "张"));
        assert!(!annotation_matches_query(annotation, "李四"));
    }

    #[test]
    fn recent_order_puts_newest_first_and_unknown_last() {
        let row = |id: u64, modified: Option<i64>| AnnotationRow {
            page: PageIndex::FIRST,
            id: AnnotationId(id),
            label: "高亮",
            readonly: false,
            external: false,
            summary: String::new(),
            preview: String::new(),
            author: None,
            created: None,
            modified,
            bounds: None,
        };
        let mut rows = [row(1, Some(100)), row(2, None), row(3, Some(300))];
        rows.sort_by(recent_order);
        let ids: Vec<u64> = rows.iter().map(|row| row.id.get()).collect();
        assert_eq!(ids, vec![3, 1, 2], "按修改时间倒序，未知时间在最后");
    }

    #[test]
    fn annotation_list_delete_is_undoable_for_inline_external() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert!(app.delete_annotation(id));
        assert!(app.annotations.get(id).is_none());
        app.undo_edit();
        assert!(app.annotations.get(id).is_some());

        let id = app.annotations.iter().next().unwrap().id;
        app.external_ids.insert(id);
        app.inline_refs.insert(id, (PageIndex::FIRST, 2));
        app.opened_inline_bodies
            .insert((PageIndex::FIRST, 2), Some("旧正文".to_string()));
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = None;
            annotation.body = Some("新正文".to_string());
        }
        let (updates, deletes) = app.inline_changes();
        assert_eq!(updates.len(), 1);
        assert!(deletes.is_empty());
        assert!(app.delete_annotation(id));
        assert!(app.annotations.get(id).is_none());
        let (_, deletes) = app.inline_changes();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].position, 2);
    }

    #[test]
    fn conflict_message_mentions_unsaved_edits_and_save_as() {
        assert!(PDF_CONFLICT_MESSAGE.contains("当前修改仍在编辑器中"));
        assert!(PDF_CONFLICT_MESSAGE.contains("另存为"));
    }

    #[test]
    fn pdf_file_stamp_detects_external_size_change() {
        let path = std::env::temp_dir().join(format!(
            "pdf-reader-stamp-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, b"pdf").expect("写");
        let before = pdf_file_stamp(&path).expect("stamp");
        std::fs::write(&path, b"pdf changed").expect("改");
        let after = pdf_file_stamp(&path).expect("stamp2");
        assert_ne!(before, after);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_as_warning_truth_table() {
        let table = |original, target, dirty| save_as_warnings(original, target, dirty, true);
        assert_eq!(
            table(false, false, false),
            SaveAsWarnings {
                new_missing_data: true,
                ..Default::default()
            }
        );
        assert_eq!(table(false, true, false), SaveAsWarnings::default());
        assert_eq!(
            table(true, false, false),
            SaveAsWarnings {
                new_missing_data: true,
                ..Default::default()
            }
        );
        assert_eq!(table(true, true, false), SaveAsWarnings::default());
        assert_eq!(
            table(false, false, true),
            SaveAsWarnings {
                new_missing_data: true,
                no_data_file: true,
                ..Default::default()
            }
        );
        assert_eq!(
            table(false, true, true),
            SaveAsWarnings {
                original_stale: true,
                ..Default::default()
            }
        );
        assert_eq!(
            table(true, false, true),
            SaveAsWarnings {
                new_missing_data: true,
                unsaved_new: true,
                ..Default::default()
            }
        );
        assert_eq!(table(true, true, true), SaveAsWarnings::default());
    }

    #[test]
    fn annotation_list_summary_and_tooltip_are_compact() {
        assert_eq!(body_summary(None), "无正文");
        assert_eq!(
            body_summary(Some("  \n  first   line \nsecond")),
            "first line"
        );
        assert_eq!(body_tooltip(Some("a\nb")), "a b");
        let long = "字".repeat(300);
        let tooltip = body_tooltip(Some(&long));
        assert_eq!(tooltip.chars().count(), 281);
        assert!(tooltip.ends_with('…'));
    }

    #[test]
    fn preview_title_body_and_resize_are_undoable() {
        let mut app = app();
        let preview = CustomPreview {
            id: 7,
            page: PageIndex::FIRST,
            point: Point::new(10.0, 10.0),
            rect: Rect::new(10.0, 10.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "旧标题".to_string(),
            text: Some("旧正文".to_string()),
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        };
        let key = PreviewKey {
            page: preview.page,
            id: preview.id,
        };
        app.apply_edit(
            AppEdit::AddPreview {
                key,
                position: 0,
                preview: std::sync::Arc::new(preview),
            },
            true,
        );
        app.apply_edit(
            AppEdit::SetPreviewTitle {
                key,
                before: "旧标题".to_string(),
                after: "新标题".to_string(),
            },
            true,
        );
        app.apply_edit(
            AppEdit::SetPreviewBody {
                key,
                before: Some("旧正文".to_string()),
                after: Some("新正文".to_string()),
            },
            true,
        );
        app.apply_edit(
            AppEdit::ResizePreview {
                key,
                before: DEFAULT_CUSTOM_TEXT_BODY,
                after: EVec2::new(400.0, 200.0),
            },
            true,
        );
        assert_eq!(app.custom_previews[0].title, "新标题");
        assert_eq!(app.custom_previews[0].text.as_deref(), Some("新正文"));
        assert_eq!(app.custom_previews[0].body_size, EVec2::new(400.0, 200.0));
        app.undo_edit();
        assert_eq!(app.custom_previews[0].body_size, DEFAULT_CUSTOM_TEXT_BODY);
        app.undo_edit();
        assert_eq!(app.custom_previews[0].text.as_deref(), Some("旧正文"));
        app.undo_edit();
        assert_eq!(app.custom_previews[0].title, "旧标题");
    }

    #[test]
    fn consecutive_preview_resizes_merge_into_one_undo() {
        let mut app = app();
        let preview = CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "t".to_string(),
            text: None,
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        };
        let key = PreviewKey {
            page: preview.page,
            id: preview.id,
        };
        app.apply_edit(
            AppEdit::AddPreview {
                key,
                position: 0,
                preview: std::sync::Arc::new(preview),
            },
            true,
        );
        app.apply_preview_resize(key, DEFAULT_CUSTOM_TEXT_BODY, EVec2::new(400.0, 200.0));
        app.apply_preview_resize(key, EVec2::new(400.0, 200.0), EVec2::new(500.0, 250.0));
        assert_eq!(app.history.len(), 2, "创建 + 合并后的 resize 应只有两条");
        assert_eq!(app.custom_previews[0].body_size, EVec2::new(500.0, 250.0));
        app.floats.push(Float {
            id: 1,
            title: "t".to_string(),
            content: FloatContent::Text(String::new()),
            rect: egui::Rect::from_min_size(
                egui::pos2(10.0, 10.0),
                crate::view::float_size_for_body(EVec2::new(500.0, 250.0), FLOAT_TITLE_HEIGHT),
            ),
            owner: FloatOwner::Custom(0),
            body: EVec2::new(500.0, 250.0),
            origin: None,
        });
        app.undo_edit();
        assert_eq!(app.custom_previews[0].body_size, DEFAULT_CUSTOM_TEXT_BODY);
        assert_eq!(app.floats[0].body, DEFAULT_CUSTOM_TEXT_BODY);
    }

    #[test]
    fn preview_move_interface_keeps_stable_key() {
        let mut app = app();
        for (index, title) in ["A", "B"].iter().enumerate() {
            let preview = CustomPreview {
                id: index as u64,
                page: PageIndex::FIRST,
                point: Point::new(index as f32, 0.0),
                rect: Rect::new(index as f32, 0.0, 0.0, 0.0),
                kind: CustomKind::Text,
                title: (*title).to_string(),
                text: Some(String::new()),
                snapshot: None,
                body_size: DEFAULT_CUSTOM_TEXT_BODY,
                scale: 1.0,
                image_id: String::new(),
                image_revision: 0,
            };
            app.apply_edit(
                AppEdit::AddPreview {
                    key: PreviewKey {
                        page: preview.page,
                        id: preview.id,
                    },
                    position: app.custom_previews.len(),
                    preview: std::sync::Arc::new(preview),
                },
                true,
            );
        }
        let key = PreviewKey {
            page: PageIndex::FIRST,
            id: 0,
        };
        app.move_custom_preview(key, 1);
        assert_eq!(app.custom_previews[0].id, 1);
        assert_eq!(app.custom_previews[1].id, 0);
        app.undo_edit();
        assert_eq!(app.custom_previews[0].id, 0);
    }

    #[test]
    fn markup_tool_mode_enters_select_and_toggles_off() {
        let mut app = app();
        app.toggle_markup_tool(MarkupTool::StrikeOut);
        assert_eq!(app.annotation_tool, Some(MarkupTool::StrikeOut));
        assert_eq!(app.tool, Tool::Select, "标记模式应自动切到选字");
        app.toggle_markup_tool(MarkupTool::StrikeOut);
        assert_eq!(app.annotation_tool, None);
        assert_eq!(app.tool, Tool::Pan, "退出后应恢复进入前的工具");
    }

    #[test]
    fn switching_between_annotation_modes_keeps_the_reading_mode() {
        // D-214 起取代 D-174 的"回到墨迹"：注释模式之间切换不改写 A，
        // 退出时回到进入注释模式之前的那一个**阅读模式**。
        let mut app = app();
        app.switch_tool(Tool::Pan);
        app.enter_ink_mode();
        app.toggle_markup_tool(MarkupTool::Highlight);
        assert_eq!(app.tool, Tool::Select);
        app.exit_markup_tool();
        assert_eq!(app.annotation_tool, None);
        assert_eq!(app.tool, Tool::Pan, "Esc/退出应回到最初的阅读模式");
    }

    #[test]
    fn ink_uses_the_style_color() {
        let mut app = app();
        app.ink_color = [200, 30, 30];
        assert!(app.annotate_ink(
            PageIndex::FIRST,
            &[Point::new(10.0, 10.0), Point::new(30.0, 20.0)],
            2.0,
        ));
        let annotation = app.annotations.iter().next().expect("墨迹");
        assert_eq!(annotation.color, [200, 30, 30]);
        assert_eq!(annotation.opacity, INK_OPACITY);
    }

    #[test]
    fn styled_annotation_keeps_style_through_undo_redo() {
        let mut app = app();
        assert!(app.add_annotation_styled(
            PageIndex::FIRST,
            AnnotationKind::Underline {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
            [235, 70, 70],
            1.0,
        ));
        let id = app.annotations.iter().next().unwrap().id;
        assert_eq!(app.annotations.get(id).unwrap().color, [235, 70, 70]);
        app.undo_edit();
        app.undo_edit();
        app.redo_edit();
        assert_eq!(
            app.annotations.get(id).unwrap().color,
            [235, 70, 70],
            "撤销/重做必须保留样式"
        );
    }

    #[test]
    fn three_markup_buttons_map_to_three_annotation_kinds() {
        let quads = vec![Rect::new(10.0, 20.0, 30.0, 8.0)];
        for (tool, label) in [
            (MarkupTool::Highlight, "高亮"),
            (MarkupTool::Underline, "下划线"),
            (MarkupTool::StrikeOut, "删除线"),
        ] {
            assert_eq!(tool.label(), label);
            match tool.kind(quads.clone()) {
                AnnotationKind::Highlight { .. } => assert_eq!(label, "高亮"),
                AnnotationKind::Underline { .. } => assert_eq!(label, "下划线"),
                AnnotationKind::StrikeOut { .. } => assert_eq!(label, "删除线"),
                _ => panic!("不应生成墨迹"),
            }
        }
    }

    #[test]
    fn undo_back_to_saved_state_clears_the_right_dirty_flag() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        assert!(app.pdf_dirty, "新增注释是 PDF 脏");
        assert!(!app.data_dirty, "新增注释不是自定义数据脏");
        app.undo_edit();
        assert!(!app.pdf_dirty, "撤销回到干净点应清 PDF 脏");
        assert!(!app.data_dirty);
    }

    #[test]
    fn deleting_and_undoing_annotation_keeps_its_body() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: None,
                after: Some("正文".to_string()),
            },
            true,
        );
        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        assert!(app.annotations.get(id).is_none());
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.body.as_deref()),
            Some("正文")
        );
        assert!(app.custom_previews.is_empty());
    }

    /// 回归：Markdown 模式下**空正文**浮窗也能双击打开正文编辑器。
    ///
    /// 这条之前是 roadmap 的已知限制：`markdown_floats` 时 `detect_body_double_click`
    /// 直接返回，而空正文又不走正文内容控件，双击没有任何入口。
    #[test]
    fn empty_markdown_float_opens_editor_on_double_click() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(320.0, 240.0));
        app.custom_previews.push(CustomPreview {
            id: 1,
            page: PageIndex::FIRST,
            point: Point::new(0.0, 0.0),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            kind: CustomKind::Text,
            title: "空正文".to_string(),
            text: None,
            snapshot: None,
            body_size: DEFAULT_CUSTOM_TEXT_BODY,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        });
        app.floats.push(Float {
            id: 1,
            title: "空正文".to_string(),
            content: FloatContent::Text(String::new()),
            rect,
            owner: FloatOwner::Custom(0),
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let pos = egui::pos2(200.0, 160.0);
        let click = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        // 两帧各一次点击：第二帧构成双击，验证全窗层能接住空正文。
        for frame in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(canvas),
                events: vec![egui::Event::PointerMoved(pos), click(true), click(false)],
                time: Some(1.0 + frame as f64 * 0.05),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.draw_floats(ui, canvas);
                app.detect_body_double_click(ui);
            });
            output.drop_without_applying_deltas();
        }
        assert!(
            app.editing_body.is_some(),
            "Markdown 空正文双击应打开正文编辑器"
        );
    }

    #[test]
    fn resizing_a_dest_float_is_dirty_labelled_and_undoable() {
        let mut app = app();
        let page = PageIndex::FIRST;
        let before = egui::vec2(120.0, 80.0);
        let after = egui::vec2(300.0, 200.0);
        app.dest_marks = vec![vec![DestMark {
            point: Point::new(10.0, 20.0),
            rect: Rect::new(10.0, 20.0, 120.0, 80.0),
            snapshot: None,
            body_size: before,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        }]];
        let rect = egui::Rect::from_min_size(
            egui::pos2(40.0, 40.0),
            crate::view::float_size_for_body(before, FLOAT_TITLE_HEIGHT),
        );
        app.floats.push(Float {
            id: 1,
            title: "引用点".to_string(),
            content: FloatContent::Text(String::new()),
            rect,
            owner: FloatOwner::None,
            body: before,
            origin: Some((page, 0)),
        });
        // 模拟“刚保存/刚打开”的干净基线。
        app.data_clean_hash = Some(app.data_fingerprint());
        app.refresh_dirty();
        assert!(!app.data_dirty, "基线应干净");

        app.apply_dest_resize(page, 0, before, after);
        assert!(app.data_dirty, "引用点浮窗改大小应置数据脏");
        assert_eq!(app.dest_marks[0][0].body_size, after);
        assert_eq!(app.floats[0].body, after);
        let description = app
            .history
            .last_mut()
            .map(|entry| entry.description.clone())
            .expect("一条历史");
        assert!(
            description.contains("调整引用点浮窗大小"),
            "历史应写明是改大小：{description}"
        );
        assert!(!description.contains("重选引用点"), "{description}");

        app.undo_edit();
        assert_eq!(app.dest_marks[0][0].body_size, before);
        assert_eq!(app.floats[0].body, before);
        assert!(!app.data_dirty, "撤销回保存点后应重新干净");
    }

    #[test]
    fn consecutive_dest_resizes_merge_into_one_history_entry() {
        let mut app = app();
        let page = PageIndex::FIRST;
        let a = egui::vec2(120.0, 80.0);
        let b = egui::vec2(200.0, 120.0);
        let c = egui::vec2(300.0, 200.0);
        app.dest_marks = vec![vec![DestMark {
            point: Point::new(10.0, 20.0),
            rect: Rect::new(10.0, 20.0, 120.0, 80.0),
            snapshot: None,
            body_size: a,
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        }]];
        let rect = egui::Rect::from_min_size(
            egui::pos2(40.0, 40.0),
            crate::view::float_size_for_body(a, FLOAT_TITLE_HEIGHT),
        );
        app.floats.push(Float {
            id: 1,
            title: "引用点".to_string(),
            content: FloatContent::Text(String::new()),
            rect,
            owner: FloatOwner::None,
            body: a,
            origin: Some((page, 0)),
        });
        app.apply_dest_resize(page, 0, a, b);
        app.apply_dest_resize(page, 0, b, c);
        assert_eq!(app.history.len(), 1, "连续 resize 应合并成一条");
        assert_eq!(app.dest_marks[0][0].body_size, c);
        assert_eq!(app.floats[0].body, c);
    }

    #[test]
    fn canvas_hover_is_clipped_by_panels_floats_and_gestures() {
        let mut app = app();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let inside = egui::pos2(100.0, 100.0);
        assert!(app.canvas_hover_allowed(&ctx, canvas, inside, false));
        // 侧栏/顶栏方向（canvas 之外）
        assert!(!app.canvas_hover_allowed(&ctx, canvas, egui::pos2(900.0, 100.0), false));
        // 连续动作进行中
        assert!(!app.canvas_hover_allowed(&ctx, canvas, inside, true));
        // 浮窗覆盖（任意部分，包括标题栏）
        app.floats.push(Float {
            id: 1,
            title: "遮挡".to_string(),
            content: FloatContent::Text(String::new()),
            rect: egui::Rect::from_min_size(inside, egui::vec2(200.0, 160.0)),
            owner: FloatOwner::None,
            body: egui::vec2(200.0, 142.0),
            origin: None,
        });
        assert!(!app.canvas_hover_allowed(&ctx, canvas, inside, false));
    }

    #[test]
    fn topmost_float_wins_over_lower_float() {
        let mut app = app();
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 120.0));
        for id in [1, 2] {
            app.floats.push(Float {
                id,
                title: format!("浮窗 {id}"),
                content: FloatContent::Text(String::new()),
                rect,
                owner: FloatOwner::None,
                body: egui::vec2(200.0, 102.0),
                origin: None,
            });
        }
        // 后加入的（id=2，下标 1）在最上层。
        assert_eq!(app.topmost_float_at(rect.center()), Some(1));
    }

    /// 回归：高浮窗标题栏压在低浮窗正文上时，双击 ✎ 必须打开**高**浮窗的编辑器。
    #[test]
    fn double_click_on_upper_title_bar_does_not_open_lower_editor() {
        let mut app = app();
        app.markdown_floats = false;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // 低浮窗：正文覆盖 (100,100)-(500,400)
        let lower = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(400.0, 300.0));
        // 高浮窗：标题栏 y=130..148，压在低浮窗正文上
        let upper = egui::Rect::from_min_size(egui::pos2(150.0, 130.0), egui::vec2(300.0, 200.0));
        for (id, rect, annotation) in [(1, lower, 1), (2, upper, 2)] {
            app.floats.push(Float {
                id,
                title: format!("标注 #{annotation}"),
                content: FloatContent::Text(String::new()),
                rect,
                owner: FloatOwner::Annotation(AnnotationId(annotation)),
                body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
                origin: None,
            });
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        // 高浮窗标题栏里的 ✎ 中心：right-39, title 中心。
        let position = egui::pos2(upper.right() - 39.0, upper.top() + FLOAT_TITLE_HEIGHT * 0.5);
        let click = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        for frame in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(canvas),
                events: vec![
                    egui::Event::PointerMoved(position),
                    click(true),
                    click(false),
                ],
                time: Some(1.0 + frame as f64 * 0.05),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.draw_floats(ui, canvas);
                app.detect_body_double_click(ui);
            });
            output.drop_without_applying_deltas();
        }
        assert_eq!(
            app.body_edit_owner(),
            Some(FloatOwner::Annotation(AnnotationId(2))),
            "应打开高浮窗的编辑器，而不是被压住的低浮窗"
        );
    }
}

#[cfg(test)]
mod pointer_ownership_tests {
    use super::super::*;
    use crate::view::FLOAT_TITLE_HEIGHT;
    use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache, MemoryPage};
    use std::sync::Arc;

    fn app() -> PdfReaderApp {
        PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        )
    }

    fn run_frame(
        app: &mut PdfReaderApp,
        ctx: &egui::Context,
        screen: egui::Rect,
        events: Vec<egui::Event>,
        time: f64,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            time: Some(time),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            app.frame(ui);
        });
        output.drop_without_applying_deltas();
    }

    /// 一个打开的内存文档 + 一条高亮注释；返回指针应落到的屏幕坐标。
    fn app_with_annotation() -> (
        PdfReaderApp,
        egui::Context,
        AnnotationId,
        egui::Rect,
        egui::Pos2,
    ) {
        let backend = MemoryBackend::new(vec![MemoryPage::new(
            Size::new(600.0, 800.0),
            Rotation::None,
            vec!["Hello".to_string()],
        )]);
        let opened = backend.open(Source::Bytes(b"x"), None).unwrap();
        let raster = opened.raster.clone();
        let mut session = DocumentSession::new(
            opened,
            Arc::new(MemoryCache::new()),
            Arc::new(ImmediateScheduler::new(raster)),
            Box::new(VerticalLayout),
            Box::new(RangeSelection::new()),
        )
        .unwrap();
        let mut events = Vec::new();
        session.handle(
            ViewerCommand::ResizeViewport(Size::new(800.0, 600.0)),
            &mut events,
        );
        session.handle(ViewerCommand::SetZoom(1.0), &mut events);

        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        app.session = Some(session);
        app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(250.0, 400.0, 120.0, 30.0)],
            },
        );
        let id = app.annotations.iter().next().expect("annotation").id;
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // 两帧让面板/画布布局稳定，再按真实布局算注释的屏幕坐标。
        for frame in 0..2 {
            run_frame(&mut app, &ctx, screen, Vec::new(), frame as f64);
        }
        let session = app.session.as_ref().expect("session");
        let placement = session.placement(PageIndex::FIRST).expect("placement");
        let content = placement.rect_to_content(Rect::new(250.0, 400.0, 120.0, 30.0));
        let scroll = session.viewport().scroll;
        let position = egui::pos2(
            app.canvas_rect.min.x + content.x - scroll.x + 10.0,
            app.canvas_rect.min.y + content.y - scroll.y + 10.0,
        );
        (app, ctx, id, screen, position)
    }

    /// 两个 Markdown 文字浮窗重叠：在上浮窗正文上拖动，不能拖动下层浮窗的标题栏。
    #[test]
    fn dragging_upper_markdown_float_does_not_move_lower_float() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let lower = egui::Rect::from_min_size(egui::pos2(120.0, 150.0), egui::vec2(300.0, 200.0));
        let upper = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0));
        for (id, rect, text) in [(1, lower, "lower"), (2, upper, "upper")] {
            app.floats.push(Float {
                id,
                title: format!("浮窗 {id}"),
                content: FloatContent::Text(text.to_string()),
                rect,
                owner: FloatOwner::None,
                body: egui::vec2(rect.width(), rect.height() - FLOAT_TITLE_HEIGHT),
                origin: None,
            });
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // 指针落在上浮窗正文、同时压住下浮窗标题栏（y=150..168）。
        let start = egui::pos2(200.0, 160.0);
        let mut frames = Vec::new();
        for frame in 0..5 {
            let events = match frame {
                0 => vec![],
                1 => vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                4 => vec![egui::Event::PointerButton {
                    pos: egui::pos2(start.x + 40.0, start.y),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
                _ => vec![egui::Event::PointerMoved(egui::pos2(
                    start.x + 20.0 * (frame - 1) as f32,
                    start.y,
                ))],
            };
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(frame as f64 * 0.05),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                app.draw_floats(ui, canvas);
            });
            out.drop_without_applying_deltas();
            frames.push(());
        }
        assert_eq!(
            app.floats[0].rect, lower,
            "下浮窗不应被上浮窗正文上的拖动带着走"
        );
    }

    /// 上浮窗正文只感知 click（Markdown）时，拖动也不能漏给 PDF 画布。
    #[test]
    fn dragging_markdown_float_body_does_not_fall_through_to_canvas() {
        let mut app = app();
        app.markdown_floats = true;
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0));
        app.floats.push(Float {
            id: 1,
            title: "浮窗".to_string(),
            content: FloatContent::Text("markdown".to_string()),
            rect,
            owner: FloatOwner::None,
            body: egui::vec2(rect.width(), rect.height() - FLOAT_TITLE_HEIGHT),
            origin: None,
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let start = egui::pos2(200.0, 200.0);
        let mut canvas_dragged = false;
        for frame in 0..5 {
            let events = match frame {
                0 => vec![],
                1 => vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                4 => vec![egui::Event::PointerButton {
                    pos: egui::pos2(start.x + 40.0, start.y),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
                _ => vec![egui::Event::PointerMoved(egui::pos2(
                    start.x + 20.0 * (frame - 1) as f32,
                    start.y,
                ))],
            };
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(frame as f64 * 0.05),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                let response = ui.interact(
                    canvas,
                    egui::Id::unique("test-canvas"),
                    egui::Sense::click_and_drag(),
                );
                app.draw_floats(ui, canvas);
                canvas_dragged |= response.dragged() || response.drag_started();
            });
            out.drop_without_applying_deltas();
        }
        assert!(!canvas_dragged, "拖动浮窗正文不应落到 PDF 画布");
    }

    #[test]
    fn clicking_editor_window_over_a_float_does_not_raise_it() {
        let mut app = app();
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        for (id, left, top, title) in [(1, 50.0, 50.0, "低"), (2, 150.0, 150.0, "高")] {
            let rect = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(300.0, 200.0));
            app.floats.push(Float {
                id,
                title: title.to_string(),
                content: FloatContent::Text(String::new()),
                rect,
                owner: FloatOwner::None,
                body: egui::vec2(300.0, 182.0),
                origin: None,
            });
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let point = egui::pos2(60.0, 60.0);
        for frame in 0..3 {
            let events = if frame < 2 {
                vec![egui::Event::PointerMoved(point)]
            } else {
                vec![
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    },
                ]
            };
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                time: Some(frame as f64 * 0.05),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.draw_floats(ui, canvas);
                egui::Window::new("editor")
                    .fixed_rect(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(400.0, 300.0),
                    ))
                    .show(ui.ctx(), |ui| {
                        ui.label("editor");
                    });
            });
            output.drop_without_applying_deltas();
        }
        let order: Vec<u64> = app.floats.iter().map(|float| float.id).collect();
        assert_eq!(order, vec![1, 2], "编辑窗口覆盖处点击不应把下面浮窗置顶");
        assert!(
            !app.canvas_hover_allowed(&ctx, canvas, point, false),
            "编辑窗口覆盖处也不应允许画布链接/注释 hover"
        );
    }

    #[test]
    fn canvas_annotation_hover_right_click_and_delete_are_scoped() {
        let (mut app, ctx, id, screen, pos) = app_with_annotation();
        // 1) 画布上 hover → 记录 Delete 目标
        run_frame(
            &mut app,
            &ctx,
            screen,
            vec![egui::Event::PointerMoved(pos)],
            10.0,
        );
        assert_eq!(app.hovered_annotation, Some(id), "画布 hover 应命中注释");

        // 2) 指针移到侧栏 → 悬停目标立即清空
        run_frame(
            &mut app,
            &ctx,
            screen,
            vec![egui::Event::PointerMoved(egui::pos2(50.0, 300.0))],
            11.0,
        );
        assert_eq!(app.hovered_annotation, None, "侧栏 hover 不应保留注释目标");

        // 3) 此时 Delete 不得删除
        run_frame(
            &mut app,
            &ctx,
            screen,
            vec![egui::Event::Key {
                key: egui::Key::Delete,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            12.0,
        );
        assert!(
            app.annotations.get(id).is_some(),
            "侧栏 hover + Delete 不应删除"
        );

        // 4) 回到画布 hover，再右键 → 菜单目标
        run_frame(
            &mut app,
            &ctx,
            screen,
            vec![egui::Event::PointerMoved(pos)],
            13.0,
        );
        assert_eq!(app.hovered_annotation, Some(id));
        let secondary = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        run_frame(
            &mut app,
            &ctx,
            screen,
            vec![secondary(true), secondary(false)],
            14.0,
        );
        assert_eq!(app.context_annotation, Some(id), "右键应指向注释");

        // 5) Delete 删除悬停注释
        run_frame(
            &mut app,
            &ctx,
            screen,
            vec![egui::Event::Key {
                key: egui::Key::Delete,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            15.0,
        );
        assert!(
            app.annotations.get(id).is_none(),
            "画布 hover + Delete 应删除"
        );
    }
}

#[cfg(test)]
mod float_link_tests {
    use super::super::*;
    use crate::view::{body_size_of, FLOAT_TITLE_HEIGHT};

    fn app() -> PdfReaderApp {
        PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        )
    }

    fn text_float(id: u64, owner: FloatOwner, title: &str) -> Float {
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(280.0, 150.0));
        Float {
            id,
            title: title.to_string(),
            content: FloatContent::Text(String::new()),
            rect,
            owner,
            body: body_size_of(rect, FLOAT_TITLE_HEIGHT),
            origin: None,
        }
    }

    fn text_preview(page: PageIndex, id: u64) -> CustomPreview {
        CustomPreview {
            id,
            page,
            point: Point::new(10.0, 20.0),
            rect: Rect::new(10.0, 20.0, 120.0, 80.0),
            kind: CustomKind::Text,
            title: format!("preview {id}"),
            text: Some("正文".to_string()),
            snapshot: None,
            body_size: egui::vec2(200.0, 100.0),
            scale: 1.0,
            image_id: String::new(),
            image_revision: 0,
        }
    }

    #[test]
    fn external_annotation_links_require_token_and_validation() {
        let mut app = app();
        app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        );
        let id = app.annotations.iter().next().expect("annotation").id;
        app.external_ids.insert(id);
        app.document_load_token = 42;
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.created = Some(123);
        }

        // 默认关闭：不提供复制 URI。
        assert!(app.annotation_link_uri(id).is_none());

        app.allow_external_annotation_links = true;
        let uri = app.annotation_link_uri(id).expect("external uri");
        assert!(uri.contains("s=42"), "{uri}");
        assert!(uri.contains("k=highlight"), "{uri}");
        assert!(uri.contains("c=123"), "{uri}");

        // token 匹配：当前会话直接打开。
        app.follow_float_link(&uri);
        assert_eq!(
            app.floats.last().map(|float| float.owner),
            Some(FloatOwner::Annotation(id))
        );

        // token 不匹配 + 开关关闭：断链。
        app.floats.clear();
        app.document_load_token = 99;
        app.allow_external_annotation_links = false;
        app.follow_float_link(&uri);
        assert!(app.message.contains("跨会话失效"), "{}", app.message);
        assert!(app.floats.is_empty());

        // token 不匹配 + 开关开启：类型 + 创建时间都匹配 → 打开。
        app.allow_external_annotation_links = true;
        app.follow_float_link(&uri);
        assert_eq!(app.floats.len(), 1);

        // 类型不匹配 → 断链。
        app.floats.clear();
        let wrong_kind = uri.replace("k=highlight", "k=ink");
        app.follow_float_link(&wrong_kind);
        assert!(app.message.contains("类型"), "{}", app.message);
        assert!(app.floats.is_empty());

        // 创建时间不匹配 → 断链。
        let wrong_created = uri.replace("c=123", "c=456");
        app.follow_float_link(&wrong_created);
        assert!(app.message.contains("创建时间"), "{}", app.message);

        // 缺少创建时间 → 进入确认框，不直接打开。
        let unverified = uri.replace("&c=123", "");
        app.follow_float_link(&unverified);
        assert!(app.pending_external_link.is_some(), "应进入确认");
        assert!(app.floats.is_empty());
    }

    #[test]
    fn preview_record_id_is_used_in_links() {
        let mut app = app();
        let mut preview = text_preview(PageIndex::new(2), 7);
        preview.image_id = "preview-abc-1".to_string();
        app.custom_previews.push(preview);
        app.floats
            .push(text_float(1, FloatOwner::Custom(0), "preview"));
        assert_eq!(
            app.preview_link_uri(0).as_deref(),
            Some("pd-float://preview/preview-abc-1")
        );
        assert_eq!(
            app.float_link_uri(0).as_deref(),
            Some("pd-float://preview/preview-abc-1")
        );
    }

    #[test]
    fn deleting_a_preview_keeps_other_preview_floats() {
        let mut app = app();
        app.custom_previews.push(text_preview(PageIndex::FIRST, 1));
        app.custom_previews.push(text_preview(PageIndex::FIRST, 2));
        app.floats.push(text_float(1, FloatOwner::Custom(0), "a"));
        app.floats.push(text_float(2, FloatOwner::Custom(1), "b"));
        app.delete_custom_preview(0);
        assert_eq!(app.custom_previews.len(), 1);
        assert_eq!(app.floats.len(), 1, "只应关闭被删 preview 的浮窗");
        assert_eq!(app.floats[0].owner, FloatOwner::Custom(0));
        assert_eq!(app.floats[0].title, "b");
    }

    #[test]
    fn annotation_ids_are_not_reused_and_restyle_keeps_the_id() {
        let mut app = app();
        assert!(app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("annotation").id;
        assert!(id.get() > 0);
        assert!(
            id.get() < (1u64 << 53),
            "id 必须能被写回 job 的 JSON number 精确表示"
        );

        // 改颜色：id 不变，旧对象仍进入 deletes（pdf_ref = None）。
        assert!(app.recolor_annotation(id, [70, 180, 90]));
        assert!(app.annotations.get(id).is_some(), "改样式不得换 id");
        assert_eq!(app.annotations.get(id).unwrap().color, [70, 180, 90]);
        app.undo_edit();
        assert!(app.annotations.get(id).is_some(), "撤销后仍是同一 id");

        // 删除后新注释不得复用旧 id。
        let old_id = app.annotations.iter().next().unwrap().id;
        app.apply_edit(
            AppEdit::DeleteAnnotation {
                annotation: app.annotations.get(old_id).unwrap().clone(),
            },
            true,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 40.0, 30.0, 8.0)],
            },
        ));
        let new_id = app.annotations.iter().next().unwrap().id;
        assert_ne!(new_id, old_id, "新注释不得复用已删除的 id");
        assert!(new_id.get() < (1u64 << 53));
    }

    #[test]
    fn annotation_link_uri_round_trips_and_reuses_the_click_helper() {
        let mut app = app();
        app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        );
        let id = app.annotations.iter().next().expect("annotation").id;
        app.floats
            .push(text_float(1, FloatOwner::Annotation(id), "标注"));
        let uri = app.float_link_uri(0).expect("link uri");
        assert_eq!(
            uri,
            format!(
                "pd-float://annotation/{}/{}",
                PageIndex::FIRST.get(),
                id.get()
            )
        );

        // 关闭后点击链接 → 重新打开，且带一次性高亮
        app.floats.clear();
        app.follow_float_link(&uri);
        assert_eq!(app.floats.len(), 1);
        assert_eq!(app.floats[0].owner, FloatOwner::Annotation(id));
        assert!(app.click_highlight.is_some(), "应从链接打开后高亮");

        // 已有浮窗 → 置顶（最后一条）
        app.floats.push(text_float(2, FloatOwner::None, "其他"));
        app.follow_float_link(&uri);
        assert_eq!(
            app.floats.last().map(|f| f.owner),
            Some(FloatOwner::Annotation(id))
        );
    }

    #[test]
    fn title_bar_link_button_copies_pd_float_uri() {
        let mut app = app();
        app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        );
        let id = app.annotations.iter().next().expect("annotation").id;
        app.floats
            .push(text_float(1, FloatOwner::Annotation(id), "标注"));
        let rect = app.floats[0].rect;
        // 标题栏宽度固定 18；↗ 在 close 左侧 52。
        let position = egui::pos2(rect.right() - 65.0, rect.top() + FLOAT_TITLE_HEIGHT * 0.5);
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let mut copied = None;
        for frame in 0..3 {
            let events = match frame {
                0 => vec![],
                1 => vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                _ => vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
            };
            let input = egui::RawInput {
                screen_rect: Some(canvas),
                events,
                time: Some(frame as f64 * 0.05),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.draw_floats(ui, canvas);
            });
            for command in &output.platform_output.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    copied = Some(text.clone());
                }
            }
            output.drop_without_applying_deltas();
        }
        assert_eq!(
            copied.as_deref(),
            Some(
                format!(
                    "pd-float://annotation/{}/{}",
                    PageIndex::FIRST.get(),
                    id.get()
                )
                .as_str(),
            )
        );
    }

    #[test]
    fn title_bar_goto_button_pushes_jump_commands() {
        let mut app = app();
        app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        );
        let id = app.annotations.iter().next().expect("annotation").id;
        app.floats
            .push(text_float(1, FloatOwner::Annotation(id), "标注"));
        let rect = app.floats[0].rect;
        let position = egui::pos2(rect.right() - 91.0, rect.top() + FLOAT_TITLE_HEIGHT * 0.5);
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        for frame in 0..3 {
            let events = match frame {
                0 => vec![],
                1 => vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                _ => vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
            };
            let input = egui::RawInput {
                screen_rect: Some(canvas),
                events,
                time: Some(frame as f64 * 0.05),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.draw_floats(ui, canvas);
            });
            output.drop_without_applying_deltas();
        }
        let jumped = app.commands.iter().any(
            |command| matches!(command, ViewerCommand::GoToPage(page) if *page == PageIndex::FIRST),
        );
        let centered = app.commands.iter().any(|command| {
            matches!(
                command,
                ViewerCommand::CenterOnPagePoint { page, .. } if *page == PageIndex::FIRST
            )
        });
        assert!(jumped && centered, "goto 应发跳页 + 居中命令");
        assert_eq!(
            app.annotation_focus.map(|(focus, _)| focus),
            Some(id),
            "注释跳转应带定位轮廓"
        );
    }

    #[test]
    fn preview_links_support_page_id_and_legacy_unique_id() {
        let mut app = app();
        app.custom_previews.push(text_preview(PageIndex::new(2), 7));
        app.floats
            .push(text_float(1, FloatOwner::Custom(0), "preview"));
        assert_eq!(
            app.float_link_uri(0).as_deref(),
            Some("pd-float://preview/2/7")
        );

        // 显式页号 + id
        app.floats.clear();
        app.follow_float_link("pd-float://preview/2/7");
        assert_eq!(app.floats.len(), 1);
        assert_eq!(app.floats[0].owner, FloatOwner::Custom(0));

        // 旧写法：id 唯一时可解析
        app.floats.clear();
        app.follow_float_link("pd-float://preview/7");
        assert_eq!(app.floats.len(), 1);
        assert_eq!(app.floats[0].owner, FloatOwner::Custom(0));
    }

    #[test]
    fn markdown_link_consumption_dispatches_pd_float_and_keeps_external_hint() {
        let mut app = app();
        app.add_annotation(
            PageIndex::FIRST,
            pdfcore::annotation::AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        );
        let id = app.annotations.iter().next().expect("annotation").id;
        let uri = format!(
            "pd-float://annotation/{}/{}",
            PageIndex::FIRST.get(),
            id.get()
        );
        let ctx = egui::Context::default();

        app.markdown.rendered = true;
        ctx.output_mut(|output| {
            output
                .commands
                .push(egui::OutputCommand::OpenUrl(egui::OpenUrl::same_tab(&uri)));
        });
        app.consume_markdown_links(&ctx);
        assert_eq!(
            app.floats.last().map(|float| float.owner),
            Some(FloatOwner::Annotation(id)),
            "pd-float 链接应被内部分发"
        );

        app.markdown.rendered = true;
        ctx.output_mut(|output| {
            output
                .commands
                .push(egui::OutputCommand::OpenUrl(egui::OpenUrl::same_tab(
                    "https://example.com",
                )));
        });
        app.consume_markdown_links(&ctx);
        assert!(app.message.contains("外部链接"), "{}", app.message);
    }

    #[test]
    fn broken_and_ambiguous_links_only_report() {
        let mut app = app();
        app.follow_float_link("pd-float://annotation/9999");
        assert!(app.message.contains("断链"), "{}", app.message);
        app.follow_float_link("pd-float://preview/3/9999");
        assert!(app.message.contains("断链"), "{}", app.message);

        // 同一个 id 出现在两页 → 旧写法必须提示歧义，不打开错误的浮窗
        app.custom_previews.push(text_preview(PageIndex::new(0), 5));
        app.custom_previews.push(text_preview(PageIndex::new(1), 5));
        app.follow_float_link("pd-float://preview/5");
        assert!(app.message.contains("不明确"), "{}", app.message);
        assert!(app.floats.is_empty());
    }
}

mod external_annotation_tests {
    use super::super::*;
    use pdfcore::annotation::AnnotationKind;

    fn app_with_external() -> (
        PdfReaderApp,
        AnnotationId,
        pdfcore::annotation::PdfObjectRef,
    ) {
        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().expect("一条").id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 42, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
            annotation.body = Some("旧".to_string());
        }
        app.external_ids.insert(id);
        app.opened_pdf_refs.insert(reference);
        app.opened_pdf_bodies
            .insert(reference, Some("旧".to_string()));
        app.saved_pdf_revision = app.history.current_pdf_revision();
        app.refresh_dirty();
        (app, id, reference)
    }

    #[test]
    fn undoing_own_delete_after_save_requeues_it_as_new() {
        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 37, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
        }
        app.opened_pdf_refs.insert(reference);
        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        // 模拟删除已经写入 PDF：当前文件里没有这个对象
        app.opened_pdf_refs.clear();
        app.undo_edit();
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.pdf_ref),
            None,
            "自有注释的旧 ref 失效后也要按新注释写回"
        );
    }

    #[test]
    fn own_pdf_annotation_body_update_is_a_ref_update() {
        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        assert!(app.add_annotation(
            PageIndex::FIRST,
            AnnotationKind::Highlight {
                quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
            },
        ));
        let id = app.annotations.iter().next().unwrap().id;
        let reference = pdfcore::annotation::PdfObjectRef { num: 37, gen: 0 };
        if let Some(annotation) = app.annotations.get_mut(id) {
            annotation.pdf_ref = Some(reference);
            annotation.body = Some("旧".to_string());
        }
        app.opened_pdf_refs.insert(reference);
        app.opened_pdf_bodies
            .insert(reference, Some("旧".to_string()));
        app.saved_pdf_revision = app.history.current_pdf_revision();
        app.refresh_dirty();
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: Some("旧".to_string()),
                after: Some("新".to_string()),
            },
            true,
        );
        let (updates, deletes) = app.pdf_ref_changes();
        assert_eq!(updates.len(), 1, "自有注释改正文也要走对象级更新");
        assert_eq!(updates[0].reference, reference);
        assert!(deletes.is_empty());
    }

    #[test]
    fn undoing_external_delete_after_save_requeues_it_as_new() {
        let (mut app, id, _reference) = app_with_external();
        // 模拟“删除已经写入 PDF”：当前 PDF 里已经没有这个对象
        app.opened_pdf_refs.clear();
        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        app.undo_edit();
        assert!(
            !app.external_ids.contains(&id),
            "恢复的注释不能继续当外部对象"
        );
        assert_eq!(
            app.annotations.get(id).and_then(|a| a.pdf_ref),
            None,
            "要作为新注释重新写回"
        );
    }

    #[test]
    fn marked_ids_are_reserved_before_external_annotations() {
        use backend_pdfium::pdf_index::{PageAnnot, PdfObjectRef};
        use backend_pdfium::readback::{ReadAnnotation, Readback};

        let mut app = PdfReaderApp::new_headless(
            crate::clock::Clock::system(),
            crate::diag::diagnostics_for_tests(),
            None,
        );
        let annotation = |id: Option<u64>, body: &str, reference: PdfObjectRef| {
            (
                ReadAnnotation {
                    page: PageIndex::FIRST,
                    id,
                    kind: AnnotationKind::Highlight {
                        quads: vec![Rect::new(10.0, 20.0, 30.0, 8.0)],
                    },
                    color: [255, 255, 0],
                    opacity: 0.35,
                    body: Some(body.to_string()),
                    author: None,
                    created: None,
                    modified: None,
                    ink_strokes: Vec::new(),
                },
                PageAnnot {
                    reference: Some(reference),
                    subtype: Some(9),
                    popup: None,
                    hidden: false,
                },
            )
        };
        let entries = [
            annotation(None, "外1", PdfObjectRef { num: 11, gen: 0 }),
            annotation(None, "外2", PdfObjectRef { num: 12, gen: 0 }),
            annotation(Some(1), "自有", PdfObjectRef { num: 13, gen: 0 }),
        ];
        let readback = Readback {
            annotations: entries.iter().map(|(item, _)| item.clone()).collect(),
            ..Readback::default()
        };
        let refs = vec![entries.iter().map(|(_, entry)| *entry).collect()];
        let _ = app.merge_readback(&readback, &refs);
        assert_eq!(app.annotations.len(), 3, "三条都要存在，不能被 id 覆盖");
        assert_eq!(app.external_ids.len(), 2, "只有无标记的两条是外部");
        assert_eq!(
            app.annotations
                .get(AnnotationId(1))
                .and_then(|a| a.body.as_deref()),
            Some("自有")
        );
    }

    #[test]
    fn external_body_edit_and_delete_become_ref_changes() {
        let (mut app, id, reference) = app_with_external();
        app.apply_edit(
            AppEdit::SetBody {
                id,
                page: PageIndex::FIRST,
                before: Some("旧".to_string()),
                after: Some("新".to_string()),
            },
            true,
        );
        let (updates, deletes) = app.pdf_ref_changes();
        assert_eq!(updates.len(), 1, "正文变化要按对象号更新");
        assert_eq!(updates[0].reference, reference);
        assert_eq!(updates[0].body.as_deref(), Some("新"));
        assert!(deletes.is_empty());

        let annotation = app.annotations.get(id).cloned().expect("标注");
        app.apply_edit(AppEdit::DeleteAnnotation { annotation }, true);
        let (_, deletes) = app.pdf_ref_changes();
        assert_eq!(deletes, vec![reference], "删除要按对象号删");
        assert!(app.pdf_dirty, "删除/改正文都要写 PDF");
    }
}
