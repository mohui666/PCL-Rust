use super::{
    download_ui::{ListFailure, Phase, VersionListEvent},
    loading_ui, Event, Launcher, MUTED,
};
use crate::theme;
use crate::ui_style;
use anyhow::Context;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};
use pcl_core::loaders::optifine::{self, OptiFineVersion};
use pcl_core::resources::{self, ModrinthVersion};
use pcl_core::{
    config,
    forge::{self, ForgeKind},
    install,
    loaders::{self, LoaderKind, LoaderVersion},
    metadata,
    model::Platform,
};

#[derive(Default)]
pub(super) struct OptiFineState {
    minecraft: Option<String>,
    selected: Option<OptiFineVersion>,
    versions: Vec<OptiFineVersion>,
    phase: Phase,
    indicator: loading_ui::Indicator,
    generation: u64,
    expanded: bool,
    api: CompanionState,
    bridge: CompanionState,
    lite: Option<String>,
}
#[derive(Default)]
struct CompanionState {
    target: Option<(String, String)>,
    selected: Option<ModrinthVersion>,
    versions: Vec<ModrinthVersion>,
    phase: Phase,
    indicator: loading_ui::Indicator,
    generation: u64,
    expanded: bool,
}
pub(crate) enum OptiFineListEvent {
    OptiFine {
        generation: u64,
        minecraft: String,
        result: Result<Vec<OptiFineVersion>, (String, bool)>,
    },
    Companion {
        generation: u64,
        minecraft: String,
        loader: String,
        bridge: bool,
        result: Result<Vec<ModrinthVersion>, (String, bool)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallKind {
    Forge,
    NeoForge,
    Fabric,
    Quilt,
    LiteLoader,
}
impl InstallKind {
    fn label(self) -> &'static str {
        match self {
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
            Self::LiteLoader => "LiteLoader",
        }
    }
    fn meta_kind(self) -> Option<LoaderKind> {
        match self {
            Self::Fabric => Some(LoaderKind::Fabric),
            Self::Quilt => Some(LoaderKind::Quilt),
            _ => None,
        }
    }
    fn forge_kind(self) -> Option<ForgeKind> {
        match self {
            Self::Forge => Some(ForgeKind::Forge),
            Self::NeoForge => Some(ForgeKind::NeoForge),
            _ => None,
        }
    }
    fn default_id(self, minecraft: &str, loader: &str) -> String {
        match self {
            Self::Forge => format!("{minecraft}-forge-{loader}"),
            Self::NeoForge => format!("neoforge-{loader}"),
            Self::Fabric => format!("fabric-loader-{loader}-{minecraft}"),
            Self::Quilt => format!("quilt-loader-{loader}-{minecraft}"),
            Self::LiteLoader => format!("liteloader-{loader}-{minecraft}"),
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Forge => "block-forge",
            Self::NeoForge => "block-neoforge",
            Self::Fabric => "block-fabric",
            Self::Quilt | Self::LiteLoader => "mod",
        }
    }
}
impl Launcher {
    pub(super) fn optifine_request_active(&self) -> bool {
        matches!(self.optifine.phase, Phase::Loading)
            || matches!(self.optifine.api.phase, Phase::Loading)
            || matches!(self.optifine.bridge.phase, Phase::Loading)
    }
    pub(super) fn handle_optifine_list(&mut self, event: OptiFineListEvent) {
        match event {
            OptiFineListEvent::OptiFine {
                generation,
                minecraft,
                result,
            } => {
                if generation != self.optifine.generation
                    || self.optifine.minecraft.as_deref() != Some(&minecraft)
                    || !matches!(self.optifine.phase, Phase::Loading)
                {
                    return;
                }
                self.busy = None;
                match result {
                    Ok(versions) => {
                        self.optifine.versions = versions;
                        self.optifine.phase = Phase::Ready;
                    }
                    Err((_, true)) => self.optifine.phase = Phase::Cancelled,
                    Err((error, false)) => self.optifine.phase = Phase::Failed(error),
                }
            }
            OptiFineListEvent::Companion {
                generation,
                minecraft,
                loader,
                bridge,
                result,
            } => {
                let state = if bridge {
                    &mut self.optifine.bridge
                } else {
                    &mut self.optifine.api
                };
                if generation != state.generation
                    || state.target.as_ref() != Some(&(minecraft, loader))
                    || !matches!(state.phase, Phase::Loading)
                {
                    return;
                }
                self.busy = None;
                match result {
                    Ok(versions) => {
                        state.versions = versions;
                        state.phase = Phase::Ready;
                    }
                    Err((_, true)) => state.phase = Phase::Cancelled,
                    Err((error, false)) => state.phase = Phase::Failed(error),
                }
            }
        }
    }
    fn companion_card(&mut self, ui: &mut egui::Ui, minecraft: &str, bridge: bool) {
        let Some(kind) = self
            .loader_kind
            .filter(|kind| matches!(kind, InstallKind::Fabric | InstallKind::Quilt))
        else {
            return;
        };
        if bridge && kind != InstallKind::Fabric {
            return;
        }
        let loader = if kind == InstallKind::Fabric {
            "fabric"
        } else {
            "quilt"
        };
        let title = if bridge {
            "OptiFabric"
        } else if kind == InstallKind::Fabric {
            "Fabric API"
        } else {
            "Quilt API"
        };
        let project = if bridge {
            "cf:322385"
        } else if kind == InstallKind::Fabric {
            "P7dR8mSH"
        } else {
            "qvIfYCYJ"
        };
        let state = if bridge {
            &mut self.optifine.bridge
        } else {
            &mut self.optifine.api
        };
        let target = (minecraft.to_owned(), loader.to_owned());
        if state.target.as_ref() != Some(&target) {
            *state = CompanionState {
                target: Some(target),
                generation: state.generation.wrapping_add(1),
                ..Default::default()
            };
        }
        let mut fetch = false;
        component_frame().show(ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::click());
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
            ui_style::place_left(
                ui,
                Rect::from_min_size(rect.min + Vec2::new(15.0, 12.0), Vec2::new(110.0, 18.0)),
                egui::Label::new(ui_style::card_title(title)),
            );
            let text = state
                .selected
                .as_ref()
                .map(|v| v.version_number.as_str())
                .unwrap_or("可以添加");
            ui_style::place_left(
                ui,
                Rect::from_min_size(
                    rect.min + Vec2::new(132.0, 11.0),
                    Vec2::new((rect.width() - 210.0).max(40.0), 18.0),
                ),
                egui::Label::new(text),
            );
            let clear = state.selected.is_some()
                && ui
                    .place(
                        Rect::from_min_size(
                            rect.right_top() + Vec2::new(-61.0, 5.0),
                            Vec2::splat(30.0),
                        ),
                        egui::Button::new("×").frame(false),
                    )
                    .clicked();
            if clear {
                state.selected = None;
            }
            let center = rect.right_center() + Vec2::new(-20.0, 0.0);
            let dy = if state.expanded { -3.0 } else { 3.0 };
            ui.painter().add(egui::Shape::line(
                vec![
                    center + Vec2::new(-5.0, -dy),
                    center + Vec2::new(0.0, dy),
                    center + Vec2::new(5.0, -dy),
                ],
                egui::Stroke::new(1.0_f32, theme::palette(ui.ctx()).text),
            ));
            if response.clicked() && !clear {
                state.expanded = !state.expanded;
                fetch = state.expanded && !matches!(state.phase, Phase::Ready | Phase::Loading);
            }
            if state.expanded {
                let status = match &state.phase {
                    Phase::Loading => loading_ui::Status::Running {
                        cancelling: self.cancel.load(std::sync::atomic::Ordering::Relaxed),
                    },
                    Phase::Failed(error) => loading_ui::Status::Failed(error),
                    Phase::Cancelled => loading_ui::Status::Cancelled,
                    _ => loading_ui::Status::Ready,
                };
                match state.indicator.show_status(
                    ui,
                    "正在获取版本列表",
                    status,
                    loading_ui::Placement::Component,
                ) {
                    Some(loading_ui::Action::Retry) => fetch = true,
                    Some(loading_ui::Action::Cancel) => self
                        .cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed),
                    _ => (),
                }
                if matches!(state.phase, Phase::Ready) {
                    if state.versions.is_empty() {
                        ui.label("此 Minecraft 版本没有发行方声明兼容的版本");
                    }
                    for (index, entry) in state.versions.iter().enumerate() {
                        let label = format!(
                            "{}{}",
                            entry.version_number,
                            if index == 0 { "  · 推荐" } else { "" }
                        );
                        if ui
                            .add_sized(
                                [ui.available_width(), 30.0],
                                egui::Button::selectable(
                                    state.selected.as_ref().is_some_and(|v| v.id == entry.id),
                                    label,
                                ),
                            )
                            .clicked()
                        {
                            state.selected = Some(entry.clone());
                            state.expanded = false;
                        }
                    }
                }
            }
        });
        ui.add_space(12.0);
        if fetch {
            let Some((tx, cancel)) = self.start_job("正在获取组件版本") else {
                return;
            };
            let state = if bridge {
                &mut self.optifine.bridge
            } else {
                &mut self.optifine.api
            };
            state.phase = Phase::Loading;
            state.indicator.start();
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            let minecraft = minecraft.to_owned();
            let loader = loader.to_owned();
            std::thread::spawn(move || {
                let result = resources::list_versions(project, &minecraft, &loader, &cancel)
                    .map(|mut versions| {
                        versions.sort_by(|a, b| {
                            (b.version_type == "release")
                                .cmp(&(a.version_type == "release"))
                                .then(b.date_published.cmp(&a.date_published))
                        });
                        versions
                    })
                    .map_err(|error| {
                        let cancelled = error
                            .chain()
                            .any(|cause| cause.is::<pcl_core::model::OperationCancelled>());
                        (format!("获取组件列表失败：{error:#}"), cancelled)
                    });
                let _ = tx.send(Event::OptiFineList(OptiFineListEvent::Companion {
                    generation,
                    minecraft,
                    loader,
                    bridge,
                    result,
                }));
            });
        }
    }
    fn load_optifine(&mut self, minecraft: String) {
        let Some((tx, cancel)) = self.start_job("正在获取 OptiFine 版本") else {
            return;
        };
        self.optifine.generation = self.optifine.generation.wrapping_add(1);
        let generation = self.optifine.generation;
        self.optifine.minecraft = Some(minecraft.clone());
        self.optifine.phase = Phase::Loading;
        self.optifine.indicator.start();
        self.optifine.expanded = true;
        std::thread::spawn(move || {
            let result = optifine::list_versions(&minecraft, &cancel).map_err(|error| {
                let cancelled = error.is::<pcl_core::model::OperationCancelled>()
                    || error
                        .chain()
                        .any(|cause| cause.is::<pcl_core::model::OperationCancelled>());
                (format!("获取 OptiFine 列表失败：{error:#}"), cancelled)
            });
            let _ = tx.send(Event::OptiFineList(OptiFineListEvent::OptiFine {
                generation,
                minecraft,
                result,
            }));
        });
    }
    fn optifine_card(&mut self, ui: &mut egui::Ui, minecraft: &str) {
        if self.optifine.minecraft.as_deref() != Some(minecraft)
            && !matches!(self.optifine.phase, Phase::Loading)
        {
            self.optifine = OptiFineState {
                minecraft: Some(minecraft.into()),
                generation: self.optifine.generation.wrapping_add(1),
                ..Default::default()
            };
        }
        let mut retry = false;
        component_frame().show(ui,|ui| {
            let (rect,response)=ui.allocate_exact_size(Vec2::new(ui.available_width(),40.0),egui::Sense::click());
            response.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Button,self.busy.is_none(),"OptiFine"));
            ui_style::place_left(ui,Rect::from_min_size(rect.min+Vec2::new(15.0,12.0),Vec2::new(110.0,18.0)),egui::Label::new(ui_style::card_title("OptiFine")));
            let text=self.optifine.selected.as_ref().map(|v|v.version.as_str()).unwrap_or("可以添加");
            ui_style::place_left(ui,Rect::from_min_size(rect.min+Vec2::new(132.0,11.0),Vec2::new((rect.width()-210.0).max(40.0),18.0)),egui::Label::new(text));
            let clear_clicked = if self.optifine.selected.is_some() {
                ui.place(Rect::from_min_size(rect.right_top()+Vec2::new(-61.0,5.0),Vec2::splat(30.0)),egui::Button::new("×").frame(false)).on_hover_text("取消选择 OptiFine").clicked()
            } else { false };
            if clear_clicked && self.busy.is_none(){self.optifine.selected=None;}
            let center=rect.right_center()+Vec2::new(-20.0,0.0);
            let dy=if self.optifine.expanded {-3.0} else {3.0};
            ui.painter().add(egui::Shape::line(vec![center+Vec2::new(-5.0,-dy),center+Vec2::new(0.0,dy),center+Vec2::new(5.0,-dy)],egui::Stroke::new(1.0_f32,theme::palette(ui.ctx()).text)));
            if response.clicked() && !clear_clicked && self.busy.is_none(){
                self.optifine.expanded = !self.optifine.expanded;
                retry=self.optifine.expanded && !matches!(self.optifine.phase,Phase::Ready);
            }
            if self.optifine.expanded {
                let status=match &self.optifine.phase {
                    Phase::Loading=>loading_ui::Status::Running{cancelling:self.cancel.load(std::sync::atomic::Ordering::Relaxed)},
                    Phase::Failed(error)=>loading_ui::Status::Failed(error),Phase::Cancelled=>loading_ui::Status::Cancelled,
                    Phase::Idle|Phase::Ready=>loading_ui::Status::Ready};
                match self.optifine.indicator.show_status(ui,"正在获取版本列表",status,loading_ui::Placement::Component) {
                    Some(loading_ui::Action::Retry)=>retry=true,
                    Some(loading_ui::Action::Cancel)=>self.cancel.store(true,std::sync::atomic::Ordering::Relaxed),_=>()}
                if matches!(self.optifine.phase,Phase::Ready) {
                    if self.optifine.versions.is_empty(){ui.label("官方暂无此 Minecraft 版本的 OptiFine");}
                    for entry in &self.optifine.versions {
                        let compatible=match self.loader_kind {None=>true,Some(InstallKind::Forge)=>self.loader_version.as_deref().is_some_and(|forge|entry.compatible_forge(minecraft,forge)),Some(InstallKind::Fabric)=>true,_=>false};
                        let label=format!("{}{}",entry.version,if entry.preview{" · 预览版"}else{""});
                        let response=ui.add_enabled(compatible && self.busy.is_none(),egui::Button::selectable(self.optifine.selected.as_ref()==Some(entry),label));
                        if !compatible {response.clone().on_hover_text("此组合不在 OptiFine 官方 Forge 兼容列表中；Fabric 组合还需要选中兼容的 OptiFabric 桥接。");}
                        if response.clicked(){self.optifine.selected=Some(entry.clone());self.optifine.expanded=false;
                            if !self.install_name_edited {self.install_name=if let (Some(kind),Some(loader))=(self.loader_kind,self.loader_version.as_deref()){format!("{}-OptiFine_{}",kind.default_id(minecraft,loader),entry.version)}else{entry.id()};}}
                    }
                }
                ui.add_space(18.0);
            }
        });
        if retry {
            self.load_optifine(minecraft.into());
        }
    }
    fn load_loader_versions(&mut self, kind: InstallKind, minecraft: String) {
        let Some((tx, cancel)) = self.start_job("正在获取可用加载器") else {
            return;
        };
        self.loader_versions.clear();
        let request = self.version_lists.loader.start();
        self.version_lists.loader_target = Some((minecraft.clone(), kind));
        std::thread::spawn(move || {
            let result = if kind == InstallKind::LiteLoader {
                loaders::liteloader::list_versions(&minecraft, &cancel)
            } else if let Some(kind) = kind.meta_kind() {
                loaders::list_loader_versions(kind, &minecraft, &cancel)
            } else {
                forge::list_versions(kind.forge_kind().unwrap(), &minecraft, &cancel).map(
                    |versions| {
                        versions
                            .into_iter()
                            .map(|version| LoaderVersion {
                                stable: !version.contains("beta"),
                                version,
                            })
                            .collect()
                    },
                )
            };
            let _ = tx.send(Event::VersionList(VersionListEvent::Loader(
                request,
                minecraft,
                kind,
                result.map_err(ListFailure::from_error),
            )));
        });
    }
    fn install_selected_loader(
        &mut self,
        kind: InstallKind,
        minecraft: String,
        loader: String,
        instance_id: String,
    ) {
        let optifine = self.optifine.selected.clone();
        let extra_lite = self.optifine.lite.clone();
        let companions = [&self.optifine.api, &self.optifine.bridge]
            .into_iter()
            .filter(|state| {
                state.target.as_ref().is_some_and(|(mc, loader)| {
                    mc == &minecraft
                        && ((kind == InstallKind::Fabric && loader == "fabric")
                            || (kind == InstallKind::Quilt && loader == "quilt"))
                })
            })
            .filter_map(|state| state.selected.clone())
            .collect::<Vec<_>>();
        let java = self.settings.java_path.clone().or_else(|| {
            let version = metadata::resolve_version(&self.settings.game_root, &minecraft).ok()?;
            let major = version["javaVersion"]["majorVersion"].as_u64().unwrap_or(8);
            self.java
                .iter()
                .find(|runtime| {
                    u64::from(runtime.major) == major
                        && runtime.architecture == Platform::current().arch
                })
                .map(|runtime| runtime.path.clone())
        });
        if kind.forge_kind().is_some()
            && java.is_none()
            && !self
                .settings
                .game_root
                .join("versions")
                .join(kind.default_id(&minecraft, &loader))
                .is_dir()
        {
            self.error=Some(format!("{} 安装处理器需要 Java。请先在设置中选择与 Minecraft {minecraft} 对应的 Java 安装目录。",kind.label()));
            self.page = super::Page::Settings;
            return;
        }
        let Some((tx, _cancel)) = self.start_download_job(
            &format!("正在安装 {} {loader}", kind.label()),
            Some(instance_id.clone()),
        ) else {
            return;
        };
        let root = self.settings.game_root.clone();
        let policy = self.settings.default_isolation;
        let existed = root
            .join("versions")
            .join(&instance_id)
            .join(format!("{instance_id}.json"))
            .exists();
        let registration = loaders::RetryRegistration::default();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let progress = |p| {
                let _ = tx.send(Event::Progress(p));
            };
            let mut parts = if kind == InstallKind::LiteLoader && optifine.is_some() {
                vec![
                    "安装 OptiFine 与 Minecraft".into(),
                    "安装 LiteLoader".into(),
                ]
            } else {
                vec![format!("安装 {} 与 Minecraft", kind.label())]
            };
            if extra_lite.is_some() && kind != InstallKind::LiteLoader {
                parts.push("安装 LiteLoader".into());
            }
            parts.push("登记实例与启动设置".into());
            parts.extend(
                companions
                    .iter()
                    .map(|entry| format!("安装 {}", entry.name)),
            );
            if optifine.is_some() && kind != InstallKind::LiteLoader {
                parts.push("安装 OptiFine".into());
            }
            let _ = tx.send(Event::TaskPlan(parts));
            let mut part = 0usize;
            let _ = tx.send(Event::TaskPart(part));
            let result = if kind == InstallKind::LiteLoader {
                let parent = optifine
                    .as_ref()
                    .map(|entry| {
                        optifine::ensure_optifine(
                            &root,
                            entry,
                            java.as_deref().unwrap_or(std::path::Path::new("java")),
                            &Platform::current(),
                            &cancel,
                            progress,
                        )
                    })
                    .transpose();
                parent.and_then(|parent| {
                    if parent.is_some() {
                        let _ = tx.send(Event::TaskPartDone(part));
                        part += 1;
                        let _ = tx.send(Event::TaskPart(part));
                    }
                    loaders::liteloader::install_liteloader(
                        &root,
                        &minecraft,
                        &loader,
                        parent.as_deref(),
                        &Platform::current(),
                        &cancel,
                        progress,
                    )
                })
            } else if let Some(kind) = kind.meta_kind() {
                loaders::install_loader(
                    &root,
                    kind,
                    &minecraft,
                    &loader,
                    &Platform::current(),
                    &cancel,
                    progress,
                )
            } else {
                pcl_core::packs::ensure_forge_version(
                    &root,
                    kind.forge_kind().unwrap(),
                    &minecraft,
                    &loader,
                    java.as_deref(),
                    &Platform::current(),
                    &cancel,
                    &progress,
                )
            };
            let result = result.and_then(|id| {
                let _ = tx.send(Event::TaskPartDone(part));
                part += 1;
                if kind != InstallKind::LiteLoader {
                    if let Some(lite) = &extra_lite {
                        let _ = tx.send(Event::TaskPart(part));
                        let installed = loaders::liteloader::install_liteloader(
                            &root,
                            &minecraft,
                            lite,
                            Some(&id),
                            &Platform::current(),
                            &cancel,
                            progress,
                        )?;
                        let _ = tx.send(Event::TaskPartDone(part));
                        part += 1;
                        return Ok(installed);
                    }
                }
                Ok(id)
            });
            let result = result.and_then(|id| {
                let _ = tx.send(Event::TaskPart(part));
                registration.register(&root, &id, &instance_id, &Platform::current(), &cancel)
            });
            let result = result.and_then(|id| {
                if !existed {
                    config::initialize_instance_settings(&root, &id, policy)
                        .context("新版本已登记，但默认隔离设置未保存")?;
                }
                let _ = tx.send(Event::TaskPartDone(part));
                part += 1;
                let mod_loader = match kind {
                    InstallKind::Fabric => "fabric",
                    InstallKind::Quilt => "quilt",
                    _ => "",
                };
                let game = config::instance_game_dir(&root, &id)?;
                for entry in &companions {
                    let _ = tx.send(Event::TaskPart(part));
                    anyhow::ensure!(
                        entry.game_versions.iter().any(|v| v == &minecraft)
                            && entry.loaders.iter().any(|v| v == mod_loader),
                        "组件不属于当前游戏和加载器版本"
                    );
                    let plan = resources::plan_mod_install(
                        &game, &entry.id, &minecraft, mod_loader, &cancel,
                    )?;
                    resources::execute_install_plan(&plan, &cancel, progress)
                        .context("加载器已建立，但组件 Mod 安装未完成")?;
                    let _ = tx.send(Event::TaskPartDone(part));
                    part += 1;
                }
                if let Some(entry) = &optifine {
                    if kind != InstallKind::LiteLoader {
                        let _ = tx.send(Event::TaskPart(part));
                    }
                    if kind == InstallKind::Fabric {
                        optifine::install_fabric_mod(&root, &id, entry, &cancel)?;
                    } else if kind != InstallKind::LiteLoader {
                        optifine::install_forge_mod(&root, &id, entry, &loader, &cancel)
                            .context("加载器实例已建立，但 OptiFine Mod 安装未完成")?;
                    }
                    if kind != InstallKind::LiteLoader {
                        let _ = tx.send(Event::TaskPartDone(part));
                    }
                }
                Ok(id)
            });
            let _ = tx.send(match result {
                Ok(id) => Event::Installed(id),
                Err(error) => Event::download_failed("加载器安装未完成", error),
            });
        });
    }
    fn install_named_vanilla(&mut self, minecraft: String, instance_id: String) {
        let Some((tx, _cancel)) = self.start_download_job(
            &format!("正在安装 {instance_id}"),
            Some(instance_id.clone()),
        ) else {
            return;
        };
        let parent_exists = self
            .settings
            .game_root
            .join("versions")
            .join(&minecraft)
            .join(format!("{minecraft}.json"))
            .is_file();
        if let Some(task) = self.task.as_mut() {
            task.set_overall_plan_known(parent_exists || minecraft == instance_id);
        }
        let root = self.settings.game_root.clone();
        let policy = self.settings.default_isolation;
        let existed = root
            .join("versions")
            .join(&instance_id)
            .join(format!("{instance_id}.json"))
            .exists();
        let registration = loaders::RetryRegistration::default();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let _ = tx.send(Event::TaskPlan(if existed {
                vec!["安装 Minecraft 与实例".into()]
            } else {
                vec!["安装 Minecraft 与实例".into(), "初始化启动设置".into()]
            }));
            let _ = tx.send(Event::TaskPart(0));
            let progress = |p| {
                let _ = tx.send(Event::Progress(p));
            };
            let result = if minecraft == instance_id {
                install::install_version(&root, &minecraft, &Platform::current(), &cancel, progress)
                    .map(|()| minecraft)
            } else {
                registration.vanilla(
                    &root,
                    &minecraft,
                    &instance_id,
                    &Platform::current(),
                    &cancel,
                    progress,
                )
            };
            let result = result.and_then(|id| {
                let _ = tx.send(Event::TaskPartDone(0));
                if !existed {
                    let _ = tx.send(Event::TaskPart(1));
                    config::initialize_instance_settings(&root, &id, policy)
                        .context("新版本已登记，但默认隔离设置未保存")?;
                    let _ = tx.send(Event::TaskPartDone(1));
                }
                Ok(id)
            });
            let _ = tx.send(match result {
                Ok(id) => Event::Installed(id),
                Err(error) => Event::download_failed("实例安装未完成", error),
            });
        });
    }
    fn install_selected_optifine(&mut self, entry: OptiFineVersion, instance_id: String) {
        let java = self.settings.java_path.clone().or_else(|| {
            self.java
                .iter()
                .max_by_key(|runtime| runtime.major)
                .map(|runtime| runtime.path.clone())
        });
        let Some(java) = java else {
            self.error = Some("OptiFine 安装器需要 Java，请先到设置选择或下载 Java。".into());
            self.page = super::Page::Settings;
            return;
        };
        let Some((tx, _cancel)) = self.start_download_job(
            &format!("正在安装 {instance_id}"),
            Some(instance_id.clone()),
        ) else {
            return;
        };
        if let Some(task) = self.task.as_mut() {
            task.set_overall_plan_known(false);
            task.group_vanilla_install(false);
        }
        let root = self.settings.game_root.clone();
        let policy = self.settings.default_isolation;
        let registration = loaders::RetryRegistration::default();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let _ = tx.send(Event::TaskPlan(vec![
                "安装 OptiFine 与 Minecraft".into(),
                "登记实例与启动设置".into(),
            ]));
            let _ = tx.send(Event::TaskPart(0));
            let result = optifine::ensure_optifine(
                &root,
                &entry,
                &java,
                &Platform::current(),
                &cancel,
                |p| {
                    let _ = tx.send(Event::Progress(p));
                },
            )
            .and_then(|parent| {
                let _ = tx.send(Event::TaskPartDone(0));
                let _ = tx.send(Event::TaskPart(1));
                registration.register(&root, &parent, &instance_id, &Platform::current(), &cancel)
            })
            .and_then(|id| {
                config::initialize_instance_settings(&root, &id, policy)?;
                let _ = tx.send(Event::TaskPartDone(1));
                Ok(id)
            });
            let _ = tx.send(match result {
                Ok(id) => Event::Installed(id),
                Err(error) => Event::download_failed("OptiFine 安装未完成", error),
            });
        });
    }
    pub(super) fn install_selection_page(&mut self, ui: &mut egui::Ui, minecraft: String) {
        if self.install_name.is_empty() && !self.install_name_edited {
            self.install_name = minecraft.clone();
        }
        let mut back = false;
        let mut open = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            component_frame().show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 62.0),
                    egui::Sense::hover(),
                );
                if ui
                    .ctx()
                    .data_mut(|data| data.remove_temp::<bool>(egui::Id::new("install-page-opened")))
                    == Some(true)
                {
                    ui.scroll_to_rect(rect, Some(egui::Align::TOP));
                }
                let arrow =
                    Rect::from_min_size(rect.min + Vec2::new(13.0, 18.0), Vec2::splat(26.0));
                let response = ui.place(
                    arrow,
                    egui::Button::new("")
                        .fill(Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE),
                );
                back = response.on_hover_text("返回版本列表").clicked();
                let center = arrow.center();
                ui.painter().line_segment(
                    [center + Vec2::new(-6.0, 0.0), center + Vec2::new(6.0, 0.0)],
                    egui::Stroke::new(1.6_f32, MUTED),
                );
                ui.painter().add(egui::Shape::line(
                    vec![
                        center + Vec2::new(-1.0, -5.0),
                        center + Vec2::new(-6.0, 0.0),
                        center + Vec2::new(-1.0, 5.0),
                    ],
                    egui::Stroke::new(1.6_f32, MUTED),
                ));
                self.assets.icon(
                    ui,
                    "block-grass",
                    Rect::from_min_size(rect.min + Vec2::new(52.0, 15.0), Vec2::splat(32.0)),
                    Color32::WHITE,
                );
                let field = Rect::from_min_size(
                    rect.min + Vec2::new(93.0, 16.0),
                    Vec2::new(rect.width() - 109.0, 30.0),
                );
                ui.scope(|ui| {
                    ui.add_enabled_ui(self.game_pid.is_none(), |ui| {
                        if ui
                            .place(
                                field,
                                egui::TextEdit::singleline(&mut self.install_name)
                                    .font(egui::FontId::proportional(15.0))
                                    .char_limit(70)
                                    .hint_text("实例名称")
                                    .margin(Vec2::new(5.0, 5.0)),
                            )
                            .changed()
                        {
                            self.install_name_edited = true;
                        }
                    });
                });
            });
            ui.add_space(27.0);
            for kind in [
                InstallKind::Forge,
                InstallKind::NeoForge,
                InstallKind::Fabric,
            ] {
                self.install_component_card(ui, kind, &minecraft, &mut open);
                ui.add_space(12.0);
            }
            self.optifine_card(ui, &minecraft);
            ui.add_space(12.0);
            self.companion_card(ui, &minecraft, false);
            self.companion_card(ui, &minecraft, true);
            egui::CollapsingHeader::new(
                RichText::new("更多组件（Quilt、LiteLoader）")
                    .size(12.0)
                    .color(MUTED),
            )
            .id_salt("additional-install-components")
            .show(ui, |ui| {
                self.install_component_card(ui, InstallKind::Quilt, &minecraft, &mut open);
                ui.add_space(12.0);
                self.install_component_card(ui, InstallKind::LiteLoader, &minecraft, &mut open);
            });
            let validation = self.install_selection_validation(&minecraft);
            if let Err(error) = &validation {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(error.to_string())
                        .small()
                        .color(Color32::from_rgb(190, 65, 65)),
                );
            }
            // The fixed download button overlays the viewport; leave room to scroll
            // the last component and validation message fully above it.
            ui.add_space(75.0);
        });
        if back {
            self.download_selection = None;
            return;
        }
        if let Some(kind) = open {
            self.loader_expanded = Some(kind);
            self.load_loader_versions(kind, minecraft);
        }
    }
    fn install_selection_validation(&self, minecraft: &str) -> anyhow::Result<()> {
        if let Some(entry) = &self.optifine.selected {
            anyhow::ensure!(
                entry.minecraft == minecraft,
                "OptiFine 属于另一 Minecraft 版本，请重新选择"
            );
            anyhow::ensure!(
                self.install_name != minecraft,
                "OptiFine 实例必须使用独立名称"
            );
            match self.loader_kind {
                None => (),
                Some(InstallKind::Forge) => anyhow::ensure!(
                    self.loader_version
                        .as_deref()
                        .is_some_and(|forge| entry.compatible_forge(minecraft, forge)),
                    "OptiFine 仅兼容官方列表指定的 Forge 版本"
                ),
                Some(InstallKind::Fabric) => anyhow::ensure!(
                    self.optifine
                        .bridge
                        .selected
                        .as_ref()
                        .is_some_and(|v| v.game_versions.iter().any(|mc| mc == minecraft)),
                    "OptiFine 与 Fabric 组合需要选择对应版本的 OptiFabric"
                ),
                _ => anyhow::bail!("当前选择的加载器不能与 OptiFine 自动组合，请取消其中一项"),
            }
        }
        if self.optifine.lite.is_some() {
            anyhow::ensure!(
                self.loader_kind
                    .is_none_or(|kind| kind == InstallKind::Forge),
                "LiteLoader 仅能与原版、Forge 或旧版 OptiFine 组合"
            );
            anyhow::ensure!(
                minecraft.starts_with("1.")
                    && minecraft
                        .split('.')
                        .nth(1)
                        .and_then(|v| v.parse::<u32>().ok())
                        .is_some_and(|minor| minor <= 12),
                "此 Minecraft 版本没有 LiteLoader"
            );
        }
        let source_id = self.selected_install_parent_id(minecraft);
        if self.loader_kind.is_some() && self.install_name == minecraft {
            anyhow::bail!("名称与原版相同，请为加载器实例使用独立名称");
        }
        install::validate_instance_id(&self.settings.game_root, &source_id, &self.install_name)
    }

    fn selected_install_parent_id(&self, minecraft: &str) -> String {
        let parent = if let (Some(kind), Some(version)) =
            (self.loader_kind, self.loader_version.as_deref())
        {
            kind.default_id(minecraft, version)
        } else if let Some(entry) = &self.optifine.selected {
            entry.id()
        } else {
            minecraft.into()
        };
        if let Some(lite) = &self.optifine.lite {
            format!("liteloader-{lite}-{parent}")
        } else {
            parent
        }
    }

    pub(super) fn install_footer(&mut self, ctx: &egui::Context) {
        let Some(minecraft) = self.download_selection.clone() else {
            return;
        };
        let ready =
            self.game_pid.is_none() && self.install_selection_validation(&minecraft).is_ok();
        let screen = ctx.content_rect();
        let center_x = (screen.left() + 135.0 + screen.right()) / 2.0;
        let size = Vec2::new(135.0, 42.0);
        let position = egui::pos2(center_x - size.x / 2.0, screen.bottom() - 20.0 - size.y);
        let clicked = egui::Area::new(egui::Id::new("install-download-footer"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .movable(false)
            .default_size(size)
            .show(ctx, |ui| {
                ui.set_min_size(size);
                ui.set_max_size(size);
                let response = ui.add_enabled(
                    ready,
                    egui::Button::new("")
                        .min_size(size)
                        .fill(theme::palette(ui.ctx()).accent)
                        .corner_radius(23)
                        .stroke(egui::Stroke::NONE),
                );
                let rect = response.rect;
                let color = if ready {
                    Color32::WHITE
                } else {
                    Color32::from_gray(210)
                };
                self.assets.icon(
                    ui,
                    "download",
                    Rect::from_min_size(rect.min + Vec2::new(19.0, 12.0), Vec2::splat(19.0)),
                    color,
                );
                ui.painter().text(
                    rect.min + Vec2::new(83.0, 21.0),
                    egui::Align2::CENTER_CENTER,
                    "开始下载",
                    egui::FontId::proportional(17.0),
                    color,
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, ready, "开始下载")
                });
                response.clicked()
            })
            .inner;
        if clicked {
            if let (Some(kind), Some(version)) = (self.loader_kind, self.loader_version.clone()) {
                self.install_selected_loader(kind, minecraft, version, self.install_name.clone());
            } else if let Some(version) = self.optifine.lite.clone() {
                self.install_selected_loader(
                    InstallKind::LiteLoader,
                    minecraft,
                    version,
                    self.install_name.clone(),
                );
            } else if let Some(entry) = self.optifine.selected.clone() {
                self.install_selected_optifine(entry, self.install_name.clone());
            } else {
                self.install_named_vanilla(minecraft, self.install_name.clone());
            }
        }
    }

    fn install_component_card(
        &mut self,
        ui: &mut egui::Ui,
        kind: InstallKind,
        minecraft: &str,
        open: &mut Option<InstallKind>,
    ) {
        let expanded = self.loader_expanded == Some(kind);
        let selected = if kind == InstallKind::LiteLoader {
            self.optifine.lite.is_some()
        } else {
            self.loader_kind == Some(kind)
        };
        component_frame().show(ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    self.busy.is_none(),
                    kind.label(),
                )
            });
            if response.hovered() {
                ui.painter().rect_filled(
                    rect,
                    5,
                    theme::palette(ui.ctx()).light.gamma_multiply(100.0 / 255.0),
                );
            }
            ui_style::place_left(
                ui,
                Rect::from_min_size(rect.min + Vec2::new(15.0, 12.0), Vec2::new(110.0, 18.0)),
                egui::Label::new(ui_style::card_title(kind.label())).halign(egui::Align::Min),
            );
            let text_x = if selected {
                self.assets.icon(
                    ui,
                    kind.icon(),
                    Rect::from_min_size(rect.min + Vec2::new(132.0, 11.0), Vec2::splat(18.0)),
                    if kind == InstallKind::Quilt {
                        MUTED
                    } else {
                        Color32::WHITE
                    },
                );
                157.0
            } else {
                132.0
            };
            let status = if selected {
                if kind == InstallKind::LiteLoader {
                    self.optifine.lite.as_deref().unwrap_or("可以添加")
                } else {
                    self.loader_version.as_deref().unwrap_or("可以添加")
                }
            } else if self
                .version_lists
                .loader_target
                .as_ref()
                .is_some_and(|(mc, target)| mc == minecraft && *target == kind)
            {
                match &self.version_lists.loader.phase {
                    Phase::Loading => "获取中……",
                    Phase::Failed(_) => "获取失败，点击重试",
                    Phase::Cancelled => "已取消，点击重试",
                    Phase::Ready if self.loader_versions.is_empty() => "无可用版本",
                    _ => "可以添加",
                }
            } else {
                "可以添加"
            };
            ui_style::place_left(
                ui,
                Rect::from_min_size(
                    rect.min + Vec2::new(text_x, 11.0),
                    Vec2::new((rect.width() - text_x - 70.0).max(20.0), 18.0),
                ),
                egui::Label::new(RichText::new(status).color(if selected {
                    theme::palette(ui.ctx()).text
                } else {
                    MUTED
                }))
                .halign(egui::Align::Min),
            );
            let center = rect.right_center() + Vec2::new(-20.0, 0.0);
            let points = if expanded {
                vec![
                    center + Vec2::new(-4.0, -2.0),
                    center + Vec2::new(0.0, 2.0),
                    center + Vec2::new(4.0, -2.0),
                ]
            } else {
                vec![
                    center + Vec2::new(-2.0, -4.0),
                    center + Vec2::new(2.0, 0.0),
                    center + Vec2::new(-2.0, 4.0),
                ]
            };
            ui.painter().add(egui::Shape::line(
                points,
                egui::Stroke::new(1.3_f32, theme::palette(ui.ctx()).text),
            ));
            let mut cleared = false;
            if selected {
                let clear = Rect::from_min_size(
                    rect.right_top() + Vec2::new(-62.0, 5.0),
                    Vec2::splat(30.0),
                );
                if ui
                    .place(
                        clear,
                        egui::Button::new("×")
                            .fill(Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE),
                    )
                    .on_hover_text("取消选择")
                    .clicked()
                    && self.busy.is_none()
                {
                    if kind == InstallKind::LiteLoader {
                        self.optifine.lite = None;
                    } else {
                        self.loader_kind = None;
                        self.loader_version = None;
                    }
                    cleared = true;
                    if !self.install_name_edited {
                        self.install_name = minecraft.to_owned();
                    }
                }
            }
            if response.clicked() && !cleared && self.busy.is_none() {
                let retry = self
                    .version_lists
                    .loader_target
                    .as_ref()
                    .is_some_and(|(mc, target)| mc == minecraft && *target == kind)
                    && matches!(
                        self.version_lists.loader.phase,
                        Phase::Failed(_) | Phase::Cancelled
                    );
                if expanded && !retry {
                    self.loader_expanded = None;
                } else {
                    *open = Some(kind);
                }
            }
            if expanded {
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 20,
                        right: 18,
                        top: 0,
                        bottom: 15,
                    })
                    .show(ui, |ui| {
                        if let Some(action) = self.version_lists.loader.show(
                            ui,
                            self.cancel.load(std::sync::atomic::Ordering::Relaxed),
                            loading_ui::Placement::Component,
                        ) {
                            self.version_list_action(action);
                            if action == loading_ui::Action::Retry && self.busy.is_none() {
                                *open = Some(kind);
                            }
                            return;
                        }
                        if self.loader_versions.is_empty() {
                            ui.label("此 Minecraft 版本暂无可安装版本。");
                            if ui
                                .add_enabled(self.busy.is_none(), egui::Button::new("重新获取"))
                                .clicked()
                            {
                                *open = Some(kind);
                            }
                        }
                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .id_salt(("component-list", kind as u8))
                            .show(ui, |ui| {
                                for version in &self.loader_versions {
                                    let label = format!(
                                        "{}{}",
                                        version.version,
                                        if version.stable { "" } else { "（预览）" }
                                    );
                                    if ui
                                        .selectable_label(
                                            selected
                                                && (if kind == InstallKind::LiteLoader {
                                                    self.optifine.lite.as_deref()
                                                } else {
                                                    self.loader_version.as_deref()
                                                }) == Some(&version.version),
                                            label,
                                        )
                                        .clicked()
                                        && self.busy.is_none()
                                    {
                                        if kind == InstallKind::LiteLoader {
                                            self.optifine.lite = Some(version.version.clone());
                                        } else {
                                            self.loader_kind = Some(kind);
                                            self.loader_version = Some(version.version.clone());
                                        }
                                        self.loader_expanded = None;
                                        if !self.install_name_edited {
                                            self.install_name =
                                                self.selected_install_parent_id(minecraft);
                                        }
                                    }
                                }
                            });
                    });
            }
        });
    }
}
fn component_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
        .corner_radius(5)
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        })
}

