//! 回放器：用脚本驱动**真实的 `App::ui`**（无窗口），逐帧采集并检查不变量（D-103）。
//!
//! ## 为什么需要它
//! 最近几轮真机回归（白块 D-97/98/99、预览生成 D-101）全部出在"绘制决策 → 实际绘制"
//! 这条缝上：`view.rs` 的纯函数有单测，但"按这个决策把三层画出来"只能靠人眼。
//! 这个模块把那块缝补上：
//!
//! ```text
//!   script.json ──▶ 逐动作驱动 App::ui（egui headless）──▶ 逐帧采集
//!                                                          ├─ 画法快照（PlanStamp）
//!                                                          ├─ 真正画出的纹理 quad（→ 页坐标格子）
//!                                                          ├─ 帧耗时 / 上传字节 / 纹理条目
//!                                                          └─ 诊断文本（Diag 缓冲）
//!                     ──▶ 断言：① 已清晰区域不得变白 ② 一次波段切换只翻一次
//!                               ③ 预览尺寸 == 区域 × 分辨率
//!                     ──▶ JSONL（每个动作一行，含该动作从开始到稳定之间的全部输出）
//! ```
//!
//! ## 两种用法
//! * **确定性**：注入 `Clock::manual()`，每帧推进固定时长（默认 20ms）→ 缩放预览窗口、
//!   就绪兜底、静默窗口都按"帧"精确复现；
//! * **性能**：同一份脚本在真机上跑（`cargo run -p app -- --replay …`），采集
//!   帧耗时/上传字节/内存，作为本机基线（见 `docs/current/testing.md`）。
//!
//! ## 边界
//! * 只驱动"主视图渲染"相关动作（打开/缩放/滚动/跳页/调整视口/悬停链接）；
//!   浮窗、侧栏、编辑等交互不在范围内。
//! * 没有 GPU：测不到纹理上传的真实开销；`textures_delta` 只反映"打算上传多少字节"。

pub mod baseline;
pub mod samples;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use pdfcore::prelude::*;

use super::{CustomKind, PdfReaderApp};
use crate::clock::Clock;
use crate::diag::Diagnostics;
use crate::json::{Json, JsonLookup};
use crate::view::{PlanStamp, Preview, READY_TIMEOUT_MS};

/// 页坐标格子数（每轴）。与 `TileGrid` 的 32×32 上限对齐到 16×16，够粗也够细。
const CELLS: usize = 16;

/// 脚本里的一个动作。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// 调整视口。
    Resize {
        /// 视口宽（内容像素）。
        width: f32,
        /// 视口高（内容像素）。
        height: f32,
    },
    /// 设置显示比例（1.0 = 100%）。
    Zoom {
        /// 显示比例。
        value: f32,
    },
    /// 相对滚动。
    ScrollBy {
        /// 横向位移（正 = 内容右移，即滚动量减小）。
        dx: f32,
        /// 纵向位移。
        dy: f32,
    },
    /// 绝对滚动。
    SetScroll {
        /// 内容空间横坐标。
        x: f32,
        /// 内容空间纵坐标。
        y: f32,
    },
    /// 跳页（0 起）。
    GoToPage {
        /// 页号（0 起）。
        page: u32,
    },
    /// 把指针移到某页第 `link` 个链接上（触发悬停预览）。
    HoverLink {
        /// 页号（0 起）。
        page: u32,
        /// 该页链接下标。
        link: usize,
    },
    /// 唤出第 `index` 条自定义图片 preview 的浮窗（触发"按记录分辨率生成预览图"）。
    OpenPreview {
        /// 自定义 preview 的下标（来自 sidecar 的加载顺序）。
        index: usize,
    },
    /// 在页面上点击一次（**页面空间**坐标，左下原点）。
    ///
    /// 用页面空间而不是屏幕坐标：脚本因此与缩放、窗口尺寸、页边距无关
    /// （S0 的目的：让注释等"点/拖"交互可以被自动化复现）。
    Click {
        /// 页号（0 起）。
        page: u32,
        /// 页内 x（PDF 页面单位）。
        x: f32,
        /// 页内 y（PDF 页面单位，左下原点）。
        y: f32,
    },
    /// 在页面上拖拽（**页面空间**坐标）；按下 → `steps` 次插值移动 → 抬起。
    Drag {
        /// 页号（0 起）。
        page: u32,
        /// 起点 x（页面空间）。
        from_x: f32,
        /// 起点 y（页面空间）。
        from_y: f32,
        /// 终点 x（页面空间）。
        to_x: f32,
        /// 终点 y（页面空间）。
        to_y: f32,
        /// 中间移动帧数（默认 6；越大越接近真实拖拽）。
        steps: u32,
    },
    /// 把页面上的一块矩形变成高亮标注（**页面空间**；S1 用它在回放里造标注）。
    Highlight {
        /// 页号（0 起）。
        page: u32,
        /// 区域左上角 x（页面空间）。
        x: f32,
        /// 区域左上角 y（页面空间，注意 PDF 左下原点）。
        y: f32,
        /// 区域宽。
        width: f32,
        /// 区域高。
        height: f32,
    },
    /// 保存所有有改动的文件（等价于点"保存"或 Ctrl+S）。
    Save,
    /// 指针移出画布。
    Unhover,
    /// 什么都不做，只跑若干帧（用来观察"稳定后"的状态）。
    Wait {
        /// 最多跑多少帧。
        frames: u64,
    },
}

impl Action {
    /// 脚本里的名字（JSONL 用）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Resize { .. } => "resize",
            Self::Zoom { .. } => "zoom",
            Self::ScrollBy { .. } => "scroll_by",
            Self::SetScroll { .. } => "set_scroll",
            Self::GoToPage { .. } => "go_to_page",
            Self::HoverLink { .. } => "hover_link",
            Self::OpenPreview { .. } => "open_preview",
            Self::Click { .. } => "click",
            Self::Highlight { .. } => "highlight",
            Self::Save => "save",
            Self::Drag { .. } => "drag",
            Self::Unhover => "unhover",
            Self::Wait { .. } => "wait",
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Resize { width, height } => format!("resize {width}×{height}"),
            Self::Zoom { value } => format!("zoom {value}"),
            Self::ScrollBy { dx, dy } => format!("scroll_by ({dx},{dy})"),
            Self::SetScroll { x, y } => format!("set_scroll ({x},{y})"),
            Self::GoToPage { page } => format!("go_to_page {page}"),
            Self::HoverLink { page, link } => format!("hover_link p{page} #{link}"),
            Self::OpenPreview { index } => format!("open_preview #{index}"),
            Self::Click { page, x, y } => format!("click p{page} ({x},{y})"),
            Self::Drag {
                page,
                from_x,
                from_y,
                to_x,
                to_y,
                ..
            } => {
                format!("drag p{page} ({from_x},{from_y})→({to_x},{to_y})")
            }
            Self::Highlight {
                page,
                x,
                y,
                width,
                height,
            } => {
                format!("highlight p{page} ({x},{y},{width},{height})")
            }
            Self::Save => "save".to_string(),
            Self::Unhover => "unhover".to_string(),
            Self::Wait { frames } => format!("wait {frames}"),
        }
    }
}

/// 一份回放脚本。
#[derive(Debug, Clone)]
pub struct Script {
    /// 脚本名（输出文件名与报告标题用）。
    pub name: String,
    /// 要打开的 PDF。
    pub pdf: PathBuf,
    /// 动作序列（`id` + 动作）。
    pub actions: Vec<(String, Action)>,
    /// 打开后**先**设置的显示比例（模拟"一打开就在高倍"，D-110 的关键场景）。
    pub start_zoom: Option<f32>,
}

/// 一次回放的控制参数。
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// 无窗口时的画布尺寸（逻辑点）。
    pub screen: (f32, f32),
    /// 每个动作最多跑多少帧（防死循环；**timing 模式下帧很便宜**，这里的数量级要够大）。
    pub max_frames_per_action: u64,
    /// 每个动作最多跑多少**墙钟**时间（timing 模式下才是真正的兜底）。
    ///
    /// 为什么必须有这一条：timing 模式用真实时钟，而"缩放预览窗口"是 160ms 墙钟。
    /// 若一帧只要 0.1ms，240 帧总共才 24ms —— 窗口还没过，渲染请求都还没提交，
    /// 动作却已经用光帧数（本项目的真机基线第一次录制就是这么失败的）。
    pub max_action_duration: Duration,
    /// 连续多少帧"没有在飞任务 + 纹理不变 + 画法不变"就算稳定。
    pub quiet_frames: u32,
    /// 手动时钟每帧推进的时长（确定性回放用）。
    pub advance_per_frame: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            screen: (1100.0, 900.0),
            // 帧上限只是"防死循环"的保险；真正的兜底是墙钟时长。
            // 确定性模式下每帧推进 20ms，20 帧就跨过预览窗口；timing 模式下帧可能只要 0.1ms，
            // 需要上千帧才能等到 160ms 的窗口结束 —— 所以这里给足数量。
            max_frames_per_action: 20_000,
            max_action_duration: Duration::from_secs(5),
            quiet_frames: 3,
            advance_per_frame: Duration::from_millis(20),
        }
    }
}

