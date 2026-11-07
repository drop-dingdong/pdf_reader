//! `PdfReaderApp` 的运行时状态组：诊断、注入时钟与“上一帧观察值”。
//!
//! 这些字段不参与持久化，也不决定文档内容；把它们收在一组，避免继续散落在
//! `PdfReaderApp` 的平面字段里。热点字段（session/floats/annotations 等）仍留在顶层。

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
