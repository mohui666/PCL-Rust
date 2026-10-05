//! The fixed upstream help is inert display data, never executable XAML.
use super::{account_ui, xaml_ui, Launcher, MUTED};
use crate::{theme, ui_style};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, FontId, Rect, RichText, Vec2};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::Path,
    sync::{mpsc, Arc},
};

#[derive(Default, Deserialize)]
struct Catalog {
    entries: Vec<HelpEntry>,
    thanks: Vec<Credit>,
    sponsors: Vec<String>,
    upstream_licenses: Vec<HelpNode>,
    icons: HashMap<String, String>,
}
#[derive(Deserialize)]
struct HelpEntry {
    id: String,
    meta: HelpMetadata,
    nodes: Vec<HelpNode>,
    #[serde(default)]
    xaml: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HelpMetadata {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    keywords: String,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default = "yes")]
    show_in_public: bool,
    #[serde(default = "yes")]
    show_in_search: bool,
    #[serde(default)]
    is_event: bool,
    #[serde(default)]
    event_type: String,
    #[serde(default)]
    event_data: String,
}
fn yes() -> bool {
    true
}
type HelpNode = xaml_ui::Node;
#[derive(Deserialize)]
struct Credit {
    title: String,
    info: String,
    head: String,
    button: String,
    url: String,
}

pub(super) struct MoreState {
    pub(super) tab: usize,
    query: String,
    detail: Option<String>,
    history: Vec<String>,
    catalog: Arc<Catalog>,
    load_error: Option<String>,
    textures: HashMap<String, egui::TextureHandle>,
    message: Option<(String, String)>,
    renderer: xaml_ui::Renderer,
    external: Option<ExternalHelp>,
    pending_external: Option<mpsc::Receiver<Result<ExternalHelp>>>,
    external_target: Option<String>,
}
struct ExternalHelp {
    title: String,
    nodes: Vec<HelpNode>,
    origin: xaml_ui::Origin,
}

fn parse_help_document(source: &str) -> Result<Vec<HelpNode>> {
    // PageOtherHelpDetail.PanCustom: document defaults, without duplicating
    // the shell's 25/25/25/10 content margin.
    xaml_ui::parse(&format!(
        r#"<StackPanel><StackPanel.Resources>
        <Style TargetType="TextBlock"><Setter Property="TextWrapping" Value="Wrap"/></Style>
        <Style TargetType="local:MyCard"><Setter Property="Margin" Value="0,0,0,15"/></Style>
        <Style TargetType="Image"><Setter Property="HorizontalAlignment" Value="Center"/></Style>
        </StackPanel.Resources>{source}</StackPanel>"#
    ))
}

fn load_catalog() -> Result<Catalog> {
    let mut catalog: Catalog =
        serde_json::from_str(include_str!("../../assets/help/catalog.json"))?;
    for entry in &mut catalog.entries {
        if !entry.meta.is_event && !entry.xaml.is_empty() {
            entry.nodes = parse_help_document(&entry.xaml)
                .with_context(|| format!("帮助格式解析失败：{}", entry.id))?;
        }
    }
    // MyListItem_Loaded resolves the referenced HelpEntry when Title/Info
    // are omitted. The fixed catalog can resolve these without a network read.
    let metadata: HashMap<_, _> = catalog
        .entries
        .iter()
        .map(|entry| {
            (
                entry.id.clone(),
                (
                    entry.meta.title.clone(),
                    entry.meta.description.clone(),
                    if !entry.meta.is_event {
                        "block-grass"
                    } else if entry.meta.event_type == "弹出窗口" {
                        "block-path"
                    } else {
                        "block-command"
                    },
                ),
            )
        })
        .collect();
    for entry in &mut catalog.entries {
        hydrate_help_links(&mut entry.nodes, &metadata);
    }
    Ok(catalog)
}

fn hydrate_help_links(nodes: &mut [HelpNode], metadata: &HashMap<String, (String, String, &str)>) {
    for node in nodes {
        if node.tag == "MyListItem"
            && node.attr("EventType") == "打开帮助"
            && (node.attr("Title").is_empty() || node.attr("Info").is_empty())
        {
            let target = node.attr("EventData").replace('\\', "/");
            if let Some((title, description, icon)) = metadata.get(target.trim_end_matches(".json"))
            {
                node.attrs.insert("Title".into(), title.clone());
                node.attrs.insert("Info".into(), description.clone());
                node.attrs.insert("__pcl_help_icon".into(), (*icon).into());
            } else if !target.contains("://") && target.ends_with(".json") {
                // The fixed upstream writing guide contains one stale internal
                // reference. As in MyListItem_Loaded failure, it cannot fire an
                // unresolved action; show a readable row instead of a blank one.
                node.attrs.insert("Title".into(), "帮助条目未收录".into());
                node.attrs.insert("Info".into(), target);
                node.attrs.insert("IsEnabled".into(), "False".into());
                node.attrs.remove("EventType");
                node.attrs.remove("EventData");
            }
        }
        hydrate_help_links(&mut node.children, metadata);
    }
}

impl Default for MoreState {
    fn default() -> Self {
        let (catalog, load_error) = match load_catalog() {
            Ok(catalog) => (catalog, None),
            Err(error) => (
                Catalog::default(),
                Some(format!("本地帮助资料读取失败：{error}")),
            ),
        };
        Self {
            tab: 0,
            query: String::new(),
            detail: None,
            history: Vec::new(),
            catalog: Arc::new(catalog),
            load_error,
            textures: HashMap::new(),
            message: None,
            renderer: xaml_ui::Renderer::default(),
            external: None,
            pending_external: None,
            external_target: None,
        }
    }
}