/// 一帧的采集结果。
#[derive(Debug, Clone)]
pub struct FrameStat {
    /// 脚本内的绝对帧号。
    pub frame: u64,
    /// 该帧的 CPU 时间（毫秒，`App::ui` 的墙钟耗时）。
    pub cpu_ms: f64,
    /// 本帧"打算上传"的字节数（`textures_delta` 的像素字节和；无 GPU，不代表真实上传耗时）。
    pub upload_bytes: u64,
    /// 纹理层条目数。
    pub textures: usize,
    /// 纹理层占用字节数（`w×h×4` 之和）。
    pub texture_bytes: u64,
    /// 在飞渲染任务数。
    pub pending: usize,
    /// 累计提交的渲染请求数（`DocumentSession::submitted_jobs`，单调递增）。
    pub submitted: u64,
    /// 进程常驻内存（KB；非 Linux 平台上为 0）。
    pub rss_kb: u64,
    /// 这一帧是否处于"缩放预览窗口"内（窗口内不提交渲染，所以画面会原地不动）。
    pub previewing: bool,
    /// 这一帧是否**真的干活**（转换/上传了像素，或画面内容变了）。
    ///
    /// 为什么要区分：timing 模式下循环是全速转的，160ms 的缩放预览窗口里会空转上千帧
    /// （每帧 ~0.08ms），它们会把 p50/p99 稀释成"什么都没干"的数字。
    /// 只有 busy 帧的耗时才代表真实工作成本。
    pub busy: bool,
    /// 每页的画法快照。
    pub plans: BTreeMap<u32, PlanStamp>,
    /// 每页当前可见的块范围 `(col0, row0, col1, row1)`。
    pub ranges: BTreeMap<u32, (u32, u32, u32, u32)>,
}

/// 一个动作的采集结果。
#[derive(Debug, Clone)]
pub struct ActionReport {
    /// 动作 id（脚本里给的）。
    pub id: String,
    /// 动作描述。
    pub action: String,
    /// 跑过的帧数。
    pub frames: u64,
    /// 是否在帧数上限内稳定下来。
    pub settled: bool,
    /// 该动作**从开始到稳定**之间产生的诊断文本。
    pub diag: Vec<String>,
    /// 逐帧数据。
    pub frames_data: Vec<FrameStat>,
    /// 该动作触发的断言违规（空 = 通过）。
    pub violations: Vec<String>,
    /// 不算违规、但值得记录的观察（例如走了就绪兜底：那时确实有块没到）。
    pub notes: Vec<String>,
    /// 该动作结束时各自定义 preview 的**实际纹理尺寸**（下标, 宽, 高）。
    pub previews: Vec<(usize, usize, usize)>,
    /// 该动作**新增**的渲染请求数（结构指标，与硬件无关）。
    pub submitted: u64,
    /// 该动作期间纹理层条目峰值。
    pub textures_peak: usize,
    /// 该动作期间纹理层**字节**峰值（结构指标：与硬件无关）。
    pub texture_bytes_peak: u64,
    /// 该动作期间"打算上传"的字节总数。
    pub upload_bytes: u64,
    /// 该动作实际耗时（墙钟毫秒，含等预览窗口的时间）。
    pub elapsed_ms: f64,
    /// 该动作里"真的干活"的帧数。
    pub busy_frames: u64,
    /// 忙帧耗时（毫秒）：p50 / p95 / p99 / max。
    pub busy_cpu_ms: (f64, f64, f64, f64),
    /// 忙帧的 CPU 时间合计（毫秒）—— 这个动作一共烧了多少 CPU。
    pub busy_cpu_ms_total: f64,
    /// 动作结束时当前页（0 起）。用于断言"点击链接跳页"这类由交互触发的导航。
    pub page_after: u32,
    /// 动作结束时的滚动偏移（内容空间像素）。用于断言拖拽平移。
    pub scroll_after: (f32, f32),
    /// 动作结束时的标注数量。
    pub annotations: usize,
    /// 最后一条标注的页面空间包围盒（`x,y,w,h`）—— 用于断言"几何与缩放无关"。
    pub annotation_bounds: Option<(f32, f32, f32, f32)>,
    /// 动作结束时的浮窗数量（断言"重复点击不重复开窗"）。
    pub floats: usize,
}

/// 整次回放的报告。
#[derive(Debug, Clone)]
pub struct Report {
    /// 脚本名。
    pub script: String,
    /// 动作结果。
    pub actions: Vec<ActionReport>,
    /// 汇总的违规。
    pub violations: Vec<String>,
}

impl Report {
    /// 总帧数。
    pub fn frames(&self) -> u64 {
        self.actions.iter().map(|action| action.frames).sum()
    }

    /// 进程常驻内存峰值（KB；非 Linux 平台为 0）。
    pub fn rss_peak_kb(&self) -> u64 {
        self.actions
            .iter()
            .flat_map(|action| action.frames_data.iter())
            .map(|frame| frame.rss_kb)
            .max()
            .unwrap_or(0)
    }

    /// 是否全部通过。
    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }

    /// **忙帧**耗时分位数（毫秒）：`(p50, p95, p99, max)`。
    ///
    /// 只统计"真的干活"的帧（见 [`FrameStat::busy`]）：timing 模式下上千帧空转会把
    /// 全帧分位数稀释成无意义的 0.0x ms。
    pub fn busy_frame_percentiles(&self) -> (f64, f64, f64, f64) {
        let values: Vec<f64> = self
            .actions
            .iter()
            .flat_map(|action| action.frames_data.iter())
            .filter(|frame| frame.busy)
            .map(|frame| frame.cpu_ms)
            .collect();
        percentiles(&values)
    }

    /// 全部忙帧的 CPU 时间合计（毫秒）。
    pub fn busy_cpu_ms_total(&self) -> f64 {
        self.actions
            .iter()
            .map(|action| action.busy_cpu_ms_total)
            .sum()
    }

    /// 全部动作的墙钟合计（毫秒）。
    pub fn wall_ms_total(&self) -> f64 {
        self.actions.iter().map(|action| action.elapsed_ms).sum()
    }

    /// 忙帧数合计。
    pub fn busy_frames(&self) -> u64 {
        self.actions.iter().map(|action| action.busy_frames).sum()
    }

    /// 纹理层占用字节峰值（整段脚本）。
    pub fn texture_bytes_peak(&self) -> u64 {
        self.actions
            .iter()
            .map(|action| action.texture_bytes_peak)
            .max()
            .unwrap_or(0)
    }

    /// 人读摘要。
    pub fn summary(&self) -> String {
        let (p50, p95, p99, max) = self.busy_frame_percentiles();
        let uploads: u64 = self
            .actions
            .iter()
            .flat_map(|action| action.frames_data.iter())
            .map(|frame| frame.upload_bytes)
            .sum();
        let submitted: u64 = self.actions.iter().map(|action| action.submitted).sum();
        format!(
            "脚本={} 动作={} 帧={}(忙帧 {}) 提交={} 墙钟={:.0}ms CPU合计={:.1}ms \
             忙帧 p50={p50:.2} p95={p95:.2} p99={p99:.2} max={max:.2}ms 上传={:.1}MB \
             纹理峰值={:.1}MB 违规={}",
            self.script,
            self.actions.len(),
            self.frames(),
            self.busy_frames(),
            submitted,
            self.wall_ms_total(),
            self.busy_cpu_ms_total(),
            uploads as f64 / (1024.0 * 1024.0),
            self.texture_bytes_peak() as f64 / (1024.0 * 1024.0),
            self.violations.len()
        )
    }
}

/// 进程常驻内存（KB）。只在 Linux 上有效（读 `/proc/self/statm`），其它平台返回 0。
fn resident_kb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let Ok(text) = std::fs::read_to_string("/proc/self/statm") else {
            return 0;
        };
        let pages: u64 = text
            .split_whitespace()
            .nth(1)
            .and_then(|field| field.parse().ok())
            .unwrap_or(0);
        pages * 4
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

// ------------------------------------------------------------------ 脚本解析

/// 读取并解析一个脚本文件；`pdf` 为相对路径时按**脚本所在目录**解析。
pub fn load_script(path: &Path) -> std::result::Result<Script, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut script = parse_script(&text)?;
    if script.pdf.is_relative() {
        if let Some(parent) = path.parent() {
            script.pdf = parent.join(&script.pdf);
        }
    }
    Ok(script)
}

/// 一步到位：读脚本 → 无窗口回放 → 可选写 JSONL。
///
/// 命令行（`--replay`）与测试都走这里，保证"你跑的就是测试跑的"。
pub fn run_script_file(script_path: &Path, out: Option<&Path>) -> std::io::Result<Report> {
    run_script_file_with(script_path, out, false)
}

