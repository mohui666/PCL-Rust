use super::{Launcher, MUTED};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, FontId, Rect, RichText, Vec2};
use pcl_core::model::{Progress, ProgressStage};
use std::{
    collections::VecDeque,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepStatus {
    Waiting,
    Running,
    Finished,
}
#[derive(Clone)]
struct Step {
    stage: ProgressStage,
    state: StepStatus,
    counts: Option<(u64, u64)>,
}
struct VisibleStep {
    name: String,
    depth: u8,
    state: StepStatus,
    fraction: Option<f64>,
    detail: String,
}
/// Explicit operation graph. Indices are stable within the owning job's plan;
/// weights are the upstream LoaderCombo work estimates, not elapsed time.
#[derive(Clone, Debug)]
pub(crate) struct TaskComponentSpec {
    pub(super) name: String,
    pub(super) weight: f64,
    pub(super) dependencies: Vec<usize>,
    pub(super) vanilla_weight: f64,
}
impl TaskComponentSpec {
    pub(super) fn new(name: impl Into<String>, weight: f64, dependencies: Vec<usize>) -> Self {
        Self {
            name: name.into(),
            weight,
            dependencies,
            vanilla_weight: 0.0,
        }
    }
    pub(super) fn with_vanilla(mut self) -> Self {
        self.vanilla_weight = 39.0;
        self
    }
}
impl From<&str> for TaskComponentSpec {
    fn from(name: &str) -> Self {
        Self::new(name, 1.0, Vec::new())
    }
}
impl From<String> for TaskComponentSpec {
    fn from(name: String) -> Self {
        Self::new(name, 1.0, Vec::new())
    }
}
// ModDownloadLib.vb:17–67 maps JSON (2+3), libraries (1+13),
// index (1+3), and assets (14) onto the backend's real phases. Its
// 2-weight final install is split between native extraction and commit.
fn stage_weight(stage: ProgressStage) -> f64 {
    match stage {
        ProgressStage::VersionMetadata => 5.0,
        ProgressStage::CoreLibraries => 14.0,
        ProgressStage::AssetIndex => 4.0,
        ProgressStage::AssetFiles => 14.0,
        ProgressStage::NativeLibraries
        | ProgressStage::VersionCommit
        | ProgressStage::ExistingVersionValidation => 1.0,
    }
}
fn weighted_steps(steps: &[Step], require_known: bool) -> Option<f64> {
    let total: f64 = steps.iter().map(|step| stage_weight(step.stage)).sum();
    if total <= 0.0 {
        return None;
    }
    let mut earned = 0.0;
    for step in steps {
        let fraction = match step_fraction(step) {
            Some(value) => value,
            None if require_known => return None,
            None => 0.0,
        };
        earned += stage_weight(step.stage) * fraction;
    }
    Some((earned / total).clamp(0.0, 1.0))
}
const VANILLA_PLAN: [ProgressStage; 6] = [
    ProgressStage::VersionMetadata,
    ProgressStage::AssetIndex,
    ProgressStage::CoreLibraries,
    ProgressStage::AssetFiles,
    ProgressStage::NativeLibraries,
    ProgressStage::VersionCommit,
];
pub(super) fn download_task_title(label: &str, selected_instance: Option<&str>) -> String {
    if let Some(instance) = label.strip_prefix("正在安装 ") {
        let instance = selected_instance
            .filter(|id| !id.trim().is_empty())
            .unwrap_or(instance);
        format!("{} 安装", instance.trim())
    } else {
        label.trim_start_matches("正在").to_owned()
    }
}
#[derive(PartialEq, Eq)]
enum Status {
    Queued,
    Running,
    Cancelling,
    Finished,
    Failed,
    Cancelled,
}

struct Component {
    name: String,
    weight: f64,
    dependencies: Vec<usize>,
    vanilla_weight: f64,
    earned: f64,
    state: StepStatus,
    start: usize,
    end: Option<usize>,
}

pub(super) struct TaskState {
    components: Vec<Component>,
    current_component: Option<usize>,
    title: String,
    status: Status,
    steps: Vec<Step>,
    plan: Option<Vec<ProgressStage>>,
    plan_start: usize,
    has_multiple_plans: bool,
    overall_plan_known: bool,
    group_vanilla_install: bool,
    untyped_progress: bool,
    message: String,
    error: Option<String>,
    samples: VecDeque<(u64, u64)>,
    transfer_seen: Option<Instant>,
    remaining_files: Option<u64>,
    active_downloads: Option<u32>,
    concurrency_limit: Option<u32>,
    non_transfer_stage: bool,
    icons: Option<TaskIcons>,
}
impl TaskState {
    pub(super) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            components: Vec::new(),
            current_component: None,
            status: Status::Running,
            steps: Vec::new(),
            plan: None,
            plan_start: 0,
            has_multiple_plans: false,
            overall_plan_known: false,
            group_vanilla_install: false,
            untyped_progress: false,
            message: String::new(),
            error: None,
            samples: VecDeque::new(),
            transfer_seen: None,
            remaining_files: None,
            active_downloads: None,
            concurrency_limit: None,
            non_transfer_stage: false,
            icons: None,
        }
    }
    pub(super) fn set_overall_plan_known(&mut self, known: bool) {
        self.overall_plan_known = known;
    }
    /// Only the automatic vanilla-install page opts in. Repair/download-only
    /// operations, existing-parent aliases and loader pipelines keep real rows.
    pub(super) fn group_vanilla_install(&mut self, enabled: bool) {
        self.group_vanilla_install = enabled;
    }
    fn uses_vanilla_groups(&self) -> bool {
        self.group_vanilla_install
            && !self.has_multiple_plans
            && self.plan.as_deref() == Some(VANILLA_PLAN.as_slice())
            && self.steps.len() == VANILLA_PLAN.len()
    }
    pub(super) fn component_plan(&mut self, names: Vec<TaskComponentSpec>) {
        if !self.is_running()
            || names.len() > 128
            || names.iter().enumerate().any(|(index, part)| {
                !part.weight.is_finite()
                    || part.weight <= 0.0
                    || !part.vanilla_weight.is_finite()
                    || part.vanilla_weight < 0.0
                    || part.vanilla_weight > part.weight
                    || part
                        .dependencies
                        .iter()
                        .any(|&dependency| dependency >= index)
            })
        {
            return;
        }
        if self.steps.is_empty() && self.components.is_empty() {
            self.components = names
                .into_iter()
                .map(|part| Component {
                    name: part.name,
                    weight: part.weight,
                    dependencies: part.dependencies,
                    vanilla_weight: part.vanilla_weight,
                    earned: 0.0,
                    state: StepStatus::Waiting,
                    start: 0,
                    end: None,
                })
                .collect();
        }
    }
    pub(super) fn component_start(&mut self, index: usize) {
        if !self.is_running()
            || self.current_component.is_some()
            || self.components.get(index).is_none_or(|part| {
                part.state != StepStatus::Waiting
                    || part.dependencies.iter().any(|&dependency| {
                        self.components[dependency].state != StepStatus::Finished
                    })
            })
        {
            return;
        }
        if let Some(part) = self.components.get_mut(index) {
            part.state = StepStatus::Running;
            part.start = self.steps.len();
            self.current_component = Some(index);
        }
    }
    pub(super) fn component_done(&mut self, index: usize) {
        if !self.is_running() || self.current_component != Some(index) {
            return;
        }
        if let Some(part) = self.components.get_mut(index) {
            part.state = StepStatus::Finished;
            part.earned = part.weight;
            part.end = Some(self.steps.len());
            for step in &mut self.steps[part.start..] {
                step.state = StepStatus::Finished;
            }
            self.current_component = None;
        }
    }
    fn visible_steps(&self) -> Vec<VisibleStep> {
        if self.components.is_empty() {
            return self.flat_visible_steps();
        }
        let mut result = Vec::new();
        for part in &self.components {
            result.push(VisibleStep {
                name: part.name.clone(),
                depth: 0,
                state: part.state,
                fraction: match part.state {
                    StepStatus::Finished => Some(1.0),
                    StepStatus::Running if part.earned > 0.0 => {
                        Some((part.earned / part.weight).min(0.9999))
                    }
                    _ => None,
                },
                detail: if part.dependencies.is_empty() {
                    String::new()
                } else {
                    format!(
                        "{}：{}",
                        if part.state == StepStatus::Waiting {
                            "等待"
                        } else {
                            "依赖"
                        },
                        part.dependencies
                            .iter()
                            .map(|&index| self.components[index].name.as_str())
                            .collect::<Vec<_>>()
                            .join("、")
                    )
                },
            });
            if part.state != StepStatus::Waiting {
                for step in self
                    .steps
                    .get(part.start..part.end.unwrap_or(self.steps.len()))
                    .unwrap_or_default()
                {
                    result.push(VisibleStep {
                        name: stage_name(step.stage).into(),
                        depth: 1,
                        state: step.state,
                        fraction: step_fraction(step),
                        detail: step
                            .counts
                            .map_or_else(String::new, |(done, total)| format!("{done}/{total}")),
                    });
                }
            }
        }
        result
    }
    fn flat_visible_steps(&self) -> Vec<VisibleStep> {
        if self.uses_vanilla_groups() {
            use ProgressStage::*;
            [
                ("下载原版 json 文件", &[VersionMetadata][..]),
                ("下载原版支持库文件", &[CoreLibraries][..]),
                ("下载原版资源文件", &[AssetIndex, AssetFiles][..]),
                ("安装游戏", &[NativeLibraries, VersionCommit][..]),
            ]
            .into_iter()
            .map(|(name, stages)| {
                let steps: Vec<_> = stages
                    .iter()
                    .filter_map(|stage| self.steps.iter().find(|step| step.stage == *stage))
                    .collect();
                let finished = steps.len() == stages.len()
                    && steps.iter().all(|step| step.state == StepStatus::Finished);
                let state = if finished {
                    StepStatus::Finished
                } else if steps.iter().all(|step| step.state == StepStatus::Waiting) {
                    StepStatus::Waiting
                } else {
                    StepStatus::Running
                };
                // No completion is inferred from advancing to another group.
                // Unknown running phases leave the group indeterminate.
                let fraction = weighted_steps(
                    &steps.iter().map(|step| (*step).clone()).collect::<Vec<_>>(),
                    true,
                );
                let detail = steps
                    .iter()
                    .map(|step| {
                        let status = match step.state {
                            StepStatus::Waiting => "等待中".into(),
                            StepStatus::Finished => "已完成".into(),
                            StepStatus::Running => {
                                step.counts.filter(|&(_, total)| total > 0).map_or_else(
                                    || "进行中".into(),
                                    |(done, total)| format!("{done}/{total}"),
                                )
                            }
                        };
                        format!("{}：{status}", stage_name(step.stage))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                VisibleStep {
                    name: name.into(),
                    depth: 0,
                    state,
                    fraction,
                    detail,
                }
            })
            .collect()
        } else {
            self.steps
                .iter()
                .map(|step| VisibleStep {
                    name: stage_name(step.stage).into(),
                    depth: 0,
                    state: step.state,
                    fraction: step_fraction(step),
                    detail: step
                        .counts
                        .filter(|&(_, total)| total > 0)
                        .map_or_else(String::new, |(done, total)| format!("{done}/{total}")),
                })
                .collect()
        }
    }
    pub(super) fn update(&mut self, progress: &Progress) {
        if !self.is_running() || (!self.components.is_empty() && self.current_component.is_none()) {
            return;
        }
        self.message = progress.message.clone();
        if let Some(plan) = &progress.plan {
            // Each declared plan starts a new pipeline. Preserve prior pipeline rows,
            // but repeated stage kinds must not inherit their completed state.
            self.plan_start = self.steps.len();
            self.has_multiple_plans |= self.plan.is_some();
            self.plan = Some(plan.clone());
            for &stage in plan {
                self.steps.push(Step {
                    stage,
                    state: StepStatus::Waiting,
                    counts: None,
                });
            }
        }
        self.non_transfer_stage = matches!(
            progress.stage,
            Some(
                ProgressStage::NativeLibraries
                    | ProgressStage::VersionCommit
                    | ProgressStage::ExistingVersionValidation
            )
        );
        if progress.transfer.is_none() {
            // Uninstrumented loader work must never inherit a completed vanilla
            // transfer's zero remaining count or its former download speed.
            self.samples.clear();
            self.transfer_seen = None;
            self.remaining_files = None;
            self.active_downloads = None;
            self.concurrency_limit = None;
        }
        self.untyped_progress = progress.stage.is_none();
        if let Some(stage) = progress.stage {
            let index = self
                .steps
                .iter()
                .skip(self.plan_start)
                .position(|step| step.stage == stage)
                .map(|index| index + self.plan_start)
                .unwrap_or_else(|| {
                    self.steps.push(Step {
                        stage,
                        state: StepStatus::Waiting,
                        counts: None,
                    });
                    self.steps.len() - 1
                });
            let step = &mut self.steps[index];
            step.counts = progress.stage_progress;
            step.state = if progress
                .stage_progress
                .is_some_and(|(done, total)| total > 0 && done >= total)
            {
                StepStatus::Finished
            } else {
                StepStatus::Running
            };
        }
        if let Some(index) = self.current_component {
            let part = &mut self.components[index];
            // Only explicitly declared, typed stage plans earn the vanilla
            // share. Generic file counters can describe a sub-operation and
            // must not incorrectly complete an entire loader component.
            if self.plan.is_some() && self.plan_start >= part.start {
                if let Some(fraction) = weighted_steps(&self.steps[self.plan_start..], false) {
                    part.earned = part.earned.max(part.vanilla_weight * fraction);
                }
            }
        }
        if let Some(transfer) = &progress.transfer {
            if self.samples.back().is_some_and(|&(time, bytes)| {
                transfer.elapsed_ms < time || transfer.downloaded_bytes < bytes
            }) {
                self.samples.clear();
            }
            let sample = (transfer.elapsed_ms, transfer.downloaded_bytes);
            if self
                .samples
                .back()
                .is_some_and(|&(time, _)| time == sample.0)
            {
                self.samples.pop_back();
            }
            self.samples.push_back(sample);
            while self.samples.len() > 31
                || self.samples.front().is_some_and(|&(time, _)| {
                    self.samples.len() > 2 && sample.0.saturating_sub(time) > 3000
                })
            {
                self.samples.pop_front();
            }
            self.transfer_seen = Some(Instant::now());
            self.remaining_files = transfer.remaining_files;
            self.active_downloads = transfer.active_downloads;
            self.concurrency_limit = transfer.concurrency_limit;
        }
    }
    pub(super) fn finish(&mut self) {
        self.status = Status::Finished;
        self.remaining_files = Some(0);
    }
    pub(super) fn fail(&mut self, error: impl Into<String>, cancelled: bool) {
        self.status = if cancelled {
            Status::Cancelled
        } else {
            Status::Failed
        };
        self.error = Some(error.into());
    }
    pub(super) fn queued(&mut self) {
        self.status = Status::Queued;
        self.message = "等待空闲下载任务".into();
    }
    pub(super) fn started(&mut self) {
        if self.status == Status::Queued {
            self.status = Status::Running;
            self.message = "准备中".into();
        }
    }
    pub(super) fn was_cancelled(&self) -> bool {
        self.status == Status::Cancelled
    }
    pub(super) fn title(&self) -> &str {
        &self.title
    }
    pub(super) fn short_summary(&self) -> String {
        format!("{} · {}", self.title, self.status_label())
    }
    fn status_label(&self) -> &'static str {
        match self.status {
            Status::Queued => "等待中",
            Status::Running => "运行中",
            Status::Cancelling => "正在取消",
            Status::Finished => "已完成",
            Status::Failed => "失败",
            Status::Cancelled => "已取消",
        }
    }
    pub(super) fn history_summary(&self) -> (String, String, Vec<String>, Option<String>) {
        (
            self.title.clone(),
            self.status_label().into(),
            self.visible_steps()
                .iter()
                .map(|s| {
                    format!(
                        "{} · {}",
                        s.name,
                        match s.state {
                            StepStatus::Waiting => "等待中",
                            StepStatus::Running => "进行中",
                            StepStatus::Finished => "已完成",
                        }
                    )
                })
                .collect(),
            self.error.clone(),
        )
    }
    pub(super) fn is_running(&self) -> bool {
        matches!(
            self.status,
            Status::Queued | Status::Running | Status::Cancelling
        )
    }
    pub(super) fn is_finished(&self) -> bool {
        matches!(self.status, Status::Finished | Status::Cancelled)
    }
    pub(super) fn is_failed(&self) -> bool {
        self.status == Status::Failed
    }
    fn thread_text(&self) -> String {
        // A terminal job has no remaining workers. This is a lifecycle fact, not
        // a synthesized network sample; preserve an unreported limit as unknown.
        let active = if self.is_running() {
            self.active_downloads
        } else {
            Some(0)
        };
        let number = |value: Option<u32>| value.map_or_else(|| "—".into(), |n| n.to_string());
        format!("{} / {}", number(active), number(self.concurrency_limit))
    }
    fn progress_text(&self) -> String {
        if self.status == Status::Queued {
            return "等待中".into();
        }
        if self.status == Status::Finished {
            return "100 %".into();
        }
        if self.components.is_empty() && (self.untyped_progress || self.has_multiple_plans) {
            return "—".into();
        }
        if self.plan.is_none() && self.steps.is_empty() && self.message.is_empty() {
            return "准备中".into();
        }
        self.overall_progress().map_or_else(
            || "—".into(),
            |fraction| format!("{:.2} %", fraction * 100.0),
        )
    }
    /// The task-entry fill must use the same known denominator as the sidebar.
    pub(super) fn overall_progress(&self) -> Option<f64> {
        if self.status == Status::Finished {
            return Some(1.0);
        }
        if !self.components.is_empty() {
            let total: f64 = self.components.iter().map(|part| part.weight).sum();
            let earned: f64 = self.components.iter().map(|part| part.earned).sum();
            return Some((earned / total).clamp(0.0, 0.9999));
        }
        if self.untyped_progress || self.has_multiple_plans || !self.overall_plan_known {
            return None;
        }
        let plan = self.plan.as_ref().filter(|plan| !plan.is_empty())?;
        let sum: f64 = plan
            .iter()
            .map(|stage| {
                let Some(step) = self
                    .steps
                    .iter()
                    .skip(self.plan_start)
                    .find(|step| &step.stage == stage)
                else {
                    return 0.0;
                };
                let fraction = if step.state == StepStatus::Finished {
                    1.0
                } else {
                    step.counts
                        .filter(|&(_, total)| total > 0)
                        .map_or(0.0, |(done, total)| {
                            (done as f64 / total as f64).clamp(0.0, 1.0)
                        })
                };
                fraction * stage_weight(*stage)
            })
            .sum();
        let total: f64 = plan.iter().map(|&stage| stage_weight(stage)).sum();
        let percentage = ((sum / total * 10000.0).floor() / 100.0).min(99.99);
        Some(percentage / 100.0)
    }
    fn speed(&self, now: Instant) -> Option<f64> {
        if self.non_transfer_stage || self.is_finished() || self.is_failed() {
            return Some(0.0);
        }
        let last = self.transfer_seen?;
        if !self.is_running() || now.saturating_duration_since(last) > Duration::from_secs(2) {
            return Some(0.0);
        }
        if self.samples.len() < 2 {
            return None;
        }
        let mut total = 0.0;
        let mut weights = 0.0;
        for (index, ((old_time, old_bytes), (time, bytes))) in self
            .samples
            .iter()
            .zip(self.samples.iter().skip(1))
            .enumerate()
        {
            let elapsed = time.saturating_sub(*old_time);
            if elapsed == 0 {
                continue;
            }
            let weight = (index + 1) as f64;
            total += bytes.saturating_sub(*old_bytes) as f64 * 1000.0 / elapsed as f64 * weight;
            weights += weight;
        }
        (weights > 0.0).then_some(total / weights)
    }
}
fn step_fraction(step: &Step) -> Option<f64> {
    match step.state {
        StepStatus::Waiting => Some(0.0),
        StepStatus::Finished => Some(1.0),
        StepStatus::Running => step
            .counts
            .filter(|&(_, total)| total > 0)
            .map(|(done, total)| (done as f64 / total as f64).clamp(0.0, 1.0)),
    }
}
fn stage_name(stage: ProgressStage) -> &'static str {
    match stage {
        ProgressStage::VersionMetadata => "下载原版 json 文件",
        ProgressStage::AssetIndex => "下载资源文件索引",
        ProgressStage::CoreLibraries => "下载原版支持库文件",
        ProgressStage::AssetFiles => "下载原版资源文件",
        ProgressStage::NativeLibraries => "解压原生库文件",
        ProgressStage::VersionCommit => "登记版本",
        ProgressStage::ExistingVersionValidation => "校验已有原版文件",
    }
}
fn speed_text(speed: Option<f64>) -> String {
    let Some(mut speed) = speed else {
        return "—".into();
    };
    let units = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
    let mut index = 0;
    while speed >= 1024.0 && index < units.len() - 1 {
        speed /= 1024.0;
        index += 1;
    }
    if index == 0 {
        format!("{speed:.0} {}", units[index])
    } else {
        format!("{speed:.1} {}", units[index])
    }
}
// Fixed 2.13.1.1 PageSpeedLeft.xaml.vb paths. WPF's F1 fill-rule prefix
// becomes SVG's nonzero default; dimensions below preserve Stretch=Uniform.
const TASK_VECTORS: [(&str, &str); 4] = [
    ("waiting", "M5,0 a5,5 360 1 0 0,0.0001 m15,0 a5,5 360 1 0 0,0.0001 m15,0 a5,5 360 1 0 0,0.0001 Z"),
    ("finished", "M23.7501,33.25 L34.8334,44.3333 L52.2499,22.1668 L56.9999,26.9168 L34.8334,53.8333 L19.0001,38 L23.7501,33.25 Z"),
    ("failed", "M2.5,0 L0,2.5 7.5,10 0,17.5 2.5,20 10,12.5 17.5,20 20,17.5 12.5,10 20,2.5 17.5,0 10,7.5 2.5,0 Z"),
    ("cancel", "M2,0 L0,2 8,10 0,18 2,20 10,12 18,20 20,18 12,10 20,2 18,0 10,8 2,0 Z"),
];
#[derive(Clone)]
struct TaskIcons([egui::TextureHandle; 4]);
impl TaskIcons {
    fn new(ctx: &egui::Context) -> Self {
        Self(TASK_VECTORS.map(|(name, path)| {
            let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><path d="{path}" fill="white"/></svg>"#);
            let tree = resvg::usvg::Tree::from_str(&svg,&resvg::usvg::Options::default()).expect("fixed upstream task vector");
            let bounds=tree.root().abs_bounding_box();
            let scale=64.0/bounds.width().max(bounds.height());
            let mut pixmap=resvg::tiny_skia::Pixmap::new((bounds.width()*scale).ceil() as u32,(bounds.height()*scale).ceil() as u32).unwrap();
            resvg::render(&tree,resvg::tiny_skia::Transform::from_scale(scale,scale).pre_translate(-bounds.x(),-bounds.y()),&mut pixmap.as_mut());
            ctx.load_texture(format!("task-{name}"),egui::ColorImage::from_rgba_premultiplied([pixmap.width() as usize,pixmap.height() as usize],pixmap.data()),egui::TextureOptions::LINEAR)
        }))
    }
    fn paint(&self, ui: &egui::Ui, index: usize, rect: Rect, color: Color32) {
        let texture = &self.0[index];
        let size = texture.size_vec2();
        let scale = (rect.width() / size.x).min(rect.height() / size.y);
        ui.painter().image(
            texture.id(),
            Rect::from_center_size(rect.center(), size * scale),
            Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            color,
        );
    }
}
fn task_row_label(ui: &mut egui::Ui, rect: Rect, text: &str) -> egui::Response {
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    row.set_clip_rect(rect.intersect(ui.clip_rect()));
    row.add(
        egui::Label::new(
            RichText::new(text)
                .size(14.0)
                .color(theme::palette(ui.ctx()).text),
        )
        .truncate()
        .show_tooltip_when_elided(false),
    )
}
const TASK_STATISTIC_LABELS: [&str; 4] = ["总进度", "下载速度", "剩余文件", "剩余线程"];