#[derive(Debug, PartialEq)]
enum HelpAction {
    Web(String),
    Local(String),
    Unsupported(String),
}
fn web_link(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && !rest.is_empty()
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
        && !rest.split('/').next().unwrap_or("").contains('@')
}
fn help_action(kind: &str, data: &str, catalog: &Catalog) -> HelpAction {
    match kind {
        "打开网页" if web_link(data) => HelpAction::Web(data.into()),
        "打开帮助" => {
            let id = data.strip_suffix(".json").unwrap_or(data);
            if catalog.entries.iter().any(|entry| entry.id == id) {
                HelpAction::Local(id.into())
            } else {
                HelpAction::Unsupported("固定帮助快照中没有这个条目".into())
            }
        }
        _ => HelpAction::Unsupported(format!("本页仅浏览上游帮助说明，不执行“{kind}”事件")),
    }
}
fn categories(catalog: &Catalog) -> Vec<&str> {
    let mut categories = vec![];
    for entry in &catalog.entries {
        if !entry.meta.show_in_public {
            continue;
        }
        for category in &entry.meta.types {
            if !categories.contains(&category.as_str()) {
                categories.push(category.as_str());
            }
        }
    }
    if let Some(index) = categories.iter().position(|c| *c == "指南") {
        categories.remove(index);
        categories.insert(0, "指南");
    }
    categories
}
fn search_entries<'a>(catalog: &'a Catalog, query: &str) -> Vec<&'a HelpEntry> {
    let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut matches: Vec<_> = catalog
        .entries
        .iter()
        .filter_map(|entry| {
            if !entry.meta.show_in_public || !entry.meta.show_in_search {
                return None;
            }
            let title = entry.meta.title.to_lowercase();
            let keywords = entry.meta.keywords.to_lowercase();
            let desc = entry.meta.description.to_lowercase();
            let mut score = 0;
            for word in &words {
                let value = 3 * usize::from(keywords.contains(word))
                    + 2 * usize::from(title.contains(word))
                    + usize::from(desc.contains(word));
                if value == 0 {
                    return None;
                }
                score += value;
            }
            Some((score, entry))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    matches.into_iter().map(|(_, entry)| entry).collect()
}

fn frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
        .corner_radius(5)
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        })
}
fn more_card(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    title: &str,
    collapsed: Option<bool>,
    margin: egui::Margin,
    body: impl FnOnce(&mut egui::Ui),
) {
    let id = ui.id().with(salt);
    let mut open =
        ui.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || !collapsed.unwrap_or(false)));
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), 40.0),
                if collapsed.is_some() {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                },
            );
            ui_style::place_left(
                ui,
                Rect::from_min_size(
                    rect.min + Vec2::new(15.0, 12.0),
                    Vec2::new(rect.width() - 55.0, 18.0),
                ),
                egui::Label::new(ui_style::card_title(title)).truncate(),
            );
            if collapsed.is_some() {
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title)
                });
                if response.clicked() {
                    open = !open;
                    ui.data_mut(|d| d.insert_temp(id, open));
                }
                ui_style::card_chevron(ui, rect, open, theme::palette(ui.ctx()).text);
            }
            if open {
                egui::Frame::NONE.inner_margin(margin).show(ui, body);
            }
        });
    });
    ui.add_space(15.0);
}
const LIST_MARGIN: egui::Margin = egui::Margin {
    left: 20,
    right: 18,
    top: 0,
    bottom: 18,
};
const TEXT_MARGIN: egui::Margin = egui::Margin {
    left: 25,
    right: 23,
    top: 0,
    bottom: 18,
};