/// 同上，但可以选 **timing 模式**：用真实时钟（不按帧推进），测出来的帧耗时反映真实节奏。
///
/// * 断言用**确定性**模式（默认，手动时钟）：同样一段脚本在任何机器上走同一条时间线；
/// * 性能基线用 **timing** 模式（`--timing`）：真实时钟 + 真实渲染线程，才代表本机手感。
pub fn run_script_file_with(
    script_path: &Path,
    out: Option<&Path>,
    timing: bool,
) -> std::io::Result<Report> {
    let script = load_script(script_path)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut options = RunOptions::default();
    let clock = if timing {
        // 真实时钟：`advance` 是空操作，帧之间按真实时间流逝
        options.advance_per_frame = Duration::ZERO;
        Clock::system()
    } else {
        Clock::manual()
    };
    let mut app = PdfReaderApp::new_headless(clock.clone(), Diagnostics::capturing(), None);
    let report = run(&mut app, &script, &clock, &options);
    if let Some(out) = out {
        write_jsonl(&report, out)?;
    }
    Ok(report)
}

/// 解析脚本文本。
///
/// 格式（JSON）：
/// ```json
/// { "name": "tiling_800", "pdf": "samples/a4_text.pdf",
///   "actions": [ { "id": "zoom_100", "op": "zoom", "value": 1.0 } ] }
/// ```
/// `pdf` 为相对路径时按**脚本文件所在目录**解析。
pub fn parse_script(text: &str) -> std::result::Result<Script, String> {
    let root = Json::parse(text)?;
    let root = root
        .as_object()
        .ok_or_else(|| "脚本根节点不是对象".to_string())?;
    let name = root
        .field("name")
        .and_then(|v| v.as_str())
        .unwrap_or("script")
        .to_string();
    let pdf = root
        .field("pdf")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "脚本缺 pdf".to_string())?;
    let list = root
        .field("actions")
        .ok_or_else(|| "脚本缺 actions".to_string())?
        .as_array()
        .ok_or_else(|| "actions 不是数组".to_string())?;
    let start_zoom = root.field("start_zoom").and_then(|value| value.as_number());
    let mut actions = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("actions[{index}] 不是对象"))?;
        let id = object
            .field("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("action_{index}"));
        let op = object
            .field("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "动作缺 op".to_string())?;
        let number = |key: &str| -> std::result::Result<f32, String> {
            object
                .field(key)
                .and_then(|v| v.as_number())
                .ok_or_else(|| format!("{id}: 缺数字字段 {key}"))
        };
        let action = match op {
            "resize" => Action::Resize {
                width: number("width")?,
                height: number("height")?,
            },
            "zoom" => Action::Zoom {
                value: number("value")?,
            },
            "scroll_by" => Action::ScrollBy {
                dx: number("dx")?,
                dy: number("dy")?,
            },
            "set_scroll" => Action::SetScroll {
                x: number("x")?,
                y: number("y")?,
            },
            "go_to_page" => Action::GoToPage {
                page: number("page")? as u32,
            },
            "hover_link" => Action::HoverLink {
                page: number("page")? as u32,
                link: number("link").unwrap_or(0.0) as usize,
            },
            "open_preview" => Action::OpenPreview {
                index: number("index").unwrap_or(0.0) as usize,
            },
            "click" => Action::Click {
                page: number("page")? as u32,
                x: number("x")?,
                y: number("y")?,
            },
            "drag" => Action::Drag {
                page: number("page")? as u32,
                from_x: number("from_x")?,
                from_y: number("from_y")?,
                to_x: number("to_x")?,
                to_y: number("to_y")?,
                steps: number("steps").unwrap_or(6.0) as u32,
            },
            "highlight" => Action::Highlight {
                page: number("page")? as u32,
                x: number("x")?,
                y: number("y")?,
                width: number("width")?,
                height: number("height")?,
            },
            "save" => Action::Save,
            "unhover" => Action::Unhover,
            "wait" => Action::Wait {
                frames: number("frames").unwrap_or(3.0) as u64,
            },
            other => return Err(format!("{id}: 未知动作 {other}")),
        };
        actions.push((id, action));
    }
    Ok(Script {
        name,
        pdf: PathBuf::from(pdf),
        actions,
        start_zoom,
    })
}

/// 分位数：返回 `(p50, p95, p99, max)`（空输入返回全 0）。
fn percentiles(values: &[f64]) -> (f64, f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |p: f64| -> f64 {
        let index = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    };
    (pick(50.0), pick(95.0), pick(99.0), *sorted.last().unwrap())
}

// ------------------------------------------------------------------ 画法追踪

/// 某页"可见区域里被盖住的格子比例"（`None` = 看不见或页面无效）。
fn visible_covered_ratio(
    placement: &PagePlacement,
    scroll: Vec2,
    canvas_width: f32,
    canvas_height: f32,
    covered: &[bool],
) -> Option<f64> {
    let size = placement.page_size;
    if !(size.width > 0.0 && size.height > 0.0) {
        return None;
    }
    let page_rect = egui::Rect::from_min_size(
        egui::pos2(placement.rect.x, placement.rect.y),
        egui::vec2(placement.rect.width, placement.rect.height),
    );
    let view_rect = egui::Rect::from_min_size(
        egui::pos2(scroll.x, scroll.y),
        egui::vec2(canvas_width, canvas_height),
    );
    let visible = page_rect.intersect(view_rect);
    if !visible.is_positive() {
        return None;
    }
    let (mut expected, mut missing) = (0usize, 0usize);
    for row in 0..CELLS {
        for col in 0..CELLS {
            let cx = (col as f32 + 0.5) / CELLS as f32 * size.width;
            let cy = (row as f32 + 0.5) / CELLS as f32 * size.height;
            let content = placement.page_to_content(Point::new(cx, cy));
            if content.x < visible.min.x
                || content.x > visible.max.x
                || content.y < visible.min.y
                || content.y > visible.max.y
            {
                continue;
            }
            expected += 1;
            if !covered.get(row * CELLS + col).copied().unwrap_or(false) {
                missing += 1;
            }
        }
    }
    if expected == 0 {
        return None;
    }
    Some((expected - missing) as f64 / expected as f64)
}

/// 每个 `(页, 波段)` 的一次**连续逗留**里的画法序列，检查"只翻一次、不得翻回去"。
///
/// 注意"逗留"：同一页可能先看 4096、再回 1600、再回到 4096（连续缩放/来回缩放），
/// 每次重新进入某波段都应重新走一遍"预览 → 瓦片"，因此换波段时要清掉该页的历史。
#[derive(Default)]
struct FlipTracker {
    history: HashMap<(u32, u32), Vec<PlanStamp>>,
    /// 每页当前所在波段（用来发现"离开又回来"）。
    band: HashMap<u32, u32>,
}

/// 一帧的判定结果。
struct FlipOutcome {
    /// 违规说明。
    violations: Vec<String>,
    /// 这一帧是否**刚刚**从预览翻到瓦片（断言 ④ 只在这个时刻检查就绪度）。
    became_tiles: bool,
}

impl FlipTracker {
    fn observe(&mut self, page: u32, stamp: PlanStamp, frame: u64) -> FlipOutcome {
        // 换波段（含离开后又回来）→ 该页的历史作废，重新开始一段
        if self.band.insert(page, stamp.band) != Some(stamp.band) {
            self.history.retain(|(p, _), _| *p != page);
        }
        let history = self.history.entry((page, stamp.band)).or_default();
        if history.last() == Some(&stamp) {
            return FlipOutcome {
                violations: Vec::new(),
                became_tiles: false,
            };
        }
        let was_tiles = history
            .last()
            .is_some_and(|last| matches!(last.plan, crate::view::PageDraw::Tiles));
        let now_tiles = matches!(stamp.plan, crate::view::PageDraw::Tiles);
        let flipped_before = history
            .iter()
            .any(|item| matches!(item.plan, crate::view::PageDraw::Tiles));
        let mut violations = Vec::new();
        if was_tiles && !now_tiles {
            violations.push(format!(
                "第 {frame} 帧：页 {page} 波段 {} 从瓦片退回了预览（{} → {}）",
                stamp.band,
                history.last().map_or("—", |last| last.plan.label()),
                stamp.plan.label()
            ));
        } else if !was_tiles && now_tiles && flipped_before {
            violations.push(format!(
                "第 {frame} 帧：页 {page} 波段 {} 第二次翻到瓦片（同一波段只允许翻一次）",
                stamp.band
            ));
        }
        history.push(stamp);
        FlipOutcome {
            violations,
            became_tiles: now_tiles && !was_tiles,
        }
    }
}