#[cfg(test)]
mod optifine_ui_tests {
    use super::*;
    #[test]
    fn companion_response_cannot_replace_another_game_or_cancelled_request() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        app.busy = Some("current".into());
        app.optifine.api.generation = 4;
        app.optifine.api.target = Some(("1.21.1".into(), "fabric".into()));
        app.optifine.api.phase = Phase::Loading;
        app.handle_optifine_list(OptiFineListEvent::Companion {
            generation: 3,
            minecraft: "1.21.1".into(),
            loader: "fabric".into(),
            bridge: false,
            result: Err(("stale".into(), false)),
        });
        assert!(app.busy.is_some());
        assert!(matches!(app.optifine.api.phase, Phase::Loading));
        app.handle_optifine_list(OptiFineListEvent::Companion {
            generation: 4,
            minecraft: "1.21.1".into(),
            loader: "fabric".into(),
            bridge: false,
            result: Err(("cancelled".into(), true)),
        });
        assert!(app.busy.is_none());
        assert!(matches!(app.optifine.api.phase, Phase::Cancelled));
    }
    #[test]
    fn liteloader_is_independent_of_forge_and_invalid_primary_combinations_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        app.loader_kind = Some(InstallKind::Forge);
        app.loader_version = Some("14.23.5.2860".into());
        app.optifine.lite = Some("1.12.2-SNAPSHOT".into());
        app.install_name = "Combined".into();
        assert_eq!(
            app.selected_install_parent_id("1.12.2"),
            "liteloader-1.12.2-SNAPSHOT-1.12.2-forge-14.23.5.2860"
        );
        assert!(app.install_selection_validation("1.12.2").is_ok());
        app.loader_kind = Some(InstallKind::Fabric);
        assert!(app.install_selection_validation("1.12.2").is_err());
        app.loader_kind = None;
        assert_eq!(
            app.selected_install_parent_id("1.12.2"),
            "liteloader-1.12.2-SNAPSHOT-1.12.2"
        );
    }
    #[test]
    fn optifine_list_late_duplicate_and_cancel_results_keep_their_request_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        app.busy = Some("current request".into());
        app.optifine.generation = 2;
        app.optifine.minecraft = Some("1.21.1".into());
        app.optifine.phase = Phase::Loading;
        app.handle_optifine_list(OptiFineListEvent::OptiFine {
            generation: 1,
            minecraft: "1.21.1".into(),
            result: Err(("old failure".into(), false)),
        });
        assert!(app.busy.is_some());
        assert!(matches!(app.optifine.phase, Phase::Loading));
        app.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        app.handle_optifine_list(OptiFineListEvent::OptiFine {
            generation: 2,
            minecraft: "1.21.1".into(),
            result: Err(("real network error".into(), false)),
        });
        assert!(app.busy.is_none());
        assert!(
            matches!(&app.optifine.phase,Phase::Failed(message) if message=="real network error")
        );
        app.busy = Some("different task".into());
        app.handle_optifine_list(OptiFineListEvent::OptiFine {
            generation: 2,
            minecraft: "1.21.1".into(),
            result: Ok(Vec::new()),
        });
        assert!(app.busy.is_some());
        app.optifine.generation = 3;
        app.optifine.phase = Phase::Loading;
        app.handle_optifine_list(OptiFineListEvent::OptiFine {
            generation: 3,
            minecraft: "1.21.1".into(),
            result: Err(("cancelled".into(), true)),
        });
        assert!(app.busy.is_none());
        assert!(matches!(app.optifine.phase, Phase::Cancelled));
    }
}
