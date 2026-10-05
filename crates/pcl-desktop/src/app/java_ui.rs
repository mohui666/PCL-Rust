use super::{account_ui, loading_ui, Event, Launcher, MUTED};
use crate::theme;
use eframe::egui::{self, RichText};
use pcl_core::{
    config::{self, InstanceSettings, Settings},
    java::{self, JavaRuntime},
    java_download::{self, RuntimeDownload},
    java_selection::{self, JavaRequirement, JavaSelectionRequest},
    model::{OperationCancelled, Platform},
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

pub(crate) enum RuntimeEvent {
    Listed(u64, String, Result<Vec<RuntimeDownload>, RuntimeFailure>),
    Installed(JavaRuntime),
    Imported(Result<JavaRuntime, RuntimeFailure>),
    SelectionUnavailable(String, JavaRequirement),
}
pub(crate) struct RuntimeFailure {
    message: String,
    cancelled: bool,
}
#[derive(Default)]
pub(super) struct JavaDownloadState {
    open: bool,
    loading: bool,
    cancelled: bool,
    indicator: loading_ui::Indicator,
    request: u64,
    entries: Vec<RuntimeDownload>,
    selected: Option<usize>,
    major: Option<u32>,
    platform_label: String,
    error: Option<String>,
    installed: Option<JavaRuntime>,
    detecting: bool,
    selection_error: Option<String>,
    download_requirement: Option<JavaRequirement>,
}
impl JavaDownloadState {
    pub(super) fn is_loading(&self) -> bool {
        self.loading
    }
    pub(super) fn has_dialog(&self) -> bool {
        self.open || self.installed.is_some() || self.selection_error.is_some()
    }
    pub(super) fn set_detecting(&mut self, value: bool) {
        self.detecting = value;
    }
    pub(super) fn is_detecting(&self) -> bool {
        self.detecting
    }
    fn selected(&self) -> Option<&RuntimeDownload> {
        self.entries
            .get(self.selected?)
            .filter(|entry| self.major.is_none_or(|major| entry.major == major))
            .filter(|entry| runtime_matches(entry, self.download_requirement.as_ref()))
    }
}
impl Launcher {
    pub(super) fn apply_discovered_java(&mut self, runtimes: Vec<JavaRuntime>) {
        let mut ordered = java_selection::merge_java_priority(
            &runtimes,
            &priority_paths(&self.settings),
            &self.settings.java_excluded,
        );
        // A saved pre-migration selection remains preferred until the user changes the list.
        if let Some(path) = &self.settings.java_path {
            if let Some(index) = ordered
                .iter()
                .position(|runtime| same_java_path(&runtime.path, path))
            {
                let selected = ordered.remove(index);
                ordered.insert(0, selected);
            }
        }
        let mut next = self.settings.clone();
        next.java_priority = ordered.iter().map(|runtime| runtime.path.clone()).collect();
        if next.java_priority != self.settings.java_priority {
            if let Err(error) = config::save_settings(&self.settings_path, &next) {
                self.error = Some(format!("Java 已检测，但优先顺序未保存：{error:#}"));
            } else {
                self.settings = next;
            }
        }
        self.java = ordered;
        self.status = format!("已找到 {} 个 Java", self.java.len());
    }
    fn save_java_preferences(&mut self, next: Settings) -> bool {
        match config::save_settings(&self.settings_path, &next) {
            Ok(()) => {
                self.settings = next;
                self.java_text = self
                    .settings
                    .java_path
                    .as_ref()
                    .map_or_else(String::new, |p| p.display().to_string());
                true
            }
            Err(error) => {
                self.error = Some(format!("Java 设置未应用：{error:#}"));
                false
            }
        }
    }
    pub(super) fn prefer_java(&mut self, runtime: JavaRuntime) -> bool {
        let next = preferences_for_import(&self.settings, &runtime.path);
        if !self.save_java_preferences(next) {
            return false;
        }
        self.java
            .retain(|other| !same_java_path(&other.path, &runtime.path));
        self.java.insert(0, runtime);
        self.status = "Java 已加入列表顶部；启动时仍检查版本范围。".into();
        true
    }
    pub(super) fn move_java_priority(&mut self, index: usize, offset: isize) {
        let Some(target) = index
            .checked_add_signed(offset)
            .filter(|target| *target < self.java.len())
        else {
            return;
        };
        if index >= self.java.len() {
            return;
        }
        let mut ordered = self.java.clone();
        ordered.swap(index, target);
        let mut next = self.settings.clone();
        next.java_priority = ordered.iter().map(|r| r.path.clone()).collect();
        next.java_path = None;
        if self.save_java_preferences(next) {
            self.java = ordered;
            self.status = "Java 优先顺序已保存".into();
        }
    }
    pub(super) fn exclude_java(&mut self, path: &Path) {
        if is_official_runtime(path) {
            self.error = Some("无法从列表移除官方 Java。".into());
            return;
        }
        let mut next = self.settings.clone();
        next.java_priority = priority_paths(&next);
        next.java_priority.retain(|p| !same_java_path(p, path));
        if !next.java_excluded.iter().any(|p| same_java_path(p, path)) {
            next.java_excluded.push(path.to_owned());
        }
        if next
            .java_path
            .as_ref()
            .is_some_and(|p| same_java_path(p, path))
        {
            next.java_path = None;
        }
        if self.save_java_preferences(next) {
            self.java.retain(|r| !same_java_path(&r.path, path));
            self.status = "已从自动选择列表移除；Java 文件没有删除。".into();
        }
    }
    pub(super) fn restore_excluded_java(&mut self, path: &Path) {
        let mut next = self.settings.clone();
        next.java_excluded.retain(|p| !same_java_path(p, path));
        if !next.java_priority.iter().any(|p| same_java_path(p, path)) {
            next.java_priority.push(path.to_owned());
        }
        if self.save_java_preferences(next) {
            self.detect_java();
        }
    }
    pub(super) fn import_global_java(&mut self, path: PathBuf) {
        let Some((tx, cancel)) = self.start_job("正在验证导入的 Java") else {
            return;
        };
        // An older search may finish while this explicit import is in progress.
        self.java_request = self.java_request.wrapping_add(1);
        self.java_download.set_detecting(false);
        std::thread::spawn(move || {
            let result =
                java::inspect_java_with_cancel(&path, &cancel).map_err(|error| RuntimeFailure {
                    cancelled: error.chain().any(|cause| cause.is::<OperationCancelled>()),
                    message: format!("Java 导入未完成：{error:#}"),
                });
            let _ = tx.send(Event::Runtime(RuntimeEvent::Imported(result)));
        });
    }
    pub(super) fn open_java_downloads(&mut self) {
        self.open_java_downloads_for(None);
    }
    fn open_java_downloads_for(&mut self, requirement: Option<JavaRequirement>) {
        let Some((tx, cancel)) = self.start_job("正在获取官方 Java 列表") else {
            return;
        };
        self.java_download.download_requirement = requirement;
        self.java_download.open = true;
        self.java_download.loading = true;
        self.java_download.cancelled = false;
        self.java_download.indicator.start();
        self.java_download.selected = None;
        self.java_download.error = None;
        self.java_download.entries.clear();
        self.java_download.request = self.java_download.request.wrapping_add(1);
        let request = self.java_download.request;
        std::thread::spawn(move || {
            let platform = Platform::current();
            let label = platform_label(&platform);
            let result =
                java_download::list_runtimes(&platform, &cancel).map_err(|error| RuntimeFailure {
                    cancelled: error.is::<OperationCancelled>()
                        || error.chain().any(|cause| cause.is::<OperationCancelled>()),
                    message: format!("Java 列表获取失败：{error:#}"),
                });
            let _ = tx.send(Event::Runtime(RuntimeEvent::Listed(request, label, result)));
        });
    }
    fn install_selected_java(&mut self) {
        let Some(target) = self.java_download.selected().cloned() else {
            self.java_download.error = Some("请先选择一个 Java 版本。".into());
            return;
        };
        let root = java_download::runtime_root();
        let Some((tx, cancel)) = self.start_download_job_at(
            &format!("正在下载 Java {}", target.version),
            root.clone(),
            Some(target.version.clone()),
        ) else {
            return;
        };
        self.java_download.open = false;
        self.java_download.error = None;
        self.java_download.installed = None;
        std::thread::spawn(move || {
            let result =
                java_download::download_runtime(&root, &target, &cancel, |mut progress| {
                    // These are verified file counts, not an invented whole-task percentage.
                    if progress.total > 0 {
                        progress.message = format!(
                            "文件 {}/{} · {}",
                            progress.completed, progress.total, progress.message
                        );
                    }
                    let _ = tx.send(Event::Progress(progress));
                });
            let event = match result {
                Ok(runtime) => Event::Runtime(RuntimeEvent::Installed(runtime)),
                Err(error) => Event::download_failed("Java 下载未完成", error),
            };
            let _ = tx.send(event);
        });
    }
    pub(super) fn handle_runtime_event(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::SelectionUnavailable(message, requirement) => {
                self.java_download.download_requirement = Some(requirement);
                self.busy = None;
                self.java_download.selection_error = Some(message);
                self.status = "没有找到兼容 Java".into();
            }
            RuntimeEvent::Imported(result) => {
                self.busy = None;
                match result {
                    Ok(runtime) => {
                        self.prefer_java(runtime);
                    }
                    Err(error) if error.cancelled => self.status = "Java 导入已取消".into(),
                    Err(error) => self.error = Some(error.message),
                }
            }
            RuntimeEvent::Listed(request, label, result) => {
                if request != self.java_download.request || !self.java_download.loading {
                    return;
                }
                self.busy = None;
                self.java_download.loading = false;
                self.java_download.platform_label = label;
                match result {
                    Ok(entries) => {
                        self.java_download.major = None;
                        let requirement = self.java_download.download_requirement.as_ref();
                        let compatible: Vec<_> = entries
                            .iter()
                            .enumerate()
                            .filter(|(_, entry)| runtime_matches(entry, requirement))
                            .collect();
                        self.java_download.selected = requirement.and_then(|req| {
                            compatible
                                .iter()
                                .find(|(_, entry)| {
                                    req.recommended_component.as_deref() == Some(&entry.component)
                                })
                                .or_else(|| compatible.first())
                                .map(|(index, _)| *index)
                        });
                        self.java_download.entries = entries;
                        self.status = "官方 Java 列表已更新".into();
                    }
                    Err(error) => {
                        self.java_download.cancelled = error.cancelled;
                        self.status = if error.cancelled {
                            "Java 列表获取已取消"
                        } else {
                            "Java 列表获取失败"
                        }
                        .into();
                        if !error.cancelled {
                            self.java_download.error = Some(error.message);
                        }
                    }
                }
            }
            RuntimeEvent::Installed(runtime) => {
                self.finish_download_task();
                self.busy = None;
                self.progress = None;
                self.status = format!("Java {} 已下载并验证", runtime.major);
                // Merely finishing a download must not move it ahead of the current Java.
                let next = preferences_after_download(&self.settings, &runtime.path);
                let order_saved = self.save_java_preferences(next);
                if !self
                    .java
                    .iter()
                    .any(|r| same_java_path(&r.path, &runtime.path))
                {
                    self.java.push(runtime.clone());
                }
                self.java_download.installed = Some(runtime);
                // A failed save must not reclassify this download as a new discovery
                // and move it ahead of the user's existing preference in memory.
                if order_saved {
                    self.detect_java();
                }
            }
        }
    }
    pub(super) fn java_download_dialog(&mut self, ctx: &egui::Context) {
        if let Some(message) = self.java_download.selection_error.clone() {
            if let Some(action) = account_ui::account_modal(
                ctx,
                "java-selection-missing",
                "没有找到兼容 Java",
                &message,
                &["下载 Java", "Java 设置", "取消"],
            ) {
                self.java_download.selection_error = None;
                if action == 0 {
                    let requirement = self.java_download.download_requirement.clone();
                    self.open_java_downloads_for(requirement);
                } else if action == 1 {
                    self.open_java_settings();
                }
            }
            return;
        }
        if let Some(runtime) = self.java_download.installed.clone() {
            let caption = format!("Java {}（{}）已安装并验证。\n{}\n\n可以继续使用当前 Java，也可以将此版本加入全局列表顶部。自动选择仍检查兼容范围，实例的单独设置优先生效。",runtime.major,runtime.architecture,runtime.path.display());
            if let Some(action) = account_ui::account_modal(
                ctx,
                "java-installed",
                "Java 下载完成",
                &caption,
                &["选用此 Java", "完成"],
            ) {
                if action != 0 || self.prefer_java(runtime) {
                    self.java_download.installed = None;
                }
            }
            return;
        }
        if !self.java_download.open {
            return;
        }
        let width = (ctx.content_rect().width() - 50.0).clamp(400.0, 600.0);
        let height = (ctx.content_rect().height() - 225.0).clamp(180.0, 320.0);
        let mut selected = self.java_download.selected;
        let mut major = self.java_download.major;
        let loading = self.java_download.loading;
        let cancelling = loading && self.cancel.load(Ordering::Relaxed);
        let entries = &self.java_download.entries;
        let requirement = self.java_download.download_requirement.as_ref();
        let mut majors: Vec<u32> = entries
            .iter()
            .filter(|entry| runtime_matches(entry, requirement))
            .map(|entry| entry.major)
            .collect();
        majors.sort_unstable_by(|a, b| b.cmp(a));
        majors.dedup();
        let mut download = false;
        let mut retry = false;
        let action = account_ui::modal_frame(
            ctx,
            "java-download",
            "下载 Java",
            width,
            height,
            if cancelling {
                &["取消中…"]
            } else if loading {
                &["取消"]
            } else {
                &["刷新列表", "关闭"]
            },
            |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · Mojang 官方运行时",
                        if self.java_download.platform_label.is_empty() {
                            "当前系统"
                        } else {
                            &self.java_download.platform_label
                        }
                    ))
                    .size(15.0)
                    .color(theme::palette(ui.ctx()).text),
                );
                ui.add_space(8.0);
                if let Some(requirement) = requirement {
                    ui.label(
                        RichText::new(format!(
                            "游戏要求 {}；仅显示符合范围的官方版本。",
                            requirement.range
                        ))
                        .size(12.0)
                        .color(MUTED),
                    );
                    ui.add_space(6.0);
                }
                let status = if loading {
                    loading_ui::Status::Running { cancelling }
                } else if self.java_download.cancelled {
                    loading_ui::Status::Cancelled
                } else if let Some(error) = &self.java_download.error {
                    loading_ui::Status::Failed(error)
                } else {
                    loading_ui::Status::Ready
                };
                if let Some(action) = self.java_download.indicator.show_status(
                    ui,
                    "正在获取版本列表",
                    status,
                    loading_ui::Placement::Dialog,
                ) {
                    retry = action == loading_ui::Action::Retry;
                } else {
                    ui.horizontal_wrapped(|ui| {
                        if ui.selectable_label(major.is_none(), "全部").clicked() {
                            major = None;
                            selected = None;
                        }
                        for &value in &majors {
                            if ui
                                .selectable_label(major == Some(value), format!("Java {value}"))
                                .clicked()
                            {
                                major = Some(value);
                                selected = None;
                            }
                        }
                    });
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("java-download-list")
                        .max_height(height - if requirement.is_some() {160.0} else {138.0})
                        .show(ui, |ui| {
                            for (index, entry) in entries
                                .iter()
                                .enumerate()
                                .filter(|(_, entry)| major.is_none_or(|value| entry.major == value) && runtime_matches(entry,requirement))
                            {
                                let response = ui.add_sized(
                                    [ui.available_width(), 28.0],
                                    egui::Button::new(format!("Java {}", entry.version))
                                        .selected(selected == Some(index)),
                                );
                                if response.clicked() {
                                    selected = Some(index);
                                }
                                response.on_hover_text(format!(
                                    "{}\n{}",
                                    entry.component, entry.platform
                                ));
                            }
                            if !entries.iter().any(|entry| runtime_matches(entry,requirement)) {
                                ui.label(
                                    RichText::new(if requirement.is_some() {"官方列表没有当前平台满足此范围的 Java。可返回设置，手动导入对应版本。"}else{"官方列表没有当前平台可安装的 Java。"})
                                        .color(MUTED),
                                );
                            }
                        });
                    ui.add_space(8.0);
                    let can_download =
                        selected
                            .and_then(|index| entries.get(index))
                            .is_some_and(|entry| {
                                major.is_none_or(|value| value == entry.major)
                                    && runtime_matches(entry, requirement)
                            });
                    if ui
                        .add_enabled(
                            can_download && self.busy.is_none(),
                            egui::Button::new(
                                RichText::new("下载所选 Java").color(egui::Color32::WHITE),
                            )
                            .fill(theme::palette(ui.ctx()).accent)
                            .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                    {
                        download = true;
                    }
                }
            },
        );
        self.java_download.selected = selected;
        self.java_download.major = major;
        if download {
            self.install_selected_java();
        }
        if retry && self.busy.is_none() {
            self.open_java_downloads_for(self.java_download.download_requirement.clone());
            return;
        }
        match action {
            Some(0) if !loading && self.busy.is_none() => {
                self.open_java_downloads_for(self.java_download.download_requirement.clone())
            }
            Some(0) if loading => {
                self.cancel.store(true, Ordering::Relaxed);
                self.status = "正在取消 Java 列表获取…".into();
            }
            Some(1) => {
                if loading {
                    self.cancel.store(true, Ordering::Relaxed);
                }
                self.java_download.open = false;
            }
            _ => (),
        }
    }
}
fn platform_label(platform: &Platform) -> String {
    let os = match platform.os.as_str() {
        "osx" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    let arch = match platform.arch.as_str() {
        "aarch64" | "arm64" => "ARM64",
        "x86_64" | "amd64" => "64 位",
        "x86" | "i686" => "32 位",
        other => other,
    };
    format!("{os} · {arch}")
}

/// Keep the legacy single selection as priority, never as an override of instance modes.
pub(super) fn priority_paths(settings: &Settings) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for path in settings
        .java_path
        .iter()
        .chain(settings.java_priority.iter())
    {
        if !paths.iter().any(|p| same_java_path(p, path)) {
            paths.push(path.clone());
        }
    }
    paths
}
fn preferred_paths(settings: &Settings, path: &Path) -> Vec<PathBuf> {
    let mut paths = priority_paths(settings);
    paths.retain(|p| !same_java_path(p, path));
    paths.insert(0, path.to_owned());
    paths
}
pub(super) fn same_java_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_owned());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_owned());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}
pub(super) fn is_official_runtime(path: &Path) -> bool {
    let root = java_download::runtime_root();
    let root = root.canonicalize().unwrap_or(root);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    path.starts_with(root)
}
pub(super) fn selection_request(
    settings: &Settings,
    instance: &InstanceSettings,
    id: &str,
) -> JavaSelectionRequest {
    JavaSelectionRequest {
        root: settings.game_root.clone(),
        version_id: id.to_owned(),
        mode: java_selection::effective_mode(instance.java_mode, instance.java_path.as_deref()),
        version_range: instance.java_range.clone(),
        specified_path: instance.java_path.clone(),
        priority: priority_paths(settings),
        excluded: settings.java_excluded.clone(),
    }
}

