//! Community resources use PageResource.xaml / MyResourceItem.xaml geometry.
use super::{loading_ui, Event, Launcher, MUTED};
use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};
use pcl_core::{
    config, metadata,
    resources::{self, ResourceKind},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Default)]
pub(super) struct ResourceBrowser {
    kind: ResourceKind,
    provider: resources::ResourceProvider,
    generation: u64,
    epoch: Arc<()>,
    pending: Option<RequestKey>,
    plan_key: Option<RequestKey>,
    dependency_plan: Vec<resources::PlannedResource>,
    cancel: Option<Arc<AtomicBool>>,
    query: String,
    minecraft: String,
    loader: String,
    category: String,
    last_category: String,
    page: Option<resources::SearchPage>,
    last_search: Option<resources::SearchOptions>,
    last_attempt: Option<(resources::SearchOptions, String)>,
    project: Option<resources::ModrinthProject>,
    detail_id: Option<String>,
    detail_hit: Option<resources::ProjectHit>,
    page_error: Option<String>,
    cancelled: bool,
    loading: bool,
    loading_indicator: loading_ui::Indicator,
    version_filter: String,
    filter_scheme: (bool, bool),
    filters: Vec<String>,
    open_groups: HashSet<String>,
    install_selection: Option<String>,
    versions: Vec<resources::ModrinthVersion>,
    target: Option<String>,
    world: Option<PathBuf>,
    pack_name: String,
    pack_optional: bool,
    local_pack: bool,
    auto_searched: bool,
    icons: HashMap<String, egui::TextureHandle>,
    images: Vec<(String, Vec<u8>)>,
}

#[derive(Clone)]
pub(crate) struct RequestKey {
    kind: ResourceKind,
    generation: u64,
    root: PathBuf,
    epoch: Arc<()>,
}
impl RequestKey {
    fn same(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.generation == other.generation
            && self.root == other.root
            && Arc::ptr_eq(&self.epoch, &other.epoch)
    }
}
impl ResourceBrowser {
    fn accepts(&self, key: &RequestKey, root: &std::path::Path) -> bool {
        self.kind == key.kind
            && self.generation == key.generation
            && key.root == root
            && Arc::ptr_eq(&self.epoch, &key.epoch)
    }
    pub(super) fn scroll_key(&self) -> String {
        format!(
            "{:?}-{}",
            self.kind,
            self.detail_id.clone().unwrap_or_else(|| format!(
                "search-{}",
                self.page.as_ref().map_or(0, |p| p.offset)
            ))
        )
    }
}

pub(crate) enum ResourceEvent {
    Search(
        RequestKey,
        resources::SearchPage,
        resources::SearchOptions,
        String,
    ),
    Detail(
        RequestKey,
        Box<resources::ModrinthProject>,
        Vec<resources::ModrinthVersion>,
    ),
    Icon(RequestKey, String, Vec<u8>),
    Failed(RequestKey, String),
    Cancelled(RequestKey),
    Installed(String, String),
    Plan(RequestKey, Vec<resources::PlannedResource>),
    PlanFinished(RequestKey),
}

fn tab_kind(tab: usize) -> ResourceKind {
    match tab {
        2 => ResourceKind::Modpack,
        3 => ResourceKind::DataPack,
        4 => ResourceKind::ResourcePack,
        5 => ResourceKind::Shader,
        _ => ResourceKind::Mod,
    }
}
impl Launcher {
    pub(super) fn resource_request_active(&self) -> bool {
        self.resource_browser.loading
            && self
                .resource_browser
                .pending
                .as_ref()
                .is_some_and(|key| self.resource_browser.accepts(key, &self.settings.game_root))
    }

