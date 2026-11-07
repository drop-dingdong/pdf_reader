//! `PdfReaderApp` 的状态组：运行时诊断/时钟、保存/关闭流程。
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