fn runtime_matches(entry: &RuntimeDownload, requirement: Option<&JavaRequirement>) -> bool {
    requirement.is_none_or(|req| {
        java::JavaVersion::from_runtime(&entry.version)
            .is_ok_and(|version| req.range.contains(version))
    })
}

fn preferences_for_import(settings: &Settings, path: &Path) -> Settings {
    let mut next = settings.clone();
    next.java_priority = preferred_paths(settings, path);
    next.java_excluded.retain(|p| !same_java_path(p, path));
    next.java_path = None;
    next
}
fn preferences_after_download(settings: &Settings, path: &Path) -> Settings {
    let mut next = settings.clone();
    next.java_priority = priority_paths(settings);
    if !next.java_priority.iter().any(|p| same_java_path(p, path)) {
        next.java_priority.push(path.to_owned());
    }
    next
}

/// Directory grants on macOS must cover the JDK libraries loaded by the probe.
pub(super) fn pick_java_path() -> Option<Result<PathBuf, String>> {
    #[cfg(target_os = "macos")]
    {
        rfd::FileDialog::new()
            .set_title("选择 Java 安装目录（JDK 文件夹或 Contents/Home）")
            .pick_folder()
            .map(|folder| {
                [
                    folder.join("Contents/Home/bin/java"),
                    folder.join("bin/java"),
                ]
                .into_iter()
                .find(|path| path.is_file())
                .ok_or_else(|| "所选目录中没有 bin/java，请选择 Java 安装目录。".to_owned())
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        rfd::FileDialog::new()
            .set_title("选择 bin/java 或 java.exe")
            .pick_file()
            .map(Ok::<_, String>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcl_core::java_selection::JavaSelectionMode;
    #[test]
    fn java_list_stale_and_duplicate_terminals_preserve_the_new_operation() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temp.path());
        app.java_download.request = 2;
        app.java_download.loading = true;
        app.busy = Some("new Java list".into());
        app.handle_runtime_event(RuntimeEvent::Listed(1, "old platform".into(), Ok(vec![])));
        assert!(app.java_download.loading);
        assert_eq!(app.busy.as_deref(), Some("new Java list"));
        app.cancel.store(true, Ordering::Relaxed);
        app.handle_runtime_event(RuntimeEvent::Listed(2, "macOS ARM64".into(), Ok(vec![])));
        assert!(!app.java_download.loading);
        assert!(!app.java_download.cancelled);
        assert!(app.java_download.error.is_none());
        assert!(app.busy.is_none());
        app.busy = Some("next task".into());
        app.handle_runtime_event(RuntimeEvent::Listed(
            2,
            "old platform".into(),
            Err(RuntimeFailure {
                message: "duplicate".into(),
                cancelled: false,
            }),
        ));
        assert_eq!(app.busy.as_deref(), Some("next task"));
        assert!(app.java_download.error.is_none());
    }
    #[test]
    fn java_list_cancellation_and_error_stay_inside_the_list_dialog() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temp.path());
        app.java_download.open = true;
        for cancelled in [true, false] {
            app.java_download.request += 1;
            app.java_download.loading = true;
            app.java_download.cancelled = false;
            app.java_download.error = None;
            app.busy = Some("Java list".into());
            app.cancel.store(true, Ordering::Relaxed);
            app.handle_runtime_event(RuntimeEvent::Listed(
                app.java_download.request,
                "macOS ARM64".into(),
                Err(RuntimeFailure {
                    message: "HTTP 503".into(),
                    cancelled,
                }),
            ));
            assert_eq!(app.java_download.cancelled, cancelled);
            assert_eq!(app.java_download.error.is_some(), !cancelled);
            assert!(app.java_download.open);
            assert!(app.error.is_none());
            assert!(app.busy.is_none());
        }
    }
    #[test]
    fn legacy_global_preference_never_overrides_instance_modes_or_specific_path() {
        let global = Settings {
            java_path: Some(PathBuf::from("/fixture/legacy/bin/java")),
            java_priority: vec![
                PathBuf::from("/fixture/second/bin/java"),
                PathBuf::from("/fixture/legacy/bin/java"),
            ],
            java_excluded: vec![PathBuf::from("/fixture/excluded/bin/java")],
            ..Settings::default()
        };
        for mode in [
            JavaSelectionMode::Automatic,
            JavaSelectionMode::VersionRange,
            JavaSelectionMode::VersionFolder,
            JavaSelectionMode::Specific,
        ] {
            let instance = InstanceSettings {
                java_mode: Some(mode),
                java_range: "[17,22)".into(),
                java_path: Some(PathBuf::from("/fixture/instance/bin/java")),
                ..Default::default()
            };
            let request = selection_request(&global, &instance, "1.21.1");
            assert_eq!(request.mode, mode);
            assert_eq!(request.specified_path, instance.java_path);
            assert_eq!(request.version_range, "[17,22)");
            assert_eq!(
                request.priority,
                vec![
                    PathBuf::from("/fixture/legacy/bin/java"),
                    PathBuf::from("/fixture/second/bin/java")
                ]
            );
            assert_eq!(request.excluded, global.java_excluded);
        }
        let request = selection_request(&global, &InstanceSettings::default(), "1.21.1");
        assert_eq!(request.mode, JavaSelectionMode::Automatic);
        assert_eq!(request.specified_path, None);
    }
    #[test]
    fn download_completion_preserves_old_choice_and_explicit_import_unexcludes() {
        let old = PathBuf::from("/fixture/old/bin/java");
        let new = PathBuf::from("/fixture/new/bin/java");
        let settings = Settings {
            java_path: Some(old.clone()),
            java_priority: vec![old.clone()],
            java_excluded: vec![new.clone()],
            ..Default::default()
        };
        let completed = preferences_after_download(&settings, &new);
        assert_eq!(completed.java_path, Some(old.clone()));
        assert_eq!(completed.java_priority, vec![old.clone(), new.clone()]);
        assert_eq!(completed.java_excluded, vec![new.clone()]);
        let explicit = preferences_for_import(&completed, &new);
        assert_eq!(explicit.java_path, None);
        assert_eq!(explicit.java_priority, vec![new, old]);
        assert!(explicit.java_excluded.is_empty());
        assert_eq!(settings.java_priority.len(), 1);
    }
}