/// 断言 ④：**翻到瓦片的那一刻**可见范围必须全就绪，除非已等满 [`READY_TIMEOUT_MS`]。
///
/// 只检查"翻页那一刻"：翻过去之后（D-104 的粘性模式）滚出新块时，缺的块由底料补，
/// 已清晰的部分保持清晰 —— 那是设计允许的，不该报。
fn premature_switch(page: u32, stamp: PlanStamp, frame: u64) -> Option<String> {
    if !matches!(stamp.plan, crate::view::PageDraw::Tiles) {
        return None;
    }
    if stamp.preview == Preview::None || stamp.total == 0 || stamp.ready >= stamp.total {
        return None;
    }
    if stamp.waited_ms >= READY_TIMEOUT_MS {
        return None;
    }
    Some(format!(
        "第 {frame} 帧：页 {page} 波段 {} 提前切到瓦片（{}/{} 块，等待 {}ms < {READY_TIMEOUT_MS}ms）",
        stamp.band, stamp.ready, stamp.total, stamp.waited_ms
    ))
}

/// 逐帧观察"已清晰 / 已被覆盖"的页坐标格子，检查断言 ①。
#[derive(Default)]
struct CoverageTracker {
    /// `(页, 波段)` → 曾经清晰过的格子。
    sharp_ever: HashMap<(u32, u32), Vec<bool>>,
    /// 最近一帧每页"被任一图层覆盖"的格子。
    covered_last: HashMap<u32, Vec<bool>>,
    /// 曾经"整页可见区域都被盖住"的页：一旦盖满过，就不允许再出现大片空白（D-110 的判据）。
    fully_covered_ever: std::collections::HashSet<u32>,
    /// 本次动作里已经报过"露白"的页（避免每帧刷屏）。
    reported_gap: std::collections::HashSet<u32>,
    /// `(页, 波段)` → 当前的清晰格子。
    sharp_now: HashMap<(u32, u32), Vec<bool>>,
    /// 上一帧每页是否有可见（有 quad）。
    visible_pages: Vec<u32>,
    /// 违规描述。
    violations: Vec<String>,
}

impl CoverageTracker {
    fn reset_band(&mut self, page: u32, band: u32) {
        // 波段变化：该页其它波段的记录不再参与比较（旧内容已作废）
        self.sharp_now.retain(|(p, _), _| *p != page);
        self.sharp_now
            .insert((page, band), vec![false; CELLS * CELLS]);
    }

    /// 提交一帧：`quads` 是 `(页, 是否清晰, 覆盖格子)`。
    fn observe(&mut self, frame: u64, page_ids: &[u32], quads: &[(u32, bool, Vec<usize>)]) {
        for page in page_ids {
            let covered: Vec<usize> = quads
                .iter()
                .filter(|(p, _, _)| p == page)
                .flat_map(|(_, _, cells)| cells.iter().copied())
                .collect();
            let sharp: Vec<usize> = quads
                .iter()
                .filter(|(p, sharp, _)| p == page && *sharp)
                .flat_map(|(_, _, cells)| cells.iter().copied())
                .collect();
            // 波段只取当前页的（同一页同一帧只可能有一个波段）
            let band = self
                .sharp_now
                .keys()
                .find(|(p, _)| p == page)
                .map(|(_, band)| *band)
                .unwrap_or(0);
            let entry = self
                .sharp_now
                .entry((*page, band))
                .or_insert_with(|| vec![false; CELLS * CELLS]);
            for cell in &sharp {
                entry[*cell] = true;
            }
            let ever = self
                .sharp_ever
                .entry((*page, band))
                .or_insert_with(|| vec![false; CELLS * CELLS]);
            let covered_last = self
                .covered_last
                .entry(*page)
                .or_insert_with(|| vec![false; CELLS * CELLS]);
            covered_last.iter_mut().for_each(|cell| *cell = false);
            for cell in &covered {
                covered_last[*cell] = true;
            }
            let covered: std::collections::HashSet<usize> = covered.into_iter().collect();
            // 先收集"该报的格子"，再改 `ever`（避免同时不可变/可变借用）
            let lost: Vec<usize> = ever
                .iter()
                .enumerate()
                .filter(|(cell, sharp)| **sharp && !covered.contains(cell))
                .map(|(cell, _)| cell)
                .take(CELLS * CELLS)
                .collect();
            for cell in lost {
                self.violations.push(format!(
                    "第 {frame} 帧：页 {page} 的格子 {cell} 曾经清晰，现在没有任何图层（=纸白）"
                ));
                // 只报一次
                ever[cell] = false;
            }
            for cell in &sharp {
                ever[*cell] = true;
            }
            if !self.visible_pages.contains(page) {
                self.visible_pages.push(*page);
            }
        }
    }

    /// 逐帧检查：一旦某页"盖满过"，它的可见区域就不允许再出现大片空白。
    ///
    /// 为什么必须**逐帧**：缩小过程中空白只出现在中间几帧（新一档渲染完就没了），
    /// 只在"动作稳定后"检查会全部漏掉 —— 这正是"从高倍往回缩露白"的真实形态。
    fn check_gaps(&mut self, frame: u64, app: &PdfReaderApp) -> Vec<String> {
        let Some(session) = app.session.as_ref() else {
            return Vec::new();
        };
        let mut violations = Vec::new();
        for page in app.last_plan.keys() {
            let Some(placement) = session.placement(*page) else {
                continue;
            };
            let Some(covered) = self.covered_last.get(&page.get()) else {
                continue;
            };
            let ratio = visible_covered_ratio(
                placement,
                session.viewport().scroll,
                app.canvas_rect.width(),
                app.canvas_rect.height(),
                covered,
            );
            let Some(ratio) = ratio else { continue };
            if ratio >= 0.95 {
                self.fully_covered_ever.insert(page.get());
                continue;
            }
            // 还没盖满过（例如刚打开、第一块还没到）：允许
            if !self.fully_covered_ever.contains(&page.get()) {
                continue;
            }
            if self.reported_gap.insert(page.get()) {
                violations.push(format!(
                    "第 {frame} 帧：页 {} 的可见区域只有 {:.0}% 被图层盖住（露白）",
                    page.display_number(),
                    ratio * 100.0
                ));
            }
        }
        violations
    }
}

// ------------------------------------------------------------------ 驱动

