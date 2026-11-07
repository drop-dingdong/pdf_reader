//! `PdfReaderApp` 的状态组：运行时诊断/时钟、保存/关闭流程、Markdown/数学/图片缓存。
//!
//! 这些字段不参与持久化，也不决定文档内容；把它们收在明确的组里，避免继续散落在
//! `PdfReaderApp` 的平面字段中。热点字段（session/floats/annotations 等）仍留在顶层。

use super::*;

/// 运行时诊断/时间状态。
pub(super) struct RuntimeState {
    /// 诊断输出（环境变量 `PDF_READER_DIAGNOSE=1` 开启）。
    pub(super) diag: crate::diag::Diagnostics,
    /// 行为性截止时间用的时钟（真实时间；回放器注入可推进的时钟）。
    pub(super) clock: crate::clock::Clock,
    /// 进程/回放启动时刻；时序诊断相对于它。
    pub(super) started_at: std::time::Instant,
    /// 上次诊断输出的时间（限流到每秒一次）。
    pub(super) last_report: std::time::Instant,
    /// 本帧累计的像素转换/上传耗时。
    pub(super) last_convert_ms: f64,
    /// 本帧累计的纹理上传耗时。
    pub(super) last_upload_ms: f64,
    /// 缩放/旋转后的静默截止时间：此前不提交新渲染。
    pub(super) preview_until: Option<std::time::Instant>,
    /// 上一帧观察到的缩放。
    pub(super) last_zoom: f32,
    /// 上一帧观察到的旋转。
    pub(super) last_rotation: Rotation,
    /// 本次拖动开始时刻（拖动结束清空）。
    pub(super) drag_started_at: Option<std::time::Instant>,
    /// 上一次“拖动位移生效”的诊断时刻。
    pub(super) last_pan_at: Option<std::time::Instant>,
    /// 上一次记录的可见页集。
    pub(super) last_visible: Vec<PageIndex>,
    /// 是否已经提示过“当前视口纹理需求超出预算”。
    pub(super) budget_warned: bool,
    /// 上一帧的保留统计（A/B/C 各占多少字节）。
    pub(super) last_retain: Desired,
}

impl RuntimeState {
    /// 用注入的时钟/诊断构造；启动时刻与上次报告时刻都取注入时钟的当前值。
    pub(super) fn new(clock: crate::clock::Clock, diag: crate::diag::Diagnostics) -> Self {
        let boot = clock.now();
        Self {
            diag,
            clock,
            started_at: boot,
            last_report: boot,
            last_convert_ms: 0.0,
            last_upload_ms: 0.0,
            preview_until: None,
            last_zoom: 1.0,
            last_rotation: Rotation::None,
            drag_started_at: None,
            last_pan_at: None,
            last_visible: Vec::new(),
            budget_warned: false,
            last_retain: Desired::default(),
        }
    }
}

/// 保存/关闭流程的对话框与一次性状态。
///
/// 这些字段都只在“用户发起保存/整理/关闭”到“动作完成或取消”之间有意义；
/// 按组放在这里，避免和文档内容/浮窗状态混在一起。
pub(super) struct SaveFlowState {
    /// 另存为对话框中的目标路径；`None` = 未打开。
    pub(super) save_as: Option<String>,
    /// 另存为对话框的错误提示。
    pub(super) save_as_error: Option<String>,
    /// 另存为：同时写原 PDF 的数据文件。
    pub(super) save_as_write_original: bool,
    /// 另存为：写新 PDF 的数据文件。
    pub(super) save_as_write_target: bool,
    /// 另存为：目标已存在时是否覆盖。
    pub(super) save_as_overwrite: bool,
    /// 整理副本对话框里的目标路径；`None` = 未打开。
    pub(super) compact_dialog: Option<String>,
    /// 整理副本的错误提示。
    pub(super) compact_error: Option<String>,
    /// 是否正在等“原地写入 PDF”的确认。
    pub(super) pending_in_place: bool,
    /// 保存确认框里的错误。
    pub(super) in_place_error: Option<String>,
    /// 关闭/切换文件前的未保存提示。
    pub(super) pending_action: Option<PendingAction>,
    /// 正文编辑尚未保存时挂起的关闭/切换动作。
    pub(super) pending_edit_action: Option<EditDiscardAction>,
    /// 提示里的错误（保存失败/冲突等）。
    pub(super) pending_action_error: Option<String>,
    /// 用户已经决定关闭：下一次 close_requested 不再拦截。
    pub(super) closing_after_decision: bool,
    /// Ctrl+S 在 egui 帧内无法安全借 session；延后到 session 放回后执行。
    pub(super) pending_save: bool,
}

