//! Community resources use PageResource.xaml / MyResourceItem.xaml geometry.
use super::{loading_ui, Event, Launcher, MUTED};
use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};
use pcl_core::{
    config, metadata,
    resources::{self, ResourceKind},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
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
    dependency_projects: BTreeMap<DependencyRef, resources::ProjectHit>,
    dependency_failed: BTreeSet<DependencyRef>,
    dependency_pending: Option<RequestKey>,
    target: Option<String>,
    world: Option<PathBuf>,
    pack_name: String,
    pack_optional: bool,
    saved_folders: Vec<(ResourceKind, PathBuf)>,
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
    Dependencies(RequestKey, DependencyBatch),
    Failed(RequestKey, String),
    Cancelled(RequestKey),
    Installed(String, String),
    Plan(RequestKey, Vec<resources::PlannedResource>),
    PlanFinished(RequestKey),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DependencyRef {
    Project(String),
    Version(String),
}
#[derive(Default)]
pub(crate) struct DependencyBatch {
    projects: BTreeMap<DependencyRef, resources::ProjectHit>,
    failed: BTreeSet<DependencyRef>,
}
fn visible_dependency_project(id: &str) -> bool {
    // ResourceVersion.FromJson hides Fabric API and Quilt API from this list.
    !matches!(id, "P7dR8mSH" | "qvIfYCYJ" | "cf:306612" | "cf:634179")
}
fn required_dependencies<'a>(
    kind: ResourceKind,
    versions: impl IntoIterator<Item = &'a resources::ModrinthVersion>,
) -> Vec<DependencyRef> {
    if kind == ResourceKind::Modpack {
        return Vec::new();
    }
    versions
        .into_iter()
        .flat_map(|version| {
            version.dependencies.iter().filter_map(|dependency| {
                if dependency.dependency_type != "required" {
                    return None;
                }
                if let Some(id) = dependency.project_id.as_ref().filter(|id| !id.is_empty()) {
                    (id != &version.project_id && visible_dependency_project(id))
                        .then(|| DependencyRef::Project(id.clone()))
                } else {
                    dependency
                        .version_id
                        .as_ref()
                        .filter(|id| !id.is_empty())
                        .map(|id| DependencyRef::Version(id.clone()))
                }
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn dependency_hit(project: resources::ModrinthProject) -> resources::ProjectHit {
    resources::ProjectHit {
        project_id: project.id,
        slug: project.slug,
        title: project.title,
        description: project.description,
        author: String::new(),
        downloads: project.downloads,
        categories: project.loaders,
        icon_url: project.icon_url,
        date_modified: project.updated,
        versions: project.game_versions,
    }
}
fn resolve_dependency_projects(
    references: &[DependencyRef],
    cancel: &AtomicBool,
    mut get_project: impl FnMut(&str, &AtomicBool) -> anyhow::Result<resources::ModrinthProject>,
    mut get_version: impl FnMut(&str, &AtomicBool) -> anyhow::Result<resources::ModrinthVersion>,
) -> DependencyBatch {
    let mut batch = DependencyBatch::default();
    let mut cached = HashMap::<String, resources::ProjectHit>::new();
    for reference in references {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let result = (|| {
            let id = match reference {
                DependencyRef::Project(id) => id.clone(),
                DependencyRef::Version(id) => get_version(id, cancel)?.project_id,
            };
            if !visible_dependency_project(&id) {
                return Ok(None);
            }
            let project = if let Some(project) = cached.get(&id) {
                project.clone()
            } else {
                let project = dependency_hit(get_project(&id, cancel)?);
                cached.insert(id, project.clone());
                project
            };
            anyhow::Ok(Some(project))
        })();
        match result {
            Ok(Some(project)) => {
                batch.projects.insert(reference.clone(), project);
            }
            Ok(None) => {}
            Err(_) => {
                batch.failed.insert(reference.clone());
            }
        }
    }
    batch
}
fn resource_save_subdirectory(
    kind: ResourceKind,
    version: &resources::ModrinthVersion,
    instance: &std::path::Path,
) -> PathBuf {
    match kind {
        ResourceKind::Mod => instance.join("mods"),
        ResourceKind::ResourcePack => instance.join("resourcepacks"),
        ResourceKind::Shader => {
            let vanilla = version.loaders.iter().any(|loader| loader == "vanilla")
                && !version
                    .loaders
                    .iter()
                    .any(|loader| matches!(loader.as_str(), "iris" | "optifine"));
            instance.join(if vanilla {
                "resourcepacks"
            } else {
                "shaderpacks"
            })
        }
        _ => instance.to_path_buf(),
    }
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
        state.dependency_projects.clear();
        state.dependency_failed.clear();
        state.dependency_pending = None;
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
                if state.open_groups.is_empty() {
                    if let Some(first) = groups.first() {
                        state.open_groups.insert(first.title.clone());
                    }
                }
                self.status = format!("{}版本列表已更新", key.kind.label());
                self.load_resource_dependencies(false);
            }
            ResourceEvent::Dependencies(key, batch) => {
                let state = &mut self.resource_browser;
                if !state.accepts(&key, &self.settings.game_root)
                    || !state
                        .dependency_pending
                        .as_ref()
                        .is_some_and(|pending| pending.same(&key))
                {
                    return;
                }
                state.dependency_projects.extend(batch.projects);
                state.dependency_failed = batch.failed;
                state.dependency_pending = None;
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
            state.dependency_projects.clear();
            state.dependency_failed.clear();
            state.dependency_pending = None;
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
        state.dependency_projects.clear();
        state.dependency_failed.clear();
        state.dependency_pending = None;
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
        let dependency_hit = self
            .resource_browser
            .dependency_projects
            .values()
            .find(|project| project.project_id == id)
            .cloned();
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
                .cloned()
                .or(dependency_hit);
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
    fn load_resource_dependencies(&mut self, retry: bool) {
        let state = &mut self.resource_browser;
        if state.dependency_pending.is_some() {
            return;
        }
        let references = if retry {
            state.dependency_failed.iter().cloned().collect()
        } else {
            required_dependencies(state.kind, state.versions.iter())
        };
        if references.is_empty() {
            return;
        }
        let key = RequestKey {
            kind: state.kind,
            generation: state.generation,
            root: self.settings.game_root.clone(),
            epoch: state.epoch.clone(),
        };
        let cancel = if retry {
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(previous) = state.cancel.replace(cancel.clone()) {
                previous.store(true, Ordering::Relaxed);
            }
            cancel
        } else {
            state.cancel.clone().unwrap_or_default()
        };
        state.dependency_pending = Some(key.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let batch = resolve_dependency_projects(
                &references,
                &cancel,
                resources::get_project,
                resources::get_version,
            );
            let icons: BTreeMap<_, _> = batch
                .projects
                .values()
                .filter_map(|project| {
                    project
                        .icon_url
                        .as_ref()
                        .map(|url| (project.project_id.clone(), url.clone()))
                })
                .collect();
            let _ = tx.send(Event::Resource(ResourceEvent::Dependencies(
                key.clone(),
                batch,
            )));
            for (id, url) in icons {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(bytes) = resources::fetch_project_icon(&url, &cancel) {
                    let _ = tx.send(Event::Resource(ResourceEvent::Icon(key.clone(), id, bytes)));
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
        let mut refresh_versions = false;
        let minecraft_versions = resource_minecraft_versions(&self.manifest);
        let version_choices: Vec<_> = minecraft_versions.iter().map(String::as_str).collect();
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
            let version_response = editable_version(
                ui,
                version_rect,
                &mut self.resource_browser.minecraft,
                &version_choices,
            );
            if let super::download_ui::Phase::Failed(message) = &self.version_lists.manifest.phase {
                version_response
                    .clone()
                    .on_hover_text(format!("{message}\n右键可重新获取版本列表。"));
            }
            version_response.context_menu(|ui| {
                if ui
                    .add_enabled(self.busy.is_none(), egui::Button::new("刷新版本列表"))
                    .clicked()
                {
                    refresh_versions = true;
                    ui.close();
                }
            });
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
            ui.add_space(9.0);
            let previous = self.settings.resource_sort;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (label, _) =
                    ui.allocate_exact_size(Vec2::new(44.0, 28.0), egui::Sense::hover());
                crate::ui_style::place_left(ui, label, egui::Label::new("排序"));
                crate::ui_style::PclComboBox::from_id_salt("resource-sort")
                    .width(field_width)
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
            });
            if previous != self.settings.resource_sort {
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
        });
        if refresh_versions {
            self.load_manifest();
        }
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
                let show_mc = request.is_none_or(|request| request.minecraft.is_none());
                let show_loader = kind == ResourceKind::Mod
                    && request.is_none_or(|request| request.loader.is_none());
                let metadata_widths =
                    resource_metadata_widths(ui, &page.hits, show_mc, show_loader);
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
                    resource_item(
                        ui,
                        rect,
                        &self.assets,
                        &self.resource_browser.icons,
                        kind,
                        hit,
                        show_mc,
                        show_loader,
                        metadata_widths,
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
                    let metadata_widths = resource_metadata_widths(
                        ui,
                        std::slice::from_ref(hit),
                        true,
                        kind == ResourceKind::Mod,
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
                        metadata_widths,
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
                                .add_sized(
                                    Vec2::new(140.0, 35.0),
                                    egui::Button::new("转到 MC 百科"),
                                )
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
        let mut save_as = None;
        let mut dependency_chosen = None;
        let mut retry_dependencies = false;
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
                        header.min + Vec2::new(15.0, 10.0),
                        Vec2::new(header.width() - 55.0, 20.0),
                    ),
                    egui::Label::new(crate::ui_style::card_title(&group.title)).truncate(),
                );
                let center = Pos2::new(header.right() - 22.0, header.center().y);
                chevron(ui, center, open, theme::palette(ui.ctx()).text);
                toggle = response.clicked();
                if open {
                    let references = required_dependencies(
                        kind,
                        group
                            .versions
                            .iter()
                            .map(|index| &self.resource_browser.versions[*index]),
                    );
                    let mut projects = BTreeMap::new();
                    for reference in &references {
                        if let Some(project) =
                            self.resource_browser.dependency_projects.get(reference)
                        {
                            if Some(&project.project_id)
                                != self
                                    .resource_browser
                                    .project
                                    .as_ref()
                                    .map(|project| &project.id)
                            {
                                projects.insert(project.project_id.clone(), project.clone());
                            }
                        }
                    }
                    let failed = references.iter().any(|reference| {
                        self.resource_browser.dependency_failed.contains(reference)
                    });
                    let loading = !references.is_empty()
                        && self.resource_browser.dependency_pending.is_some();
                    let (selected, retry) = dependency_rows(
                        ui,
                        &self.assets,
                        &self.resource_browser.icons,
                        kind,
                        &projects.into_values().collect::<Vec<_>>(),
                        loading,
                        failed,
                        self.busy.is_none(),
                    );
                    if selected.is_some() {
                        dependency_chosen = selected;
                    }
                    retry_dependencies |= retry;
                    let duplicate_names =
                        group_has_duplicate_names(group, &self.resource_browser.versions);
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(
                            ui.available_width(),
                            42.0 * group.versions.len() as f32 + 18.0,
                        ),
                        egui::Sense::hover(),
                    );
                    for (row, index) in group.versions.iter().enumerate() {
                        let version = &self.resource_browser.versions[*index];
                        let row_rect = Rect::from_min_size(
                            rect.min + Vec2::new(20.0, 42.0 * row as f32),
                            Vec2::new(rect.width() - 38.0, 42.0),
                        );
                        if !ui.is_rect_visible(row_rect) {
                            continue;
                        }
                        let (row_response, save_clicked) = version_row(
                            ui,
                            row_rect,
                            &self.assets,
                            version,
                            self.busy.is_none() && self.game_pid.is_none(),
                            duplicate_names,
                            kind == ResourceKind::Modpack,
                        );
                        if save_clicked {
                            save_as = Some(version.id.clone());
                        } else if row_response.clicked() {
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
        if retry_dependencies {
            self.load_resource_dependencies(true);
        }
        if let Some(project) = dependency_chosen {
            self.open_resource(project);
            return;
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
        if let Some(version) = save_as {
            self.save_resource_file(&version);
        }
        self.resource_install_dialog(ui.ctx());
    }

    fn default_resource_save_directory(&self, version: &resources::ModrinthVersion) -> PathBuf {
        let kind = self.resource_browser.kind;
        if let Some((_, folder)) = self
            .resource_browser
            .saved_folders
            .iter()
            .find(|(saved, folder)| *saved == kind && folder.is_dir())
        {
            return folder.clone();
        }
        if kind == ResourceKind::Modpack {
            return self.settings.game_root.clone();
        }
        let mut candidates: Vec<_> = self
            .versions
            .iter()
            .filter_map(|installed| {
                let (minecraft, loader) = self.resource_target(&installed.id)?;
                if !resources::resource_compatible(kind, version, &minecraft, &loader) {
                    return None;
                }
                let instance =
                    config::instance_game_dir(&self.settings.game_root, &installed.id).ok()?;
                let folder = resource_save_subdirectory(kind, version, &instance);
                let selected = self.settings.selected_version.as_ref() == Some(&installed.id);
                let modified = std::fs::metadata(&folder)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                Some((selected, modified, folder))
            })
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        candidates
            .into_iter()
            .next()
            .map_or_else(|| self.settings.game_root.clone(), |(_, _, folder)| folder)
    }

    fn save_resource_file(&mut self, id: &str) {
        let Some(version) = self
            .resource_browser
            .versions
            .iter()
            .find(|version| version.id == id)
            .cloned()
        else {
            return;
        };
        let Some(file) = version
            .files
            .iter()
            .find(|file| file.primary)
            .or_else(|| version.files.first())
        else {
            self.error = Some("此版本没有可下载的文件。".into());
            return;
        };
        if let Err(error) = metadata::validate_id(&file.filename) {
            self.error = Some(format!("资源文件名无效：{error:#}"));
            return;
        }
        let kind = self.resource_browser.kind;
        let mut directory = self.default_resource_save_directory(&version);
        // Native pickers need an existing directory; no folder is created before the user chooses.
        while !directory.is_dir() && directory.pop() {}
        let extension = std::path::Path::new(&file.filename)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("zip");
        let Some(destination) = rfd::FileDialog::new()
            .set_title("选择保存位置")
            .set_directory(&directory)
            .set_file_name(&file.filename)
            .add_filter(format!("{}文件", kind.label()), &[extension])
            .save_file()
        else {
            return;
        };
        self.start_resource_save_at(kind, version, destination);
    }

    fn start_resource_save_at(
        &mut self,
        kind: ResourceKind,
        version: resources::ModrinthVersion,
        destination: PathBuf,
    ) {
        let Some(parent) = destination
            .parent()
            .filter(|parent| parent.is_dir())
            .map(std::path::Path::to_path_buf)
        else {
            self.error = Some("保存文件夹不存在，请重新选择。".into());
            return;
        };
        if std::fs::symlink_metadata(&destination).is_ok() {
            self.error = Some("文件已存在，请选择其他文件名；原文件未更改。".into());
            return;
        }
        let filename = destination
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let Some((tx, _)) = self.start_download_job_at(
            &format!("{}下载：{filename}", kind.label()),
            parent.clone(),
            Some(filename.clone()),
        ) else {
            return;
        };
        self.resource_browser
            .saved_folders
            .retain(|(saved, _)| *saved != kind);
        self.resource_browser.saved_folders.push((kind, parent));
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = resources::save_resource_version(
                kind,
                &version,
                &destination,
                &cancel,
                |progress| {
                    let _ = tx.send(Event::Progress(progress));
                },
            );
            let _ = tx.send(match result {
                Ok(()) => Event::Done(format!("已保存 {}", destination.display())),
                Err(error) => Event::download_failed("资源下载未完成", error),
            });
        });
    }

    fn resource_install_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.resource_browser.install_selection.clone() else {
            return;
        };
        if self.resource_browser.kind != ResourceKind::Modpack {
            self.resource_browser.install_selection = None;
            self.save_resource_file(&id);
            return;
        }
        if !self
            .resource_browser
            .versions
            .iter()
            .any(|version| version.id == id)
        {
            self.resource_browser.install_selection = None;
            return;
        }
        let valid = self.busy.is_none()
            && self.game_pid.is_none()
            && metadata::validate_id(self.resource_browser.pack_name.trim()).is_ok();
        let buttons: &[&str] = if valid {
            &["安装", "取消"]
        } else {
            &["关闭"]
        };
        let action = super::account_ui::modal_frame(
            ctx,
            "resource-install",
            "输入版本名称",
            508.0,
            62.0,
            buttons,
            |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.add_sized(
                    Vec2::new(ui.available_width(), 28.0),
                    egui::TextEdit::singleline(&mut self.resource_browser.pack_name)
                        .char_limit(100),
                );
                crate::ui_style::checkbox(
                    ui,
                    &mut self.resource_browser.pack_optional,
                    "安装可选客户端文件",
                    "",
                );
            },
        );
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
fn resource_minecraft_versions(manifest: &[serde_json::Value]) -> Vec<String> {
    let latest_release = manifest
        .iter()
        .filter(|entry| entry["type"] == "release")
        .filter_map(|entry| entry["releaseTime"].as_str())
        .max();
    let mut versions: Vec<_> = manifest
        .iter()
        .filter(|entry| {
            entry["type"] == "release"
                || (entry["type"] == "snapshot" && entry["releaseTime"].as_str() > latest_release)
        })
        .collect();
    versions.sort_by(|a, b| b["releaseTime"].as_str().cmp(&a["releaseTime"].as_str()));
    let mut seen = HashSet::new();
    let mut options = vec![String::new()];
    for entry in versions {
        if let Some(id) = entry["id"].as_str().filter(|id| !id.is_empty()) {
            if seen.insert(id) {
                options.push(id.to_owned());
            }
        }
    }
    options
}

fn editable_version(
    ui: &mut egui::Ui,
    rect: Rect,
    value: &mut String,
    versions: &[&str],
) -> egui::Response {
    crate::ui_style::editable_combo(
        ui,
        rect,
        "resource-minecraft-version",
        value,
        versions,
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
fn resource_version_text(hit: &resources::ProjectHit, show_mc: bool, show_loader: bool) -> String {
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
    parts.join(" ")
}
#[allow(clippy::too_many_arguments)]
fn dependency_rows(
    ui: &mut egui::Ui,
    assets: &crate::ui_style::Assets,
    icons: &HashMap<String, egui::TextureHandle>,
    kind: ResourceKind,
    projects: &[resources::ProjectHit],
    loading: bool,
    failed: bool,
    enabled: bool,
) -> (Option<String>, bool) {
    if projects.is_empty() && !loading && !failed {
        return (None, false);
    }
    let mut selected = None;
    let mut retry = false;
    let status_height = if loading || failed { 28.0 } else { 0.0 };
    // StackInstall left/right 20/18; headings add 6 left, 2/12 top, 5 bottom.
    let height = 25.0 + projects.len() as f32 * 64.0 + status_height + 35.0;
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let heading = |ui: &mut egui::Ui, y: f32, text: &str| {
        crate::ui_style::place_left(
            ui,
            Rect::from_min_size(
                Pos2::new(rect.left() + 26.0, y),
                Vec2::new(rect.width() - 44.0, 18.0),
            ),
            egui::Label::new(RichText::new(text).size(14.0)),
        );
    };
    heading(ui, rect.top() + 2.0, "前置资源");
    let metadata_widths = resource_metadata_widths(ui, projects, false, false);
    for (index, project) in projects.iter().enumerate() {
        let row = Rect::from_min_size(
            rect.min + Vec2::new(20.0, 25.0 + index as f32 * 64.0),
            Vec2::new(rect.width() - 38.0, 64.0),
        );
        if !ui.is_rect_visible(row) {
            continue;
        }
        let response = ui.interact(
            row,
            ui.id().with((
                "dependency-project",
                rect.top().to_bits(),
                &project.project_id,
            )),
            if enabled {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        if response.hovered() || response.has_focus() {
            ui.painter()
                .rect_filled(row, 6, theme::palette(ui.ctx()).light);
        }
        resource_item(
            ui,
            row,
            assets,
            icons,
            kind,
            project,
            false,
            false,
            metadata_widths,
        );
        if response.clicked() {
            selected = Some(project.project_id.clone());
        }
    }
    let status_y = rect.top() + 25.0 + projects.len() as f32 * 64.0;
    if loading || failed {
        let status = Rect::from_min_size(
            Pos2::new(rect.left() + 26.0, status_y),
            Vec2::new(rect.width() - 44.0, 28.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(status), |ui| {
            if loading {
                loading_ui::inline(ui, "正在获取前置资源");
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("前置资源获取失败").color(MUTED));
                    retry = ui.link("重试").clicked();
                });
            }
        });
    }
    heading(ui, status_y + status_height + 12.0, "版本列表");
    (selected, retry)
}

fn resource_metadata_widths(
    ui: &egui::Ui,
    hits: &[resources::ProjectHit],
    show_mc: bool,
    show_loader: bool,
) -> [f32; 3] {
    let measure = |text: String| {
        ui.painter()
            .layout_no_wrap(text, egui::FontId::proportional(12.0), MUTED)
            .size()
            .x
    };
    let mut widths = [0.0_f32, 57.0_f32, 0.0_f32];
    for hit in hits {
        widths[0] = widths[0].max(measure(resource_version_text(hit, show_mc, show_loader)));
        widths[1] = widths[1].max(measure(relative_date(&hit.date_modified)));
        widths[2] = widths[2].max(measure(
            if hit.project_id.starts_with("cf:") {
                "CurseForge"
            } else {
                "Modrinth"
            }
            .into(),
        ));
    }
    widths
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
    metadata_widths: [f32; 3],
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
    let title = display_title(hit);
    let mut title_job = egui::text::LayoutJob::default();
    title_job.append(
        title,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(14.0),
            color: theme::palette(ui.ctx()).text,
            ..Default::default()
        },
    );
    if title != hit.title && !title.contains(hit.title.as_str()) {
        title_job.append(
            &format!(" ({})", hit.title),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color: theme::palette(ui.ctx()).text.gamma_multiply(0.4),
                ..Default::default()
            },
        );
    }
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(
            rect.min + Vec2::new(65.0, 6.2),
            Vec2::new(rect.width() - 72.0, 17.0),
        ),
        egui::Label::new(title_job).truncate(),
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
    let version_text = resource_version_text(hit, show_mc, show_loader);
    // MyResourceItem's metadata grid uses Auto / .7* / 55 / 1* / min57 / 1* / Auto / 1.7*.
    let origin = rect.min + Vec2::new(65.0, 41.2);
    let version_width = if !show_mc && !show_loader {
        0.0
    } else {
        metadata_widths[0].min((rect.width() - 72.0) * 0.42) + 17.0
    };
    let time_width = metadata_widths[1];
    let source_width = metadata_widths[2];
    let fixed = version_width + 72.5 + 18.5 + time_width + 18.5 + source_width;
    let star =
        ((rect.width() - 72.0 - fixed) / if version_width > 0.0 { 4.4 } else { 3.7 }).max(0.0);
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
        time_width,
        &relative_date(&hit.date_modified),
    );
    px += 18.5 + time_width + star;
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
        source_width,
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
fn save_as_icon(ui: &egui::Ui, rect: Rect, color: Color32) {
    // Fixed upstream Modules/Base/ModBase.vb, Logo.IconButtonSave.
    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><path fill="white" d="M819.392 0L1024 202.752v652.16a168.96 168.96 0 0 1-168.832 168.768h-104.192a47.296 47.296 0 0 1-10.752 0H283.776a47.232 47.232 0 0 1-10.752 0H168.832A168.96 168.96 0 0 1 0 854.912V168.768A168.96 168.96 0 0 1 168.832 0h650.56z m110.208 854.912V242.112l-149.12-147.776H168.896c-41.088 0-74.432 33.408-74.432 74.432v686.144c0 41.024 33.344 74.432 74.432 74.432h62.4v-190.528c0-33.408 27.136-60.544 60.544-60.544h440.448c33.408 0 60.544 27.136 60.544 60.544v190.528h62.4c41.088 0 74.432-33.408 74.432-74.432z m-604.032 74.432h372.864v-156.736H325.568v156.736z m403.52-596.48a47.168 47.168 0 1 1 0 94.336H287.872a47.168 47.168 0 1 1 0-94.336h441.216z m0-153.728a47.168 47.168 0 1 1 0 94.4H287.872a47.168 47.168 0 1 1 0-94.4h441.216z"/></svg>"#;
    let key = egui::Id::new("resource-source-save-icon");
    let texture = ui
        .ctx()
        .data_mut(|data| data.get_temp::<egui::TextureHandle>(key))
        .unwrap_or_else(|| {
            let tree = resvg::usvg::Tree::from_str(SVG, &resvg::usvg::Options::default())
                .expect("fixed source icon");
            let mut bitmap = resvg::tiny_skia::Pixmap::new(60, 60).expect("small icon allocation");
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(60.0 / 1024.0, 60.0 / 1024.0),
                &mut bitmap.as_mut(),
            );
            let texture = ui.ctx().load_texture(
                "resource-save-as",
                egui::ColorImage::from_rgba_premultiplied([60, 60], bitmap.data()),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx()
                .data_mut(|data| data.insert_temp(key, texture.clone()));
            texture
        });
    ui.painter().image(
        texture.id(),
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        color,
    );
}

fn group_has_duplicate_names(
    group: &VersionGroup,
    versions: &[resources::ModrinthVersion],
) -> bool {
    let mut names = HashSet::new();
    group
        .versions
        .iter()
        .any(|index| !names.insert(versions[*index].name.as_str()))
}

fn version_file(version: &resources::ModrinthVersion) -> Option<&resources::VersionFile> {
    version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
}

fn version_title(version: &resources::ModrinthVersion, duplicate_name: bool) -> &str {
    trim_extension(if duplicate_name {
        version_file(version).map_or(version.name.as_str(), |file| file.filename.as_str())
    } else {
        &version.name
    })
}

#[allow(clippy::too_many_arguments)]
fn version_row(
    ui: &mut egui::Ui,
    rect: Rect,
    assets: &crate::ui_style::Assets,
    version: &resources::ModrinthVersion,
    enabled: bool,
    duplicate_name: bool,
    allow_save_as: bool,
) -> (egui::Response, bool) {
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
    let title = version_title(version, duplicate_name);
    let save_rect = Rect::from_min_size(
        egui::pos2(rect.right() - 30.0, rect.center().y - 12.5),
        Vec2::splat(25.0),
    );
    let save = allow_save_as.then(|| {
        ui.interact(
            save_rect,
            response.id.with("save-as"),
            if enabled {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        )
        .on_hover_text("另存为")
    });
    let show_save = save.as_ref().is_some_and(|save| {
        response.hovered() || response.has_focus() || save.hovered() || save.has_focus()
    });
    if show_save {
        if save.as_ref().is_some_and(|save| save.hovered()) {
            ui.painter()
                .rect_filled(save_rect, 3, theme::palette(ui.ctx()).pale);
        }
        save_as_icon(
            ui,
            save_rect.shrink(5.0),
            if enabled {
                theme::palette(ui.ctx()).accent
            } else {
                MUTED
            },
        );
    }
    let text_width = (rect.width() - 48.0 - if show_save { 31.0 } else { 0.0 }).max(0.0);
    crate::ui_style::place_left(
        ui,
        Rect::from_min_size(rect.min + Vec2::new(44.0, 3.0), Vec2::new(text_width, 18.0)),
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
    if let Some(file) = version_file(version) {
        let filename = trim_extension(&file.filename);
        if filename != title {
            info.push(filename.into());
        }
    }
    if !version.dependencies.is_empty() {
        info.push(format!("{} 项前置", version.dependencies.len()));
    }
    if version.game_versions.iter().all(|game| {
        !game.contains('.')
            || ["w", "snapshot", "rc", "pre", "experimental", "-"]
                .iter()
                .any(|part| game.to_ascii_lowercase().contains(part))
    }) && !version.game_versions.is_empty()
    {
        info.push(format!("游戏版本 {}", version.game_versions.join("、")));
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
            rect.min + Vec2::new(44.0, 23.0),
            Vec2::new(text_width, 16.0),
        ),
        egui::Label::new(RichText::new(&text).size(12.0).color(MUTED)).truncate(),
    )
    .on_hover_text(text);
    (response, save.is_some_and(|save| save.clicked()))
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
    fn ui_context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let regular = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), regular);
        ctx.set_fonts(fonts);
        ctx
    }

    fn project(id: &str) -> resources::ModrinthProject {
        serde_json::from_value(serde_json::json!({
            "id":id, "slug":id, "title":format!("Real project {id}"), "description":"Project summary", "body":"",
            "project_type":"mod", "icon_url":null, "downloads":42, "updated":"2026-10-04T00:00:00Z", "source_url":null
        })).unwrap()
    }
    #[test]
    fn dependency_projects_follow_required_source_rules_and_deduplicate_reads() {
        let mut file = version("main", &["1.21.1"], &["fabric"]);
        for (id, kind) in [
            ("a", "required"),
            ("a", "required"),
            ("b", "optional"),
            ("c", "incompatible"),
            ("project", "required"),
            ("P7dR8mSH", "required"),
            ("qvIfYCYJ", "required"),
            ("cf:306612", "required"),
            ("cf:634179", "required"),
        ] {
            file.dependencies.push(resources::Dependency {
                project_id: Some(id.into()),
                version_id: None,
                file_name: None,
                dependency_type: kind.into(),
            });
        }
        file.dependencies.push(resources::Dependency {
            project_id: None,
            version_id: Some("a-version".into()),
            file_name: None,
            dependency_type: "required".into(),
        });
        let refs = required_dependencies(ResourceKind::Mod, [&file]);
        assert_eq!(
            refs,
            vec![
                DependencyRef::Project("a".into()),
                DependencyRef::Version("a-version".into())
            ]
        );
        assert!(required_dependencies(ResourceKind::Modpack, [&file]).is_empty());
        let mut calls = Vec::new();
        let batch = resolve_dependency_projects(
            &refs,
            &AtomicBool::new(false),
            |id, _| {
                calls.push(id.to_owned());
                Ok(project(id))
            },
            |id, _| {
                assert_eq!(id, "a-version");
                let mut file = version(id, &[], &[]);
                file.project_id = "a".into();
                Ok(file)
            },
        );
        assert_eq!(calls, ["a"]);
        assert_eq!(batch.projects.len(), 2);
        assert!(batch.failed.is_empty());
        assert!(batch
            .projects
            .values()
            .all(|project| project.title == "Real project a"));
        let cancelled = resolve_dependency_projects(
            &refs,
            &AtomicBool::new(true),
            |_, _| panic!("cancelled project read"),
            |_, _| panic!("cancelled version read"),
        );
        assert!(cancelled.projects.is_empty());
    }
    #[test]
    fn dependency_failure_retry_and_late_events_keep_the_current_detail() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(folder.path());
        let reference = DependencyRef::Project("a".into());
        let key = app.resource_request(Some(Arc::new(AtomicBool::new(false))));
        app.resource_browser.dependency_pending = Some(key.clone());
        let failed = resolve_dependency_projects(
            std::slice::from_ref(&reference),
            &AtomicBool::new(false),
            |_, _| anyhow::bail!("HTTP 503"),
            |_, _| unreachable!(),
        );
        app.handle_resource_event(ResourceEvent::Dependencies(key.clone(), failed));
        assert!(app.resource_browser.dependency_failed.contains(&reference));
        assert!(app.resource_browser.dependency_projects.is_empty());
        app.resource_browser.dependency_pending = Some(key.clone());
        let recovered = resolve_dependency_projects(
            std::slice::from_ref(&reference),
            &AtomicBool::new(false),
            |id, _| Ok(project(id)),
            |_, _| unreachable!(),
        );
        app.handle_resource_event(ResourceEvent::Dependencies(key.clone(), recovered));
        assert!(app.resource_browser.dependency_failed.is_empty());
        assert_eq!(
            app.resource_browser.dependency_projects[&reference].title,
            "Real project a"
        );
        let current = app.resource_request(Some(Arc::new(AtomicBool::new(false))));
        app.resource_browser.dependency_pending = Some(current.clone());
        let old = resolve_dependency_projects(
            &[reference],
            &AtomicBool::new(false),
            |id, _| Ok(project(id)),
            |_, _| unreachable!(),
        );
        app.handle_resource_event(ResourceEvent::Dependencies(key, old));
        assert!(app.resource_browser.dependency_projects.is_empty());
        assert!(app
            .resource_browser
            .dependency_pending
            .as_ref()
            .unwrap()
            .same(&current));
    }
    #[test]
    fn dependency_project_row_retains_source_spacing_and_opens_actual_id() {
        let ctx = ui_context();
        let assets = crate::ui_style::Assets::new(&ctx);
        let projects = [dependency_hit(project("dependency-a"))];
        let icons = HashMap::new();
        let draw = |events| {
            let mut action = None;
            let mut height = 0.0;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let start = ui.next_widget_position().y;
                        action = dependency_rows(
                            ui,
                            &assets,
                            &icons,
                            ResourceKind::Mod,
                            &projects,
                            false,
                            false,
                            true,
                        )
                        .0;
                        height = ui.next_widget_position().y - start;
                    });
                },
            );
            (action, height, output)
        };
        let (_, height, output) = draw(vec![]);
        assert!(
            (height - 124.0).abs() < 1.0,
            "source heading + 64-DIP row + heading: {height}"
        );
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert!(text.contains(&"前置资源"));
        assert!(text.contains(&"版本列表"));
        assert!(text.contains(&"Real project dependency-a"));
        let point = Pos2::new(500.0, 55.0);
        let click = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(draw(vec![egui::Event::PointerMoved(point), click(true)])
            .0
            .is_none());
        assert_eq!(draw(vec![click(false)]).0.as_deref(), Some("dependency-a"));
    }
    #[test]
    fn vanilla_shader_save_directory_matches_archive_install_rules() {
        let root = std::path::Path::new("/instance");
        for (loaders, folder) in [
            (&["vanilla"][..], "resourcepacks"),
            (&["vanilla", "iris"][..], "shaderpacks"),
            (&["vanilla", "optifine"][..], "shaderpacks"),
            (&["iris"][..], "shaderpacks"),
        ] {
            assert_eq!(
                resource_save_subdirectory(
                    ResourceKind::Shader,
                    &version("shader", &["1.21.1"], loaders),
                    root
                ),
                root.join(folder)
            );
        }
    }
    #[test]
    fn saving_distinct_files_in_one_directory_has_distinct_queue_targets() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(folder.path());
        let first = app.settings.game_root.join("first.jar");
        let second = app.settings.game_root.join("second.jar");
        // Missing file metadata fails before network; terminal events stay queued here.
        app.start_resource_save_at(ResourceKind::Mod, version("a", &[], &[]), first.clone());
        let first_id = app.task_hub.selected.unwrap();
        app.start_resource_save_at(ResourceKind::Mod, version("b", &[], &[]), second.clone());
        let second_id = app.task_hub.selected.unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(
            app.jobs.get(first_id).unwrap().context.target.as_deref(),
            Some("first.jar")
        );
        assert_eq!(
            app.jobs.get(second_id).unwrap().context.target.as_deref(),
            Some("second.jar")
        );
        app.start_resource_save_at(ResourceKind::Mod, version("a", &[], &[]), first.clone());
        assert!(app
            .error
            .as_deref()
            .unwrap()
            .contains("此目标已有待完成的任务"));
        assert!(!first.exists() && !second.exists());
        app.jobs.cancel_all();
    }

    #[test]
    fn save_file_without_an_installed_target_has_a_directory_and_preserves_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        let version = version("download", &["1.21.1"], &["fabric"]);
        assert!(app.versions.is_empty());
        assert_eq!(
            app.default_resource_save_directory(&version),
            app.settings.game_root
        );
        let target = app.settings.game_root.join("existing.jar");
        std::fs::write(&target, b"existing user data").unwrap();
        app.settings.resource_naming = resources::ResourceNaming::ProjectVersion;
        app.start_resource_save_at(ResourceKind::Mod, version, target.clone());
        assert_eq!(std::fs::read(&target).unwrap(), b"existing user data");
        assert!(app.error.as_deref().unwrap().contains("文件已存在"));
        assert!(app.task.is_none());
        assert_eq!(
            app.settings.resource_naming,
            resources::ResourceNaming::ProjectVersion
        );
    }

    #[test]
    fn duplicate_version_names_show_distinct_file_titles() {
        let mut first = version("first", &["1.21.1"], &["fabric"]);
        let mut second = version("second", &["1.21.1"], &["fabric"]);
        for (version, file) in [(&mut first, "first.jar"), (&mut second, "second.jar")] {
            version.name = "Same release".into();
            version.files.push(resources::VersionFile {
                filename: file.into(),
                primary: true,
                url: String::new(),
                size: 1,
                file_type: None,
                hashes: BTreeMap::new(),
            });
        }
        let versions = vec![first, second];
        let group = VersionGroup {
            title: "1.21.1".into(),
            versions: vec![0, 1],
            selected: false,
        };
        assert!(group_has_duplicate_names(&group, &versions));
        assert_eq!(version_title(&versions[0], true), "first");
        assert_eq!(version_title(&versions[1], true), "second");
        assert_eq!(version_title(&versions[0], false), "Same release");
    }

    #[test]
    fn file_row_body_and_source_save_button_have_separate_pointer_actions() {
        let ctx = ui_context();
        let assets = crate::ui_style::Assets::new(&ctx);
        let version = version("pack", &["1.21.1"], &["fabric"]);
        let rect = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(500.0, 42.0));
        let draw = |events| {
            let mut action = None;
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let (row, save) =
                            version_row(ui, rect, &assets, &version, true, false, true);
                        action = if save {
                            Some("save")
                        } else if row.clicked() {
                            Some("install")
                        } else {
                            None
                        };
                    });
                },
            );
            action
        };
        assert!(draw(vec![]).is_none());
        for (point, expected) in [
            (Pos2::new(480.0, 41.0), "install"),
            (Pos2::new(502.5, 41.0), "save"),
        ] {
            let click = |pressed| egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            assert!(draw(vec![egui::Event::PointerMoved(point), click(true)]).is_none());
            assert_eq!(draw(vec![click(false)]), Some(expected));
        }
    }

    #[test]
    fn search_sort_lines_up_with_name_and_version_and_does_not_edit_naming() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        app.download_tab = 1;
        app.resource_browser.auto_searched = true;
        app.settings.resource_naming = resources::ResourceNaming::ProjectVersion;
        let ctx = ui_context();
        theme::apply(&ctx, &app.settings);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(818.0, 500.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.resource_page(ui));
            },
        );
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.text(), text.pos)),
                _ => None,
            })
            .collect();
        let position = |label: &str| labels.iter().find(|(text, _)| *text == label).unwrap().1;
        assert_eq!(position("名称").x, position("版本").x);
        assert_eq!(position("名称").x, position("排序").x);
        let inputs: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(shape)
                    if shape.rect.width() > 200.0
                        && (18.0..=32.0).contains(&shape.rect.height())
                        && shape.stroke.width > 0.0 =>
                {
                    Some(shape.rect)
                }
                _ => None,
            })
            .collect();
        assert!(
            inputs.len() >= 3,
            "expected name/version/sort frames: {inputs:?}"
        );
        let input_left = position("名称").x + 44.0;
        assert!(
            inputs
                .iter()
                .all(|rect| (rect.left() - input_left).abs() < 0.1),
            "name/version/sort input frames must share a left edge: {inputs:?}"
        );
        assert!(!labels.iter().any(|(text, _)| text.contains("文件命名")));
        assert_eq!(
            app.settings.resource_naming,
            resources::ResourceNaming::ProjectVersion
        );
    }
    #[test]
    fn resource_versions_follow_official_dates_without_a_fixed_ceiling() {
        let manifest = serde_json::json!([
            {"id":"26.2", "type":"release", "releaseTime":"2026-06-16T00:00:00Z"},
            {"id":"26.3", "type":"release", "releaseTime":"2026-09-15T00:00:00Z"},
            {"id":"26.3-rc-1", "type":"snapshot", "releaseTime":"2026-09-10T00:00:00Z"},
            {"id":"26.4-snapshot-2", "type":"snapshot", "releaseTime":"2026-09-29T00:00:00Z"},
            {"id":"26.3", "type":"release", "releaseTime":"2026-09-15T00:00:00Z"},
            {"id":"c0.30", "type":"old_alpha", "releaseTime":"2009-11-10T00:00:00Z"}
        ]);
        assert_eq!(
            resource_minecraft_versions(manifest.as_array().unwrap()),
            ["", "26.4-snapshot-2", "26.3", "26.2"]
        );
        let later = serde_json::json!([
            {"id":"27.10", "type":"release", "releaseTime":"2027-10-01T00:00:00Z"},
            {"id":"27.9", "type":"release", "releaseTime":"2027-09-01T00:00:00Z"}
        ]);
        assert_eq!(
            resource_minecraft_versions(later.as_array().unwrap()),
            ["", "27.10", "27.9"]
        );
        assert_eq!(resource_minecraft_versions(&[]), [""]);
    }
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