/// 跑一份脚本。`app` 由调用方构造（`new_headless`），`script.pdf` 会被自动打开。
pub fn run(
    app: &mut crate::PdfReaderApp,
    script: &Script,
    clock: &Clock,
    options: &RunOptions,
) -> Report {
    let ctx = egui::Context::default();
    let frame = eframe::Frame::_new_kittest();
    let mut harness = Harness {
        ctx,
        frame,
        options: options.clone(),
        clock: clock.clone(),
        frame_index: 0,
        tracker: CoverageTracker::default(),
        flips: FlipTracker::default(),
        violations: Vec::new(),
        notes: Vec::new(),
        pending: Vec::new(),
        screen: options.screen,
        pending_pointer: None,
        gesture: None,
    };

    // 打开文档：直接走 app 自己的入口（与命令行/拖放同一条路径）
    app.open(&script.pdf, None);
    let mut report = Report {
        script: script.name.clone(),
        actions: Vec::new(),
        violations: Vec::new(),
    };
    if app.session.is_none() {
        report
            .violations
            .push(format!("打开失败：{}（该脚本跳过）", script.pdf.display()));
        return report;
    }
    if let Some(zoom) = script.start_zoom {
        let mut events = Vec::new();
        if let Some(session) = app.session.as_mut() {
            session.handle(ViewerCommand::SetZoom(zoom), &mut events);
        }
        app.events.extend(events);
    }

    for (id, action) in &script.actions {
        let mut action_report = ActionReport {
            id: id.clone(),
            action: action.describe(),
            frames: 0,
            settled: false,
            diag: Vec::new(),
            frames_data: Vec::new(),
            violations: Vec::new(),
            notes: Vec::new(),
            previews: Vec::new(),
            submitted: 0,
            textures_peak: 0,
            texture_bytes_peak: 0,
            upload_bytes: 0,
            elapsed_ms: 0.0,
            busy_frames: 0,
            busy_cpu_ms: (0.0, 0.0, 0.0, 0.0),
            busy_cpu_ms_total: 0.0,
            page_after: 0,
            scroll_after: (0.0, 0.0),
            annotations: 0,
            annotation_bounds: None,
            floats: 0,
        };
        harness.apply_action(app, action);
        // 每个动作开始时清空诊断缓冲：这一行只属于这个动作
        let _ = app.diag.take();

        let mut quiet = 0u32;
        let mut last_state = (usize::MAX, 0usize, BTreeMap::new());
        let mut last_visual = (usize::MAX, BTreeMap::new());
        let action_started = Instant::now();
        for _ in 0..options.max_frames_per_action {
            let stat = harness.run_frame(app);
            action_report.frames += 1;
            let state = (
                stat.textures,
                stat.pending,
                stat.plans
                    .iter()
                    .map(|(page, stamp)| (*page, stamp.stable_key()))
                    .collect::<BTreeMap<_, _>>(),
            );
            // "这一帧真的干活了吗"：传了像素，或画面（纹理条目/画法）变了
            let visual = (
                stat.textures,
                stat.plans
                    .iter()
                    .map(|(page, stamp)| (*page, stamp.stable_key()))
                    .collect::<BTreeMap<_, _>>(),
            );
            let busy = stat.upload_bytes > 0 || visual != last_visual;
            last_visual = visual;
            let mut stat = stat;
            stat.busy = busy;
            // "稳定"必须同时满足：没有在飞渲染、**不在缩放预览窗口内**、纹理与画法都不变。
            // 少了"预览窗口"这一条，动作会在窗口还没结束时就判定稳定 —— 此时渲染请求
            // 还没提交，后面自然看不到新瓦片（第一版回放就踩了这个坑）。
            let previewing = app.previewing();
            // 手势（点击/拖拽）还在逐帧投喂时**不算稳定**：否则 3 个静止帧就会让动作提前结束，
            // 按下/抬起根本来不及发出（实测：点击动作只跑了 3 帧，一次 click 都没发生）。
            let gesture_busy = harness.gesture.is_some();
            let quiet_now =
                !previewing && !gesture_busy && stat.pending == 0 && state == last_state;
            action_report.frames_data.push(stat);
            if quiet_now {
                quiet += 1;
            } else {
                quiet = 0;
                last_state = state;
            }
            if quiet >= options.quiet_frames {
                action_report.settled = true;
                break;
            }
            if let Action::Wait { frames } = action {
                if action_report.frames >= *frames {
                    action_report.settled = true;
                    break;
                }
            }
            if action_started.elapsed() > options.max_action_duration {
                break;
            }
        }
        action_report.elapsed_ms = action_started.elapsed().as_secs_f64() * 1000.0;
        action_report.diag = app.diag.take();
        // 结构指标（与硬件无关，可做硬门禁）：
        let first = action_report.frames_data.first();
        let last = action_report.frames_data.last();
        action_report.submitted = match (first, last) {
            (Some(first), Some(last)) => last.submitted.saturating_sub(first.submitted),
            _ => 0,
        };
        action_report.textures_peak = action_report
            .frames_data
            .iter()
            .map(|frame| frame.textures)
            .max()
            .unwrap_or(0);
        action_report.texture_bytes_peak = action_report
            .frames_data
            .iter()
            .map(|frame| frame.texture_bytes)
            .max()
            .unwrap_or(0);
        action_report.upload_bytes = action_report
            .frames_data
            .iter()
            .map(|frame| frame.upload_bytes)
            .sum();
        let busy: Vec<f64> = action_report
            .frames_data
            .iter()
            .filter(|frame| frame.busy)
            .map(|frame| frame.cpu_ms)
            .collect();
        action_report.busy_frames = busy.len() as u64;
        action_report.busy_cpu_ms_total = busy.iter().sum();
        action_report.busy_cpu_ms = percentiles(&busy);
        if let Some(session) = app.session.as_ref() {
            action_report.page_after = session.state().current_page.get();
            action_report.scroll_after = (session.viewport().scroll.x, session.viewport().scroll.y);
        }
        action_report.annotations = app.annotations.len();
        action_report.floats = app.floats.len();
        action_report.annotation_bounds = app
            .annotations
            .iter()
            .last()
            .and_then(|annotation| annotation.bounds())
            .map(|rect| (rect.x, rect.y, rect.width, rect.height));
        action_report.previews = app
            .custom_previews
            .iter()
            .enumerate()
            .filter_map(|(index, preview)| {
                let texture = preview.snapshot.as_ref()?;
                let size = texture.size();
                Some((index, size[0], size[1]))
            })
            .collect();
        let (violations, notes) = harness.finish_action(&action_report);
        // 断言 ③ 归到动作名下（不然所有违规都堆在报告末尾，不知道该怪谁）
        let mut violations = violations;
        violations.extend(harness.assert_preview_sizes(app));
        action_report.violations = violations;
        action_report.notes = notes;
        report
            .violations
            .extend(action_report.violations.iter().cloned());
        report.actions.push(action_report);
    }

    // 断言 ③：预览尺寸 == 区域 × 分辨率（每个动作结束后查一次，最后一次已覆盖）
    for violation in harness.assert_preview_sizes(app) {
        report.violations.push(violation);
    }
    report.violations.extend(harness.violations.clone());
    report
}

/// 一次指针手势：每帧消费一条，用真实事件流驱动 egui。
///
/// 为什么不是"一帧内按下+抬起"：egui 的点击判定依赖 `any_pressed()` / `any_released()`
/// 两个**按帧**的布尔量，同一帧里先后发生会被合并掉，点击可能根本不会被识别。
struct Gesture {
    /// `(位置, 按键)`：`Some(true)` = 按下，`Some(false)` = 抬起，`None` = 只移动。
    frames: std::collections::VecDeque<(egui::Pos2, Option<bool>)>,
    /// 手势结束后指针停在哪里（保持悬停状态，避免下一帧状态抖动）。
    rest: Option<egui::Pos2>,
}

struct Harness {
    ctx: egui::Context,
    frame: eframe::Frame,
    options: RunOptions,
    clock: Clock,
    frame_index: u64,
    tracker: CoverageTracker,
    /// 断言 ② 的记账器（按可见范围分段：滚动导致的范围变化允许"瓦片 → 预览 → 瓦片"）。
    flips: FlipTracker,
    violations: Vec<String>,
    /// 已经产生但还没归属到某个动作的观察（就绪兜底等）。
    notes: Vec<String>,
    /// 逐帧产生、尚未归属到动作的违规。
    pending: Vec<String>,
    screen: (f32, f32),
    /// 当前动作要求的指针位置（悬停链接时持续保持；`None` = 移出画布）。
    pending_pointer: Option<egui::Pos2>,
    /// 正在进行的"点击/拖拽"手势（逐帧消费事件）。
    gesture: Option<Gesture>,
}

impl Harness {
    fn apply_action(&mut self, app: &mut crate::PdfReaderApp, action: &Action) {
        // 点击/拖拽：先把页面空间坐标换算成屏幕坐标、建好手势，再让 `run_frame` 逐帧喂事件。
        // 这里必须**先**于 `app.session.as_mut()` 借用，否则拿不到 `canvas_rect`。
        if let Action::Highlight {
            page,
            x,
            y,
            width,
            height,
        } = action
        {
            app.annotate_page_rect(PageIndex::new(*page), Rect::new(*x, *y, *width, *height));
            return;
        }
        if let Action::Save = action {
            app.save_all();
            return;
        }
        if matches!(action, Action::Click { .. } | Action::Drag { .. }) {
            self.pending_pointer = None;
            self.gesture = self.gesture_for(app, action);
            return;
        }
        let Some(session) = app.session.as_mut() else {
            return;
        };
        let mut events = Vec::new();
        match action {
            Action::Resize { width, height } => {
                self.screen = (*width, *height);
                session.handle(
                    ViewerCommand::ResizeViewport(Size::new(*width, *height)),
                    &mut events,
                );
            }
            Action::Zoom { value } => session.handle(ViewerCommand::SetZoom(*value), &mut events),
            Action::ScrollBy { dx, dy } => {
                session.handle(ViewerCommand::ScrollBy(Vec2::new(*dx, *dy)), &mut events)
            }
            Action::SetScroll { x, y } => {
                session.handle(ViewerCommand::SetScroll(Vec2::new(*x, *y)), &mut events)
            }
            Action::GoToPage { page } => {
                session.handle(ViewerCommand::GoToPage(PageIndex::new(*page)), &mut events)
            }
            Action::OpenPreview { index } => {
                // 故意**不**自己合成纹理：唤出浮窗后由 app 的
                // `ensure_pending_custom_snapshots` 每帧重试，走的正是真机上的那条路径。
                let rect = egui::Rect::from_min_size(
                    app.canvas_rect.min + egui::vec2(40.0, 40.0),
                    egui::vec2(320.0, 240.0),
                );
                app.spawn_custom_float(*index, rect);
            }
            // 已在函数开头处理（不借用 session）
            Action::Click { .. }
            | Action::Drag { .. }
            | Action::Highlight { .. }
            | Action::Save => {}
            Action::HoverLink { .. } | Action::Unhover => {
                // 指针位置必须走 egui 的输入事件：在 `run_frame` 里按帧重放
                self.pending_pointer = self.pointer_for_action(app, action);
            }
            Action::Wait { .. } => {}
        }
        app.events.extend(events);
    }