impl Default for SaveFlowState {
    fn default() -> Self {
        Self {
            save_as: None,
            save_as_error: None,
            save_as_write_original: true,
            save_as_write_target: true,
            save_as_overwrite: false,
            compact_dialog: None,
            compact_error: None,
            pending_in_place: false,
            in_place_error: None,
            pending_action: None,
            pending_edit_action: None,
            pending_action_error: None,
            closing_after_decision: false,
            pending_save: false,
        }
    }
}

/// Markdown / 数学 / 内部图片的会话内缓存。
///
/// 这些数据都能从正文/图片记录重建，不持久化；放在一组是为了让“按需重建”
/// 的生命周期清晰，并避免和文档/浮窗字段混在一起。
pub(super) struct MarkdownState {
    /// hover Markdown 专用的解析/标题缓存。
    pub(super) hover_cache: egui_commonmark::CommonMarkCache,
    /// 当前 hover Markdown 对应的 preview 下标；变化时清缓存。
    pub(super) hover_index: Option<usize>,
    /// 每个浮窗独立的 `egui_commonmark` 排版/图片缓存。
    pub(super) caches: std::collections::HashMap<u64, egui_commonmark::CommonMarkCache>,
    /// 每个 Markdown 浮窗上一帧的滚动偏移/内容尺寸。
    pub(super) viewports: std::collections::HashMap<u64, (egui::Vec2, egui::Vec2)>,
    /// 每个浮窗的 Markdown 预处理缓存（按源文本哈希失效）。
    pub(super) prepared: std::collections::HashMap<u64, PreparedMarkdown>,
    /// 本帧是否有 Markdown 浮窗渲染过；用于把外部链接改成状态栏提示。
    pub(super) rendered: bool,
    /// LaTeX 公式的 PNG 纹理缓存与内嵌字体（全局共用）。
    pub(super) math_cache: std::rc::Rc<std::cell::RefCell<crate::math::MathCache>>,
    /// 保存正文后，下一帧把公式错误合并到“已更新内容”提示里。
    pub(super) math_recheck_pending: bool,
    /// Markdown 内部图片的 PNG 字节缓存。
    pub(super) images: crate::image_store::ImageStore,
}

impl Default for MarkdownState {
    fn default() -> Self {
        Self {
            hover_cache: egui_commonmark::CommonMarkCache::default(),
            hover_index: None,
            caches: std::collections::HashMap::new(),
            viewports: std::collections::HashMap::new(),
            prepared: std::collections::HashMap::new(),
            rendered: false,
            math_cache: std::rc::Rc::new(std::cell::RefCell::new(crate::math::MathCache::new())),
            math_recheck_pending: false,
            images: crate::image_store::ImageStore::default(),
        }
    }
}

/// 注释与 PDF 对象号之间的会话内簿记。
///
/// 这组字段只在“打开/读回 → 编辑 → 写回/重开”之间有意义；集中后，
/// `merge_readback`、`pdf_ref_changes` 和重开逻辑的输入边界更清楚。
#[derive(Default)]
pub(super) struct AnnotationPdfState {
    /// 本会话里“来自 PDF、但不属于 overlay”的注释 id。
    pub(super) external_ids: std::collections::HashSet<AnnotationId>,
    /// 打开时 PDF 里所有支持注释的对象号（用于计算待删除集合）。
    pub(super) opened_pdf_refs: std::collections::BTreeSet<pdfcore::annotation::PdfObjectRef>,
    /// 打开时每个对象号的正文（用于外部注释正文差分）。
    pub(super) opened_pdf_bodies:
        std::collections::HashMap<pdfcore::annotation::PdfObjectRef, Option<String>>,
    /// 打开时对象号 → 标记 id（撤销/重做后快照可能暂时没有 pdf_ref）。
    pub(super) opened_id_by_ref:
        std::collections::HashMap<pdfcore::annotation::PdfObjectRef, AnnotationId>,
    /// 外部内联注释：id → (页号, 该页可见支持注释中的位置)。
    pub(super) inline_refs: std::collections::HashMap<AnnotationId, (PageIndex, usize)>,
    /// 打开时内联注释的正文，用于判断是否需要页级正文 patch。
    pub(super) opened_inline_bodies: std::collections::HashMap<(PageIndex, usize), Option<String>>,
    /// 外部注释对象号 → 稳定本地 id（写回重开时复用，撤销栈才能对上）。
    pub(super) pdf_ref_ids:
        std::collections::HashMap<pdfcore::annotation::PdfObjectRef, AnnotationId>,
    /// 上次打开时 PDF 里带 `pd-fr:` 标记的注释数（决定空 overlay 能否写回/导出）。
    pub(super) pdf_marked_annotations: usize,
}