    pub(super) fn resource_detail_title(&self) -> Option<&str> {
        self.resource_browser.detail_id.as_ref()?;
        Some(
            self.resource_browser
                .project
                .as_ref()
                .map(|p| p.title.as_str())
                .or_else(|| {
                    self.resource_browser
                        .detail_hit
                        .as_ref()
                        .map(|p| p.title.as_str())
                })
                .unwrap_or("资源详情"),
        )
    }
    pub(super) fn leave_resource_detail(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let state = &mut self.resource_browser;
        if let Some(cancel) = state.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        state.generation += 1;
        state.detail_id = None;
        state.detail_hit = None;
        state.project = None;
        state.page_error = None;
        state.cancelled = false;
        state.versions.clear();
        state.open_groups.clear();
        state.install_selection = None;
    }
    pub(super) fn resource_dependency_plan(&self) -> Option<&[resources::PlannedResource]> {
        self.resource_browser
            .plan_key
            .as_ref()
            .filter(|key| {
                self.busy.is_some() && self.resource_browser.accepts(key, &self.settings.game_root)
            })
            .map(|_| self.resource_browser.dependency_plan.as_slice())
    }
    fn finish_resource_request(&mut self, key: &RequestKey) -> bool {
        let pending = self
            .resource_browser
            .pending
            .as_ref()
            .is_some_and(|pending| pending.same(key));
        if pending {
            self.resource_browser.pending = None;
            self.resource_browser.loading = false;
            self.busy = None;
            self.progress = None;
        }
        pending && self.resource_browser.accepts(key, &self.settings.game_root)
    }
    pub(super) fn handle_resource_event(&mut self, event: ResourceEvent) {
        match event {
            ResourceEvent::Search(key, page, request, category) => {
                if !self.finish_resource_request(&key) {
                    return;
                }
                self.resource_browser.last_search = Some(request);
                self.resource_browser.last_category = category;
                self.resource_browser.page = Some(page);
                self.status = format!("{}搜索完成", key.kind.label());
            }
            ResourceEvent::Detail(key, project, versions) => {
                if !self.finish_resource_request(&key) {
                    return;
                }
                let state = &mut self.resource_browser;
                let (scheme, options) = filter_options(&versions);
                state.filter_scheme = scheme;
                state.filters = options.clone();
                state.version_filter = state
                    .last_search
                    .as_ref()
                    .and_then(|r| r.minecraft.as_deref())
                    .map(|mc| grouped_version(mc, state.filter_scheme))
                    .filter(|f| options.contains(f))
                    .unwrap_or_default();
                state.project = Some(*project);
                state.versions = versions;
                let groups = version_groups(state);
                state.open_groups = groups
                    .iter()
                    .filter(|g| g.selected || groups.len() == 1)
                    .map(|g| g.title.clone())
                    .collect();
                self.status = format!("{}版本列表已更新", key.kind.label());
            }
            ResourceEvent::Icon(key, id, data) => {
                if self
                    .resource_browser
                    .accepts(&key, &self.settings.game_root)
                {
                    self.resource_browser.images.push((id, data));
                }
            }
            ResourceEvent::Failed(key, message) => {
                if !self.finish_resource_request(&key) {
                    return;
                }
                self.record(message.clone());
                self.resource_browser.page_error = Some(message);
            }
            ResourceEvent::Cancelled(key) => {
                if !self.finish_resource_request(&key) {
                    return;
                }
                self.resource_browser.cancelled = true;
                self.status = "资源列表获取已取消".into();
            }
            ResourceEvent::Plan(key, plan) => {
                if self
                    .resource_browser
                    .accepts(&key, &self.settings.game_root)
                    && self
                        .resource_browser
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.same(&key))
                {
                    self.record(format!(
                        "本次资源计划：{}",
                        plan.iter()
                            .map(|item| format!(
                                "{} {} [{}]{}",
                                item.project,
                                item.version,
                                item.filename,
                                if item.reused { "（复用）" } else { "" }
                            ))
                            .collect::<Vec<_>>()
                            .join("，")
                    ));
                    self.resource_browser.plan_key = Some(key);
                    self.resource_browser.dependency_plan = plan;
                }
            }
            ResourceEvent::PlanFinished(key) => {
                if self
                    .resource_browser
                    .plan_key
                    .as_ref()
                    .is_some_and(|saved| saved.same(&key))
                {
                    self.resource_browser.plan_key = None;
                    self.resource_browser.dependency_plan.clear();
                }
                if self
                    .resource_browser
                    .pending
                    .as_ref()
                    .is_some_and(|saved| saved.same(&key))
                {
                    self.resource_browser.pending = None;
                }
            }
            ResourceEvent::Installed(target, filename) => {
                self.finish_download_task();
                self.busy = None;
                self.progress = None;
                self.status = format!("已将 {filename} 安装到 {target}");
                self.refresh_mods();
            }
        }
    }
    fn resource_target(&self, id: &str) -> Option<(String, String)> {
        let version = metadata::resolve_version(&self.settings.game_root, id).ok()?;
        if self.resource_browser.kind == ResourceKind::Mod {
            resource_target_info(&version)
        } else {
            Some((version["_pcl_jar_id"].as_str()?.to_owned(), String::new()))
        }
    }
    pub(super) fn open_local_pack_import(&mut self) {
        self.page = super::Page::Download;
        self.download_tab = 2;
        self.task_view = false;
        self.ensure_resource_target();
        self.resource_browser.local_pack = true;
    }

    pub(super) fn ensure_resource_target(&mut self) {
        let kind = tab_kind(self.download_tab);
        if self.resource_browser.kind != kind {
            let state = &mut self.resource_browser;
            if let Some(cancel) = state.cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            state.kind = kind;
            state.generation += 1;
            state.query.clear();
            state.category.clear();
            state.loader.clear();
            state.page = None;
            state.project = None;
            state.detail_id = None;
            state.detail_hit = None;
            state.page_error = None;
            state.cancelled = false;
            state.loading = false;
            state.install_selection = None;
            state.open_groups.clear();
            state.last_search = None;
            state.last_attempt = None;
            state.loading_indicator.start();
            state.versions.clear();
            state.images.clear();
            state.world = None;
            state.local_pack = false;
            state.auto_searched = false;
            state.plan_key = None;
            state.dependency_plan.clear();
        }
        if kind == ResourceKind::Modpack {
            return;
        }
        if self
            .resource_browser
            .target
            .as_ref()
            .is_some_and(|id| self.resource_target(id).is_some())
        {
            return;
        }
        self.resource_browser.target = self
            .settings
            .selected_version
            .clone()
            .filter(|id| self.resource_target(id).is_some())
            .or_else(|| {
                self.versions
                    .iter()
                    .find(|v| self.resource_target(&v.id).is_some())
                    .map(|v| v.id.clone())
            });
        if let Some((minecraft, loader)) = self
            .resource_browser
            .target
            .as_ref()
            .and_then(|id| self.resource_target(id))
        {
            self.resource_browser.minecraft = minecraft;
            self.resource_browser.loader = loader;
        }
    }
    fn resource_request(&mut self, cancel: Option<Arc<AtomicBool>>) -> RequestKey {
        let state = &mut self.resource_browser;
        if let Some(previous) = std::mem::replace(&mut state.cancel, cancel) {
            previous.store(true, Ordering::Relaxed);
        }
        state.generation += 1;
        state.page_error = None;
        state.cancelled = false;
        state.loading = false;
        state.plan_key = None;
        state.dependency_plan.clear();
        let key = RequestKey {
            kind: state.kind,
            generation: state.generation,
            root: self.settings.game_root.clone(),
            epoch: state.epoch.clone(),
        };
        state.pending = Some(key.clone());
        key
    }
    fn search_resources(&mut self, offset: u32, use_form: bool) {
        let state = &self.resource_browser;
        let mut request = resources::SearchOptions {
            sort: self.settings.resource_sort,
            provider: state.provider,
            query: state.query.trim().into(),
            minecraft: (!state.minecraft.trim().is_empty()).then(|| state.minecraft.trim().into()),
            loader: (state.kind == ResourceKind::Mod && !state.loader.is_empty())
                .then(|| state.loader.clone()),
            offset,
            limit: 20,
        };
        let mut category = state.category.clone();
        if !use_form {
            if let Some(previous) = &state.last_search {
                request = previous.clone();
                request.offset = offset;
                category = state.last_category.clone();
            }
        }
        self.start_resource_search(request, category);
    }
    fn retry_resource_search(&mut self) {
        if let Some((request, category)) = self.resource_browser.last_attempt.clone() {
            self.start_resource_search(request, category);
        }
    }
    fn cancel_resource_request(&mut self) {
        if let Some(cancel) = &self.resource_browser.cancel {
            cancel.store(true, Ordering::Relaxed);
            self.status = "正在取消资源列表获取…".into();
        }
    }
    fn start_resource_search(&mut self, request: resources::SearchOptions, category: String) {
        let Some((tx, cancel)) = self.start_job("正在获取资源列表") else {
            return;
        };
        let key = self.resource_request(Some(cancel.clone()));
        self.resource_browser.auto_searched = true;
        self.resource_browser.loading = true;
        self.resource_browser.loading_indicator.start();
        self.resource_browser.last_attempt = Some((request.clone(), category.clone()));
        self.resource_browser.project = None;
        self.resource_browser.detail_id = None;
        self.resource_browser.detail_hit = None;
        std::thread::spawn(move || {
            match resources::search_resources(key.kind, &request, &category, &cancel) {
                Ok(page) => {
                    let icons: Vec<_> = page
                        .hits
                        .iter()
                        .filter_map(|hit| {
                            hit.icon_url
                                .as_ref()
                                .map(|url| (hit.project_id.clone(), url.clone()))
                        })
                        .collect();
                    let _ = tx.send(Event::Resource(ResourceEvent::Search(
                        key.clone(),
                        page,
                        request,
                        category,
                    )));
                    for (id, url) in icons {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Ok(bytes) = resources::fetch_project_icon(&url, &cancel) {
                            let _ = tx.send(Event::Resource(ResourceEvent::Icon(
                                key.clone(),
                                id,
                                bytes,
                            )));
                        }
                    }
                }
                Err(error) => {
                    let event = if error.is::<pcl_core::model::OperationCancelled>()
                        || error
                            .chain()
                            .any(|cause| cause.is::<pcl_core::model::OperationCancelled>())
                    {
                        ResourceEvent::Cancelled(key)
                    } else {
                        ResourceEvent::Failed(key, format!("资源搜索失败：{error:#}"))
                    };
                    let _ = tx.send(Event::Resource(event));
                }
            }
        });
    }
    fn open_resource(&mut self, id: String) {
        let Some((tx, cancel)) = self.start_job("正在获取版本列表") else {
            return;
        };
        let key = self.resource_request(Some(cancel.clone()));
        let state = &mut self.resource_browser;
        if state.detail_id.as_deref() != Some(&id) {
            state.project = None;
            state.versions.clear();
            state.open_groups.clear();
            state.detail_hit = state
                .page
                .as_ref()
                .and_then(|page| page.hits.iter().find(|h| h.project_id == id))
                .cloned();
        }
        state.loading = true;
        state.loading_indicator.start();
        state.detail_id = Some(id.clone());
        state.install_selection = None;
        std::thread::spawn(move || {
            let result = (|| {
                let project = resources::get_project(&id, &cancel)?;
                let versions =
                    resources::list_resource_versions(key.kind, &project.id, "", "", &cancel)?;
                anyhow::Ok((project, versions))
            })();
            match result {
                Ok((project, versions)) => {
                    let icon = project
                        .icon_url
                        .clone()
                        .map(|url| (project.id.clone(), url));
                    let _ = tx.send(Event::Resource(ResourceEvent::Detail(
                        key.clone(),
                        Box::new(project),
                        versions,
                    )));
                    if let Some((id, url)) = icon {
                        if let Ok(bytes) = resources::fetch_project_icon(&url, &cancel) {
                            let _ = tx.send(Event::Resource(ResourceEvent::Icon(key, id, bytes)));
                        }
                    }
                }
                Err(error) => {
                    let event = if error.is::<pcl_core::model::OperationCancelled>()
                        || error
                            .chain()
                            .any(|cause| cause.is::<pcl_core::model::OperationCancelled>())
                    {
                        ResourceEvent::Cancelled(key)
                    } else {
                        ResourceEvent::Failed(key, format!("获取资源版本失败：{error:#}"))
                    };
                    let _ = tx.send(Event::Resource(event));
                }
            }
        });
    }
    fn install_resource(&mut self, version: String) {
        let kind = self.resource_browser.kind;
        let Some(project) = self.resource_browser.project.as_ref().map(|p| p.id.clone()) else {
            return;
        };
        if kind == ResourceKind::Modpack {
            let id = self.resource_browser.pack_name.trim().to_owned();
            if let Err(error) = metadata::validate_id(&id) {
                self.error = Some(format!("新实例名称无效：{error:#}"));
                return;
            }
            let Some((tx, _)) = self.start_download_job("正在下载并安装整合包", Some(id.clone()))
            else {
                return;
            };
            let root = self.settings.game_root.clone();
            let optional = self.resource_browser.pack_optional;
            let retry = pcl_core::packs::PackRetry::default();
            tx.spawn(move |tx| {
                let cancel = tx.cancel_token();
                let result = (|| {
                    let pack = resources::download_modpack(&project, &version, &cancel, |p| {
                        let _ = tx.send(Event::Progress(p));
                    })?;
                    retry.install_with_java(
                        &root,
                        pack.path(),
                        &id,
                        optional,
                        None,
                        &pcl_core::model::Platform::current(),
                        &cancel,
                        |p| {
                            let _ = tx.send(Event::Progress(p));
                        },
                    )
                })();
                if !retry.retryable() {
                    tx.disable_retry();
                }
                let _ = tx.send(match result {
                    Ok(id) => Event::Installed(id),
                    Err(error) => Event::download_failed("整合包安装未完成", error),
                });
            });
            return;
        }
        let Some(target) = self.resource_browser.target.clone() else {
            return;
        };
        let Some((minecraft, loader)) = self.resource_target(&target) else {
            self.error = Some("请选择兼容的已安装游戏版本。".into());
            return;
        };
        let instance = match config::instance_game_dir(&self.settings.game_root, &target) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        let world = self.resource_browser.world.clone();
        if kind == ResourceKind::DataPack && world.is_none() {
            self.error = Some("请先明确选择此实例中的目标世界。".into());
            return;
        }
        let Some((tx, _)) = self.start_download_job("正在下载资源", Some(target.clone()))
        else {
            return;
        };
        // Browser navigation owns only its request generation, not this writer's
        // cancellation token. The task cancel button controls installation.
        let key = self.resource_request(None);
        let root = self.settings.game_root.clone();
        let naming = self.settings.resource_naming;
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = (|| {
                std::fs::create_dir_all(&instance)?;
                let checked = config::instance_game_dir(&root, &target)?;
                anyhow::ensure!(checked == instance, "实例目录在下载前发生变化");
                let plan = if kind == ResourceKind::Mod {
                    resources::plan_mod_install(&instance, &version, &minecraft, &loader, &cancel)?
                } else {
                    resources::plan_resource_install(
                        &resources::ResourceInstall {
                            kind,
                            instance: &instance,
                            world: world.as_deref(),
                            project_id: &project,
                            version_id: &version,
                            minecraft: &minecraft,
                        },
                        &cancel,
                    )?
                };
                let plan = plan.with_naming(naming)?;
                let _ = tx.send(Event::Resource(ResourceEvent::Plan(
                    key.clone(),
                    plan.resources(),
                )));
                resources::execute_install_plan(&plan, &cancel, |p| {
                    let _ = tx.send(Event::Progress(p));
                })
            })();
            let _ = tx.send(Event::Resource(ResourceEvent::PlanFinished(key)));
            let _ = tx.send(match result {
                Ok(paths) => Event::Resource(ResourceEvent::Installed(
                    target,
                    if paths.is_empty() {
                        "已复用现有资源".into()
                    } else {
                        format!("{} 个资源文件（含必需依赖）", paths.len())
                    },
                )),
                Err(error) => Event::download_failed("资源安装未完成", error),
            });
        });
    }
    pub(super) fn resource_page(&mut self, ui: &mut egui::Ui) {
        self.ensure_resource_target();
        let kind = self.resource_browser.kind;
        if !self.resource_browser.auto_searched && self.busy.is_none() {
            self.search_resources(0, true);
        }
        for (id, bytes) in std::mem::take(&mut self.resource_browser.images) {
            match decode_icon(ui.ctx(), &id, &bytes) {
                Ok(texture) => {
                    self.resource_browser.icons.insert(id, texture);
                }
                Err(error) => self.record(format!("资源图标解码失败：{error:#}")),
            }
        }
        if self.resource_browser.detail_id.is_some() {
            self.resource_detail(ui);
            return;
        }
        let mut search = false;
        let mut reset = false;
        source_card(ui, &format!("搜索{}", kind.label()), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            let start = ui.cursor().min;
            let star = (width - 138.0) / 3.5;
            let field_width = 2.5 * star;
            let right_x = start.x + 44.0 + field_width + 50.0;
            let left_x = start.x + 44.0;
            ui.allocate_space(Vec2::new(width, 65.0));
            crate::ui_style::place_left(
                ui,
                Rect::from_min_size(start, Vec2::new(44.0, 28.0)),
                egui::Label::new("名称").halign(egui::Align::Min),
            );
            let name_response = ui.place(
                Rect::from_min_size(Pos2::new(left_x, start.y), Vec2::new(field_width, 28.0)),
                egui::TextEdit::singleline(&mut self.resource_browser.query)
                    .id_salt("resource-search-name"),
            );
            crate::ui_style::place_left(
                ui,
                Rect::from_min_size(Pos2::new(right_x, start.y), Vec2::new(44.0, 28.0)),
                egui::Label::new("来源").halign(egui::Align::Min),
            );
            let source_rect =
                Rect::from_min_size(Pos2::new(right_x + 44.0, start.y), Vec2::new(star, 28.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(source_rect), |ui| {
                crate::ui_style::PclComboBox::from_id_salt("resource-source")
                    .width(star - 12.0)
                    .selected_text(self.resource_browser.provider.label())
                    .show_ui(ui, |ui| {
                        for provider in [
                            resources::ResourceProvider::Modrinth,
                            resources::ResourceProvider::CurseForge,
                        ] {
                            if ui
                                .selectable_value(
                                    &mut self.resource_browser.provider,
                                    provider,
                                    provider.label(),
                                )
                                .changed()
                            {
                                self.resource_browser.category.clear();
                            }
                        }
                    });
            });
            crate::ui_style::place_left(
                ui,
                Rect::from_min_size(start + Vec2::new(0.0, 37.0), Vec2::new(44.0, 28.0)),
                egui::Label::new("版本").halign(egui::Align::Min),
            );
            let show_loader =
                kind == ResourceKind::Mod && concrete_minecraft(&self.resource_browser.minecraft);
            if !show_loader {
                self.resource_browser.loader.clear();
            }
            let version_rect = Rect::from_min_size(
                Pos2::new(left_x, start.y + 37.0),
                Vec2::new(if show_loader { star * 1.3 } else { field_width }, 28.0),
            );
            let version_response =
                editable_version(ui, version_rect, &mut self.resource_browser.minecraft);
            if show_loader {
                let combo_rect = Rect::from_min_size(
                    Pos2::new(left_x + star * 1.3 + 10.0, start.y + 37.0),
                    Vec2::new(star * 1.2 - 10.0, 28.0),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(combo_rect), |ui| {
                    crate::ui_style::PclComboBox::from_id_salt("resource-loader")
                        .width(combo_rect.width() - 12.0)
                        .selected_text(loader_label(&self.resource_browser.loader))
                        .show_ui(ui, |ui| {
                            for (key, label) in [
                                ("", "任意 Mod 加载器"),
                                ("forge", "Forge"),
                                ("neoforge", "NeoForge"),
                                ("fabric", "Fabric"),
                                ("quilt", "Quilt"),
                            ] {
                                ui.selectable_value(
                                    &mut self.resource_browser.loader,
                                    key.into(),
                                    label,
                                );
                            }
                        });
                });
            }
            crate::ui_style::place_left(
                ui,
                Rect::from_min_size(Pos2::new(right_x, start.y + 37.0), Vec2::new(44.0, 28.0)),
                egui::Label::new("类型").halign(egui::Align::Min),
            );
            let type_rect = Rect::from_min_size(
                Pos2::new(right_x + 44.0, start.y + 37.0),
                Vec2::new(star, 28.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(type_rect), |ui| {
                crate::ui_style::PclComboBox::from_id_salt("resource-category")
                    .width(star - 12.0)
                    .selected_text(
                        categories_for(kind, self.resource_browser.provider)
                            .iter()
                            .find(|(key, _)| *key == self.resource_browser.category)
                            .map_or("全部", |(_, label)| *label),
                    )
                    .show_ui(ui, |ui| {
                        for (key, label) in categories_for(kind, self.resource_browser.provider) {
                            ui.selectable_value(
                                &mut self.resource_browser.category,
                                (*key).into(),
                                *label,
                            );
                        }
                    });
            });
            ui.add_space(10.0);
            let previous = (self.settings.resource_sort, self.settings.resource_naming);
            ui.horizontal(|ui| {
                ui.label("排序");
                crate::ui_style::PclComboBox::from_id_salt("resource-sort")
                    .width(130.0)
                    .selected_text(self.settings.resource_sort.label())
                    .show_ui(ui, |ui| {
                        for value in [
                            resources::SearchSort::Relevance,
                            resources::SearchSort::Downloads,
                            resources::SearchSort::Updated,
                            resources::SearchSort::Newest,
                        ] {
                            ui.selectable_value(
                                &mut self.settings.resource_sort,
                                value,
                                value.label(),
                            );
                        }
                    });
                ui.label("文件命名");
                crate::ui_style::PclComboBox::from_id_salt("resource-naming")
                    .width(150.0)
                    .selected_text(self.settings.resource_naming.label())
                    .show_ui(ui, |ui| {
                        for value in [
                            resources::ResourceNaming::Original,
                            resources::ResourceNaming::ProjectVersion,
                        ] {
                            ui.selectable_value(
                                &mut self.settings.resource_naming,
                                value,
                                value.label(),
                            );
                        }
                    });
            });
            if previous != (self.settings.resource_sort, self.settings.resource_naming) {
                self.persist();
            }
            search |= self.busy.is_none()
                && (name_response.lost_focus() || version_response.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.add_space(15.0);
            ui.horizontal(|ui| {
                search |= ui
                    .add_enabled(
                        self.busy.is_none(),
                        egui::Button::new(
                            RichText::new("搜索").color(theme::palette(ui.ctx()).accent),
                        )
                        .min_size(Vec2::new(140.0, 35.0)),
                    )
                    .clicked();
                ui.add_space(10.0);
                reset = ui
                    .add_sized(Vec2::new(140.0, 35.0), egui::Button::new("重置条件"))
                    .clicked();
                if kind == ResourceKind::Modpack {
                    ui.add_space(10.0);
                }
                if kind == ResourceKind::Modpack
                    && ui
                        .add_sized(Vec2::new(140.0, 35.0), egui::Button::new("安装已有整合包"))
                        .clicked()
                {
                    self.resource_browser.local_pack = !self.resource_browser.local_pack;
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "公开列表缓存保留 5 分钟；过期后重新获取，获取失败不会显示为最新结果。",
                )
                .size(11.0)
                .color(MUTED),
            );
        });
        if kind == ResourceKind::Modpack && self.resource_browser.local_pack {
            self.modpack_page(ui);
        }

        if reset {
            self.resource_browser.query.clear();
            self.resource_browser.minecraft.clear();
            self.resource_browser.loader.clear();
            self.resource_browser.category.clear();
        }
        if search {
            self.search_resources(0, true);
        }
        let mut selected = None;
        let mut page_request = None;
        let state = &mut self.resource_browser;
        let status = if state.loading {
            loading_ui::Status::Running {
                cancelling: state
                    .cancel
                    .as_ref()
                    .is_some_and(|token| token.load(Ordering::Relaxed)),
            }
        } else if state.cancelled {
            loading_ui::Status::Cancelled
        } else if let Some(error) = &state.page_error {
            loading_ui::Status::Failed(error)
        } else {
            loading_ui::Status::Ready
        };
        let loading = state.loading_indicator.show_status(
            ui,
            &format!(
                "正在获取{}列表",
                if kind == ResourceKind::Mod {
                    " Mod "
                } else {
                    kind.label()
                }
            ),
            status,
            loading_ui::Placement::List,
        );
        if let Some(action) = loading {
            match action {
                loading_ui::Action::Retry => self.retry_resource_search(),
                loading_ui::Action::Cancel => self.cancel_resource_request(),
                loading_ui::Action::None => (),
            }
        } else if let Some(page) = &self.resource_browser.page {
            resource_frame().inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                let request = self.resource_browser.last_search.as_ref();
                for hit in &page.hits {
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 64.0),
                        egui::Sense::click(),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &hit.title)
                    });
                    if response.hovered() {
                        ui.painter()
                            .rect_filled(rect, 6, theme::palette(ui.ctx()).light);
                    }
                    let show_mc = request.is_none_or(|r| r.minecraft.is_none());
                    let show_loader =
                        kind == ResourceKind::Mod && request.is_none_or(|r| r.loader.is_none());
                    resource_item(
                        ui,
                        rect,
                        &self.assets,
                        &self.resource_browser.icons,
                        kind,
                        hit,
                        show_mc,
                        show_loader,
                    );
                    if response.clicked() && self.busy.is_none() {
                        selected = Some(hit.project_id.clone());
                    }
                }
                if page.hits.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(format!(
                            "没有找到符合条件的{}，请尝试修改搜索条件。",
                            kind.label()
                        ));
                    });
                    ui.add_space(16.0);
                }
            });
            ui.add_space(7.0);
            ui.horizontal(|ui| {
                let count = page
                    .total_hits
                    .div_ceil(u64::from(page.limit.max(1)))
                    .max(1);
                let label = format!("{} / {count}", page.offset / page.limit.max(1) + 1);
                let text_width = ui
                    .painter()
                    .layout_no_wrap(
                        label.clone(),
                        egui::FontId::proportional(15.0),
                        theme::palette(ui.ctx()).accent,
                    )
                    .size()
                    .x;
                let width = 10.0 + 23.0 + 5.0 + 23.0 + 8.0 + text_width + 13.0 + 23.0 + 30.0 + 10.0;
                ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                resource_frame()
                    .inner_margin(egui::Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.horizontal(|ui| {
                            if page_arrow(ui, true, true, page.offset > 0).clicked() {
                                page_request = Some(0);
                            }
                            ui.add_space(5.0);
                            if page_arrow(ui, true, false, page.offset > 0).clicked() {
                                page_request = Some(page.offset.saturating_sub(page.limit));
                            }
                            ui.add_space(8.0);
                            ui.add_sized(
                                Vec2::new(text_width, 23.0),
                                egui::Label::new(
                                    RichText::new(label)
                                        .size(15.0)
                                        .color(theme::palette(ui.ctx()).accent),
                                ),
                            );
                            ui.add_space(13.0);
                            if page_arrow(
                                ui,
                                false,
                                false,
                                u64::from(page.offset) + u64::from(page.limit) < page.total_hits,
                            )
                            .clicked()
                            {
                                page_request = Some(page.offset.saturating_add(page.limit));
                            }
                            ui.add_space(30.0);
                        });
                    });
            });
        }
        if let Some(id) = selected {
            self.open_resource(id);
        }
        if let Some(offset) = page_request {
            self.search_resources(offset, false);
        }
    }

    fn resource_detail(&mut self, ui: &mut egui::Ui) {
        let kind = self.resource_browser.kind;
        let hit = self.resource_browser.detail_hit.clone().or_else(|| {
            self.resource_browser
                .project
                .as_ref()
                .map(|p| resources::ProjectHit {
                    project_id: p.id.clone(),
                    slug: p.slug.clone(),
                    title: p.title.clone(),
                    description: p.description.clone(),
                    author: String::new(),
                    downloads: p.downloads,
                    categories: p.loaders.clone(),
                    icon_url: p.icon_url.clone(),
                    date_modified: p.updated.clone(),
                    versions: p.game_versions.clone(),
                })
        });
        if let Some(hit) = &hit {
            // PanIntro 22,18,22,19; MyResourceItem margin -7,-7,0,8.
            // Net resource slot height 65, then a 35-DIP button row => card height 137.
            resource_frame()
                .inner_margin(egui::Margin {
                    left: 22,
                    right: 22,
                    top: 18,
                    bottom: 19,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let (slot, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 65.0),
                        egui::Sense::hover(),
                    );
                    let rect = Rect::from_min_size(
                        slot.min - Vec2::splat(7.0),
                        Vec2::new(slot.width() + 7.0, 64.0),
                    );
                    resource_item(
                        ui,
                        rect,
                        &self.assets,
                        &self.resource_browser.icons,
                        kind,
                        hit,
                        true,
                        kind == ResourceKind::Mod,
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 20.0;
                        if ui
                            .add_sized(
                                Vec2::new(140.0, 35.0),
                                egui::Button::new(if hit.project_id.starts_with("cf:") {
                                    "转到 CurseForge"
                                } else {
                                    "转到 Modrinth"
                                }),
                            )
                            .clicked()
                        {
                            ui.ctx()
                                .open_url(egui::OpenUrl::new_tab(resource_url(kind, hit)));
                        }
                        if let Some(entry) = wiki_entry(hit) {
                            if ui
                                .add_sized(Vec2::new(120.0, 35.0), egui::Button::new("MC 百科"))
                                .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(entry.url()));
                            }
                        }
                        if ui
                            .add_sized(Vec2::new(140.0, 35.0), egui::Button::new("复制名称"))
                            .clicked()
                        {
                            ui.ctx().copy_text(hit.title.clone());
                        }
                    });
                });
            ui.add_space(17.0); // parent spacing contributes the remaining 8 DIP.
        }
        let state = &mut self.resource_browser;
        let status = if state.loading {
            loading_ui::Status::Running {
                cancelling: state
                    .cancel
                    .as_ref()
                    .is_some_and(|token| token.load(Ordering::Relaxed)),
            }
        } else if state.cancelled {
            loading_ui::Status::Cancelled
        } else if let Some(error) = &state.page_error {
            loading_ui::Status::Failed(error)
        } else {
            loading_ui::Status::Ready
        };
        if let Some(action) = state.loading_indicator.show_status(
            ui,
            "正在获取版本列表",
            status,
            loading_ui::Placement::Detail,
        ) {
            if action == loading_ui::Action::Retry {
                if let Some(id) = self.resource_browser.detail_id.clone() {
                    self.open_resource(id);
                }
            } else if action == loading_ui::Action::Cancel {
                self.cancel_resource_request();
            }
            return;
        }
        let filters = self.resource_browser.filters.clone();
        if filters.len() > 1 {
            resource_frame()
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 0,
                    top: 10,
                    bottom: 10,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // Source uses one horizontal row. Scroll at narrow widths instead of stretching pills.
                    egui::ScrollArea::horizontal()
                        .id_salt("resource-version-filters")
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(2.0);
                                for value in std::iter::once(&String::new()).chain(filters.iter()) {
                                    let label = if value.is_empty() { "全部" } else { value };
                                    let active = self.resource_browser.version_filter == *value;
                                    let galley = ui.painter().layout_no_wrap(
                                        label.into(),
                                        egui::FontId::proportional(13.0),
                                        if active {
                                            Color32::WHITE
                                        } else {
                                            theme::palette(ui.ctx()).accent
                                        },
                                    );
                                    let (rect, response) = ui.allocate_exact_size(
                                        Vec2::new(galley.size().x + 20.0, 27.0),
                                        egui::Sense::click(),
                                    );
                                    if active || response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            14,
                                            if active {
                                                theme::palette(ui.ctx()).accent
                                            } else {
                                                theme::palette(ui.ctx()).light
                                            },
                                        );
                                    }
                                    ui.painter().galley(
                                        rect.center() - galley.size() / 2.0,
                                        galley,
                                        if active {
                                            Color32::WHITE
                                        } else {
                                            theme::palette(ui.ctx()).accent
                                        },
                                    );
                                    if response.clicked() {
                                        self.resource_browser.version_filter = value.clone();
                                    }
                                }
                            });
                        });
                });
            ui.add_space(7.0);
        }
        let groups = version_groups(&self.resource_browser);
        let mut chosen = None;
        for group in &groups {
            let open = self.resource_browser.open_groups.contains(&group.title);
            let mut toggle = false;
            resource_frame().show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let (header, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 40.0),
                    egui::Sense::click(),
                );
                crate::ui_style::place_left(
                    ui,
                    Rect::from_min_size(
                        header.min + Vec2::new(15.0, 12.0),
                        Vec2::new(header.width() - 55.0, 18.0),
                    ),
                    egui::Label::new(crate::ui_style::card_title(&group.title)).truncate(),
                );
                let center = Pos2::new(header.right() - 22.0, header.center().y);
                chevron(ui, center, open, theme::palette(ui.ctx()).text);
                toggle = response.clicked();
                if open {
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 42.0 * group.versions.len() as f32),
                        egui::Sense::hover(),
                    );
                    for (row, index) in group.versions.iter().enumerate() {
                        let version = &self.resource_browser.versions[*index];
                        let row_rect = Rect::from_min_size(
                            rect.min + Vec2::new(20.0, 42.0 * row as f32),
                            Vec2::new(rect.width() - 38.0, 42.0),
                        );
                        if version_row(
                            ui,
                            row_rect,
                            &self.assets,
                            version,
                            self.busy.is_none() && self.game_pid.is_none(),
                        )
                        .clicked()
                        {
                            chosen = Some(version.id.clone());
                        }
                    }
                }
            });
            if toggle {
                if open {
                    self.resource_browser.open_groups.remove(&group.title);
                } else {
                    self.resource_browser
                        .open_groups
                        .insert(group.title.clone());
                }
            }
            ui.add_space(7.0);
        }
        if groups.is_empty() {
            resource_frame().inner_margin(25).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label("没有符合当前筛选条件的文件。");
                if !self.resource_browser.version_filter.is_empty()
                    && ui.button("显示全部版本").clicked()
                {
                    self.resource_browser.version_filter.clear();
                }
            });
        }
        if let Some(version) = chosen {
            if kind == ResourceKind::Modpack && self.resource_browser.pack_name.is_empty() {
                self.resource_browser.pack_name = self
                    .resource_browser
                    .project
                    .as_ref()
                    .map(|p| {
                        p.title
                            .chars()
                            .filter(|c| !"<>:\"/\\|?*".contains(*c) && !c.is_control())
                            .collect::<String>()
                            .trim()
                            .trim_end_matches('.')
                            .to_owned()
                    })
                    .unwrap_or_default();
            }
            self.resource_browser.install_selection = Some(version);
        }
        self.resource_install_dialog(ui.ctx());
    }

    fn resource_install_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.resource_browser.install_selection.clone() else {
            return;
        };
        let Some(version) = self
            .resource_browser
            .versions
            .iter()
            .find(|v| v.id == id)
            .cloned()
        else {
            self.resource_browser.install_selection = None;
            return;
        };
        let kind = self.resource_browser.kind;
        let targets: Vec<_> = self
            .versions
            .iter()
            .filter_map(|v| {
                self.resource_target(&v.id)
                    .map(|(mc, loader)| (v.id.clone(), mc, loader))
            })
            .filter(|(_, mc, loader)| resources::resource_compatible(kind, &version, mc, loader))
            .collect();
        if kind != ResourceKind::Modpack
            && !targets
                .iter()
                .any(|(id, _, _)| Some(id) == self.resource_browser.target.as_ref())
        {
            self.resource_browser.target = None;
            self.resource_browser.world = None;
        }
        let worlds = if kind == ResourceKind::DataPack {
            self.resource_browser
                .target
                .as_ref()
                .map(|id| {
                    config::instance_game_dir(&self.settings.game_root, id).and_then(|path| {
                        if path.exists() {
                            resources::list_worlds(&path)
                        } else {
                            Ok(Vec::new())
                        }
                    })
                })
                .transpose()
        } else {
            Ok(None)
        };
        if let Ok(Some(worlds)) = &worlds {
            if self
                .resource_browser
                .world
                .as_ref()
                .is_some_and(|path| !worlds.contains(path))
            {
                self.resource_browser.world = None;
            }
        }
        let valid = self.busy.is_none()
            && self.game_pid.is_none()
            && if kind == ResourceKind::Modpack {
                metadata::validate_id(self.resource_browser.pack_name.trim()).is_ok()
            } else {
                self.resource_browser.target.is_some()
                    && (kind != ResourceKind::DataPack || self.resource_browser.world.is_some())
            };
        let old_target = self.resource_browser.target.clone();
        let buttons: &[&str] = if valid {
            &["安装", "取消"]
        } else {
            &["关闭"]
        };
        let height = if kind == ResourceKind::DataPack {
            230.0
        } else {
            174.0
        };
        let action = super::account_ui::modal_frame(
            ctx,
            "resource-install",
            &format!("安装{}", kind.label()),
            570.0,
            height,
            buttons,
            |ui| {
                ui.label(RichText::new(&version.name).color(theme::palette(ui.ctx()).text));
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        version.game_versions.join(" / "),
                        version.loaders.join(" / ")
                    ))
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(8.0);
                if kind == ResourceKind::Modpack {
                    ui.label("新游戏版本名称");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.resource_browser.pack_name)
                            .desired_width(ui.available_width()),
                    );
                    ui.checkbox(
                        &mut self.resource_browser.pack_optional,
                        "安装可选客户端文件",
                    );
                    ui.label(
                        RichText::new("整合包安装到独立的新实例；已有文件不会被覆盖。")
                            .size(12.0)
                            .color(MUTED),
                    );
                } else {
                    ui.label("安装到游戏版本");
                    crate::ui_style::PclComboBox::from_id_salt("resource-install-instance")
                        .width(ui.available_width() - 12.0)
                        .selected_text(
                            self.resource_browser
                                .target
                                .as_deref()
                                .unwrap_or("请选择兼容的已安装游戏版本"),
                        )
                        .show_ui(ui, |ui| {
                            for (id, mc, loader) in &targets {
                                ui.selectable_value(
                                    &mut self.resource_browser.target,
                                    Some(id.clone()),
                                    format!(
                                        "{id} · {mc}{}",
                                        if loader.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" / {}", loader_label(loader))
                                        }
                                    ),
                                );
                            }
                        });
                    if targets.is_empty() {
                        ui.label(
                            RichText::new("没有兼容的已安装游戏版本，请先安装对应版本及加载器。")
                                .color(MUTED),
                        );
                    }
                    if kind == ResourceKind::DataPack {
                        ui.add_space(6.0);
                        ui.label("目标世界");
                        match &worlds {
                            Ok(Some(worlds)) => {
                                crate::ui_style::PclComboBox::from_id_salt(
                                    "resource-install-world",
                                )
                                .width(ui.available_width() - 12.0)
                                .selected_text(
                                    self.resource_browser
                                        .world
                                        .as_ref()
                                        .and_then(|p| p.file_name())
                                        .map(|p| p.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "请明确选择世界".into()),
                                )
                                .show_ui(ui, |ui| {
                                    for world in worlds {
                                        ui.selectable_value(
                                            &mut self.resource_browser.world,
                                            Some(world.clone()),
                                            world.file_name().unwrap_or_default().to_string_lossy(),
                                        );
                                    }
                                });
                                if worlds.is_empty() {
                                    ui.label("此实例没有含 level.dat 的本地世界。");
                                }
                            }
                            Ok(None) => {
                                ui.label("请先选择游戏版本。");
                            }
                            Err(error) => {
                                ui.colored_label(
                                    Color32::DARK_RED,
                                    format!("无法读取世界：{error:#}"),
                                );
                            }
                        }
                        ui.label(
                            RichText::new("请关闭目标世界后安装，仅写入所选世界的 datapacks。")
                                .size(12.0)
                                .color(MUTED),
                        );
                    } else if kind == ResourceKind::Shader {
                        ui.label(
                            RichText::new(
                                "光影需在游戏中用对应的 Iris / OptiFine 或资源包入口启用。",
                            )
                            .size(12.0)
                            .color(MUTED),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "将解析必需依赖；安装明细可在任务页查看，已有文件不会被覆盖。",
                            )
                            .size(12.0)
                            .color(MUTED),
                        );
                    }
                }
            },
        );
        if old_target != self.resource_browser.target {
            self.resource_browser.world = None;
        }
        if let Some(button) = action {
            self.resource_browser.install_selection = None;
            if valid && button == 0 {
                self.install_resource(id);
            }
        }
    }
}