impl MoreState {
    fn load_textures(&mut self, ctx: &egui::Context) {
        if !self.textures.is_empty() {
            return;
        }
        for (name, path) in &self.catalog.icons {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><path d="{path}" fill="white"/></svg>"#
            );
            let Ok(tree) = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
            else {
                continue;
            };
            let bounds = tree.root().abs_bounding_box();
            let scale = 64.0 / bounds.width().max(bounds.height());
            let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(
                (bounds.width() * scale).ceil() as u32,
                (bounds.height() * scale).ceil() as u32,
            ) else {
                continue;
            };
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale)
                    .pre_translate(-bounds.x(), -bounds.y()),
                &mut pixmap.as_mut(),
            );
            self.textures.insert(
                name.clone(),
                ctx.load_texture(
                    format!("more-{name}"),
                    egui::ColorImage::from_rgba_premultiplied(
                        [pixmap.width() as usize, pixmap.height() as usize],
                        pixmap.data(),
                    ),
                    egui::TextureOptions::LINEAR,
                ),
            );
        }
        macro_rules! heads { ($($name:literal),+ $(,)?) => { [$(( $name, include_bytes!(concat!("../../assets/help/heads/", $name)).as_slice() )),+] }; }
        for (name, bytes) in heads![
            "LTCat.jpg",
            "HerobrineXia.png",
            "Logo.png",
            "bangbang93.png",
            "wiki.png",
            "z0z0r4.png",
            "EasyTier.png",
            "ChongQing.png",
            "00ll00.png",
            "Patrick.png",
            "Hao_Tian.jpg",
            "MCBBS.png",
            "PCL2.png"
        ]
        .into_iter()
        .chain([
            (
                "mohui666-avatar.jpg",
                // GitHub avatar source: https://avatars.githubusercontent.com/u/68949739?v=4
                include_bytes!("../../assets/mohui666-avatar.jpg").as_slice(),
            ),
            (
                "PCL-Rust.png",
                include_bytes!("../../assets/icon.png").as_slice(),
            ),
        ]) {
            if let Ok(image) = image::load_from_memory(bytes) {
                let rgba = image.to_rgba8();
                self.textures.insert(
                    name.into(),
                    ctx.load_texture(
                        format!("credit-{name}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [rgba.width() as usize, rgba.height() as usize],
                            rgba.as_raw(),
                        ),
                        egui::TextureOptions::LINEAR,
                    ),
                );
            }
        }
    }
}
impl Launcher {
    pub(super) fn open_help_content(&mut self, target: &str, ctx: &egui::Context) -> Result<()> {
        let id = target.trim_end_matches(".json");
        if self.more.catalog.entries.iter().any(|entry| entry.id == id) {
            self.open_local_help(id);
            return Ok(());
        }
        let target = target.to_owned();
        let folder = self
            .settings_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("help");
        let (sender, receiver) = mpsc::channel();
        let request = target.clone();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("pcl-help-content".into())
            .spawn(move || {
                let result = load_external_help(&request, &folder);
                let _ = sender.send(result);
                repaint.request_repaint();
            })?;
        self.more.pending_external = Some(receiver);
        self.more.external = None;
        self.more.detail = Some(format!("external:{target}"));
        self.more.external_target = Some(target);
        self.more.message = None;
        self.more.renderer.clear();
        self.page = super::Page::More;
        self.more.tab = 0;
        self.task_view = false;
        Ok(())
    }
    fn external_help_page(&mut self, ui: &mut egui::Ui) {
        if let Some(receiver) = &self.more.pending_external {
            match receiver.try_recv() {
                Ok(Ok(entry)) => {
                    self.more.external = Some(entry);
                    self.more.pending_external = None;
                }
                Ok(Err(error)) => {
                    self.more.pending_external = None;
                    self.more.message = Some(("帮助读取失败".into(), format!("{error:#}")));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.more.pending_external = None;
                    self.more.message = Some(("帮助读取失败".into(), "读取任务提前结束".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if self.more.pending_external.is_some() {
            ui.label("正在获取帮助内容……");
        }
        if let Some(entry) = &self.more.external {
            let values = self.custom_values();
            let actions = self
                .more
                .renderer
                .render(ui, &entry.nodes, &entry.origin, &values);
            let closeable: std::collections::HashSet<_> = actions
                .iter()
                .filter(|action| {
                    action.kind == "关闭提示"
                        && xaml_ui::declared_closeable_hint(&entry.nodes, &action.data)
                })
                .map(|action| action.data.clone())
                .collect();
            for action in actions {
                if action.kind == "关闭提示" && closeable.contains(&action.data) {
                    self.dismiss_custom_hint(&action.data);
                } else {
                    self.queue_custom_actions([action]);
                }
            }
        } else if self.more.pending_external.is_none() && ui.button("重新获取帮助").clicked()
        {
            if let Some(target) = self.more.external_target.clone() {
                if let Err(error) = self.open_help_content(&target, ui.ctx()) {
                    self.more.message = Some(("帮助读取失败".into(), format!("{error:#}")));
                }
            }
        }
    }
    pub(super) fn open_local_help(&mut self, id: &str) {
        if self.more.catalog.entries.iter().any(|entry| entry.id == id) {
            self.more.external = None;
            self.more.external_target = None;
            self.more.pending_external = None;
            self.page = super::Page::More;
            self.more.tab = 0;
            self.more.history.clear();
            self.more.detail = Some(id.into());
        } else {
            self.more.message =
                Some(("帮助操作不可用".into(), "固定帮助快照中没有这个条目".into()));
        }
    }
    pub(super) fn more_detail_title(&self) -> Option<&str> {
        if self.more.external_target.is_some() {
            return Some(
                self.more
                    .external
                    .as_ref()
                    .map_or("正在获取帮助", |entry| entry.title.as_str()),
            );
        }
        let id = self.more.detail.as_ref()?;
        self.more
            .catalog
            .entries
            .iter()
            .find(|entry| &entry.id == id)
            .map(|entry| entry.meta.title.as_str())
    }
    pub(super) fn leave_more_detail(&mut self) {
        self.more.external = None;
        self.more.external_target = None;
        self.more.pending_external = None;
        self.more.detail = self.more.history.pop();
    }
    pub(super) fn more_scroll_key(&self) -> (usize, Option<&str>) {
        (self.more.tab, self.more.detail.as_deref())
    }
    pub(super) fn more_navigation(&mut self, tab: usize) {
        if tab <= 1 {
            self.more.tab = tab;
            self.more.detail = None;
            self.more.history.clear();
            self.more.external = None;
            self.more.external_target = None;
            self.more.pending_external = None;
        }
    }

    pub(super) fn refresh_help(&mut self) {
        self.more = MoreState::default();
        self.status = "已重新载入随应用附带的本地帮助快照".into();
    }
    pub(super) fn more_icon(&mut self, ui: &egui::Ui, name: &str, rect: Rect, color: Color32) {
        self.more.load_textures(ui.ctx());
        if let Some(texture) = self.more.textures.get(name) {
            let size = texture.size_vec2();
            let factor = (rect.width() / size.x).min(rect.height() / size.y);
            ui.painter().image(
                texture.id(),
                Rect::from_center_size(rect.center(), size * factor),
                Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                color,
            );
        }
    }
    pub(super) fn more_page(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        self.more.load_textures(ui.ctx());
        match self.more.tab {
            1 => self.about_page(ui),
            _ => self.help_page(ui),
        }
    }
    fn help_page(&mut self, ui: &mut egui::Ui) {
        if self.more.external_target.is_some() {
            self.external_help_page(ui);
            return;
        }
        let catalog = self.more.catalog.clone();
        if let Some(error) = &self.more.load_error {
            ui.colored_label(Color32::DARK_RED, error);
            return;
        }
        let mut action = None;
        if let Some(id) = &self.more.detail {
            if let Some(entry) = catalog.entries.iter().find(|entry| &entry.id == id) {
                if entry.meta.is_event {
                    more_card(ui, &entry.id, &entry.meta.title, None, TEXT_MARGIN, |ui| {
                        if entry.meta.event_type == "弹出窗口" {
                            ui.label(
                                entry
                                    .meta
                                    .event_data
                                    .split_once('|')
                                    .map(|(_, body)| body)
                                    .unwrap_or(&entry.meta.event_data),
                            );
                        } else {
                            if ui
                                .add_sized([160.0, 35.0], egui::Button::new(&entry.meta.title))
                                .clicked()
                            {
                                self.queue_custom_actions(vec![xaml_ui::Action {
                                    kind: entry.meta.event_type.clone(),
                                    data: entry.meta.event_data.clone(),
                                }]);
                            }
                        }
                    });
                } else {
                    let values = self.custom_values();
                    let origin = xaml_ui::Origin {
                        directory: Some(super::home_ui::folder(&self.settings_path)),
                        url: None,
                    };
                    let actions = ui
                        .push_id(&entry.id, |ui| {
                            self.more
                                .renderer
                                .render(ui, &entry.nodes, &origin, &values)
                        })
                        .inner;
                    for action in actions {
                        if action.kind == "关闭提示"
                            && xaml_ui::declared_closeable_hint(&entry.nodes, &action.data)
                        {
                            self.dismiss_custom_hint(&action.data);
                        } else {
                            self.queue_custom_actions([action]);
                        }
                    }
                }
            }
        } else {
            help_search_box(ui, &mut self.more.query);
            ui.add_space(15.0);
            if self.more.query.trim().is_empty() {
                for category in categories(&catalog) {
                    more_card(
                        ui,
                        ("category", category),
                        category,
                        Some(category != "指南"),
                        LIST_MARGIN,
                        |ui| {
                            for entry in &catalog.entries {
                                if entry.meta.show_in_public
                                    && entry.meta.types.iter().any(|c| c == category)
                                    && help_row(ui, &self.assets, entry).clicked()
                                {
                                    action = Some(entry_action(entry, &catalog));
                                }
                            }
                        },
                    );
                }
            } else {
                let entries = search_entries(&catalog, &self.more.query);
                more_card(
                    ui,
                    "search",
                    if entries.is_empty() {
                        "无搜索结果"
                    } else {
                        "搜索结果"
                    },
                    None,
                    LIST_MARGIN,
                    |ui| {
                        for entry in entries {
                            if help_row(ui, &self.assets, entry).clicked() {
                                action = Some(entry_action(entry, &catalog));
                            }
                        }
                    },
                );
            }
        }
        if let Some(action) = action {
            self.perform_help_action(action);
        }
    }
    fn perform_help_action(&mut self, action: HelpAction) {
        match action {
            HelpAction::Web(url) => {
                if let Err(error) = webbrowser::open(&url) {
                    self.more.message = Some(("无法打开网页".into(), error.to_string()));
                }
            }
            HelpAction::Local(id) => {
                if let Some(previous) = self.more.detail.replace(id) {
                    self.more.history.push(previous);
                }
            }
            HelpAction::Unsupported(message) => {
                self.more.message = Some(("帮助操作不可用".into(), message))
            }
        }
    }
    fn about_page(&mut self, ui: &mut egui::Ui) {
        let catalog = self.more.catalog.clone();
        let textures = &self.more.textures;
        let mut url = None;
        more_card(
            ui,
            "about",
            "关于",
            None,
            egui::Margin {
                left: 21,
                right: 21,
                top: 0,
                bottom: 16,
            },
            |ui| {
                credit_row(
                    ui,
                    textures,
                    "LTCat.jpg",
                    "龙腾猫跃",
                    "Plain Craft Launcher 的原作者！",
                    "赞助 PCL !",
                    Some("https://meloong.com/afd/a/LTCat"),
                    &mut url,
                );
                credit_row(
                    ui,
                    textures,
                    "HerobrineXia.png",
                    "HerobrineXia",
                    "Plain Craft Launcher 的协力开发者！",
                    "",
                    None,
                    &mut url,
                );
                credit_row(
                    ui,
                    textures,
                    "mohui666-avatar.jpg",
                    "mohui666",
                    "Rust 跨平台版作者",
                    "",
                    None,
                    &mut url,
                );
                credit_row(
                    ui,
                    textures,
                    "PCL-Rust.png",
                    "PCL Rust",
                    &format!("版本：{} · 第三方版本", env!("CARGO_PKG_VERSION")),
                    "检查更新",
                    Some("pcl-rust:check-update"),
                    &mut url,
                );
            },
        );
        more_card(
            ui,
            "thanks",
            "特别鸣谢",
            None,
            egui::Margin {
                left: 21,
                right: 21,
                top: 0,
                bottom: 16,
            },
            |ui| {
                for credit in &catalog.thanks {
                    credit_row(
                        ui,
                        textures,
                        &credit.head,
                        &credit.title,
                        &credit.info,
                        &credit.button,
                        (!credit.url.is_empty()).then_some(credit.url.as_str()),
                        &mut url,
                    );
                }
                ui.add_space(5.0);
                ui.label(
                    RichText::new("以上鸣谢来自原版 PCL，部分服务未接入。")
                        .size(12.0)
                        .color(MUTED),
                );
            },
        );
        more_card(ui, "sponsors", "赞助者", None, TEXT_MARGIN, |ui| {
            ui.label("感谢以下赞助者对原版 PCL 的支持！");
            ui.add_space(5.5);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 3.0);
                ui.horizontal_wrapped(|ui| {
                    for name in &catalog.sponsors {
                        ui.label(name);
                    }
                });
            });
        });
        more_card(ui, "legal", "法律信息", Some(true), TEXT_MARGIN, |ui| {
            ui.label(RichText::new("第三方身份与许可").strong());
            ui.label("PCL Rust 是基于 PCL 2.13.1.1 公开源码开发的第三方版本。原版作者为龙腾猫跃，名称、图标和资料的权利归原权利人所有。");
            ui.add_space(10.0);
            ui.label("非 MINECRAFT 官方产品。未经 MOJANG 或 MICROSOFT 批准，也不与 MOJANG 或 MICROSOFT 关联。");
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                for (title, target) in [
                    (
                        "PCL 分发有限许可",
                        "https://shimo.im/docs/rGrd8pY8xWkt6ryW#anchor-X0bo",
                    ),
                    ("上游源码", "https://github.com/Meloong-Git/PCL"),
                ] {
                    if ui
                        .add_sized([170.0, 35.0], egui::Button::new(title))
                        .clicked()
                    {
                        url = Some(target.into());
                    }
                }
            });
            ui.add_space(12.0);
            ui.label(include_str!("../../../../UPSTREAM-LICENCE"));
        });
        more_card(
            ui,
            "license",
            "许可与版权声明",
            Some(true),
            TEXT_MARGIN,
            |ui| {
                ui.label("以下保留原版依赖的版权声明。PCL Rust 的依赖及版本见 Cargo.lock。");
                let mut action = None;
                for node in &catalog.upstream_licenses {
                    render_nodes(ui, &node.children, &catalog, &mut action);
                }
                if let Some(HelpAction::Web(target)) = action {
                    url = Some(target);
                }
            },
        );
        if ui
            .add_sized([150.0, 35.0], egui::Button::new("查看运行日志"))
            .clicked()
        {
            self.show_logs = true;
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_sized([150.0, 35.0], egui::Button::new("分析当前版本日志"))
                .clicked()
            {
                self.analyze_selected_logs(ui.ctx());
            }
            if ui
                .add_sized([150.0, 35.0], egui::Button::new("导入日志 / 崩溃报告"))
                .clicked()
            {
                self.import_crash_logs(ui.ctx());
            }
        });
        if let Some(url) = url {
            if url == "pcl-rust:check-update" {
                self.open_system_update_check();
            } else {
                self.perform_help_action(HelpAction::Web(url));
            }
        }
    }
    pub(super) fn more_dialogs(&mut self, ctx: &egui::Context) {
        self.appearance_music_dialog(ctx);
        self.custom_content_dialogs(ctx);
        if let Some((title, message)) = self.more.message.clone() {
            if account_ui::account_modal(ctx, "help-message", &title, &message, &["关闭"]).is_some()
            {
                self.more.message = None;
            }
        }
        if self.show_logs {
            let mut text = self.logs.iter().cloned().collect::<Vec<_>>().join("\n");
            if let Some(session) = &self.session {
                if session.access_token.len() > 16 {
                    text = text.replace(&session.access_token, "<redacted>");
                }
            }
            let size = ctx.content_rect().size();
            let height = (size.y - 215.0).clamp(120.0, 400.0);
            let action = account_ui::modal_frame(
                ctx,
                "runtime-log",
                "运行日志",
                (size.x - 50.0).clamp(400.0, 820.0),
                height,
                &["复制日志", "导出日志", "关闭"],
                |ui| {
                    ui.label(
                        RichText::new("本次运行日志。分享前请检查路径、用户名和服务器地址。")
                            .size(12.0)
                            .color(MUTED),
                    );
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .max_height(height - 42.0)
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&text).monospace().size(12.0))
                                    .wrap()
                                    .selectable(true),
                            );
                        });
                },
            );
            match action {
                Some(0) => ctx.copy_text(text),
                Some(1) => {
                    self.show_logs = false;
                    self.export_runtime_logs(ctx);
                }
                Some(2) => self.show_logs = false,
                _ => (),
            }
        }
    }
}
fn load_external_help(target: &str, folder: &Path) -> Result<ExternalHelp> {
    let remote = target.starts_with("https://") || target.starts_with("http://");
    let (metadata, origin) = if remote {
        let mut url = xaml_ui::http_url(target)?;
        let path = url.path().to_string();
        if !path.ends_with(".json") {
            anyhow::bail!("联网帮助入口必须是 .json 元数据地址");
        }
        let bytes = xaml_ui::fetch_bytes(url.as_str(), 64 * 1024)?;
        let metadata: HelpMetadata =
            serde_json::from_slice(&bytes).context("帮助元数据不是有效 JSON")?;
        url.set_path(&format!("{}.xaml", path.trim_end_matches(".json")));
        (
            metadata,
            xaml_ui::Origin {
                directory: None,
                url: Some(url.to_string()),
            },
        )
    } else {
        let relative = pcl_core::metadata::safe_relative(&target.replace('\\', "/"))?;
        let root = folder.canonicalize().context("本地帮助文件夹不存在")?;
        let path = root.join(relative).with_extension("json").canonicalize()?;
        if !path.starts_with(&root) {
            anyhow::bail!("帮助文件超出本地帮助文件夹");
        }
        let metadata: HelpMetadata = serde_json::from_slice(&read_help_file(&path, 64 * 1024)?)
            .context("帮助元数据不是有效 JSON")?;
        (
            metadata,
            xaml_ui::Origin {
                directory: path.parent().map(Path::to_owned),
                url: None,
            },
        )
    };
    let nodes = if metadata.is_event {
        vec![HelpNode {
            tag: "MyButton".into(),
            attrs: HashMap::from([
                ("Text".into(), metadata.title.clone()),
                ("EventType".into(), metadata.event_type),
                ("EventData".into(), metadata.event_data),
            ]),
            children: Vec::new(),
            triggers: Vec::new(),
        }]
    } else {
        let content = if let Some(url) = &origin.url {
            xaml_ui::fetch_bytes(url, xaml_ui::MAX_DOCUMENT)?
        } else {
            let relative = pcl_core::metadata::safe_relative(&target.replace('\\', "/"))?;
            let root = folder.canonicalize()?;
            let path = root.join(relative).with_extension("xaml").canonicalize()?;
            if !path.starts_with(&root) {
                anyhow::bail!("帮助内容超出本地帮助文件夹");
            }
            read_help_file(&path, xaml_ui::MAX_DOCUMENT)?
        };
        parse_help_document(&String::from_utf8(content).context("帮助内容必须使用 UTF-8")?)?
    };
    Ok(ExternalHelp {
        title: metadata.title,
        nodes,
        origin,
    })
}