fn statistic_group_tops(rect: Rect, group_height: f32) -> [f32; 4] {
    // PageSpeedLeft.xaml uses outer 1* and three inner 0.6* spacer rows.
    // The shell supplies the upstream 200-DIP-wide task sidebar.
    let unit = ((rect.height() - 4.0 * group_height) / 3.8).max(0.0);
    std::array::from_fn(|index| rect.top() + unit + index as f32 * (group_height + unit * 0.6))
}
impl Launcher {
    pub(super) fn task_sidebar(&mut self, ui: &mut egui::Ui) {
        let Some(task) = self.task.as_ref() else {
            return;
        };
        let rect = ui.max_rect();
        let tasks = self
            .task_hub
            .others
            .values()
            .chain(self.task.iter())
            .filter(|t| t.is_running())
            .collect::<Vec<_>>();
        let values = if tasks.len() > 1 {
            let total = tasks
                .iter()
                .map(|t| t.overall_progress())
                .collect::<Option<Vec<_>>>()
                .map(|p| p.iter().sum::<f64>() / p.len() as f64);
            let sum = |v: Vec<Option<u64>>| {
                v.into_iter()
                    .collect::<Option<Vec<_>>>()
                    .map(|v| v.into_iter().sum::<u64>())
                    .map_or_else(|| "—".into(), |v| v.to_string())
            };
            [
                total.map_or_else(|| "—".into(), |v| format!("{:.2} %", v * 100.0)),
                speed_text(
                    tasks
                        .iter()
                        .map(|t| t.speed(Instant::now()))
                        .collect::<Option<Vec<_>>>()
                        .map(|v| v.iter().sum()),
                ),
                sum(tasks.iter().map(|t| t.remaining_files).collect()),
                format!(
                    "{} / {}",
                    sum(tasks
                        .iter()
                        .map(|t| t.active_downloads.map(u64::from))
                        .collect()),
                    pcl_core::network::options().threads
                ),
            ]
        } else {
            [
                task.progress_text(),
                speed_text(task.speed(Instant::now())),
                task.remaining_files
                    .map_or_else(|| "—".into(), |count| count.to_string()),
                task.thread_text(),
            ]
        };
        let labels = TASK_STATISTIC_LABELS;
        let label_height = ui
            .painter()
            .layout_no_wrap(
                labels[0].into(),
                FontId::proportional(14.0),
                theme::palette(ui.ctx()).accent,
            )
            .size()
            .y;
        let value_height = ui
            .painter()
            .layout_no_wrap(
                values[0].clone(),
                FontId::proportional(20.0),
                theme::palette(ui.ctx()).text,
            )
            .size()
            .y;
        let group_height = label_height + 16.0 + value_height;
        let tops = statistic_group_tops(rect, group_height);
        for (index, (label, value)) in labels.iter().zip(values.iter()).enumerate() {
            let y = tops[index];
            ui.painter().text(
                egui::pos2(rect.center().x, y),
                egui::Align2::CENTER_TOP,
                label,
                FontId::proportional(14.0),
                theme::palette(ui.ctx()).accent,
            );
            let line = Rect::from_min_size(
                egui::pos2(rect.left() + 25.0, y + label_height + 7.0),
                Vec2::new((rect.width() - 50.0).max(0.0), 2.0),
            );
            let edge = line.width() * 0.02;
            ui_style::gradient(
                ui.painter(),
                Rect::from_min_max(line.min, egui::pos2(line.left() + edge, line.bottom())),
                [
                    Color32::TRANSPARENT,
                    theme::palette(ui.ctx()).accent,
                    theme::palette(ui.ctx()).accent,
                    Color32::TRANSPARENT,
                ],
            );
            ui.painter().rect_filled(
                Rect::from_min_max(
                    egui::pos2(line.left() + edge, line.top()),
                    egui::pos2(line.right() - edge, line.bottom()),
                ),
                0,
                theme::palette(ui.ctx()).accent,
            );
            ui_style::gradient(
                ui.painter(),
                Rect::from_min_max(egui::pos2(line.right() - edge, line.top()), line.max),
                [
                    theme::palette(ui.ctx()).accent,
                    Color32::TRANSPARENT,
                    Color32::TRANSPARENT,
                    theme::palette(ui.ctx()).accent,
                ],
            );
            ui.painter().text(
                egui::pos2(rect.center().x, y + label_height + 16.0),
                egui::Align2::CENTER_TOP,
                value,
                FontId::proportional(20.0),
                theme::palette(ui.ctx()).text,
            );
            let tooltip = match index {
                0 if tasks.len()>1=>"当前活动任务按任务等权汇总；任一任务尚无进度分母时显示未知。完成记录不参与活动任务统计。",
                0 if !task.components.is_empty()=>"按已完成组件数 / 已声明组件总数统计，子步骤显示实际进度；只有整个任务成功才显示100%。",
                0 if task.has_multiple_plans || !task.overall_plan_known => "总任务包含额外安装阶段，尚无统一的进度分母；各步骤显示真实进度，任务成功后显示 100%。",
                0 if task.uses_vanilla_groups() => "总进度按六个底层安装阶段等权统计，右侧合并为四组展示；任务成功后才显示 100%。",
                0 => "按已声明安装步骤等权统计；任务成功后才显示 100%。尚未提供步骤计划时显示未知。",
                1 => "根据实际网络传输字节的短时变化计算；缓存和本地复制不计入下载速度。",
                2 => "尚待完成校验或下载的文件数量，包含待校验的缓存文件；尚未提供统计时显示未知。",
                _ => "沿用原版标题，数值为活动下载请求数 / 当前阶段实际并发上限；未知显示 —，已明确仅本地处理的阶段为 0 / 0。",
            };
            ui.interact(
                Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    Vec2::new(rect.width(), group_height),
                ),
                ui.id().with(("task-statistic", index)),
                egui::Sense::hover(),
            )
            .on_hover_text(tooltip);
        }
        if task.is_running() {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
    }
    pub(super) fn task_page(&mut self, ui: &mut egui::Ui) {
        self.task_list(ui);
        self.render_current_task(ui);
        self.task_history_ui(ui);
    }
    fn render_current_task(&mut self, ui: &mut egui::Ui) {
        let resource_files = self
            .resource_dependency_plan()
            .map(|files| {
                files
                    .iter()
                    .map(|file| {
                        format!(
                            "{} · {}\n{}{}",
                            file.project,
                            file.version,
                            file.filename,
                            if file.reused {
                                "（复用已安装文件）"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let Some(task) = self.task.as_mut() else {
            return;
        };
        let steps = task.visible_steps();
        let icons = task
            .icons
            .get_or_insert_with(|| TaskIcons::new(ui.ctx()))
            .clone();
        let mut cancel_or_close = false;
        let mut logs = false;
        let mut copied_error = false;
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
            .corner_radius(5)
            .shadow(egui::epaint::Shadow {
                offset: [0, 2],
                blur: 3,
                spread: 0,
                color: Color32::from_black_alpha(9),
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let error_galley = task
                    .error
                    .as_ref()
                    .filter(|_| task.is_failed())
                    .map(|error| {
                        ui.painter().layout(
                            error.clone(),
                            FontId::proportional(14.0),
                            theme::palette(ui.ctx()).text,
                            (ui.available_width() - 79.0).max(30.0),
                        )
                    });
                let extra = usize::from(
                    task.untyped_progress
                        || task.steps.is_empty()
                        || task.status == Status::Cancelling,
                );
                let body_height = error_galley
                    .as_ref()
                    .map_or(((steps.len() + extra) * 26) as f32, |galley| {
                        galley.size().y + 5.0
                    });
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 40.0 + body_height + 10.0),
                    egui::Sense::hover(),
                );
                response.context_menu(|ui| {
                    if !resource_files.is_empty() {
                        ui.menu_button("查看安装文件", |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(280.0)
                                .show(ui, |ui| {
                                    for file in &resource_files {
                                        ui.label(file);
                                        ui.add_space(6.0);
                                    }
                                });
                        });
                    }
                    if ui.button("查看日志").clicked() {
                        logs = true;
                        ui.close();
                    }
                });
                ui_style::place_left(
                    ui,
                    Rect::from_min_size(
                        rect.min + Vec2::new(15.0, 12.0),
                        Vec2::new(rect.width() - 55.0, 18.0),
                    ),
                    egui::Label::new(ui_style::card_title(&task.title))
                        .truncate()
                        .show_tooltip_when_elided(false),
                )
                .on_hover_text(if resource_files.is_empty() {
                    task.title.clone()
                } else {
                    format!("将一起安装：\n{}", resource_files.join("\n\n"))
                });
                let close = Rect::from_min_size(
                    rect.right_top() + Vec2::new(-30.0, 10.0),
                    Vec2::splat(20.0),
                );
                let response = ui.place(
                    close,
                    egui::Button::new("")
                        .fill(Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE),
                );
                let color = if response.hovered() {
                    theme::palette(ui.ctx()).accent
                } else {
                    MUTED
                };
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        if task.is_running() {
                            "取消任务"
                        } else {
                            "关闭任务"
                        },
                    )
                });
                icons.paint(
                    ui,
                    3,
                    Rect::from_center_size(close.center(), Vec2::splat(11.0)),
                    color,
                );
                cancel_or_close = response
                    .on_hover_text(if task.is_running() {
                        "取消"
                    } else {
                        "关闭"
                    })
                    .clicked();
                if let Some(galley) = error_galley {
                    icons.paint(
                        ui,
                        2,
                        Rect::from_center_size(rect.min + Vec2::new(39.0, 48.5), Vec2::splat(15.0)),
                        theme::palette(ui.ctx()).accent,
                    );
                    let text = Rect::from_min_size(rect.min + Vec2::new(64.0, 40.0), galley.size());
                    ui.painter()
                        .galley(text.min, galley, theme::palette(ui.ctx()).text);
                    if ui
                        .interact(text, ui.id().with("task-error-copy"), egui::Sense::click())
                        .on_hover_text("单击复制错误详情")
                        .clicked()
                    {
                        ui.ctx().copy_text(task.error.clone().unwrap_or_default());
                        copied_error = true;
                    }
                } else {
                    for (index, step) in steps.iter().enumerate() {
                        let row = rect.min
                            + Vec2::new(
                                14.0 + step.depth as f32 * 16.0,
                                40.0 + index as f32 * 26.0,
                            );
                        draw_step_status(ui, &icons, row, step);
                        task_row_label(
                            ui,
                            Rect::from_min_size(
                                row + Vec2::new(50.0, 0.0),
                                Vec2::new((rect.right() - 15.0 - row.x - 50.0).max(0.0), 24.0),
                            ),
                            &step.name,
                        )
                        .on_hover_text(&step.detail);
                    }
                    if extra > 0 {
                        let row = rect.min + Vec2::new(14.0, 40.0 + steps.len() as f32 * 26.0);
                        draw_waiting(ui, &icons, row);
                        let message = if task.status == Status::Cancelling {
                            "正在取消…"
                        } else if task.message.is_empty() {
                            "正在准备任务…"
                        } else {
                            &task.message
                        };
                        task_row_label(
                            ui,
                            Rect::from_min_size(
                                row + Vec2::new(50.0, 0.0),
                                Vec2::new(rect.width() - 79.0, 24.0),
                            ),
                            message,
                        )
                        .on_hover_text(&task.message);
                    }
                }
            });
        ui.add_space(15.0);
        if logs {
            self.show_logs = true;
        }
        if copied_error {
            self.status = "已复制错误详情！".into();
        }
        if cancel_or_close {
            if task.is_running() {
                if let Some(active) = self.task_hub.selected.and_then(|id| self.jobs.get(id)) {
                    active.cancel.store(true, Ordering::Relaxed);
                } else {
                    self.cancel.store(true, Ordering::Relaxed);
                }
                task.status = Status::Cancelling;
                task.message = "正在取消…".into();
                self.status = "正在取消…".into();
            } else {
                self.task_view = false;
                // Keep completed task and retry recipe available in task history.
            }
        }
    }
}
fn draw_waiting(ui: &egui::Ui, icons: &TaskIcons, row: egui::Pos2) {
    icons.paint(
        ui,
        0,
        Rect::from_min_size(row + Vec2::new(16.0, 7.0), Vec2::new(18.0, 6.0)),
        theme::palette(ui.ctx()).accent,
    );
}
fn draw_step_status(ui: &egui::Ui, icons: &TaskIcons, row: egui::Pos2, step: &VisibleStep) {
    match step.state {
        StepStatus::Waiting => draw_waiting(ui, icons, row),
        StepStatus::Running => {
            if let Some(fraction) = step.fraction {
                let percentage = (fraction * 100.0).floor().min(99.0);
                ui.painter().text(
                    row + Vec2::new(25.0, 0.0),
                    egui::Align2::CENTER_TOP,
                    format!("{percentage:.0}%"),
                    FontId::proportional(14.0),
                    theme::palette(ui.ctx()).accent,
                );
            } else {
                draw_waiting(ui, icons, row);
            }
        }
        StepStatus::Finished => {
            icons.paint(
                ui,
                1,
                Rect::from_min_size(row + Vec2::new(17.5, 3.0), Vec2::new(15.0, 16.0)),
                theme::palette(ui.ctx()).accent,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcl_core::model::TransferProgress;
    #[test]
    fn thread_statistic_uses_real_samples_and_clears_unknown_stage_capacity() {
        let mut task = TaskState::new("线程 fixture");
        assert_eq!(task.thread_text(), "— / —");
        let update = |task: &mut TaskState, active, limit| {
            task.update(&Progress {
                transfer: Some(TransferProgress {
                    active_downloads: active,
                    concurrency_limit: limit,
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        update(&mut task, Some(3), Some(8));
        assert_eq!(task.thread_text(), "3 / 8");
        task.status = Status::Cancelling;
        assert_eq!(task.thread_text(), "3 / 8");
        update(&mut task, Some(1), None);
        assert_eq!(task.thread_text(), "1 / —");
        update(&mut task, Some(0), Some(0));
        assert_eq!(task.thread_text(), "0 / 0");
        update(&mut task, Some(3), Some(8));
        task.update(&Progress {
            message: "下一未埋点加载器".into(),
            ..Default::default()
        });
        assert_eq!(task.thread_text(), "— / —");
        task.fail("fixture cancellation", true);
        assert_eq!(task.thread_text(), "0 / —");
        // Late telemetry must not reactivate a terminal card.
        update(&mut task, Some(3), Some(8));
        assert_eq!(task.thread_text(), "0 / —");
    }
    #[test]
    fn four_statistic_groups_match_upstream_200_dip_sidebar_star_rows() {
        assert_eq!(
            TASK_STATISTIC_LABELS,
            ["总进度", "下载速度", "剩余文件", "剩余线程"]
        );
        // 4 × (18-DIP label + 16-DIP separator/margins + 26-DIP value)
        // leaves 190 DIP: outer gaps 50, three inner gaps 30.
        let rect = Rect::from_min_size(egui::pos2(10.0, 48.0), Vec2::new(200.0, 430.0));
        let tops = statistic_group_tops(rect, 60.0);
        for (actual, expected) in tops.into_iter().zip([98.0, 188.0, 278.0, 368.0]) {
            assert!((actual - expected).abs() < 0.001);
        }
        assert_eq!(rect.center().x, 110.0);
        assert!((rect.bottom() - (tops[3] + 60.0) - 50.0).abs() < 0.001);
    }
    fn planned_vanilla(grouped: bool) -> TaskState {
        let mut task = TaskState::new("1.21.1 安装");
        task.group_vanilla_install(grouped);
        task.update(&Progress {
            plan: Some(VANILLA_PLAN.to_vec()),
            ..Default::default()
        });
        task
    }
    #[test]
    fn automatic_vanilla_groups_only_real_stages_and_never_finishes_early() {
        let mut task = planned_vanilla(true);
        let rows = task.visible_steps();
        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            [
                "下载原版 json 文件",
                "下载原版支持库文件",
                "下载原版资源文件",
                "安装游戏"
            ]
        );
        assert!(rows.iter().all(|row| row.state == StepStatus::Waiting));
        task.update(&Progress {
            stage: Some(ProgressStage::AssetIndex),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        let rows = task.visible_steps();
        assert_eq!(rows[2].state, StepStatus::Running);
        assert_eq!(rows[2].fraction, Some(4.0 / 18.0));
        task.update(&Progress {
            stage: Some(ProgressStage::AssetFiles),
            stage_progress: Some((0, 0)),
            ..Default::default()
        });
        assert_eq!(task.visible_steps()[2].fraction, None);
        task.update(&Progress {
            stage: Some(ProgressStage::AssetFiles),
            stage_progress: Some((10, 10)),
            ..Default::default()
        });
        assert_eq!(task.visible_steps()[2].state, StepStatus::Finished);
        task.update(&Progress {
            stage: Some(ProgressStage::NativeLibraries),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        assert_eq!(task.visible_steps()[3].state, StepStatus::Running);
        // A terminal result must not invent a missing commit-stage completion.
        task.finish();
        assert_eq!(task.visible_steps()[3].state, StepStatus::Running);
    }
    #[test]
    fn repair_loader_existing_alias_and_multiple_plans_are_not_forced_into_four_rows() {
        assert_eq!(planned_vanilla(false).visible_steps().len(), 6);
        let mut alias = TaskState::new("alias 安装");
        alias.group_vanilla_install(true);
        alias.update(&Progress {
            plan: Some(vec![
                ProgressStage::ExistingVersionValidation,
                ProgressStage::VersionCommit,
            ]),
            ..Default::default()
        });
        let rows = alias.visible_steps();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].name, "登记版本");
        let mut multi = planned_vanilla(true);
        multi.update(&Progress {
            plan: Some(VANILLA_PLAN.to_vec()),
            ..Default::default()
        });
        assert_eq!(multi.visible_steps().len(), 12);
    }
    #[test]
    fn grouping_does_not_create_an_overall_denominator_for_the_task_entry() {
        let mut task = planned_vanilla(true);
        assert_eq!(task.overall_progress(), None);
        task.update(&Progress {
            stage: Some(ProgressStage::VersionMetadata),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        assert_eq!(task.overall_progress(), None);
        task.set_overall_plan_known(true);
        assert_eq!(task.overall_progress(), Some(0.1282));
        task.update(&Progress {
            message: "another uninstrumented operation".into(),
            ..Default::default()
        });
        assert_eq!(task.overall_progress(), None);
    }
    #[test]
    fn installation_title_uses_instance_name_without_rewriting_download_tasks() {
        assert_eq!(download_task_title("正在安装 1.21.1", None), "1.21.1 安装");
        assert_eq!(
            download_task_title("正在安装 Fabric 0.18", Some("我的实例")),
            "我的实例 安装"
        );
        assert_eq!(
            download_task_title("正在下载 Java 21.0.7", Some("unused")),
            "下载 Java 21.0.7"
        );
    }
    #[test]
    fn source_vectors_keep_waiting_dots_separate_and_have_nonempty_bounds() {
        for (index, (_, path)) in TASK_VECTORS.iter().enumerate() {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><path d="{path}" fill="white"/></svg>"#
            );
            let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
            let bounds = tree.root().abs_bounding_box();
            assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
            if index == 0 {
                let scale = 64.0 / bounds.width();
                let mut pixels = resvg::tiny_skia::Pixmap::new(64, 20).unwrap();
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::from_scale(scale, scale)
                        .pre_translate(-bounds.x(), -bounds.y()),
                    &mut pixels.as_mut(),
                );
                let mut runs = 0;
                let mut inside = false;
                for x in 0..64 {
                    let opaque = pixels.pixel(x, 8).unwrap().alpha() > 128;
                    if opaque && !inside {
                        runs += 1;
                    }
                    inside = opaque;
                }
                assert_eq!(runs, 3, "waiting dots must have visible gaps");
            }
        }
    }
    #[test]
    fn indented_step_text_keeps_the_same_card_right_margin() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        let mut task = TaskState::new("nested fixture");
        task.component_plan(vec!["Minecraft".into()]);
        task.component_start(0);
        task.update(&Progress {
            plan: Some(vec![ProgressStage::CoreLibraries]),
            ..Default::default()
        });
        app.task = Some(task);
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        for width in [220.0, 420.0] {
            let mut card_right = 0.0;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 300.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        card_right = ui.max_rect().right();
                        app.render_current_task(ui);
                    });
                },
            );
            let step_clip = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if text.galley.job.text == stage_name(ProgressStage::CoreLibraries) =>
                    {
                        Some(shape.clip_rect)
                    }
                    _ => None,
                })
                .expect("production nested step was painted");
            assert!(
                (step_clip.right() - (card_right - 15.0)).abs() < 0.01,
                "{step_clip:?}, card right {card_right}"
            );
        }
    }

    #[test]
    fn row_title_starts_on_status_baseline_without_advancing_parent_cursor() {
        let ctx = egui::Context::default();
        let mut actual = None;
        let mut before = None;
        let mut after = None;
        let rect = Rect::from_min_size(egui::pos2(64.0, 80.0), Vec2::new(400.0, 24.0));
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(850.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    before = Some(ui.next_widget_position());
                    actual = Some(task_row_label(ui, rect, "下载原版资源文件").rect);
                    after = Some(ui.next_widget_position());
                });
            },
        );
        assert_eq!(actual.unwrap().min, rect.min);
        assert_eq!(before, after);
    }
    #[test]
    fn only_explicit_stage_completion_finishes_a_step_and_global_waits_for_success() {
        let mut task = TaskState::new("测试安装");
        task.set_overall_plan_known(true);
        task.update(&Progress {
            stage: Some(ProgressStage::VersionMetadata),
            plan: Some(vec![
                ProgressStage::VersionMetadata,
                ProgressStage::VersionCommit,
            ]),
            stage_progress: Some((0, 0)),
            ..Default::default()
        });
        task.update(&Progress {
            stage: Some(ProgressStage::VersionCommit),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        assert_eq!(task.steps[0].state, StepStatus::Running);
        assert_eq!(task.progress_text(), "16.66 %");
        task.update(&Progress {
            stage: Some(ProgressStage::VersionMetadata),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        assert_eq!(task.progress_text(), "99.99 %");
        task.finish();
        assert_eq!(task.progress_text(), "100 %");
    }
    #[test]
    fn a_new_pipeline_preserves_history_but_does_not_reuse_finished_stage_counters() {
        let mut task = TaskState::new("带独立名称的安装");
        task.set_overall_plan_known(true);
        task.update(&Progress {
            stage: Some(ProgressStage::VersionCommit),
            plan: Some(vec![ProgressStage::VersionCommit]),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        task.update(&Progress {
            stage: Some(ProgressStage::ExistingVersionValidation),
            plan: Some(vec![
                ProgressStage::ExistingVersionValidation,
                ProgressStage::VersionCommit,
            ]),
            stage_progress: Some((0, 0)),
            ..Default::default()
        });
        assert_eq!(task.steps.len(), 3);
        assert_eq!(task.steps[0].state, StepStatus::Finished);
        assert_eq!(task.steps[2].state, StepStatus::Waiting);
        assert!(task.has_multiple_plans);
        assert_eq!(task.progress_text(), "—");
        task.finish();
        assert_eq!(task.progress_text(), "100 %");
        // Job success cannot invent a per-stage completion event that was never sent.
        assert_eq!(task.steps[2].state, StepStatus::Waiting);
    }
    #[test]
    fn unspecified_counts_never_invent_overall_progress_and_speed_expires() {
        let mut task = TaskState::new("安装");
        task.update(&Progress {
            completed: 5,
            total: 5,
            ..Default::default()
        });
        assert_eq!(task.progress_text(), "—");
        task.update(&Progress {
            plan: Some(vec![ProgressStage::VersionCommit]),
            stage: Some(ProgressStage::VersionCommit),
            stage_progress: Some((1, 1)),
            ..Default::default()
        });
        // A vanilla sub-plan is not the complete plan of a loader/pack job.
        assert_eq!(task.progress_text(), "—");
        for (elapsed_ms, downloaded_bytes) in [(100, 1000), (1100, 3048)] {
            task.update(&Progress {
                transfer: Some(TransferProgress {
                    elapsed_ms,
                    downloaded_bytes,
                    remaining_files: Some(0),
                    active_downloads: None,
                    concurrency_limit: None,
                }),
                ..Default::default()
            });
        }
        assert_eq!(task.speed(Instant::now()), Some(2048.0));
        assert_eq!(
            task.speed(task.transfer_seen.unwrap() + Duration::from_secs(3)),
            Some(0.0)
        );
        assert_eq!(task.remaining_files, Some(0));
        task.update(&Progress {
            message: "读取 Fabric 依赖校验信息".into(),
            ..Default::default()
        });
        assert_eq!(task.speed(Instant::now()), None);
        assert_eq!(task.remaining_files, None);
        task.update(&Progress {
            stage: Some(ProgressStage::NativeLibraries),
            stage_progress: Some((0, 1)),
            ..Default::default()
        });
        assert_eq!(task.speed(Instant::now()), Some(0.0));
        task.fail("取消", true);
        assert!(task.is_finished());
        assert!(!task.is_failed());
    }
    #[test]
    fn declared_weights_accumulate_real_stage_work_and_gate_dependent_components() {
        let mut task = TaskState::new("weighted installation");
        task.component_plan(vec![
            TaskComponentSpec::new("Minecraft and Fabric", 49.0, vec![]).with_vanilla(),
            TaskComponentSpec::new("Instance registration", 2.0, vec![0]),
            TaskComponentSpec::new("Fabric API", 3.0, vec![1]),
        ]);
        task.component_start(2);
        assert_eq!(
            task.current_component, None,
            "a child cannot begin before its declared dependency succeeds"
        );
        task.component_start(0);
        task.update(&Progress {
            plan: Some(VANILLA_PLAN.to_vec()),
            stage: Some(ProgressStage::CoreLibraries),
            stage_progress: Some((1, 2)),
            ..Default::default()
        });
        assert!(
            (task.overall_progress().unwrap() - 7.0 / 54.0).abs() < 1e-9,
            "half of the 14-weight libraries earns 7, not one equally sized phase"
        );
        let rows = task.visible_steps();
        assert_eq!(rows[0].fraction, Some(7.0 / 49.0));
        assert_eq!(rows.last().unwrap().detail, "等待：Instance registration");
        for stage in VANILLA_PLAN {
            task.update(&Progress {
                stage: Some(stage),
                stage_progress: Some((1, 1)),
                ..Default::default()
            });
        }
        assert!((task.overall_progress().unwrap() - 39.0 / 54.0).abs() < 1e-9);
        task.update(&Progress {
            completed: 1,
            total: 1,
            message: "an untyped loader substep".into(),
            ..Default::default()
        });
        assert!(
            (task.overall_progress().unwrap() - 39.0 / 54.0).abs() < 1e-9,
            "one arbitrary substep must not finish a loader"
        );
        task.component_done(0);
        assert!((task.overall_progress().unwrap() - 49.0 / 54.0).abs() < 1e-9);
        task.component_start(1);
        task.component_done(1);
        task.component_start(2);
        task.fail("cancelled", true);
        let before = task.overall_progress();
        task.component_done(2);
        task.component_start(0);
        assert_eq!(
            task.overall_progress(),
            before,
            "late component events cannot rewrite a terminal task"
        );
        assert!(task.overall_progress().unwrap() < 1.0);
    }

    #[test]
    fn invalid_or_replayed_component_graph_cannot_replace_job_identity() {
        let mut task = TaskState::new("fixture");
        task.component_plan(vec![TaskComponentSpec::new("cycle", 1.0, vec![0])]);
        assert!(task.components.is_empty());
        task.component_plan(vec![TaskComponentSpec::new("invalid", f64::NAN, vec![])]);
        assert!(task.components.is_empty());
        task.component_plan(vec![
            TaskComponentSpec::new("first", 3.0, vec![]),
            TaskComponentSpec::new("second", 2.0, vec![0]),
        ]);
        task.component_plan(vec![TaskComponentSpec::new("stale plan", 100.0, vec![])]);
        task.component_start(0);
        task.component_done(0);
        let rows = task.visible_steps().len();
        task.update(&Progress {
            plan: Some(VANILLA_PLAN.to_vec()),
            ..Default::default()
        });
        assert_eq!(
            task.visible_steps().len(),
            rows,
            "progress between components must not attach to the completed parent"
        );
        task.component_start(0);
        assert_eq!(
            task.current_component, None,
            "duplicate start must not reset completed state"
        );
        assert_eq!(task.components[0].name, "first");
        assert_eq!(task.overall_progress(), Some(0.6));
    }

    #[test]
    fn component_tree_preserves_prior_rows_and_never_finishes_before_terminal() {
        let mut task = TaskState::new("组合安装");
        task.component_plan(vec!["原版".into(), "加载器".into()]);
        task.component_start(0);
        task.update(&Progress {
            plan: Some(vec![ProgressStage::VersionMetadata]),
            ..Default::default()
        });
        task.component_done(0);
        task.component_start(1);
        task.update(&Progress {
            plan: Some(vec![ProgressStage::VersionMetadata]),
            ..Default::default()
        });
        let rows = task.visible_steps();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].state, StepStatus::Finished);
        assert_eq!(rows[3].state, StepStatus::Waiting);
        assert_eq!(task.overall_progress(), Some(0.5));
        task.component_done(1);
        assert!(task.overall_progress().unwrap() < 1.0);
        task.finish();
        assert_eq!(task.overall_progress(), Some(1.0));
        assert!(task
            .visible_steps()
            .iter()
            .all(|s| s.state == StepStatus::Finished));
    }
}