fn concrete_minecraft(value: &str) -> bool {
    value.contains('.') || value.contains('w')
}
fn editable_version(ui: &mut egui::Ui, rect: Rect, value: &mut String) -> egui::Response {
    crate::ui_style::editable_combo(
        ui,
        rect,
        "resource-minecraft-version",
        value,
        &[
            "", "26.2", "26.1", "1.21.11", "1.21.8", "1.21.4", "1.21.1", "1.20.1", "1.19.2",
            "1.18.2", "1.16.5", "1.12.2", "1.7.10",
        ],
        "全部 (也可自行输入)",
    )
}

fn chevron(ui: &egui::Ui, center: Pos2, down: bool, color: Color32) {
    let points = if down {
        [
            center + Vec2::new(-4.0, -2.0),
            center + Vec2::new(0.0, 2.0),
            center + Vec2::new(4.0, -2.0),
        ]
    } else {
        [
            center + Vec2::new(-2.0, -4.0),
            center + Vec2::new(2.0, 0.0),
            center + Vec2::new(-2.0, 4.0),
        ]
    };
    ui.painter().add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(1.3_f32, color),
    ));
}
fn page_arrow(ui: &mut egui::Ui, left: bool, first: bool, enabled: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(23.0),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let color = if enabled {
        theme::palette(ui.ctx()).accent
    } else {
        Color32::from_gray(180)
    };
    let center = rect.center();
    let direction = if left { -1.0 } else { 1.0 };
    let points = [
        center + Vec2::new(-direction * 2.0, -4.5),
        center + Vec2::new(direction * 2.5, 0.0),
        center + Vec2::new(-direction * 2.0, 4.5),
    ];
    ui.painter().add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(1.8_f32, color),
    ));
    if first {
        ui.painter().line_segment(
            [
                center + Vec2::new(-5.0, -4.5),
                center + Vec2::new(-5.0, 4.5),
            ],
            egui::Stroke::new(1.8_f32, color),
        );
    }
    response.on_hover_text(if first {
        "第一页"
    } else if left {
        "上一页"
    } else {
        "下一页"
    })
}
fn compact_downloads(count: u64) -> String {
    if count > 100_000_000 {
        format!("{:.2} 亿", count as f64 / 100_000_000.0)
    } else if count > 100_000 {
        format!("{} 万", count / 10_000)
    } else {
        count.to_string()
    }
}
fn relative_date(value: &str) -> String {
    let Ok(date) = chrono::DateTime::parse_from_rfc3339(value) else {
        return "时间未知".into();
    };
    let days = (chrono::Utc::now() - date.with_timezone(&chrono::Utc))
        .num_days()
        .max(0);
    if days >= 365 {
        format!("{} 年前", days / 365)
    } else if days >= 30 {
        format!("{} 个月前", days / 30)
    } else if days > 0 {
        format!("{days} 天前")
    } else {
        "今天".into()
    }
}
#[allow(clippy::too_many_arguments)]
fn resource_item(
    ui: &mut egui::Ui,
    rect: Rect,
    assets: &crate::ui_style::Assets,
    icons: &HashMap<String, egui::TextureHandle>,
    kind: ResourceKind,
    hit: &resources::ProjectHit,
    show_mc: bool,
    show_loader: bool,
) {
    let icon = Rect::from_min_size(rect.min + Vec2::new(7.0, 6.7), Vec2::splat(50.0));
    if let Some(texture) = icons.get(&hit.project_id) {
        ui.place(
            icon,
            egui::Image::new(texture)
                .fit_to_exact_size(icon.size())
                .corner_radius(6),
        );
    } else {
        assets.icon(ui, resource_icon(kind), icon.shrink(10.0), MUTED);
    }
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(
            rect.min + Vec2::new(65.0, 6.2),
            Vec2::new(rect.width() - 72.0, 17.0),
        ),
        egui::Label::new(
            RichText::new(display_title(hit))
                .size(14.0)
                .color(theme::palette(ui.ctx()).text),
        )
        .truncate(),
    );
    let mut x = rect.left() + 64.0;
    for label in hit
        .categories
        .iter()
        .filter_map(|tag| {
            categories_for(
                kind,
                if hit.project_id.starts_with("cf:") {
                    resources::ResourceProvider::CurseForge
                } else {
                    resources::ResourceProvider::Modrinth
                },
            )
            .iter()
            .find(|(key, _)| key == tag)
            .map(|(_, label)| *label)
        })
        .take(3)
    {
        let galley = ui.painter().layout_no_wrap(
            label.into(),
            egui::FontId::proportional(11.0),
            Color32::from_gray(134),
        );
        let size = galley.size() + Vec2::new(6.0, 2.0);
        if x + size.x > rect.left() + 65.0 + (rect.width() - 72.0) * 0.5 {
            break;
        }
        let tag = Rect::from_min_size(Pos2::new(x, rect.top() + 41.2 - size.y), size);
        ui.painter()
            .rect_filled(tag, 3, Color32::from_black_alpha(17));
        ui.painter().galley(
            tag.min + Vec2::new(3.0, 1.0),
            galley,
            Color32::from_gray(134),
        );
        x += size.x + 3.0;
    }
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(
            Pos2::new(x + 1.0, rect.top() + 23.7),
            Vec2::new((rect.right() - x - 4.0).max(0.0), 17.0),
        ),
        egui::Label::new(
            RichText::new(hit.description.replace(['\r', '\n'], ""))
                .size(12.0)
                .color(MUTED),
        )
        .truncate(),
    )
    .on_hover_text(&hit.description);
    let mut parts = Vec::new();
    if show_loader {
        let loaders: Vec<_> = hit
            .categories
            .iter()
            .filter(|s| matches!(s.as_str(), "forge" | "neoforge" | "fabric" | "quilt"))
            .map(|s| loader_label(s))
            .collect();
        if !loaders.is_empty() {
            parts.push(loaders.join(" / "));
        }
    }
    if show_mc {
        parts.push(version_summary(&hit.versions));
    }
    let version_text = parts.join(" ");
    // MyResourceItem's metadata grid uses Auto / .7* / 55 / 1* / min57 / 1* / Auto / 1.7*.
    let origin = rect.min + Vec2::new(65.0, 41.2);
    let version_width = if version_text.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(
                version_text.clone(),
                egui::FontId::proportional(12.0),
                MUTED,
            )
            .size()
            .x
            .min((rect.width() - 72.0) * 0.42)
            + 17.0
    };
    let fixed =
        version_width + 10.5 + 5.0 + 55.0 + 2.0 + 11.5 + 5.0 + 57.0 + 2.0 + 11.5 + 5.0 + 53.0 + 2.0;
    let star = ((rect.width() - 72.0 - fixed) / 4.4).max(0.0);
    let mut px = origin.x;
    if version_width > 0.0 {
        assets.icon(
            ui,
            "game",
            Rect::from_min_size(Pos2::new(px, origin.y + 2.5), Vec2::splat(11.0)),
            MUTED,
        );
        metadata_text(
            ui,
            Pos2::new(px + 15.0, origin.y),
            version_width - 17.0,
            &version_text,
        );
        px += version_width + 0.7 * star;
    }
    assets.icon(
        ui,
        "download",
        Rect::from_min_size(Pos2::new(px, origin.y + 2.5), Vec2::splat(10.5)),
        MUTED,
    );
    metadata_text(
        ui,
        Pos2::new(px + 15.5, origin.y),
        55.0,
        &compact_downloads(hit.downloads),
    );
    px += 72.5 + star;
    let c = Pos2::new(px + 5.75, origin.y + 8.0);
    ui.painter()
        .circle_stroke(c, 5.0, egui::Stroke::new(1.0_f32, MUTED));
    ui.painter().line_segment(
        [c + Vec2::new(0.0, -3.0), c],
        egui::Stroke::new(1.0_f32, MUTED),
    );
    ui.painter().line_segment(
        [c, c + Vec2::new(2.5, 0.0)],
        egui::Stroke::new(1.0_f32, MUTED),
    );
    metadata_text(
        ui,
        Pos2::new(px + 16.5, origin.y),
        57.0,
        &relative_date(&hit.date_modified),
    );
    px += 75.5 + star;
    let c = Pos2::new(px + 5.75, origin.y + 8.0);
    ui.painter()
        .circle_stroke(c, 5.0, egui::Stroke::new(1.0_f32, MUTED));
    ui.painter().add(egui::Shape::ellipse_stroke(
        c,
        Vec2::new(2.3, 5.0),
        egui::Stroke::new(0.8_f32, MUTED),
    ));
    ui.painter().line_segment(
        [c - Vec2::new(5.0, 0.0), c + Vec2::new(5.0, 0.0)],
        egui::Stroke::new(0.8_f32, MUTED),
    );
    metadata_text(
        ui,
        Pos2::new(px + 16.5, origin.y),
        57.0,
        if hit.project_id.starts_with("cf:") {
            "CurseForge"
        } else {
            "Modrinth"
        },
    );
}
fn metadata_text(ui: &mut egui::Ui, pos: Pos2, width: f32, text: &str) {
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(pos, Vec2::new(width.max(0.0), 16.0)),
        egui::Label::new(RichText::new(text).size(12.0).color(MUTED)).truncate(),
    );
}
fn numeric_version(value: &str) -> Option<Vec<u32>> {
    let parts: Vec<_> = value
        .split('.')
        .map(|s| {
            s.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
        })
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() < 2 || !(parts[0] == 1 || parts[0] >= 26) {
        return None;
    }
    Some(parts)
}
fn grouped_version(value: &str, (drop, old): (bool, bool)) -> String {
    if value.contains('w') {
        return "快照版".into();
    }
    let Some(parts) = numeric_version(value) else {
        return "远古版".into();
    };
    if old && parts[0] == 1 && parts[1] < 12 {
        return "远古版".into();
    }
    if drop {
        format!("{}.{}", parts[0], parts[1])
    } else {
        value.into()
    }
}
fn version_summary(versions: &[String]) -> String {
    let mut values: Vec<_> = versions
        .iter()
        .filter(|v| numeric_version(v).is_some())
        .map(|v| grouped_version(v, (true, false)))
        .collect();
    values.sort_by_key(|value| std::cmp::Reverse(numeric_version(value)));
    values.dedup();
    match values.as_slice() {
        [] => "仅快照版本".into(),
        [only] => only.clone(),
        [a, b] => format!("{a}, {b}"),
        _ => format!(
            "{} 等",
            values
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn filter_options(versions: &[resources::ModrinthVersion]) -> ((bool, bool), Vec<String>) {
    let mut result = ((false, false), Vec::new());
    for scheme in [(false, false), (true, false), (false, true), (true, true)] {
        let mut filters: Vec<_> = versions
            .iter()
            .flat_map(|v| v.game_versions.iter())
            .map(|v| grouped_version(v, scheme))
            .collect();
        filters.sort_by(|a, b| numeric_version(b).cmp(&numeric_version(a)).then(a.cmp(b)));
        filters.dedup();
        result = (scheme, filters);
        if result.1.len() < 9 {
            break;
        }
    }
    result
}
struct VersionGroup {
    title: String,
    selected: bool,
    versions: Vec<usize>,
}
fn version_groups(state: &ResourceBrowser) -> Vec<VersionGroup> {
    let multiple_loaders = state.project.as_ref().is_some_and(|p| {
        p.loaders
            .iter()
            .filter(|s| matches!(s.as_str(), "forge" | "neoforge" | "fabric" | "quilt"))
            .count()
            > 1
    });
    let request = state.last_search.as_ref();
    let mc = request
        .and_then(|r| r.minecraft.as_deref())
        .unwrap_or_default();
    let loader = request
        .and_then(|r| r.loader.as_deref())
        .unwrap_or_default();
    let selected_base = format!(
        "{}{}",
        if loader.is_empty() {
            String::new()
        } else {
            format!("{} ", loader_label(loader))
        },
        mc
    );
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, version) in state.versions.iter().enumerate() {
        let mut seen = HashSet::new();
        for game in &version.game_versions {
            if !state.version_filter.is_empty()
                && grouped_version(game, state.filter_scheme) != state.version_filter
            {
                continue;
            }
            let name = grouped_version(game, (false, false));
            let loaders: Vec<_> = if multiple_loaders
                && state.kind == ResourceKind::Mod
                && numeric_version(game).is_some()
            {
                version
                    .loaders
                    .iter()
                    .filter(|s| matches!(s.as_str(), "forge" | "neoforge" | "fabric" | "quilt"))
                    .map(|s| format!("{} ", loader_label(s)))
                    .collect()
            } else {
                Vec::new()
            };
            for prefix in if loaders.is_empty() {
                vec![String::new()]
            } else {
                loaders
            } {
                let title = format!("{prefix}{name}");
                if seen.insert(title.clone()) {
                    groups.entry(title).or_default().push(i);
                }
            }
        }
    }
    let mut output: Vec<_> = groups
        .into_iter()
        .map(|(title, versions)| VersionGroup {
            title,
            versions,
            selected: false,
        })
        .collect();
    if !selected_base.is_empty()
        && (state.version_filter.is_empty()
            || grouped_version(mc, state.filter_scheme) == state.version_filter)
    {
        let versions: Vec<_> = state
            .versions
            .iter()
            .enumerate()
            .filter(|(_, v)| {
                (mc.is_empty() || v.game_versions.iter().any(|v| v == mc))
                    && (loader.is_empty() || v.loaders.iter().any(|v| v == loader))
            })
            .map(|(i, _)| i)
            .collect();
        if !versions.is_empty() {
            output.retain(|g| g.title != selected_base);
            output.push(VersionGroup {
                title: format!("{selected_base}（所选版本）"),
                selected: true,
                versions,
            });
        }
    }
    output.sort_by(|a, b| {
        b.selected.cmp(&a.selected).then_with(|| {
            let a_number = numeric_version(a.title.split_whitespace().last().unwrap_or(&a.title));
            let b_number = numeric_version(b.title.split_whitespace().last().unwrap_or(&b.title));
            b_number.cmp(&a_number).then(b.title.cmp(&a.title))
        })
    });
    for group in &mut output {
        group.versions.sort_by(|a, b| {
            state.versions[*b]
                .date_published
                .cmp(&state.versions[*a].date_published)
        });
    }
    output
}
fn version_row(
    ui: &mut egui::Ui,
    rect: Rect,
    assets: &crate::ui_style::Assets,
    version: &resources::ModrinthVersion,
    enabled: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.id().with((&version.id, rect.top().to_bits())),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &version.name)
    });
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 6, theme::palette(ui.ctx()).light);
    }
    let icon = match version.version_type.as_str() {
        "alpha" => "release-type-alpha",
        "beta" => "release-type-beta",
        _ => "release-type-release",
    };
    assets.icon(
        ui,
        icon,
        Rect::from_min_size(rect.min + Vec2::new(6.0, 5.0), Vec2::new(31.0, 32.0)),
        Color32::WHITE,
    );
    let title = trim_extension(&version.name);
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(
            rect.min + Vec2::new(44.0, 4.0),
            Vec2::new(rect.width() - 48.0, 18.0),
        ),
        egui::Label::new(
            RichText::new(title)
                .size(14.0)
                .color(theme::palette(ui.ctx()).text),
        )
        .truncate(),
    );
    let mut info = Vec::new();
    if !version.loaders.is_empty() {
        info.push(
            version
                .loaders
                .iter()
                .map(|v| loader_label(v))
                .collect::<Vec<_>>()
                .join("、"),
        );
    }
    if let Some(file) = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
    {
        let filename = trim_extension(&file.filename);
        if filename != title {
            info.push(filename.into());
        }
    }
    if !version.dependencies.is_empty() {
        info.push(format!("{} 项前置", version.dependencies.len()));
    }
    info.push(format!("更新于 {}", relative_date(&version.date_published)));
    if version.version_type != "release" {
        info.push(
            match version.version_type.as_str() {
                "alpha" => "测试版",
                "beta" => "预览版",
                _ => version.version_type.as_str(),
            }
            .into(),
        );
    }
    let text = info.join("，");
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(
            rect.min + Vec2::new(44.0, 22.0),
            Vec2::new(rect.width() - 48.0, 16.0),
        ),
        egui::Label::new(RichText::new(&text).size(12.0).color(MUTED)).truncate(),
    )
    .on_hover_text(text);
    response
}
fn trim_extension(value: &str) -> &str {
    for suffix in [".zip", ".jar", ".mrpack", ".litemod"] {
        if let Some(value) = value.strip_suffix(suffix) {
            return value;
        }
    }
    value
}