fn read_help_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        anyhow::bail!("帮助文件过大");
    }
    Ok(bytes)
}

fn help_search_box(ui: &mut egui::Ui, query: &mut String) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::hover());
    ui.painter().add(
        egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        }
        .as_shape(rect, 5),
    );
    ui.painter()
        .rect_filled(rect, 5, Color32::from_rgba_unmultiplied(255, 255, 255, 245));
    let center = rect.min + Vec2::new(19.5, 18.5);
    let stroke = egui::Stroke::new(1.8_f32, theme::palette(ui.ctx()).accent);
    ui.painter().circle_stroke(center, 5.0, stroke);
    ui.painter().line_segment(
        [center + Vec2::new(3.5, 3.5), center + Vec2::new(8.5, 8.5)],
        stroke,
    );
    ui.place(
        Rect::from_min_max(
            rect.min + Vec2::new(32.0, 0.0),
            rect.max - Vec2::new(40.0, 0.0),
        ),
        crate::ui_style::singleline(query)
            .hint_text("搜索帮助")
            .char_limit(50)
            .frame(false)
            .font(FontId::proportional(13.5))
            .margin(0)
            .vertical_align(egui::Align::Center),
    );
    if !query.is_empty() {
        let clear = Rect::from_center_size(
            egui::pos2(rect.right() - 22.0, rect.center().y),
            Vec2::splat(24.0),
        );
        let response = ui.place(clear, egui::Button::new("").frame(false));
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "清除帮助搜索")
        });
        let c = clear.center();
        ui.painter().line_segment(
            [c - Vec2::splat(4.0), c + Vec2::splat(4.0)],
            egui::Stroke::new(1.7_f32, theme::palette(ui.ctx()).text),
        );
        ui.painter().line_segment(
            [c + Vec2::new(-4.0, 4.0), c + Vec2::new(4.0, -4.0)],
            egui::Stroke::new(1.7_f32, theme::palette(ui.ctx()).text),
        );
        if response.on_hover_text("清除搜索").clicked() {
            query.clear();
        }
    }
}
fn entry_action(entry: &HelpEntry, catalog: &Catalog) -> HelpAction {
    if entry.meta.is_event && entry.meta.event_type == "打开网页" {
        help_action(&entry.meta.event_type, &entry.meta.event_data, catalog)
    } else {
        HelpAction::Local(entry.id.clone())
    }
}
fn help_row(ui: &mut egui::Ui, assets: &ui_style::Assets, entry: &HelpEntry) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 42.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &entry.meta.title)
    });
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 3, theme::palette(ui.ctx()).light);
    }
    let icon = if !entry.meta.is_event {
        "block-grass"
    } else if entry.meta.event_type == "弹出窗口" {
        "block-path"
    } else {
        "block-command"
    };
    assets.icon(
        ui,
        icon,
        Rect::from_min_size(rect.min + Vec2::new(6.0, 5.0), Vec2::new(31.0, 32.0)),
        Color32::WHITE,
    );
    let left = rect.left() + 44.0;
    ui_style::place_left(
        ui,
        Rect::from_min_max(
            egui::pos2(left, rect.top() + 2.0),
            egui::pos2(rect.right() - 5.0, rect.top() + 23.0),
        ),
        egui::Label::new(
            RichText::new(&entry.meta.title)
                .size(14.0)
                .color(theme::palette(ui.ctx()).text),
        )
        .truncate(),
    );
    ui_style::place_left(
        ui,
        Rect::from_min_max(
            egui::pos2(left, rect.top() + 22.0),
            egui::pos2(rect.right() - 5.0, rect.bottom() - 1.0),
        ),
        egui::Label::new(
            RichText::new(&entry.meta.description)
                .size(12.0)
                .color(MUTED),
        )
        .truncate(),
    );
    response.on_hover_text(&entry.meta.description)
}
#[allow(clippy::too_many_arguments)]
fn credit_row(
    ui: &mut egui::Ui,
    textures: &HashMap<String, egui::TextureHandle>,
    head: &str,
    title: &str,
    info: &str,
    button: &str,
    url: Option<&str>,
    clicked: &mut Option<String>,
) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 45.0), egui::Sense::hover());
    if let Some(texture) = textures.get(head) {
        ui.place(
            Rect::from_min_size(rect.min + Vec2::new(3.0, 5.5), Vec2::splat(34.0)),
            egui::Image::new(texture)
                .fit_to_exact_size(Vec2::splat(34.0))
                .corner_radius(17),
        );
    }
    let left = rect.left() + 42.0;
    let right = rect.right() - if button.is_empty() { 0.0 } else { 165.0 };
    ui_style::place_left(
        ui,
        Rect::from_min_max(
            egui::pos2(left, rect.top() + 4.0),
            egui::pos2(right, rect.top() + 25.0),
        ),
        egui::Label::new(
            RichText::new(title)
                .size(14.0)
                .color(theme::palette(ui.ctx()).text),
        )
        .truncate(),
    );
    ui_style::place_left(
        ui,
        Rect::from_min_max(
            egui::pos2(left, rect.top() + 24.0),
            egui::pos2(right, rect.bottom()),
        ),
        egui::Label::new(RichText::new(info).size(12.0).color(MUTED)).truncate(),
    )
    .on_hover_text(info);
    if !button.is_empty() {
        let response = ui_style::outline_button(
            ui,
            Rect::from_min_size(
                egui::pos2(rect.right() - 150.0, rect.top() + 5.0),
                Vec2::new(150.0, 35.0),
            ),
            button,
            None,
            false,
            url.is_some(),
        );
        if response.clicked() {
            if let Some(url) = url {
                *clicked = Some(url.into());
            }
        }
    }
}
fn render_link(
    ui: &mut egui::Ui,
    title: &str,
    kind: &str,
    data: &str,
    catalog: &Catalog,
    action: &mut Option<HelpAction>,
) {
    let target = help_action(kind, data, catalog);
    let enabled = !matches!(target, HelpAction::Unsupported(_));
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(title).min_size(Vec2::new(100.0, 35.0)),
    );
    if response.clicked() {
        *action = Some(target);
    } else if let HelpAction::Unsupported(reason) = target {
        response.on_disabled_hover_text(reason);
    }
    ui.add_space(7.0);
}
fn render_nodes(
    ui: &mut egui::Ui,
    nodes: &[HelpNode],
    catalog: &Catalog,
    action: &mut Option<HelpAction>,
) {
    for (index, node) in nodes.iter().enumerate() {
        ui.push_id(index, |ui| match node.tag.as_str() {
            "MyCard" => more_card(
                ui,
                "help-card",
                node.attr("Title"),
                (node.attr("CanSwap") == "True").then_some(node.attr("IsSwapped") == "True"),
                TEXT_MARGIN,
                |ui| render_nodes(ui, &node.children, catalog, action),
            ),
            "TextBlock" | "Label" => {
                let text = node.attr("Text");
                if !text.is_empty() {
                    let mut style = RichText::new(text).size(
                        node.attr("FontSize")
                            .parse::<f32>()
                            .unwrap_or(13.0)
                            .clamp(10.0, 24.0),
                    );
                    if node.attr("FontWeight") == "Bold" {
                        style = style.strong();
                    }
                    ui.add(egui::Label::new(style).wrap().selectable(true));
                    ui.add_space(5.0);
                }
            }
            "MyHint" => {
                egui::Frame::NONE
                    .fill(if node.attr("IsWarn") == "False" {
                        theme::palette(ui.ctx()).light
                    } else {
                        Color32::from_rgb(255, 241, 223)
                    })
                    .inner_margin(10)
                    .corner_radius(3)
                    .show(ui, |ui| {
                        ui.label(node.attr("Text"));
                    });
                ui.add_space(15.0);
            }
            "MyImage" | "Image" => {
                let source = node.attr("Source");
                if web_link(source) {
                    if ui
                        .link("查看原帮助插图（在浏览器打开）")
                        .on_hover_text(source)
                        .clicked()
                    {
                        *action = Some(HelpAction::Web(source.into()));
                    }
                } else {
                    ui.label(
                        RichText::new("[此插图未收录到本地文本版]")
                            .size(12.0)
                            .color(MUTED),
                    );
                }
                ui.add_space(12.0);
            }
            "MyButton" | "MyTextButton" | "MyIconTextButton" | "MyIconButton" | "MyListItem" => {
                let text = if node.attr("Text").is_empty() {
                    node.attr("Title")
                } else {
                    node.attr("Text")
                };
                let text = if text.is_empty() {
                    "原帮助操作"
                } else {
                    text
                };
                render_link(
                    ui,
                    text,
                    node.attr("EventType"),
                    node.attr("EventData"),
                    catalog,
                    action,
                );
                if !node.attr("Info").is_empty() {
                    ui.label(RichText::new(node.attr("Info")).size(12.0).color(MUTED));
                }
            }
            "Grid.RowDefinitions"
            | "Grid.ColumnDefinitions"
            | "CustomEventService.Events"
            | "CustomEventCollection"
            | "CustomEvent"
            | "Path" => (),
            _ => render_nodes(ui, &node.children, catalog, action),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implicit_help_links_resolve_catalog_titles_descriptions_and_icons() {
        fn links<'a>(nodes: &'a [HelpNode], result: &mut Vec<&'a HelpNode>) {
            for node in nodes {
                if node.tag == "MyListItem" && node.attr("EventType") == "打开帮助" {
                    result.push(node);
                }
                links(&node.children, result);
            }
        }
        let catalog = load_catalog().unwrap();
        let mut count = 0;
        for entry in &catalog.entries {
            let mut items = Vec::new();
            links(&entry.nodes, &mut items);
            for item in items {
                let target = item.attr("EventData").replace('\\', "/");
                if let Some(target) = catalog
                    .entries
                    .iter()
                    .find(|entry| entry.id == target.trim_end_matches(".json"))
                {
                    assert_eq!(
                        item.attr("Title"),
                        target.meta.title,
                        "{} must display referenced title",
                        entry.id
                    );
                    assert_eq!(item.attr("Info"), target.meta.description);
                    assert!(!item.attr("__pcl_help_icon").is_empty());
                    count += 1;
                }
            }
        }
        assert!(
            count >= 6,
            "guide subentries must no longer be empty buttons"
        );
        let mut stale = vec![HelpNode {
            tag: "MyListItem".into(),
            attrs: HashMap::from([
                ("EventType".into(), "打开帮助".into()),
                ("EventData".into(), "Minecraft/新手教程.json".into()),
            ]),
            ..Default::default()
        }];
        hydrate_help_links(&mut stale, &HashMap::new());
        assert_eq!(stale[0].attr("Title"), "帮助条目未收录");
        assert_eq!(stale[0].attr("IsEnabled"), "False");
        assert!(stale[0].attr("EventType").is_empty());
    }

    #[test]
    fn every_bundled_help_document_parses_original_layout() {
        let catalog = load_catalog().unwrap();
        assert_eq!(catalog.entries.len(), 40);
        let content: Vec<_> = catalog
            .entries
            .iter()
            .filter(|e| !e.meta.is_event)
            .collect();
        assert_eq!(content.len(), 30);
        for entry in content {
            assert!(
                !entry.xaml.is_empty(),
                "{} must retain its original markup",
                entry.id
            );
            assert!(!entry.nodes.is_empty(), "{} must render", entry.id);
        }
    }
    #[test]
    fn all_help_pages_keep_native_document_geometry_and_text() {
        fn expand_all(nodes: &mut [HelpNode]) {
            for node in nodes {
                if node.tag == "MyCard" && node.attr("CanSwap") == "True" {
                    node.attrs.insert("IsSwapped".into(), "False".into());
                }
                expand_all(&mut node.children);
            }
        }
        let catalog = load_catalog().unwrap();
        for width in [589.0, 768.0] {
            for entry in catalog.entries.iter().filter(|e| !e.meta.is_event) {
                let mut nodes = entry.nodes.clone();
                expand_all(&mut nodes);
                let ctx = context();
                let mut renderer = xaml_ui::Renderer::default();
                let mut height = 0.0_f32;
                let mut content_width = 0.0_f32;
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(width, 18000.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::NONE)
                            .show(ctx, |ui| {
                                ui.spacing_mut().item_spacing = Vec2::ZERO;
                                renderer.render(
                                    ui,
                                    &nodes,
                                    &xaml_ui::Origin::default(),
                                    &HashMap::new(),
                                );
                                height = ui.min_rect().height();
                                content_width = ui.min_rect().width();
                            });
                    },
                );
                assert!(
                    height.is_finite() && height > 0.0,
                    "{}: invalid document extent",
                    entry.id
                );
                assert!(
                    content_width <= width + 1.0,
                    "{}: width {} exceeds {}",
                    entry.id,
                    content_width,
                    width
                );
                let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
                assert!(!meshes.is_empty(), "{} must paint", entry.id);
            }
        }
    }
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        ctx
    }
    #[test]
    fn personalization_author_avatar_loads_and_renders_in_existing_credit_row() {
        let ctx = context();
        let mut state = MoreState::default();
        state.load_textures(&ctx);
        let texture = &state.textures["mohui666-avatar.jpg"];
        assert_eq!(texture.size(), [460, 460]);
        let mut clicked = None;
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                credit_row(
                    ui,
                    &state.textures,
                    "mohui666-avatar.jpg",
                    "mohui666",
                    "Rust 跨平台版作者",
                    "",
                    None,
                    &mut clicked,
                );
            });
        });
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        assert!(primitives.iter().any(|primitive| matches!(&primitive.primitive, egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == texture.id())));
        assert!(clicked.is_none());
    }
    #[test]
    fn local_event_help_needs_no_xaml_and_local_document_stays_confined() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("event.json"),
            r#"{"Title":"Copy","IsEvent":true,"EventType":"复制文本","EventData":"fixture"}"#,
        )
        .unwrap();
        let event = load_external_help("event.json", root.path()).unwrap();
        assert_eq!(event.nodes[0].attr("EventType"), "复制文本");
        assert!(load_external_help("../outside.json", root.path()).is_err());
        fs::write(root.path().join("doc.json"), br#"{"Title":"Doc"}"#).unwrap();
        fs::write(root.path().join("doc.xaml"), "<TextBlock Text='document'/>").unwrap();
        assert_eq!(
            load_external_help("doc.json", root.path()).unwrap().nodes[0].children[0].attr("Text"),
            "document"
        );
    }
    #[test]
    fn help_cards_keep_source_40_header_18_footer_15_gap() {
        let ctx = context();
        let mut body = None;
        let mut next = 0.0;
        let mut end = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 700.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        more_card(
                            ui,
                            "fixture-open",
                            "指南",
                            Some(false),
                            LIST_MARGIN,
                            |ui| {
                                body = Some(
                                    ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 42.0),
                                        egui::Sense::hover(),
                                    )
                                    .0,
                                );
                            },
                        );
                        next = ui.next_widget_position().y;
                        more_card(
                            ui,
                            "fixture-closed",
                            "Minecraft",
                            Some(true),
                            LIST_MARGIN,
                            |_| panic!("collapsed card body"),
                        );
                        end = ui.next_widget_position().y;
                    });
            },
        );
        let body = body.unwrap();
        assert_eq!(body.min, egui::pos2(20.0, 40.0));
        assert_eq!(body.width(), 762.0);
        assert_eq!(next, 115.0); // header 40 + one row 42 + footer 18 + outer gap 15.
        assert_eq!(end - next, 55.0);
    }
    #[test]
    fn every_local_help_body_renders_without_running_any_action() {
        let state = MoreState::default();
        let ctx = context();
        for entry in &state.catalog.entries {
            let mut action = None;
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
                        render_nodes(ui, &entry.nodes, &state.catalog, &mut action)
                    });
                },
            );
            assert!(
                action.is_none(),
                "render executed an action for {}",
                entry.id
            );
        }
    }
    #[test]
    fn snapshot_has_real_entries_public_filter_and_internal_hidden_navigation() {
        let state = MoreState::default();
        assert!(state.load_error.is_none());
        let catalog = &state.catalog;
        assert_eq!(catalog.entries.len(), 40);
        assert_eq!(catalog.thanks.len(), 10);
        assert_eq!(catalog.sponsors.len(), 382);
        assert_eq!(categories(catalog).first(), Some(&"指南"));
        assert!(search_entries(catalog, "Defender").is_empty());
        assert!(!search_entries(catalog, "java").is_empty());
        assert!(!search_entries(catalog, "MOD").is_empty());
        assert!(matches!(
            help_action("打开帮助", "帮助/提交帮助 - VSCode.json", catalog),
            HelpAction::Local(_)
        ));
        assert!(matches!(
            help_action("打开帮助", "Minecraft/新手教程.json", catalog),
            HelpAction::Unsupported(_)
        ));
    }
    #[test]
    fn snapshot_actions_never_execute_xaml_files_commands_or_remote_help() {
        let catalog = MoreState::default().catalog;
        for (kind, data) in [
            ("打开文件", "C:\\evil.exe"),
            ("启动游戏", "x"),
            ("下载文件", "https://example.com/a.exe"),
            ("打开网页", "file:///etc/passwd"),
            ("打开网页", "https://name:secret@example.com"),
            ("打开帮助", "https://example.com/a.json"),
            ("打开帮助", "../../help.json"),
        ] {
            assert!(matches!(
                help_action(kind, data, &catalog),
                HelpAction::Unsupported(_)
            ));
        }
        assert_eq!(
            help_action("打开网页", "https://example.com/help", &catalog),
            HelpAction::Web("https://example.com/help".into())
        );
    }
    #[test]
    fn fixed_sidebar_vectors_are_valid() {
        let state = MoreState::default();
        assert_eq!(state.catalog.icons.len(), 6);
        for path in state.catalog.icons.values() {
            let tree = resvg::usvg::Tree::from_str(
                &format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><path d="{path}"/></svg>"#),
                &resvg::usvg::Options::default(),
            )
            .unwrap();
            assert!(tree.root().abs_bounding_box().width() > 0.0);
        }
    }
}