    /// 把"点击/拖拽"动作变成一串逐帧的指针事件（页面空间 → 内容空间 → 屏幕空间）。
    fn gesture_for(&self, app: &crate::PdfReaderApp, action: &Action) -> Option<Gesture> {
        let session = app.session.as_ref()?;
        let canvas = app.canvas_rect;
        let scroll = session.viewport().scroll;
        let to_screen = |page: u32, point: Point| -> Option<egui::Pos2> {
            let placement = session.placement(PageIndex::new(page))?;
            let content = placement.page_to_content(point);
            Some(egui::pos2(
                canvas.min.x + content.x - scroll.x,
                canvas.min.y + content.y - scroll.y,
            ))
        };
        match action {
            Action::Click { page, x, y } => {
                let position = to_screen(*page, Point::new(*x, *y))?;
                let mut frames = std::collections::VecDeque::new();
                // **两帧**移动：egui 的 `hovered` 需要指针到达后的稳定一帧，按下发生在
                // 到达帧会判定"没有悬停在控件上"而丢掉这次点击（实测 3 帧版本点击数为 0）
                frames.push_back((position, None));
                frames.push_back((position, None));
                frames.push_back((position, Some(true)));
                frames.push_back((position, Some(false)));
                Some(Gesture {
                    frames,
                    rest: Some(position),
                })
            }
            Action::Drag {
                page,
                from_x,
                from_y,
                to_x,
                to_y,
                steps,
            } => {
                let start = to_screen(*page, Point::new(*from_x, *from_y))?;
                let end = to_screen(*page, Point::new(*to_x, *to_y))?;
                let steps = (*steps).clamp(1, 64);
                let mut frames = std::collections::VecDeque::new();
                // 同 Click：两帧移动，确保按下时控件已经 hovered（否则拖拽起点无效）
                frames.push_back((start, None));
                frames.push_back((start, None));
                frames.push_back((start, Some(true)));
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    frames.push_back((start + (end - start) * t, None));
                }
                frames.push_back((end, Some(false)));
                Some(Gesture {
                    frames,
                    rest: Some(end),
                })
            }
            _ => None,
        }
    }

    /// 指针位置（悬停链接/移出画布）—— 由动作换算成屏幕坐标，回放时每帧沿用。
    fn pointer_for_action(&self, app: &crate::PdfReaderApp, action: &Action) -> Option<egui::Pos2> {
        match action {
            Action::HoverLink { page, link } => {
                let page = PageIndex::new(*page);
                let navigation = app.navigation.as_ref()?;
                let link = navigation.links_on(page).get(*link)?;
                let placement = app.session.as_ref()?.placement(page)?;
                let content = placement.rect_to_content(link.rect);
                let scroll = app.session.as_ref()?.viewport().scroll;
                let canvas = app.canvas_rect;
                Some(egui::pos2(
                    canvas.min.x + content.x + content.width * 0.5 - scroll.x,
                    canvas.min.y + content.y + content.height * 0.5 - scroll.y,
                ))
            }
            _ => None,
        }
    }

    fn run_frame(&mut self, app: &mut crate::PdfReaderApp) -> FrameStat {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(self.screen.0, self.screen.1),
            )),
            // 无头时 egui 默认给的 2048 与真实 GPU 不符；这里按常见桌面 GPU 上报，
            // 让 `[诊断]` 里的这一项与实际运行一致（应用本身不依赖这个值做决策）。
            max_texture_side: Some(16_384),
            ..Default::default()
        };
        // 指针：手势（点击/拖拽）优先，逐帧消费；否则沿用"悬停位置"
        if let Some(gesture) = &mut self.gesture {
            if let Some((position, button)) = gesture.frames.pop_front() {
                input.events.push(egui::Event::PointerMoved(position));
                if let Some(pressed) = button {
                    input.events.push(egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    });
                }
                if gesture.frames.is_empty() {
                    self.pending_pointer = gesture.rest;
                    self.gesture = None;
                }
            }
        } else if let Some(position) = self.pending_pointer {
            input.events.push(egui::Event::PointerMoved(position));
            input.events.push(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            });
        }
        self.clock.advance(self.options.advance_per_frame);

        let started = Instant::now();
        self.ctx.begin_pass(input);
        // egui 0.37 的 CentralPanel 需要父 `Ui`；无窗口时用一个铺满屏的 Area 当根容器。
        egui::Area::new(egui::Id::unique("replay-root"))
            .fixed_pos(egui::Pos2::ZERO)
            .show(&self.ctx, |ui| {
                ui.set_min_size(egui::vec2(self.screen.0, self.screen.1));
                <PdfReaderApp as eframe::App>::ui(app, ui, &mut self.frame);
            });
        let mut output = self.ctx.end_pass();
        let cpu_ms = started.elapsed().as_secs_f64() * 1000.0;

        // 上传字节：egui 只是排队，这里统计"打算上传多少"，不代表真实 GPU 耗时
        let upload_bytes = output
            .textures_delta
            .set
            .values()
            .flat_map(|deltas| deltas.iter())
            .map(|delta| delta.image.width() * delta.image.height() * 4)
            .sum::<usize>() as u64;

        self.frame_index += 1;
        let stat = FrameStat {
            frame: self.frame_index,
            cpu_ms,
            upload_bytes,
            textures: app.textures.len(),
            texture_bytes: app.textures.bytes(),
            pending: app
                .session
                .as_ref()
                .map_or(0, |session| session.pending_renders()),
            submitted: app
                .session
                .as_ref()
                .map_or(0, |session| session.submitted_jobs()),
            rss_kb: resident_kb(),
            previewing: app.previewing(),
            // 由结算循环按"和上一帧比有没有变化"回填（见 `busy`）
            busy: false,
            // 只收**当前可见**的页：`last_plan` 是"最近一次绘制时的画法"，页滚出视口后它会变陈旧，
            // 拿它做断言会误报（例如"页 1 仍停在白底页框"，其实页 1 早就不在屏幕上了）
            plans: app
                .last_plan
                .iter()
                .filter(|(page, _)| app.visible.contains(page))
                .map(|(page, stamp)| (page.get(), *stamp))
                .collect(),
            ranges: app
                .last_plan
                .keys()
                .filter(|page| app.visible.contains(page))
                .filter_map(|page| {
                    app.session
                        .as_ref()?
                        .visible_tiles(*page)
                        .map(|range| (page.get(), range))
                })
                .collect(),
        };

        self.observe(app, &output, &stat);
        // egui 要求使用方"消费"纹理增量：无 GPU 时读完尺寸即可丢弃，
        // 不 clear 会在 Drop 时 panic（epaint 的契约）。
        output.textures_delta.clear();
        stat
    }

    /// 从**真实绘制输出**反推每页的覆盖格子与清晰层，喂给断言 ①。
    fn observe(&mut self, app: &crate::PdfReaderApp, output: &egui::FullOutput, stat: &FrameStat) {
        let Some(session) = app.session.as_ref() else {
            return;
        };
        // TextureId → RenderKey
        let mut by_id: HashMap<egui::TextureId, (PageIndex, u32, bool)> = HashMap::new();
        for (key, handle) in app.textures.iter() {
            let current =
                RenderKey::new(key.page, session.render_scale(key.page), key.rotation).scale_milli;
            by_id.insert(
                handle.id(),
                (key.page, key.scale_milli, key.scale_milli == current),
            );
        }
        let primitives = self
            .ctx
            .tessellate(output.shapes.clone(), output.pixels_per_point);
        let mut quads: Vec<(u32, bool, Vec<usize>)> = Vec::new();
        let mut pages: Vec<u32> = Vec::new();
        for primitive in primitives {
            let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
                continue;
            };
            let Some((page, _band, sharp)) = by_id.get(&mesh.texture_id).copied() else {
                continue;
            };
            let Some(placement) = session.placement(page) else {
                continue;
            };
            // 屏幕矩形 → 内容 → 页坐标
            let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
            let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
            for vertex in &mesh.vertices {
                min_x = min_x.min(vertex.pos.x);
                min_y = min_y.min(vertex.pos.y);
                max_x = max_x.max(vertex.pos.x);
                max_y = max_y.max(vertex.pos.y);
            }
            if !(max_x > min_x && max_y > min_y) {
                continue;
            }
            let scroll = session.viewport().scroll;
            let canvas = app.canvas_rect.min;
            let corners = [
                egui::pos2(min_x, min_y),
                egui::pos2(max_x, min_y),
                egui::pos2(min_x, max_y),
                egui::pos2(max_x, max_y),
            ];
            let (mut pmin_x, mut pmin_y) = (f32::MAX, f32::MAX);
            let (mut pmax_x, mut pmax_y) = (f32::MIN, f32::MIN);
            for corner in corners {
                let content = Point::new(
                    corner.x - canvas.x + scroll.x,
                    corner.y - canvas.y + scroll.y,
                );
                let page_point = placement.content_to_page(content);
                pmin_x = pmin_x.min(page_point.x);
                pmin_y = pmin_y.min(page_point.y);
                pmax_x = pmax_x.max(page_point.x);
                pmax_y = pmax_y.max(page_point.y);
            }
            let size = placement.page_size;
            if !(size.width > 0.0 && size.height > 0.0) {
                continue;
            }
            let cell_w = size.width / CELLS as f32;
            let cell_h = size.height / CELLS as f32;
            let col0 = ((pmin_x / cell_w).floor().max(0.0) as usize).min(CELLS - 1);
            let col1 = ((pmax_x / cell_w).ceil().max(0.0) as usize).min(CELLS);
            let row0 = ((pmin_y / cell_h).floor().max(0.0) as usize).min(CELLS - 1);
            let row1 = ((pmax_y / cell_h).ceil().max(0.0) as usize).min(CELLS);
            let mut cells = Vec::new();
            for row in row0..row1 {
                for col in col0..col1 {
                    cells.push(row * CELLS + col);
                }
            }
            if cells.is_empty() {
                continue;
            }
            if !pages.contains(&page.get()) {
                pages.push(page.get());
            }
            quads.push((page.get(), sharp, cells));
        }
        // 波段变化时重置该页的比较基线
        for page in &pages {
            if let Some((_, stamp)) = stat.plans.iter().find(|(p, _)| **p == *page) {
                let key = (stamp.band, *page);
                let _ = key;
                self.tracker.reset_band(*page, stamp.band);
            }
        }
        self.tracker.observe(stat.frame, &pages, &quads);
        let gaps = self.tracker.check_gaps(stat.frame, app);
        self.pending.extend(gaps);

        // 断言 ②：每次逗留某波段只允许"预览 → 瓦片"翻**一次**，且不得翻回去
        for (page, stamp) in &stat.plans {
            let outcome = self.flips.observe(*page, *stamp, stat.frame);
            self.violations.extend(outcome.violations);
            // 断言 ④（D-97 的编码）：**翻页那一刻**必须全就绪，除非等满超时。
            // 少了这条，把"有一块就翻"的旧 bug 放回去也不会被抓住（真机上的白块）。
            if outcome.became_tiles {
                if let Some(violation) = premature_switch(*page, *stamp, stat.frame) {
                    self.violations.push(violation);
                }
            }
        }
    }

    fn finish_action(&mut self, action: &ActionReport) -> (Vec<String>, Vec<String>) {
        let mut violations: Vec<String> = std::mem::take(&mut self.pending);
        self.tracker.reported_gap.clear();
        // 断言⑥：动作稳定后，可见页**不能停在糊的预览上** —— 该清晰的时候必须已经清晰。
        // 这条是"上传集合漏了 A 类"的唯一可靠信号（实测过一次：瓦片从来没被上传，
        // 画面永远是拉伸的旧图，而"露白/翻页/尺寸"三条断言全都不会报）。
        if action.settled {
            if let Some(frame) = action.frames_data.last() {
                for (page, stamp) in &frame.plans {
                    if matches!(stamp.plan, crate::view::PageDraw::Tiles)
                        || matches!(stamp.plan, crate::view::PageDraw::Whole)
                    {
                        continue;
                    }
                    violations.push(format!(
                        "动作 {} 稳定后，页 {page} 仍停在 {}（没有变成清晰的瓦片/整页图）",
                        action.id,
                        stamp.describe()
                    ));
                }
            }
        }
        if !action.settled {
            let tail = action
                .frames_data
                .iter()
                .rev()
                .take(3)
                .map(|frame| {
                    let plans: Vec<String> = frame
                        .plans
                        .iter()
                        .map(|(page, stamp)| format!("p{page}={}", stamp.describe()))
                        .collect();
                    format!(
                        "帧{} 纹理={} 在飞={} 预览窗口={} 画法=[{}]",
                        frame.frame,
                        frame.textures,
                        frame.pending,
                        if frame.previewing { "是" } else { "否" },
                        plans.join(" ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            let hint = if action
                .frames_data
                .iter()
                .rev()
                .take(3)
                .all(|frame| frame.previewing)
            {
                "；这段时间一帧都没出预览窗口（窗口内不提交渲染），说明帧跑得比 160ms 快得多"
            } else {
                ""
            };
            violations.push(format!(
                "动作 {} 在 {} 帧 / {:.0}ms 内没有稳定{hint}（最后状态：{tail}）",
                action.id, action.frames, action.elapsed_ms
            ));
        }
        // 就绪兜底：说明有块在 READY_TIMEOUT_MS（虚拟时间）内没到。
        // 这是**质量降级**而非不变量破坏（抖一下糊图是设计允许的），因此记为 note。
        if let Some(frame) = action
            .frames_data
            .iter()
            .find(|frame| frame.plans.values().any(|stamp| stamp.partial))
        {
            if let Some((page, stamp)) = frame.plans.iter().find(|(_, stamp)| stamp.partial) {
                self.notes.push(format!(
                    "动作 {} 第 {} 帧：页 {page} 波段 {} 以 {}/{} 块画瓦片（等待 {}ms，阈值 {READY_TIMEOUT_MS}ms；缺的块由底料补）",
                    action.id, frame.frame, stamp.band, stamp.ready, stamp.total, stamp.waited_ms
                ));
            }
        }
        (violations, std::mem::take(&mut self.notes))
    }

    /// 断言 ③：预览纹理的尺寸必须等于 `区域 × 渲染比例`（允许 ±1 的边缘取整）。
    fn assert_preview_sizes(&self, app: &crate::PdfReaderApp) -> Vec<String> {
        let Some(session) = app.session.as_ref() else {
            return Vec::new();
        };
        let mut violations = Vec::new();
        for preview in &app.custom_previews {
            if preview.kind != CustomKind::Image {
                continue;
            }
            let Some(texture) = &preview.snapshot else {
                continue;
            };
            let Some(placement) = session.placement(preview.page) else {
                continue;
            };
            let render = crate::view::preview_render_scale(
                preview.rect,
                placement.page_size,
                placement.rotation,
                render_scale_for(preview.scale.max(0.1), session.overscan()),
                session.max_texture_side(),
            );
            let Some(expected) = crate::view::region_pixel_size(
                preview.rect,
                placement.page_size,
                placement.rotation,
                render,
            ) else {
                violations.push(format!(
                    "图片预览（页 {}）的区域为空：{:?}",
                    preview.page.display_number(),
                    preview.rect
                ));
                continue;
            };
            let actual = texture.size();
            if expected.0.abs_diff(actual[0] as u32) > 1
                || expected.1.abs_diff(actual[1] as u32) > 1
            {
                violations.push(format!(
                    "图片预览（页 {}）尺寸 {}×{} ≠ 区域×分辨率 {:?}（记录分辨率={:.0}%）",
                    preview.page.display_number(),
                    actual[0],
                    actual[1],
                    expected,
                    preview.scale * 100.0
                ));
            }
        }
        violations
    }
}

/// 把报告写成 JSONL（每个动作一行）。
pub fn write_jsonl(report: &Report, path: &Path) -> std::io::Result<()> {
    let mut out = String::new();
    for action in &report.actions {
        out.push_str(&action_json(report, action));
        out.push('\n');
    }
    std::fs::write(path, out)
}

/// 一个动作的 JSONL 行。
///
/// 用"键值对列表"而不是位置参数：位置参数一旦插队就会静默错位
/// （曾经出现过 `"settled":5` 这种把帧数写进布尔字段的事故）。
fn action_json(report: &Report, action: &ActionReport) -> String {
    let (p50, p95, p99, max) = action.busy_cpu_ms;
    let previews: Vec<String> = action
        .previews
        .iter()
        .map(|(index, width, height)| format!("[{index},{width},{height}]"))
        .collect();
    let fields = [
        ("script", format!("\"{}\"", escape(&report.script))),
        ("action", format!("\"{}\"", escape(&action.id))),
        ("op", format!("\"{}\"", escape(&action.action))),
        ("frames", action.frames.to_string()),
        ("settled", action.settled.to_string()),
        ("busy_frames", action.busy_frames.to_string()),
        ("wall_ms", format!("{:.1}", action.elapsed_ms)),
        (
            "cpu_ms",
            format!(
                "{{\"p50\":{p50:.3},\"p95\":{p95:.3},\"p99\":{p99:.3},\"max\":{max:.3},\"total\":{:.3}}}",
                action.busy_cpu_ms_total
            ),
        ),
        ("page_after", action.page_after.to_string()),
        (
            "scroll_after",
            format!(
                "{{\"x\":{:.1},\"y\":{:.1}}}",
                action.scroll_after.0, action.scroll_after.1
            ),
        ),
        ("submitted", action.submitted.to_string()),
        ("upload_bytes", action.upload_bytes.to_string()),
        ("texture_bytes_peak", action.texture_bytes_peak.to_string()),
        (
            "textures",
            action
                .frames_data
                .last()
                .map_or(0, |frame| frame.textures)
                .to_string(),
        ),
        ("violations", serde_free_array(&action.violations)),
        ("notes", serde_free_array(&action.notes)),
        ("previews", format!("[{}]", previews.join(","))),
        ("diag", serde_free_array(&action.diag)),
    ];
    let body: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("\"{key}\":{value}"))
        .collect();
    format!("{{{}}}", body.join(","))
}

fn serde_free_array(items: &[String]) -> String {
    let body: Vec<String> = items
        .iter()
        .map(|item| format!("\"{}\"", escape(item)))
        .collect();
    format!("[{}]", body.join(","))
}

/// JSON 字符串转义（复用 `json` 模块的规则）。
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::PageDraw;

    fn stamp(
        plan: PageDraw,
        preview: Preview,
        ready: usize,
        total: usize,
        waited: u64,
    ) -> PlanStamp {
        PlanStamp {
            plan,
            band: 10486,
            partial: false,
            ready,
            total,
            waited_ms: waited,
            preview,
        }
    }

    /// JSONL 必须是合法 JSON 且字段对得上（曾经因为位置参数错位写出过 `"settled":5`）。
    #[test]
    fn jsonl_lines_are_valid_json_with_the_right_fields() {
        let mut action = ActionReport {
            id: "zoom_800".to_string(),
            action: "zoom 8".to_string(),
            frames: 42,
            settled: true,
            diag: vec!["[画法] x".to_string()],
            frames_data: Vec::new(),
            violations: vec!["违规一条".to_string()],
            notes: vec!["观察一条".to_string()],
            previews: vec![(0, 1573, 839)],
            submitted: 12,
            textures_peak: 18,
            texture_bytes_peak: 18 * 860_000,
            upload_bytes: 10327920,
            elapsed_ms: 173.4,
            busy_frames: 3,
            busy_cpu_ms: (5.0, 18.5, 22.7, 22.7),
            busy_cpu_ms_total: 41.6,
            page_after: 2,
            scroll_after: (10.0, 20.0),
            annotations: 0,
            annotation_bounds: None,
            floats: 0,
        };
        action.frames_data.push(FrameStat {
            frame: 1,
            cpu_ms: 5.0,
            upload_bytes: 10327920,
            textures: 18,
            texture_bytes: 15_480_000,
            pending: 0,
            submitted: 12,
            rss_kb: 0,
            previewing: false,
            busy: true,
            plans: BTreeMap::new(),
            ranges: BTreeMap::new(),
        });
        let report = Report {
            script: "tiling_800".to_string(),
            actions: vec![action],
            violations: Vec::new(),
        };
        let path = std::env::temp_dir().join(format!("pdf-jsonl-{}.jsonl", std::process::id()));
        write_jsonl(&report, &path).expect("写 JSONL");
        let text = std::fs::read_to_string(&path).expect("读回");
        let line = text.lines().next().expect("应有一行");
        let value = crate::json::Json::parse(line).expect("JSONL 必须是合法 JSON");
        let object = value.as_object().expect("一行是对象");
        assert_eq!(
            object.field("frames").and_then(|v| v.as_number()),
            Some(42.0)
        );
        assert_eq!(
            object.field("busy_frames").and_then(|v| v.as_number()),
            Some(3.0)
        );
        assert_eq!(
            object.field("settled").and_then(|v| v.as_str()),
            None,
            "布尔不是字符串"
        );
        assert_eq!(
            object
                .field("previews")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            object
                .field("notes")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        let _ = std::fs::remove_file(&path);
    }

    // ---------- 断言 ②：一次波段切换只翻一次 ----------

    #[test]
    fn preview_to_tiles_once_is_fine() {
        let mut flips = FlipTracker::default();
        assert!(flips
            .observe(
                0,
                stamp(PageDraw::Stretch(1600), Preview::Whole(1600), 0, 4, 0),
                1
            )
            .violations
            .is_empty());
        assert!(flips
            .observe(0, stamp(PageDraw::Tiles, Preview::Whole(1600), 4, 4, 20), 2)
            .violations
            .is_empty());
        // 稳定在瓦片上不再报
        assert!(flips
            .observe(0, stamp(PageDraw::Tiles, Preview::Whole(1600), 4, 4, 40), 3)
            .violations
            .is_empty());
    }

    #[test]
    fn going_back_to_preview_is_a_violation() {
        let mut flips = FlipTracker::default();
        flips.observe(
            0,
            stamp(PageDraw::Stretch(1600), Preview::Whole(1600), 0, 4, 0),
            1,
        );
        flips.observe(0, stamp(PageDraw::Tiles, Preview::Whole(1600), 4, 4, 20), 2);
        let violations = flips
            .observe(
                0,
                stamp(PageDraw::Stretch(1600), Preview::Whole(1600), 0, 4, 60),
                3,
            )
            .violations;
        assert_eq!(violations.len(), 1, "瓦片退回预览必须报：{violations:?}");
        assert!(violations[0].contains("退回"), "{violations:?}");
    }

    #[test]
    fn flipping_to_tiles_twice_is_a_violation() {
        let mut flips = FlipTracker::default();
        flips.observe(
            0,
            stamp(PageDraw::Stretch(1600), Preview::Whole(1600), 0, 4, 0),
            1,
        );
        flips.observe(0, stamp(PageDraw::Tiles, Preview::Whole(1600), 4, 4, 20), 2);
        flips.observe(
            0,
            stamp(PageDraw::Stretch(1600), Preview::Whole(1600), 0, 4, 60),
            3,
        );
        let violations = flips
            .observe(0, stamp(PageDraw::Tiles, Preview::Whole(1600), 4, 4, 80), 4)
            .violations;
        assert_eq!(violations.len(), 1, "第二次翻到瓦片必须报：{violations:?}");
        assert!(violations[0].contains("第二次"), "{violations:?}");
    }

    /// D-104：同一波段里滚出新块不再退回预览，因此"一个波段只翻一次"是硬规则。
    #[test]
    fn a_new_band_is_a_new_segment() {
        let mut flips = FlipTracker::default();
        flips.observe(
            0,
            stamp(PageDraw::Stretch(1600), Preview::Whole(1600), 0, 4, 0),
            1,
        );
        flips.observe(0, stamp(PageDraw::Tiles, Preview::Whole(1600), 4, 4, 20), 2);
        // 换波段：允许重新走一次"预览 → 瓦片"
        let mut next = stamp(PageDraw::Stretch(1600), Preview::Tiles(65_540), 0, 12, 0);
        next.band = 16_384;
        assert!(flips.observe(0, next, 3).violations.is_empty());
        let mut next_tiles = stamp(PageDraw::Tiles, Preview::Tiles(65_540), 12, 12, 20);
        next_tiles.band = 16_384;
        assert!(flips.observe(0, next_tiles, 4).violations.is_empty());
    }

    // ---------- 断言 ④：不得提前切到瓦片 ----------

    #[test]
    fn premature_tiles_are_rejected() {
        let violation = premature_switch(
            0,
            stamp(PageDraw::Tiles, Preview::Whole(1600), 1, 12, 20),
            7,
        )
        .expect("有旧内容却只有 1/12 块就切瓦片，必须报");
        assert!(violation.contains("提前"), "{violation}");
    }

    #[test]
    fn tiles_after_the_timeout_are_allowed() {
        let late = stamp(
            PageDraw::Tiles,
            Preview::Whole(1600),
            9,
            12,
            READY_TIMEOUT_MS,
        );
        assert!(premature_switch(0, late, 7).is_none(), "等满超时后允许兜底");
    }

    #[test]
    fn first_generation_without_old_content_is_allowed() {
        let fresh = stamp(PageDraw::Tiles, Preview::None, 0, 12, 0);
        assert!(
            premature_switch(0, fresh, 1).is_none(),
            "没有旧内容时只能画瓦片"
        );
    }

    #[test]
    fn fully_ready_tiles_are_allowed() {
        let ready = stamp(PageDraw::Tiles, Preview::Whole(1600), 12, 12, 0);
        assert!(premature_switch(0, ready, 1).is_none());
    }

    // ---------- 断言 ①：已清晰区域不得变白 ----------

    #[test]
    fn a_previously_sharp_cell_losing_all_layers_is_a_violation() {
        let mut tracker = CoverageTracker::default();
        tracker.reset_band(0, 10486);
        // 第 1 帧：格子 5 清晰且被覆盖
        tracker.observe(1, &[0], &[(0, true, vec![5])]);
        assert!(tracker.violations.is_empty());
        // 第 2 帧：格子 5 没有任何图层 → 纸白
        tracker.observe(2, &[0], &[(0, true, vec![7])]);
        assert_eq!(tracker.violations.len(), 1, "{:?}", tracker.violations);
        assert!(
            tracker.violations[0].contains("曾经清晰"),
            "{:?}",
            tracker.violations
        );
    }

    #[test]
    fn a_previously_sharp_cell_covered_by_a_blurry_layer_is_fine() {
        let mut tracker = CoverageTracker::default();
        tracker.reset_band(0, 10486);
        tracker.observe(1, &[0], &[(0, true, vec![5])]);
        // 第 2 帧：格子 5 由"旧内容"覆盖（不是当前波段，但不是白）
        tracker.observe(2, &[0], &[(0, false, vec![5])]);
        assert!(tracker.violations.is_empty(), "{:?}", tracker.violations);
    }
}