fn resource_icon(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Mod => "mod",
        ResourceKind::Modpack => "pack",
        ResourceKind::DataPack => "datapack",
        ResourceKind::ResourcePack => "resourcepack",
        ResourceKind::Shader => "shader",
    }
}
// Modrinth values are the right-hand tags in the corresponding upstream PageDownload*.xaml.
fn categories_for(
    kind: ResourceKind,
    provider: resources::ResourceProvider,
) -> &'static [(&'static str, &'static str)] {
    if provider == resources::ResourceProvider::CurseForge {
        curseforge_categories(kind)
    } else {
        categories(kind)
    }
}
fn categories(kind: ResourceKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        ResourceKind::Modpack => &[
            ("", "全部"),
            ("optimization", "性能优化"),
            ("challenging", "硬核"),
            ("combat", "战斗"),
            ("quests", "任务"),
            ("technology", "科技"),
            ("magic", "魔法"),
            ("adventure", "冒险"),
            ("kitchen-sink", "水槽包"),
            ("lightweight", "轻量整合"),
        ],
        ResourceKind::ResourcePack => &[
            ("", "全部"),
            ("vanilla-like", "原版风"),
            ("realistic", "写实风"),
            ("themed", "主题化"),
            ("simplistic", "简洁"),
            ("decoration", "装饰"),
            ("combat", "战斗"),
            ("utility", "实用"),
            ("tweaks", "改良"),
            ("cursed", "鬼畜"),
            ("entities", "含实体"),
            ("audio", "含声音"),
            ("fonts", "含字体"),
            ("models", "含模型"),
            ("locale", "含语言"),
            ("gui", "含 UI"),
            ("core-shaders", "核心着色器"),
            ("modded", "兼容 Mod"),
            ("8x-", "8x 或更低"),
            ("16x", "16x"),
            ("32x", "32x"),
            ("48x", "48x"),
            ("64x", "64x"),
            ("128x", "128x"),
            ("256x", "256x"),
            ("512x+", "512x 或更高"),
        ],
        ResourceKind::Shader => &[
            ("", "全部"),
            ("vanilla-like", "原版风"),
            ("fantasy", "幻想风"),
            ("realistic", "写实风"),
            ("semi-realistic", "半写实风"),
            ("cartoon", "卡通风"),
            ("colored-lighting", "彩色光照"),
            ("path-tracing", "路径追踪"),
            ("pbr", "PBR"),
            ("reflections", "反射"),
            ("potato", "极低"),
            ("low", "低"),
            ("medium", "中"),
            ("high", "高"),
            ("vanilla", "原版可用"),
            ("iris", "Iris"),
            ("optifine", "OptiFine"),
        ],
        ResourceKind::DataPack => &[
            ("", "全部"),
            ("worldgen", "世界元素"),
            ("technology", "科技"),
            ("game-mechanics", "游戏机制"),
            ("transportation", "运输"),
            ("storage", "仓储"),
            ("magic", "魔法"),
            ("adventure", "冒险"),
            ("decoration", "装饰"),
            ("mobs", "生物"),
            ("utility", "实用"),
            ("equipment", "装备与工具"),
            ("optimization", "性能优化"),
            ("social", "服务器"),
            ("library", "支持库"),
        ],
        ResourceKind::Mod => &[
            ("", "全部"),
            ("worldgen", "世界元素"),
            ("technology", "科技"),
            ("food", "食物与烹饪"),
            ("game-mechanics", "游戏机制"),
            ("transportation", "运输"),
            ("storage", "仓储"),
            ("magic", "魔法"),
            ("adventure", "冒险"),
            ("decoration", "装饰"),
            ("mobs", "生物"),
            ("utility", "实用"),
            ("equipment", "装备与工具"),
            ("optimization", "性能优化"),
            ("social", "服务器"),
            ("library", "支持库"),
        ],
    }
}

fn loader_label(value: &str) -> &str {
    match value {
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        _ => "任意 Mod 加载器",
    }
}
fn resource_frame() -> egui::Frame {
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
fn source_card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let response = resource_frame()
        .inner_margin(egui::Margin {
            left: 25,
            right: 25,
            top: 40,
            bottom: 15,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    let title_rect = Rect::from_min_size(
        response.response.rect.min + Vec2::new(15.0, 12.0),
        Vec2::new(response.response.rect.width() - 30.0, 18.0),
    );
    crate::ui_style::place_left(
        ui,
        title_rect,
        egui::Label::new(crate::ui_style::card_title(title)).halign(egui::Align::Min),
    );
    ui.add_space(7.0);
}
fn decode_icon(ctx: &egui::Context, id: &str, bytes: &[u8]) -> anyhow::Result<egui::TextureHandle> {
    // Reject oversized decoded images before allocating the pixel buffer.
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let dimensions = reader.into_dimensions()?;
    anyhow::ensure!(
        dimensions.0 > 0 && dimensions.1 > 0 && dimensions.0 <= 2048 && dimensions.1 <= 2048,
        "图标尺寸超出限制"
    );
    let pixels = image::load_from_memory(bytes)?.to_rgba8();
    Ok(ctx.load_texture(
        format!("resource:{id}"),
        egui::ColorImage::from_rgba_unmultiplied(
            [pixels.width() as usize, pixels.height() as usize],
            &pixels,
        ),
        egui::TextureOptions::LINEAR,
    ))
}

fn resource_target_info(version: &serde_json::Value) -> Option<(String, String)> {
    let libraries = version.get("libraries")?.as_array()?;
    let has = |prefix: &str| {
        libraries
            .iter()
            .any(|lib| lib["name"].as_str().is_some_and(|s| s.starts_with(prefix)))
    };
    let loader = if has("net.neoforged:neoforge:") || has("net.neoforged.fancymodloader:loader:") {
        "neoforge"
    } else if has("net.minecraftforge:forge:") {
        "forge"
    } else if has("org.quiltmc:quilt-loader:") {
        "quilt"
    } else if has("net.fabricmc:fabric-loader:") {
        "fabric"
    } else {
        return None;
    };
    Some((version["_pcl_jar_id"].as_str()?.to_owned(), loader.into()))
}

fn resource_url(kind: ResourceKind, hit: &resources::ProjectHit) -> String {
    let cf = hit.project_id.starts_with("cf:");
    let mut url = reqwest::Url::parse(if cf {
        "https://www.curseforge.com/minecraft/"
    } else {
        "https://modrinth.com/"
    })
    .expect("fixed URL");
    let category = if cf {
        match kind {
            ResourceKind::Mod => "mc-mods",
            ResourceKind::Modpack => "modpacks",
            ResourceKind::ResourcePack => "texture-packs",
            ResourceKind::Shader => "shaders",
            ResourceKind::DataPack => "data-packs",
        }
    } else {
        kind.web_type()
    };
    url.path_segments_mut()
        .expect("base URL")
        .pop_if_empty()
        .push(category)
        .push(if cf { &hit.slug } else { &hit.project_id });
    url.into()
}

// CurseForge IDs from upstream PageDownload*.xaml; see UPSTREAM-LICENSE.
fn curseforge_categories(kind: ResourceKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        ResourceKind::Mod => &[
            ("", "全部"),
            ("406", "世界元素"),
            ("407", "生物群系"),
            ("410", "维度"),
            ("408", "矿物与资源"),
            ("409", "天然结构"),
            ("412", "科技"),
            ("415", "管道与物流"),
            ("4843", "自动化"),
            ("417", "能源"),
            ("4558", "红石"),
            ("436", "食物与烹饪"),
            ("416", "农业"),
            ("414", "运输"),
            ("420", "仓储"),
            ("419", "魔法"),
            ("422", "冒险"),
            ("424", "装饰"),
            ("411", "生物"),
            ("5191", "实用"),
            ("434", "装备与工具"),
            ("9026", "创造模式"),
            ("6814", "性能优化"),
            ("423", "信息显示"),
            ("435", "服务器"),
            ("421", "支持库"),
        ],
        ResourceKind::Modpack => &[
            ("", "全部"),
            ("4484", "多人"),
            ("4479", "硬核"),
            ("4483", "战斗"),
            ("4478", "任务"),
            ("4472", "科技"),
            ("4473", "魔法"),
            ("4475", "冒险"),
            ("4476", "探索"),
            ("4477", "小游戏"),
            ("4474", "科幻"),
            ("4736", "空岛"),
            ("5128", "原版改良"),
            ("4487", "FTB"),
            ("4480", "基于地图"),
            ("4481", "轻量整合"),
            ("4482", "大型整合"),
        ],
        ResourceKind::DataPack => &[
            ("", "全部"),
            ("6951", "科技"),
            ("6952", "魔法"),
            ("6948", "冒险"),
            ("6949", "幻想"),
            ("6953", "实用"),
            ("6950", "支持库"),
            ("6946", "Mod 相关"),
        ],
        ResourceKind::ResourcePack => &[
            ("", "全部"),
            ("403", "原版风"),
            ("400", "写实风"),
            ("401", "现代风"),
            ("402", "中世纪"),
            ("399", "蒸汽朋克"),
            ("5244", "含字体"),
            ("404", "动态效果"),
            ("4465", "兼容 Mod"),
            ("393", "16x"),
            ("394", "32x"),
            ("395", "64x"),
            ("396", "128x"),
            ("397", "256x"),
            ("398", "512x 或更高"),
        ],
        ResourceKind::Shader => &[
            ("", "全部"),
            ("6555", "原版风"),
            ("6554", "幻想风"),
            ("6553", "写实风"),
        ],
    }
}

fn wiki_entry(hit: &resources::ProjectHit) -> Option<&'static pcl_core::wiki::Entry> {
    pcl_core::wiki::find(
        if hit.project_id.starts_with("cf:") {
            resources::ResourceProvider::CurseForge
        } else {
            resources::ResourceProvider::Modrinth
        },
        &hit.slug,
    )
}
fn display_title(hit: &resources::ProjectHit) -> &str {
    wiki_entry(hit)
        .and_then(|entry| entry.chinese.as_deref())
        .unwrap_or(&hit.title)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_pack_entry_survives_resource_kind_reset() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        app.resource_browser.kind = ResourceKind::Mod;
        app.task_view = true;
        app.open_local_pack_import();
        assert!(app.page == super::super::Page::Download);
        assert_eq!(app.download_tab, 2);
        assert_eq!(app.resource_browser.kind, ResourceKind::Modpack);
        assert!(app.resource_browser.local_pack);
        assert!(!app.task_view);
        app.ensure_resource_target();
        assert!(app.resource_browser.local_pack);
    }
    #[test]
    fn resource_form_accepts_pointer_and_text_after_idle_frames() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(folder.path());
        app.download_tab = 1;
        app.resource_browser.auto_searched = true;
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        fonts.families.insert(
            egui::FontFamily::Name("PCL Bold".into()),
            fonts.families[&egui::FontFamily::Proportional].clone(),
        );
        ctx.set_fonts(fonts);
        let frame = |app: &mut Launcher, events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.resource_page(ui);
                    });
                },
            );
        };
        let click = |app: &mut Launcher, pos| {
            for pressed in [true, false] {
                frame(
                    app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        };
        for _ in 0..5 {
            frame(&mut app, vec![]);
        }
        click(&mut app, Pos2::new(670.0, 99.0));
        assert!(
            egui::Popup::is_any_open(&ctx),
            "download form dropdown must open by pointer"
        );
        click(&mut app, Pos2::new(200.0, 60.0));
        frame(&mut app, vec![egui::Event::Text("abc".into())]);
        assert_eq!(
            app.resource_browser.query, "abc",
            "clicking the input must close the popup and accept typing"
        );
        frame(&mut app, vec![egui::Event::Ime(egui::ImeEvent::Enabled)]);
        frame(
            &mut app,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit("zhongwen".into()))],
        );
        frame(
            &mut app,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("中文".into()))],
        );
        frame(&mut app, vec![egui::Event::Ime(egui::ImeEvent::Disabled)]);
        assert_eq!(app.resource_browser.query, "abc中文");
        click(&mut app, Pos2::new(200.0, 99.0));
        frame(&mut app, vec![egui::Event::Text("1.21.1".into())]);
        assert_eq!(app.resource_browser.minecraft, "1.21.1");
        for _ in 0..3 {
            frame(&mut app, vec![]);
        }
        frame(&mut app, vec![egui::Event::Text("1".into())]);
        assert_eq!(
            app.resource_browser.minecraft, "1.21.11",
            "showing loader selector must not steal editor focus"
        );
        assert_eq!(app.resource_browser.query, "abc中文");
    }
    #[test]
    fn list_cancel_waits_for_the_matching_terminal_and_late_reply_keeps_new_busy() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temp.path());
        let cancel = Arc::new(AtomicBool::new(false));
        let old = app.resource_request(Some(cancel.clone()));
        app.resource_browser.loading = true;
        app.busy = Some("resource list".into());
        app.cancel_resource_request();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.resource_browser.loading);
        assert!(app.busy.is_some());
        app.handle_resource_event(ResourceEvent::Cancelled(old.clone()));
        assert!(app.resource_browser.cancelled);
        assert!(!app.resource_browser.loading);
        assert!(app.busy.is_none());
        let current = app.resource_request(Some(Arc::new(AtomicBool::new(false))));
        app.resource_browser.loading = true;
        app.busy = Some("new resource list".into());
        app.handle_resource_event(ResourceEvent::Cancelled(old));
        assert!(app.resource_browser.loading);
        assert_eq!(app.busy.as_deref(), Some("new resource list"));
        app.cancel_resource_request();
        app.handle_resource_event(ResourceEvent::Failed(current, "HTTP 503".into()));
        assert!(!app.resource_browser.cancelled);
        assert_eq!(app.resource_browser.page_error.as_deref(), Some("HTTP 503"));
        assert!(app.busy.is_none());
    }
    fn version(id: &str, game_versions: &[&str], loaders: &[&str]) -> resources::ModrinthVersion {
        serde_json::from_value(serde_json::json!({
            "id": id, "project_id": "project", "name": id, "version_number": id,
            "version_type": "release", "date_published": "2026-10-04T00:00:00Z",
            "game_versions": game_versions, "loaders": loaders, "files": []
        }))
        .unwrap()
    }
    #[test]
    fn detail_groups_all_versions_and_keeps_selected_loader_distinct() {
        let mut state = ResourceBrowser {
            versions: vec![
                version("fabric-new", &["1.21.1", "1.21.1"], &["fabric"]),
                version("forge-new", &["1.21.1"], &["forge"]),
                version("fabric-old", &["1.20.1"], &["fabric"]),
                version("snapshot", &["24w20a"], &["fabric"]),
            ],
            last_search: Some(resources::SearchOptions {
                sort: Default::default(),
            provider: resources::ResourceProvider::Modrinth,
                query: String::new(), minecraft: Some("1.21.1".into()), loader: Some("fabric".into()), offset: 0, limit: 20,
            }),
            project: Some(serde_json::from_value(serde_json::json!({
                "id":"project", "slug":"project", "title":"Project", "description":"", "body":"",
                "project_type":"mod", "icon_url":null, "downloads":0, "updated":"2026-10-04T00:00:00Z", "source_url":null,
                "loaders":["fabric","forge"], "game_versions":["1.21.1","1.20.1"]
            })).unwrap()),
            ..Default::default()
        };
        let groups = version_groups(&state);
        assert_eq!(groups[0].title, "Fabric 1.21.1（所选版本）");
        assert_eq!(groups[0].versions, vec![0]);
        assert!(groups
            .iter()
            .any(|g| g.title == "Forge 1.21.1" && g.versions == vec![1]));
        assert!(groups.iter().any(|g| g.title == "Fabric 1.20.1"));
        assert!(!groups.iter().any(|g| g.title == "Fabric 1.21.1"));
        state.version_filter = "1.20.1".into();
        let filtered = version_groups(&state);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].versions, vec![2]);
        // Local filtering must leave the complete API result available for the next selection.
        assert_eq!(state.versions.len(), 4);
    }
    #[test]
    fn filter_pills_follow_source_drop_folding_without_lexical_version_order() {
        let releases: Vec<_> = (0..12)
            .map(|patch| {
                version(
                    &format!("v{patch}"),
                    &[&format!("1.21.{patch}")],
                    &["fabric"],
                )
            })
            .collect();
        let (scheme, filters) = filter_options(&releases);
        assert_eq!(scheme, (true, false));
        assert_eq!(filters, vec!["1.21"]);
        let versions = [version("old", &["1.9", "1.10", "26.1", "24w20a"], &[])];
        let (_, filters) = filter_options(&versions);
        assert_eq!(filters, vec!["26.1", "1.10", "1.9", "快照版"]);
        assert_eq!(grouped_version("1.7.10", (true, true)), "远古版");
        assert!(!concrete_minecraft(""));
        assert!(concrete_minecraft("26.1"));
        assert!(concrete_minecraft("24w20a"));
    }
    #[test]
    fn resource_summary_never_claims_support_for_gaps_between_versions() {
        assert_eq!(
            version_summary(&["1.21.1".into(), "1.16.5".into(), "1.7.10".into()]),
            "1.21, 1.16, 1.7 等"
        );
        assert_eq!(compact_downloads(99999), "99999");
        assert_eq!(compact_downloads(100001), "10 万");
        assert_eq!(trim_extension("Pack.mrpack"), "Pack");
    }
    #[test]
    fn request_keys_reject_cross_category_stale_and_reset_browser_events() {
        let root = PathBuf::from("/example/game");
        let mut browser = ResourceBrowser::default();
        let key = RequestKey {
            kind: ResourceKind::Mod,
            generation: 0,
            root: root.clone(),
            epoch: browser.epoch.clone(),
        };
        assert!(browser.accepts(&key, &root));
        browser.kind = ResourceKind::DataPack;
        assert!(!browser.accepts(&key, &root));
        browser.kind = ResourceKind::Mod;
        browser.generation = 1;
        assert!(!browser.accepts(&key, &root));
        let reset = ResourceBrowser::default();
        assert!(!reset.accepts(&key, &root));
        browser.generation = 0;
        assert!(!browser.accepts(&key, std::path::Path::new("/different/root")));
    }
    #[test]
    fn official_modern_forge_profiles_select_the_correct_resource_loader() {
        for (fixture, loader) in [
            (
                include_str!("../../../pcl-core/tests/fixtures/forge/forge-version.json"),
                "forge",
            ),
            (
                include_str!("../../../pcl-core/tests/fixtures/forge/neoforge-version.json"),
                "neoforge",
            ),
        ] {
            let mut profile: serde_json::Value = serde_json::from_str(fixture).unwrap();
            // resolve_version carries the base client identity through profile inheritance.
            profile["_pcl_jar_id"] = serde_json::json!("1.21.1");
            assert_eq!(
                resource_target_info(&profile),
                Some(("1.21.1".into(), loader.into()))
            );
        }
        assert_eq!(
            resource_target_info(&serde_json::json!({"libraries":[],"_pcl_jar_id":"1.21.1"})),
            None
        );
    }
}
