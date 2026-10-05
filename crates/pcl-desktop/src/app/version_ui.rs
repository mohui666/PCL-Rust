//! Instance pages follow the supplied PCL 2.13.1.1 / 125% Windows references.
use super::{Launcher, Page, MUTED};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};
use pcl_core::{config, metadata, model::InstalledVersion, mods};
use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[path = "file_drop_ui.rs"]
mod file_drop;
#[path = "version_management_ui.rs"]
mod management;

#[derive(Clone)]
struct VersionPresentation {
    group: &'static str,
    icon: &'static str,
    custom_icon: Option<std::path::PathBuf>,
    description: String,
    repair: bool,
    favorite: bool,
    hidden: bool,
    modable: Option<bool>,
}

type Catalog = Vec<(InstalledVersion, VersionPresentation)>;
#[derive(Clone)]
struct CatalogCache {
    fingerprint: u64,
    refreshed: Instant,
    loading: bool,
    entries: Arc<Catalog>,
    pending: Arc<Mutex<Option<Arc<Catalog>>>>,
}

type DetailResult = Result<Vec<mods::RemoteModDetails>, String>;
#[derive(Clone)]
struct ModDetailCache {
    key: String,
    instance: std::path::PathBuf,
    stamp: String,
    checked: Instant,
    frame: u64,
    cancel: Arc<AtomicBool>,
    pending: Arc<Mutex<Option<DetailResult>>>,
    entries: Arc<Vec<mods::RemoteModDetails>>,
    icons: HashMap<String, egui::TextureHandle>,
    loading: bool,
    error: Option<String>,
    refresh_local: bool,
}
fn mod_details_key() -> egui::Id {
    egui::Id::new("local-mod-remote-details")
}
fn clear_mod_details(ctx: &egui::Context) {
    if let Some(cache) = ctx.data(|data| data.get_temp::<ModDetailCache>(mod_details_key())) {
        cache.cancel.store(true, Ordering::Relaxed);
    }
    ctx.data_mut(|data| data.remove::<ModDetailCache>(mod_details_key()));
}
fn mod_details(
    ctx: &egui::Context,
    instance: &Path,
    mods: &[mods::LocalMod],
    visible: &[mods::LocalMod],
) -> ModDetailCache {
    let eligible: Vec<_> = mods
        .iter()
        .filter(|m| m.error.is_none() && m.metadata.inspected)
        .cloned()
        .collect();
    let key = format!(
        "{instance:?}/{:?}",
        eligible
            .iter()
            .map(|m| (&m.file_name, &m.mod_ids, &m.version))
            .collect::<Vec<_>>()
    );
    let previous = ctx.data(|data| data.get_temp::<ModDetailCache>(mod_details_key()));
    let mut cache = previous.filter(|p| p.key == key);
    let stamp = if cache
        .as_ref()
        .is_none_or(|c| c.checked.elapsed() >= Duration::from_secs(2))
    {
        Some(mods::remote_snapshot(instance, &eligible).map_err(|e| format!("{e:#}")))
    } else {
        None
    };
    let checked = stamp.is_some();
    let mut refresh_local = false;
    if let (Some(old), Some(current)) = (&cache, &stamp) {
        if current.as_ref().ok() != Some(&old.stamp) {
            old.cancel.store(true, Ordering::Relaxed);
            refresh_local = true;
            cache = None;
        }
    }
    if cache.is_none() {
        clear_mod_details(ctx);
        let pending = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let initial = stamp.unwrap_or_else(|| {
            mods::remote_snapshot(instance, &eligible).map_err(|e| format!("{e:#}"))
        });
        let error = initial.as_ref().err().cloned();
        let value = ModDetailCache {
            key,
            instance: instance.to_owned(),
            stamp: initial.unwrap_or_default(),
            checked: Instant::now(),
            frame: ctx.cumulative_frame_nr(),
            cancel: cancel.clone(),
            pending: pending.clone(),
            entries: Arc::new(vec![]),
            icons: HashMap::new(),
            loading: false,
            error,
            refresh_local,
        };
        cache = Some(value);
    }
    let mut cache = cache.unwrap();
    if checked {
        cache.checked = Instant::now();
    }
    let result = cache.pending.lock().ok().and_then(|mut slot| slot.take());
    if let Some(result) = result {
        cache.loading = false;
        // A response can finish between periodic filesystem checks. Revalidate
        // its exact file snapshot before publishing even a read-only title.
        let result =
            if mods::remote_snapshot(instance, &eligible).as_ref().ok() != Some(&cache.stamp) {
                cache.refresh_local = true;
                Err("Mod 文件已经改变，正在重新读取本地信息".into())
            } else {
                result
            };
        match result {
            Ok(entries) => {
                for value in &entries {
                    if let Some(bytes) = &value.icon {
                        if let Ok(image) = image::load_from_memory(bytes) {
                            let rgba = image.into_rgba8();
                            cache.icons.insert(
                                value.file_name.clone(),
                                ctx.load_texture(
                                    format!("local-mod-icon-{}", value.file_name),
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
                let mut combined = cache.entries.as_ref().clone();
                for entry in entries {
                    combined.retain(|old| old.file_name != entry.file_name);
                    combined.push(entry);
                }
                cache.entries = Arc::new(combined);
                cache.error = None;
            }
            Err(error) => cache.error = Some(error),
        }
    }
    if !cache.loading && cache.error.is_none() && !cache.refresh_local {
        let batch: Vec<_> = visible
            .iter()
            .filter(|m| {
                m.error.is_none()
                    && m.metadata.inspected
                    && !cache
                        .entries
                        .iter()
                        .any(|entry| entry.file_name == m.file_name)
            })
            .take(32)
            .cloned()
            .collect();
        if !batch.is_empty() {
            cache.loading = true;
            let root = instance.to_owned();
            let cancel = cache.cancel.clone();
            let pending = cache.pending.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result =
                    mods::load_remote_details(&root, &batch, &cancel).map_err(|e| format!("{e:#}"));
                if !cancel.load(Ordering::Relaxed) {
                    if let Ok(mut slot) = pending.lock() {
                        *slot = Some(result);
                    }
                    ctx.request_repaint();
                }
            });
        }
    }
    cache.frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|data| data.insert_temp(mod_details_key(), cache.clone()));
    cache
}

impl Launcher {
    /// Call after the page has rendered. Navigation cancels only its metadata reader.
    pub(super) fn mod_details_finish_frame(&mut self, ctx: &egui::Context) {
        if ctx
            .data(|data| data.get_temp::<ModDetailCache>(mod_details_key()))
            .is_some_and(|cache| cache.frame != ctx.cumulative_frame_nr())
        {
            clear_mod_details(ctx);
        }
    }
    pub(super) fn versions_page(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        if self.versions.is_empty() {
            titled_card(ui, "暂无本地版本", |ui| {
                ui.label("可以下载新版本，或在设置里选择已有的 .minecraft 目录。");
                if button(ui, "前往下载", 140.0, true, true).clicked() {
                    self.page = Page::Download;
                    self.version_view = false;
                }
            });
            return;
        }
        let entries = version_catalog(ui.ctx(), &self.settings.game_root, &self.versions);
        let hidden_key = ui.make_persistent_id(("show-hidden-versions", &self.settings.game_root));
        let mut show_hidden = ui.data(|data| data.get_temp::<bool>(hidden_key).unwrap_or(false));
        if ui.input(|input| input.key_pressed(egui::Key::F11)) {
            show_hidden = !show_hidden;
            ui.data_mut(|data| data.insert_temp(hidden_key, show_hidden));
        }
        let mut selected = None;
        let mut tools = None;
        let mut favorite_toggle = None;
        let mut visible_count = 0;
        for group in [
            "收藏夹",
            "隐藏的版本",
            "可安装 Mod 的版本",
            "常规版本",
            "不常用版本",
            "愚人节版本",
            "Fabric 版本",
            "Forge 版本",
            "NeoForge 版本",
            "Quilt 版本",
            "正式版",
            "快照版本",
            "其他版本",
            "错误版本",
        ] {
            let values: Vec<_> = entries
                .iter()
                .filter(|(_, info)| {
                    if show_hidden {
                        group == "隐藏的版本" && info.hidden
                    } else if group == "收藏夹" {
                        info.favorite && !info.hidden
                    } else {
                        info.group == group && !info.hidden
                    }
                })
                .collect();
            if values.is_empty() {
                continue;
            }
            visible_count += values.len();
            let fold_id = ui.make_persistent_id(("version-category", group));
            let mut open = ui.data_mut(|data| data.get_temp::<bool>(fold_id).unwrap_or(true));
            page_frame().inner_margin(0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                let (header, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 40.0),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    open = !open;
                }
                ui_style::place_left(
                    ui,
                    Rect::from_min_size(
                        header.min + Vec2::new(15.0, 10.0),
                        Vec2::new(header.width() - 50.0, 20.0),
                    ),
                    egui::Label::new(ui_style::card_title(&if group == "收藏夹" {
                        group.into()
                    } else {
                        format!("{group} ({})", values.len())
                    })),
                );
                chevron(ui, header.right_center() - Vec2::new(20.0, 0.0), open);
                if open {
                    for (version, info) in values {
                        let (line, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 42.0),
                            egui::Sense::hover(),
                        );
                        // PageSelectRight's stack has 20/18 DIP left/right margins.
                        let row = Rect::from_min_max(
                            line.min + Vec2::new(20.0, 0.0),
                            line.max - Vec2::new(18.0, 0.0),
                        );
                        let response = ui.interact(
                            row,
                            ui.id().with(("instance-row", group, &version.id)),
                            egui::Sense::click(),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                version.error.is_none(),
                                &version.id,
                            )
                        });
                        response.context_menu(|ui| {
                            if ui
                                .button(if info.favorite {
                                    "取消收藏"
                                } else {
                                    "加入收藏夹"
                                })
                                .clicked()
                            {
                                favorite_toggle = Some(version.id.clone());
                                ui.close();
                            }
                            if ui.button("版本设置").clicked() {
                                tools = Some(version.id.clone());
                                ui.close();
                            }
                        });
                        if response.contains_pointer() {
                            ui.painter()
                                .rect_filled(row, 6, theme::palette(ui.ctx()).light);
                        }
                        // Clickable check column 2 + PNG margin (4,5,3,5) in logo column 38.
                        let icon = Rect::from_min_size(
                            row.min + Vec2::new(6.0, 5.0),
                            Vec2::new(31.0, 32.0),
                        );
                        if management::paint_custom_icon(ui, info.custom_icon.as_deref(), icon) {
                        } else if info.icon.starts_with("block-") {
                            ui.painter().image(
                                self.assets.icons[info.icon].id(),
                                icon,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        } else {
                            self.assets
                                .icon(ui, info.icon, icon, icon_tint(ui.ctx(), info.icon));
                        }
                        row_text(
                            ui,
                            row.min + Vec2::new(44.0, 5.0),
                            row.width() - 110.0,
                            &version.id,
                            14.0,
                            theme::palette(ui.ctx()).text,
                        );
                        row_text(
                            ui,
                            row.min + Vec2::new(44.0, 23.0),
                            row.width() - 110.0,
                            &info.description,
                            12.0,
                            if version.error.is_some() {
                                Color32::DARK_RED
                            } else {
                                MUTED
                            },
                        );
                        if response.contains_pointer() || response.has_focus() {
                            let settings = Rect::from_min_size(
                                row.right_top() + Vec2::new(-42.0, 7.0),
                                Vec2::splat(28.0),
                            );
                            let clicked = ui
                                .place(settings, egui::Button::new("").frame(false))
                                .on_hover_text("版本设置")
                                .clicked();
                            self.assets.icon(
                                ui,
                                "settings",
                                settings.shrink(6.0),
                                theme::palette(ui.ctx()).accent,
                            );
                            if clicked {
                                tools = Some(version.id.clone());
                            }
                        }
                        if response.clicked() && version.error.is_none() {
                            selected = Some(version.id.clone());
                        }
                        if let Some(error) = &version.error {
                            response.on_hover_text(error);
                        }
                    }
                    ui.add_space(18.0);
                }
            });
            ui.data_mut(|data| data.insert_temp(fold_id, open));
            ui.add_space(15.0);
        }
        if visible_count == 0 && show_hidden {
            titled_card(ui, "无隐藏版本", |ui| {
                ui.label("没有版本被隐藏，你可以在版本设置的版本分类选项中隐藏版本。");
                ui.label("再次按下 F11 即可退出隐藏版本查看模式。");
            });
        }
        if let Some(id) = favorite_toggle {
            match config::load_instance_settings(&self.settings.game_root, &id) {
                Ok(mut settings) => {
                    settings.favorite = !settings.favorite;
                    self.save_version_preferences(ui.ctx(), &id, &settings);
                }
                Err(error) => self.error = Some(format!("读取版本设置失败：{error:#}")),
            }
        }
        let open_tools = tools.is_some();
        if let Some(id) = tools.or(selected) {
            self.settings.selected_version = Some(id);
            self.persist();
            self.version_view = false;
            if open_tools {
                self.version_tools = true;
                self.tools_tab = 0;
            }
        }
    }

    pub(super) fn version_tools_page(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let Some(id) = self.settings.selected_version.clone() else {
            ui.label("请先选择游戏版本。");
            return;
        };
        if self.tools_tab == 2 {
            self.instance_setup_page(ui);
            return;
        }
        if self.tools_tab == 3 {
            self.pack_export_page(ui);
            return;
        }
        let instance = match self.instance_dir() {
            Ok(path) => path,
            Err(error) => {
                ui.colored_label(Color32::DARK_RED, format!("{error:#}"));
                return;
            }
        };
        let catalog = version_catalog(ui.ctx(), &self.settings.game_root, &self.versions);
        let info = catalog
            .iter()
            .find(|(version, _)| version.id == id)
            .map(|(_, info)| info);
        self.version_management_dialog(ui.ctx());
        if self.tools_tab == 1 {
            match info.and_then(|info| info.modable) {
                Some(true) => self.instance_mods(ui, &id, &instance),
                Some(false) => self.instance_mods_unavailable(ui),
                None => {
                    ui.label("正在读取版本信息…");
                }
            }
            return;
        }
        page_frame()
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (row, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 42.0),
                    egui::Sense::hover(),
                );
                let icon = info.as_ref().map_or("game", |info| info.icon);
                let icon_rect =
                    Rect::from_min_size(row.min + Vec2::new(7.0, 6.0), Vec2::splat(30.0));
                if !management::paint_custom_icon(
                    ui,
                    info.as_ref().and_then(|v| v.custom_icon.as_deref()),
                    icon_rect,
                ) {
                    self.assets
                        .icon(ui, icon, icon_rect, icon_tint(ui.ctx(), icon));
                }
                row_text(
                    ui,
                    row.min + Vec2::new(44.0, 4.0),
                    row.width() - 54.0,
                    &id,
                    13.0,
                    theme::palette(ui.ctx()).text,
                );
                row_text(
                    ui,
                    row.min + Vec2::new(44.0, 23.0),
                    row.width() - 54.0,
                    info.as_ref()
                        .map_or("版本信息不可用", |info| info.description.as_str()),
                    12.0,
                    MUTED,
                );
            });
        ui.add_space(15.0);
        let mut preferences = match config::load_instance_settings(&self.settings.game_root, &id) {
            Ok(settings) => settings,
            Err(error) => {
                ui.colored_label(Color32::DARK_RED, format!("版本设置无法读取：{error:#}"));
                return;
            }
        };
        let previous = preferences.clone();
        let description_key =
            ui.make_persistent_id(("version-description", &self.settings.game_root, &id));
        let mut description = ui.data_mut(|data| data.get_temp::<String>(description_key));
        let rename_key = ui.make_persistent_id(("version-rename", &self.settings.game_root, &id));
        let mut rename = ui.data_mut(|data| data.get_temp::<String>(rename_key));
        let mut rename_to = None;
        titled_card_with_margins(ui, "个性化", 40, 22, |ui| {
            for label in ["图标", "分类"] {
                if label == "分类" {
                    ui.add_space(9.0);
                }
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(42.0, 28.0), egui::Sense::hover());
                    ui_style::place_left(ui, rect, egui::Label::new(label));
                    if label == "图标" {
                        let current = PRESET_ICONS
                            .iter()
                            .find(|(_, key)| *key == preferences.display_icon)
                            .map_or("自动", |(label, _)| *label);
                        ui_style::PclComboBox::from_id_salt("version-display-icon")
                            .width(ui.available_width() - 8.0)
                            .selected_text(if preferences.custom_icon.is_some() {
                                "自定义图片"
                            } else {
                                current
                            })
                            .show_ui(ui, |ui| {
                                for (label, key) in PRESET_ICONS {
                                    if ui
                                        .selectable_value(
                                            &mut preferences.display_icon,
                                            (*key).into(),
                                            *label,
                                        )
                                        .clicked()
                                    {
                                        preferences.custom_icon = None;
                                    }
                                }
                                if ui.selectable_label(false, "选择图片…").clicked()
                                    && !self.jobs.conflicts_with(&self.settings.game_root)
                                {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .add_filter(
                                            "常用图片",
                                            &["png", "jpeg", "jpg", "gif", "webp"],
                                        )
                                        .pick_file()
                                    {
                                        match config::import_instance_icon(
                                            &self.settings.game_root,
                                            &id,
                                            &path,
                                        ) {
                                            Ok(name) => preferences.custom_icon = Some(name),
                                            Err(e) => {
                                                self.error = Some(format!("图标导入失败：{e:#}"))
                                            }
                                        }
                                    }
                                }
                            });
                    } else {
                        let mut category = if preferences.hidden {
                            "hidden".to_owned()
                        } else {
                            preferences.display_category.clone()
                        };
                        let current = CATEGORIES
                            .iter()
                            .find(|(_, key)| *key == category)
                            .map_or("自动", |(label, _)| *label);
                        ui_style::PclComboBox::from_id_salt("version-display-category")
                            .width(ui.available_width() - 8.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (label, key) in CATEGORIES {
                                    ui.selectable_value(&mut category, (*key).into(), *label);
                                }
                            });
                        preferences.hidden = category == "hidden";
                        if !preferences.hidden {
                            preferences.display_category = category;
                        }
                    }
                });
            }
            ui.add_space(15.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                if button(
                    ui,
                    "修改版本名",
                    140.0,
                    self.busy.is_none()
                        && self.game_pid.is_none()
                        && !self.jobs.conflicts_with(&self.settings.game_root),
                    false,
                )
                .clicked()
                {
                    rename = Some(id.clone());
                }
                if button(ui, "修改版本描述", 140.0, self.busy.is_none(), false).clicked() {
                    description = Some(preferences.description.clone());
                }
                if button(
                    ui,
                    if preferences.favorite {
                        "取消收藏"
                    } else {
                        "加入收藏夹"
                    },
                    140.0,
                    self.busy.is_none(),
                    false,
                )
                .clicked()
                {
                    preferences.favorite = !preferences.favorite;
                }
            });
        });
        if let Some(value) = rename.as_mut() {
            if let Some(action) = super::account_ui::account_input_modal(
                ui.ctx(),
                "version-rename-input",
                "修改版本名",
                "为该版本输入新的名称。其他版本若依赖它，需要先处理继承关系。",
                value,
                &["确定", "取消"],
            ) {
                if action == 0 && value.trim() != id {
                    rename_to = Some(value.trim().to_owned());
                }
                rename = None;
            }
        }
        ui.data_mut(|data| match rename {
            Some(value) => data.insert_temp(rename_key, value),
            None => {
                data.remove::<String>(rename_key);
            }
        });
        if let Some(value) = description.as_mut() {
            if let Some(action) = super::account_ui::account_input_modal(
                ui.ctx(),
                "version-description-input",
                "修改版本描述",
                "修改该版本在版本列表中显示的描述。留空可恢复自动描述。",
                value,
                &["确定", "取消"],
            ) {
                if action == 0 {
                    preferences.description = value.trim().to_owned();
                }
                description = None;
            }
        }
        ui.data_mut(|data| match description {
            Some(description) => data.insert_temp(description_key, description),
            None => {
                data.remove::<String>(description_key);
            }
        });
        if previous != preferences {
            self.save_version_preferences(ui.ctx(), &id, &preferences);
        }
        let mut folder = None;
        titled_card_with_margins(ui, "快捷方式", 43, 20, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                for (label, child) in [
                    ("版本文件夹", ""),
                    ("存档文件夹", "saves"),
                    ("Mod 文件夹", "mods"),
                    ("截图文件夹", "screenshots"),
                ] {
                    if button(ui, label, 140.0, true, false).clicked() {
                        folder = Some(child);
                    }
                }
            });
        });
        if let Some(child) = folder {
            self.open_instance_subfolder(&id, child);
        }
        let mut repair = false;
        let mut delete = false;
        titled_card_with_margins(ui, "高级管理", 42, 22, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                if button(ui, "导出启动脚本", 140.0, self.busy.is_none(), false).clicked() {
                    self.export_launch_script();
                }
                repair = button(
                    ui,
                    "补全文件",
                    140.0,
                    self.busy.is_none()
                        && self.game_pid.is_none()
                        && info.as_ref().is_some_and(|info| info.repair),
                    false,
                )
                .on_hover_text("补全缺失的游戏文件和加载器文件，保留版本配置。")
                .clicked();
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(140.0, 35.0), egui::Sense::hover());
                delete = ui_style::danger_button(
                    ui,
                    rect,
                    "删除版本",
                    self.busy.is_none()
                        && self.game_pid.is_none()
                        && !self.jobs.conflicts_with(&self.settings.game_root),
                )
                .on_hover_text("将该版本及其独立数据移入系统废纸篓或回收站。操作前会显示具体目录。")
                .clicked();
            });
        });
        if repair {
            self.install(id);
        } else if delete {
            if let Some((tx, cancel)) = self.start_job("正在检查版本文件") {
                let root = self.settings.game_root.clone();
                std::thread::spawn(move || {
                    let result =
                        pcl_core::deletion::preview_version_delete_with_cancel(&root, &id, &cancel)
                            .map_err(|error| format!("无法检查版本删除范围：{error:#}"));
                    let _ = tx.send(super::Event::DeletePreview(result));
                });
            }
        } else if let Some(new) = rename_to {
            if self.jobs.conflicts_with(&self.settings.game_root) {
                self.error = Some("当前游戏目录正在写入，不能重命名".into());
                return;
            }
            if let Some((tx, cancel)) = self.start_job("正在重命名版本") {
                let root = self.settings.game_root.clone();
                std::thread::spawn(move || {
                    let result =
                        pcl_core::instances::rename_version_with_cancel(&root, &id, &new, &cancel)
                            .map_err(|error| format!("版本重命名未完成：{error:#}"));
                    let _ = tx.send(super::Event::VersionRenamed {
                        root,
                        old: id,
                        new,
                        result,
                    });
                });
            }
        }
    }

    pub(super) fn version_delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(preview) = self.pending_version_delete.as_ref() else {
            return;
        };
        let root_matches = self
            .settings
            .game_root
            .canonicalize()
            .is_ok_and(|root| root == preview.game_root);
        if !root_matches
            || self.game_pid.is_some()
            || self.jobs.conflicts_with(&self.settings.game_root)
        {
            self.pending_version_delete = None;
            return;
        }
        if !preview.dependent_versions.is_empty() {
            let dependents = preview
                .dependent_versions
                .iter()
                .map(|entry| format!("{}（{}）", entry.version_id, entry.field))
                .collect::<Vec<_>>()
                .join("\n");
            let caption = format!(
                "以下版本依赖 {}，请先处理这些版本的继承关系：\n\n{dependents}",
                preview.version_id
            );
            if super::account_ui::account_modal(
                ctx,
                "version-delete-blocked",
                "无法删除版本",
                &caption,
                &["关闭"],
            )
            .is_some()
            {
                self.pending_version_delete = None;
            }
            return;
        }
        let mut caption = format!("确认删除版本 {}？\n\n以下目录及其中的存档、Mod、截图等文件会移入系统废纸篓或回收站：\n{}",preview.version_id,preview.version_directory.display());
        if let Some(path) = &preview.instance_directory {
            caption.push_str(&format!("\n{}", path.display()));
        }
        caption.push_str(&format!(
            "\n\n共 {} 个文件，{:.1} MiB。共享存档、支持库和资源缓存将保留。",
            preview.file_count,
            preview.total_bytes as f64 / 1_048_576.0
        ));
        if let Some(action) = super::account_ui::account_modal(
            ctx,
            "version-delete-confirm",
            "确认删除版本",
            &caption,
            &["删除", "取消"],
        ) {
            let preview = self.pending_version_delete.take().unwrap();
            if action == 0 {
                if let Some((tx, cancel)) = self.start_job("正在将版本移入废纸篓或回收站")
                {
                    std::thread::spawn(move || {
                        let root = preview.game_root.clone();
                        let id = preview.version_id.clone();
                        let result =
                            pcl_core::deletion::trash_version(&root, &id, &preview, &cancel)
                                .map_err(|error| error.to_string());
                        let _ = tx.send(super::Event::VersionDeleted { root, id, result });
                    });
                }
            }
        }
    }

    fn instance_mods_unavailable(&mut self, ui: &mut egui::Ui) {
        let width = (ui.available_width() - 30.0).max(300.0);
        let text = "你需要先安装 Forge、Fabric 等 Mod 加载器才能使用 Mod，请在下载页面安装这些版本。\n如果你已经安装过了 Mod 加载器，那么你很可能选择了错误的版本，请点击版本选择按钮切换版本。";
        let galley = ui.painter().layout(
            text.into(),
            egui::FontId::proportional(13.0),
            theme::palette(ui.ctx()).text,
            width - 60.0,
        );
        let height = 17.0 + 24.0 + 9.0 + 2.0 + 15.0 + galley.size().y + 5.0 + 10.0 + 35.0 + 17.0;
        let (area, _) = ui.allocate_exact_size(
            Vec2::new(
                ui.available_width(),
                (ui.ctx().content_rect().height() - 98.0).max(height + 30.0),
            ),
            egui::Sense::hover(),
        );
        let panel = Rect::from_center_size(area.center(), Vec2::new(width, height));
        ui.painter().add(page_frame().shadow.as_shape(panel, 5));
        ui.painter()
            .rect_filled(panel, 5, Color32::from_white_alpha(245));
        ui.painter().text(
            egui::pos2(panel.center().x, panel.top() + 17.0),
            egui::Align2::CENTER_TOP,
            "该版本不可使用 Mod",
            egui::FontId::proportional(19.0),
            theme::palette(ui.ctx()).accent,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(
                panel.min + Vec2::new(20.0, 50.0),
                Vec2::new(width - 40.0, 2.0),
            ),
            0,
            theme::palette(ui.ctx()).accent,
        );
        ui.put(
            Rect::from_min_size(panel.min + Vec2::new(30.0, 67.0), galley.size()),
            egui::Label::new(galley),
        );
        let first = Rect::from_min_size(
            egui::pos2(panel.center().x - 150.0, panel.bottom() - 52.0),
            Vec2::new(140.0, 35.0),
        );
        if ui_style::outline_button(ui, first, "转到下载页面", None, true, true).clicked() {
            self.page = Page::Download;
            self.download_tab = 0;
            self.version_tools = false;
        }
        if ui_style::outline_button(
            ui,
            first.translate(Vec2::new(160.0, 0.0)),
            "版本选择",
            None,
            false,
            true,
        )
        .clicked()
        {
            self.page = Page::Launch;
            self.version_tools = false;
            self.version_view = true;
        }
    }

    fn open_instance_subfolder(&mut self, id: &str, child: &str) {
        let result = (|| {
            metadata::validate_id(id)?;
            let instance = pcl_core::config::instance_game_dir(&self.settings.game_root, id)?;
            let folder = metadata::confined_path(&instance, Path::new(child))?;
            std::fs::create_dir_all(&folder)?;
            let folder = metadata::confined_path(&instance, Path::new(child))?;
            crate::process::open_folder(&folder)?;
            anyhow::Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(format!("打开目录失败：{error:#}"));
        }
    }

    fn save_version_preferences(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        settings: &config::InstanceSettings,
    ) {
        if self.jobs.conflicts_with(&self.settings.game_root) {
            self.error = Some("当前游戏目录正在写入，请完成后修改版本设置".into());
            return;
        }
        match config::save_instance_settings(&self.settings.game_root, id, settings) {
            Ok(()) => {
                ctx.data_mut(|data| {
                    data.remove::<CatalogCache>(egui::Id::new("instance-version-catalog"))
                });
                self.instance_setup = Default::default();
                self.status = "版本设置已保存".into();
            }
            Err(error) => self.error = Some(format!("保存版本设置失败：{error:#}")),
        }
    }

    fn instance_mods(&mut self, ui: &mut egui::Ui, id: &str, instance: &Path) {
        let selection_id = ui.make_persistent_id(("mod-selection", &self.settings.game_root, id));
        let mut selection = ui.data_mut(|data| {
            data.get_temp::<HashSet<String>>(selection_id)
                .unwrap_or_default()
        });
        selection.retain(|name| self.local_mods.iter().any(|value| &value.file_name == name));
        page_frame()
            .inner_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
                    ui.painter().circle_stroke(
                        r.center() - Vec2::splat(1.5),
                        4.5,
                        egui::Stroke::new(1.5_f32, theme::palette(ui.ctx()).text),
                    );
                    ui.painter().line_segment(
                        [r.center() + Vec2::splat(2.0), r.max],
                        egui::Stroke::new(1.5_f32, theme::palette(ui.ctx()).text),
                    );
                    ui.add(
                        ui_style::singleline(&mut self.mods_filter)
                            .hint_text("搜索 Mod 名称 / 文件名 / 标识")
                            .frame(false)
                            .desired_width(ui.available_width()),
                    );
                });
            });
        ui.add_space(15.0);
        let mutable = self.game_pid.is_none()
            && self.busy.is_none()
            && !self.jobs.conflicts_with(&self.settings.game_root);
        let filter = self.mods_filter.to_lowercase();
        let known = ui
            .ctx()
            .data(|data| data.get_temp::<ModDetailCache>(mod_details_key()))
            .filter(|cache| cache.instance == instance)
            .map(|cache| cache.entries)
            .unwrap_or_default();
        let matches = |value: &&mods::LocalMod| {
            value.name.to_lowercase().contains(&filter)
                || known
                    .iter()
                    .find(|entry| entry.file_name == value.file_name)
                    .is_some_and(|entry| {
                        entry.title.to_lowercase().contains(&filter)
                            || entry.description.to_lowercase().contains(&filter)
                    })
                || value.file_name.to_lowercase().contains(&filter)
                || value
                    .mod_ids
                    .iter()
                    .any(|id| id.to_lowercase().contains(&filter))
        };
        let mut choose_files = false;
        let mut check_updates = false;
        page_frame().inner_margin(15).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 15.0;
                if button(ui, "打开文件夹", 110.0, true, true).clicked() {
                    self.open_instance_subfolder(id, "mods");
                }
                choose_files = button(ui, "从文件安装", 110.0, mutable, false).clicked();
                let updates_allowed = config::load_instance_settings(&self.settings.game_root, id)
                    .is_ok_and(|s| !s.disable_mod_updates);
                if super::appearance_ui::feature_visible(ui.ctx(), &self.settings, "mod_update") {
                    check_updates =
                        button(ui, "检查更新", 110.0, mutable && updates_allowed, false)
                            .on_hover_text(if updates_allowed {
                                "检查兼容的 Mod 更新"
                            } else {
                                "该版本已禁用 Mod 更新，可在版本设置中恢复"
                            })
                            .clicked();
                }
                if button(ui, "恢复已移除", 110.0, mutable, false).clicked() {
                    self.restore_mod_removal(id, instance);
                }
                if button(ui, "下载新 Mod", 110.0, mutable, false).clicked() {
                    self.page = Page::Download;
                    self.version_tools = false;
                    self.download_tab = 1;
                    self.ensure_resource_target();
                }
                if button(
                    ui,
                    if selection.is_empty() {
                        "全选"
                    } else {
                        "取消选择"
                    },
                    110.0,
                    !self.local_mods.is_empty(),
                    false,
                )
                .clicked()
                {
                    if selection.is_empty() {
                        selection.extend(
                            self.local_mods
                                .iter()
                                .filter(matches)
                                .map(|value| value.file_name.clone()),
                        );
                    } else {
                        selection.clear();
                    }
                }
            });
        });
        ui.add_space(15.0);
        if check_updates {
            self.open_mod_updates(id, instance);
        }
        if choose_files {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter("Minecraft Mod", &["jar"])
                .pick_files()
            {
                let mut failures = Vec::new();
                let mut count = 0;
                for source in paths {
                    match mods::import_mod(instance, &source) {
                        Ok(_) => count += 1,
                        Err(error) => failures.push(format!("{}：{error:#}", source.display())),
                    }
                }
                self.status = format!("已添加 {count} 个 Mod");
                if !failures.is_empty() {
                    self.error = Some(failures.join("\n"));
                }
                self.refresh_mods();
            }
        }
        let mut toggles = Vec::new();
        page_frame()
            .inner_margin(egui::Margin {
                left: 15,
                right: 18,
                top: 13,
                bottom: 22,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let label = format!("全部 ({})", self.local_mods.len());
                    ui.add(
                        egui::Button::new(RichText::new(label).color(Color32::WHITE).size(12.0))
                            .fill(theme::palette(ui.ctx()).accent)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(14),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("刷新").clicked() {
                            clear_mod_details(ui.ctx());
                            self.refresh_mods();
                        }
                    });
                });
                // MyLocalModItem loads its project on first entry into the viewport.
                // Include two nearby rows, never start one request per off-screen file.
                let first_y = ui.cursor().top() + 7.0;
                let visible: Vec<_> = self
                    .local_mods
                    .iter()
                    .filter(matches)
                    .enumerate()
                    .filter(|(index, _)| {
                        let row = Rect::from_min_size(
                            egui::pos2(ui.cursor().left(), first_y + *index as f32 * 44.0),
                            Vec2::new(ui.available_width(), 44.0),
                        );
                        row.intersects(ui.clip_rect().expand(88.0))
                    })
                    .map(|(_, value)| value.clone())
                    .collect();
                let details = mod_details(ui.ctx(), instance, &self.local_mods, &visible);
                if details.refresh_local {
                    clear_mod_details(ui.ctx());
                    self.refresh_mods();
                }
                if details.loading {
                    super::loading_ui::inline(ui, "正在获取 Mod 信息");
                } else if let Some(error) = &details.error {
                    if ui
                        .add(
                            egui::Label::new(
                                RichText::new("Mod 信息获取失败，点击重试")
                                    .color(Color32::DARK_RED),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_text(error)
                        .clicked()
                    {
                        clear_mod_details(ui.ctx());
                    }
                }
                ui.add_space(7.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                for value in self.local_mods.iter().filter(matches) {
                    let remote = details
                        .entries
                        .iter()
                        .find(|d| d.file_name == value.file_name && !d.title.is_empty());
                    let title = remote.map(|d| d.title.as_str()).unwrap_or(&value.name);
                    let description = remote
                        .map(|d| d.description.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&value.metadata.description);
                    let (row, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 44.0),
                        egui::Sense::click(),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, mutable, title)
                    });
                    if response.contains_pointer() || selection.contains(&value.file_name) {
                        ui.painter()
                            .rect_filled(row, 4, theme::palette(ui.ctx()).light);
                    }
                    if response.clicked() && !selection.remove(&value.file_name) {
                        selection.insert(value.file_name.clone());
                    }
                    let icon = loader_icon(&value.loader);
                    let logo_rect =
                        Rect::from_min_size(row.min + Vec2::new(6.0, 5.0), Vec2::splat(34.0));
                    if let Some(texture) = details.icons.get(&value.file_name) {
                        ui.painter().image(
                            texture.id(),
                            logo_rect,
                            Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        self.assets
                            .icon(ui, icon, logo_rect, icon_tint(ui.ctx(), icon));
                    }
                    let has_problem =
                        value.error.is_some() || value.metadata.diagnostics.iter().any(|d| d.error);
                    if !value.enabled || has_problem || !value.metadata.diagnostics.is_empty() {
                        let center = logo_rect.right_bottom() - Vec2::splat(3.0);
                        ui.painter().circle_filled(
                            center,
                            7.0,
                            if has_problem {
                                Color32::DARK_RED
                            } else {
                                MUTED
                            },
                        );
                        ui.painter().text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            if !value.enabled { "−" } else { "!" },
                            egui::FontId::proportional(11.0),
                            Color32::WHITE,
                        );
                    }
                    let title_color = if value.enabled {
                        theme::palette(ui.ctx()).text
                    } else {
                        MUTED
                    };
                    let name_width = ui
                        .painter()
                        .layout_no_wrap(
                            title.to_owned(),
                            egui::FontId::proportional(14.0),
                            title_color,
                        )
                        .size()
                        .x
                        .min((row.width() - 220.0).max(60.0));
                    row_text(
                        ui,
                        row.min + Vec2::new(47.0, 5.0),
                        name_width,
                        title,
                        14.0,
                        title_color,
                    );
                    if !value.enabled {
                        ui.painter().line_segment(
                            [
                                row.min + Vec2::new(47.0, 13.0),
                                row.min + Vec2::new(47.0 + name_width, 13.0),
                            ],
                            egui::Stroke::new(1.0_f32, MUTED),
                        );
                    }
                    if let Some(version) = value
                        .version
                        .as_deref()
                        .or_else(|| remote.map(|d| d.version.as_str()))
                    {
                        row_text(
                            ui,
                            row.min + Vec2::new(57.0 + name_width, 6.0),
                            (row.width() - name_width - 145.0).max(0.0),
                            &format!("|  {version}"),
                            12.0,
                            MUTED,
                        );
                    }
                    let diagnostic = value
                        .metadata
                        .diagnostics
                        .first()
                        .map(|d| d.message.as_str());
                    let subtitle = if description.is_empty() {
                        value.file_name.clone()
                    } else {
                        format!("{}：{}", value.file_name, description.replace('\n', " "))
                    };
                    let tags = remote.map(|d| d.tags.as_slice()).unwrap_or(&[]);
                    // MyLocalModItem places tags before the description in its
                    // second row; keep the action area available on narrow pages.
                    let mut description_left = row.left() + 47.0;
                    let tag_limit = row.left() + row.width() * 0.45;
                    for tag in tags.iter().take(3) {
                        let galley = ui.painter().layout_no_wrap(
                            tag.clone(),
                            egui::FontId::proportional(11.0),
                            MUTED,
                        );
                        let width = galley.size().x + 6.0;
                        if description_left + width > tag_limit {
                            break;
                        }
                        let r = Rect::from_min_size(
                            egui::pos2(
                                description_left - 1.0,
                                row.bottom() - 5.5 - galley.size().y - 1.0,
                            ),
                            Vec2::new(width, galley.size().y + 2.0),
                        );
                        ui.painter()
                            .rect_filled(r, 3, Color32::from_black_alpha(10));
                        ui.painter()
                            .galley(r.min + Vec2::new(3.0, 1.0), galley, MUTED);
                        description_left = r.right() + 4.0;
                    }
                    // Both source elements are bottom aligned. LabInfo has a
                    // 1 DIP bottom margin, matching the tag's bottom padding.
                    let mut description_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(Rect::from_min_size(
                                egui::pos2(description_left, row.bottom() - 22.5),
                                Vec2::new((row.right() - 85.0 - description_left).max(0.0), 17.0),
                            ))
                            .layout(egui::Layout::left_to_right(egui::Align::BOTTOM)),
                    );
                    description_ui.style_mut().interaction.selectable_labels = false;
                    description_ui.add(
                        egui::Label::new(
                            RichText::new(
                                value.error.as_deref().or(diagnostic).unwrap_or(&subtitle),
                            )
                            .size(12.0)
                            .color(if has_problem {
                                Color32::DARK_RED
                            } else {
                                MUTED
                            }),
                        )
                        .truncate(),
                    );
                    let mut tooltip = format!("{title}\n{}\n{description}", value.file_name);
                    if let Some(remote) = remote {
                        if remote.original_title != title {
                            tooltip.push_str(&format!("\n{}", remote.original_title));
                        }
                        tooltip.push_str(&format!("\n标签：{}", remote.tags.join("、")));
                    }
                    if !value.metadata.authors.is_empty() {
                        tooltip.push_str(&format!("\n作者：{}", value.metadata.authors.join("、")));
                    }
                    for issue in &value.metadata.diagnostics {
                        tooltip.push_str(&format!("\n{}", issue.message));
                    }
                    for dependency in &value.metadata.dependencies {
                        tooltip.push_str(&format!(
                            "\n{:?}：{} {}",
                            dependency.kind, dependency.id, dependency.requirement
                        ));
                    }
                    if let Some(error) = details
                        .entries
                        .iter()
                        .find(|d| d.file_name == value.file_name)
                        .and_then(|d| d.error.as_ref())
                    {
                        tooltip.push_str(&format!("\n{error}"));
                    }
                    response.clone().on_hover_text(tooltip);
                    response.context_menu(|ui| {
                        if let Some(remote) = remote {
                            if ui.button("打开项目页面").clicked() {
                                ui.ctx()
                                    .open_url(egui::OpenUrl::new_tab(&remote.project_url));
                                ui.close();
                            }
                            if let Some(url) = &remote.wiki_url {
                                if ui.button("在 MC 百科中查看").clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                    ui.close();
                                }
                            }
                        }
                        if ui.button("复制文件名").clicked() {
                            ui.ctx().copy_text(value.file_name.clone());
                            ui.close();
                        }
                    });
                    let toggle = Rect::from_min_size(
                        row.right_top() + Vec2::new(-78.0, 10.0),
                        Vec2::new(72.0, 24.0),
                    );
                    let mut enabled = value.enabled;
                    if response.contains_pointer()
                        || !value.enabled
                        || selection.contains(&value.file_name)
                    {
                        let builder = egui::UiBuilder::new().max_rect(toggle);
                        let mut toggle_ui =
                            ui.new_child(if mutable { builder } else { builder.disabled() });
                        if toggle_ui
                            .place(
                                toggle,
                                egui::Checkbox::new(
                                    &mut enabled,
                                    if value.enabled { "启用" } else { "禁用" },
                                ),
                            )
                            .changed()
                        {
                            toggles.push((value.file_name.clone(), enabled));
                        }
                    }
                }
                if self.local_mods.is_empty() {
                    ui.label(RichText::new("尚未安装 Mod").color(MUTED));
                } else if !self.local_mods.iter().any(|value| matches(&value)) {
                    ui.label(RichText::new("没有符合搜索条件的 Mod").color(MUTED));
                }
            });
        if !selection.is_empty() {
            ui.add_space(15.0);
            page_frame().inner_margin(15).show(ui, |ui| {
                ui.label(format!("已选择 {} 个文件", selection.len()));
                ui.horizontal(|ui| {
                    for (label, enabled) in [("启用", true), ("禁用", false)] {
                        if button(ui, label, 110.0, mutable, false).clicked() {
                            toggles.extend(selection.iter().map(|name| (name.clone(), enabled)));
                        }
                    }
                    if button(ui, "移除所选", 110.0, mutable, false).clicked() {
                        self.confirm_mod_removal(
                            ui.ctx(),
                            id,
                            instance,
                            selection.iter().cloned().collect(),
                        );
                    }
                    if button(ui, "取消选择", 110.0, true, false).clicked() {
                        selection.clear();
                    }
                });
            });
        }
        if !toggles.is_empty() && !self.jobs.conflicts_with(&self.settings.game_root) {
            let mut failures = Vec::new();
            let mut count = 0;
            for (file, enabled) in toggles {
                match mods::set_mod_enabled(instance, &file, enabled) {
                    Ok(_) => count += 1,
                    Err(error) => failures.push(format!("{file}：{error:#}")),
                }
            }
            self.status = format!("已修改 {count} 个 Mod 的启用状态");
            if !failures.is_empty() {
                self.error = Some(format!("部分 Mod 状态修改失败：\n{}", failures.join("\n")));
            }
            selection.clear();
            self.refresh_mods();
        }
        ui.data_mut(|data| data.insert_temp(selection_id, selection));
    }
}

// Metadata is resolved off the UI thread. A folder/list change invalidates the cache;
// a periodic background refresh also catches external edits without per-frame file I/O.
fn version_catalog(
    ctx: &egui::Context,
    root: &Path,
    versions: &[InstalledVersion],
) -> Arc<Catalog> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    for version in versions {
        (
            &version.id,
            &version.kind,
            version.required_java,
            &version.error,
        )
            .hash(&mut hasher);
    }
    let fingerprint = hasher.finish();
    let cache_id = egui::Id::new("instance-version-catalog");
    let previous = ctx.data_mut(|data| data.get_temp::<CatalogCache>(cache_id));
    let mut entries = None;
    if let Some(mut cache) = previous.filter(|cache| cache.fingerprint == fingerprint) {
        if let Ok(mut pending) = cache.pending.lock() {
            if let Some(ready) = pending.take() {
                cache.entries = ready;
                cache.loading = false;
                cache.refreshed = Instant::now();
            }
        }
        if cache.loading || cache.refreshed.elapsed() < Duration::from_secs(15) {
            let entries = cache.entries.clone();
            ctx.data_mut(|data| data.insert_temp(cache_id, cache));
            return entries;
        }
        entries = Some(cache.entries);
    }
    let entries = entries.unwrap_or_else(|| {
        Arc::new(
            versions
                .iter()
                .map(|version| {
                    (
                        version.clone(),
                        VersionPresentation {
                            custom_icon: None,
                            group: "其他版本",
                            icon: "game",
                            description: "正在读取版本信息…".into(),
                            repair: false,
                            favorite: false,
                            hidden: false,
                            modable: None,
                        },
                    )
                })
                .collect(),
        )
    });
    let pending = Arc::new(Mutex::new(None));
    let result = pending.clone();
    let root = root.to_path_buf();
    let versions = versions.to_vec();
    let context = ctx.clone();
    std::thread::spawn(move || {
        let entries = versions
            .into_iter()
            .map(|version| {
                let info = version_presentation(&root, &version);
                (version, info)
            })
            .collect();
        if let Ok(mut pending) = result.lock() {
            *pending = Some(Arc::new(entries));
        }
        context.request_repaint();
    });
    ctx.data_mut(|data| {
        data.insert_temp(
            cache_id,
            CatalogCache {
                fingerprint,
                refreshed: Instant::now(),
                loading: true,
                entries: entries.clone(),
                pending,
            },
        )
    });
    entries
}

fn version_presentation(root: &Path, version: &InstalledVersion) -> VersionPresentation {
    let mut presentation = infer_version_presentation(root, version);
    match config::load_instance_settings(root, &version.id) {
        Ok(settings) => {
            presentation.custom_icon = settings
                .custom_icon
                .as_deref()
                .and_then(|name| config::instance_icon_path(root, &version.id, name).ok());
            presentation.favorite = settings.favorite;
            presentation.hidden = settings.hidden;
            if settings.display_category == "mod" {
                presentation.modable = Some(true);
            }
            if !settings.description.is_empty() {
                presentation.description = settings.description;
            }
            if !settings.display_icon.is_empty() {
                if let Some((_, icon)) = PRESET_ICONS
                    .iter()
                    .find(|(_, key)| *key == settings.display_icon)
                {
                    presentation.icon = icon;
                }
            }
            if let Some((group, _)) = CATEGORIES
                .iter()
                .find(|(_, key)| *key == settings.display_category && !key.is_empty())
            {
                presentation.group = group;
            }
        }
        Err(error) => {
            presentation.description = format!("版本设置无法读取：{error:#}");
            presentation.group = "错误版本";
            presentation.repair = false;
            presentation.modable = Some(false);
        }
    }
    presentation
}

pub(super) fn export_version_description(root: &Path, version: &InstalledVersion) -> String {
    infer_version_presentation(root, version).description
}

fn infer_version_presentation(root: &Path, version: &InstalledVersion) -> VersionPresentation {
    if let Some(error) = &version.error {
        return VersionPresentation {
            custom_icon: None,
            group: "错误版本",
            icon: "game",
            description: error.clone(),
            repair: false,
            favorite: false,
            hidden: false,
            modable: Some(false),
        };
    }
    let resolved = metadata::resolve_version(root, &version.id).ok();
    let base = resolved
        .as_ref()
        .and_then(|value| value["_pcl_jar_id"].as_str())
        .unwrap_or(&version.id);
    let kind = match version.kind.as_str() {
        "release" => "正式版",
        "snapshot" => "快照",
        _ => "历史版本",
    };
    if let Some(libraries) = resolved
        .as_ref()
        .and_then(|value| value["libraries"].as_array())
    {
        for (prefix, label, group, icon) in [
            (
                "net.neoforged:neoforge:",
                "NeoForge",
                "NeoForge 版本",
                "block-neoforge",
            ),
            (
                "net.minecraftforge:forge:",
                "Forge",
                "Forge 版本",
                "block-forge",
            ),
            ("org.quiltmc:quilt-loader:", "Quilt", "Quilt 版本", "mod"),
            (
                "com.mumfrey:liteloader:",
                "LiteLoader",
                "可安装 Mod 的版本",
                "block-egg",
            ),
            (
                "net.fabricmc:fabric-loader:",
                "Fabric",
                "Fabric 版本",
                "block-fabric",
            ),
        ] {
            if let Some(loader) = libraries
                .iter()
                .filter_map(|value| value["name"].as_str())
                .find_map(|name| name.strip_prefix(prefix))
            {
                let loader = loader.strip_prefix(&format!("{base}-")).unwrap_or(loader);
                return VersionPresentation {
                    custom_icon: None,
                    group,
                    icon,
                    description: format!("{kind} {base}, {label} {loader}"),
                    repair: resolved
                        .as_ref()
                        .is_some_and(|value| value.pointer("/downloads/client/url").is_some()),
                    favorite: false,
                    hidden: false,
                    modable: Some(true),
                };
            }
        }
    }
    VersionPresentation {
        custom_icon: None,
        favorite: false,
        hidden: false,
        modable: Some(false),
        group: match version.kind.as_str() {
            "release" => "正式版",
            "snapshot" => "快照版本",
            _ => "其他版本",
        },
        icon: "block-grass",
        description: format!("{kind} {base}"),
        repair: resolved
            .as_ref()
            .is_some_and(|value| value.pointer("/downloads/client/url").is_some()),
    }
}

const PRESET_ICONS: &[(&str, &str)] = &[
    ("自动", ""),
    ("圆石", "block-cobblestone"),
    ("命令方块", "block-command"),
    ("金块", "block-gold"),
    ("草方块", "block-grass"),
    ("土径", "block-path"),
    ("铁砧", "block-forge"),
    ("红石块", "block-redstone"),
    ("红石灯（开）", "block-lamp-on"),
    ("红石灯（关）", "block-lamp-off"),
    ("鸡蛋", "block-egg"),
    ("布料（Fabric）", "block-fabric"),
    ("狐狸（NeoForge）", "block-neoforge"),
];
const CATEGORIES: &[(&str, &str)] = &[
    ("自动", ""),
    ("从版本列表中隐藏", "hidden"),
    ("可安装 Mod 的版本", "mod"),
    ("常规版本", "normal"),
    ("不常用版本", "old"),
    ("愚人节版本", "april"),
];

fn loader_icon(loader: &str) -> &'static str {
    match loader {
        "fabric" => "block-fabric",
        "forge" => "block-forge",
        "neoforge" => "block-neoforge",
        "quilt" => "mod",
        _ => "game",
    }
}
fn icon_tint(ctx: &egui::Context, icon: &str) -> Color32 {
    if icon.starts_with("block-") {
        Color32::WHITE
    } else {
        theme::palette(ctx).text
    }
}
fn row_text(ui: &mut egui::Ui, pos: Pos2, width: f32, text: &str, size: f32, color: Color32) {
    ui_style::place_left(
        ui,
        Rect::from_min_size(pos, Vec2::new(width.max(0.0), 17.0)),
        egui::Label::new(RichText::new(text).size(size).color(color)).truncate(),
    );
}
fn chevron(ui: &egui::Ui, center: Pos2, open: bool) {
    let dy = if open { -2.0 } else { 2.0 };
    ui.painter().add(egui::Shape::line(
        vec![
            center + Vec2::new(-4.0, -dy),
            center + Vec2::new(0.0, dy),
            center + Vec2::new(4.0, -dy),
        ],
        egui::Stroke::new(1.3_f32, theme::palette(ui.ctx()).text),
    ));
}
pub(super) fn page_frame() -> egui::Frame {
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
pub(super) fn titled_card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    titled_card_with_margins(ui, title, 40, 20, body);
}
pub(super) fn titled_card_with_margins(
    ui: &mut egui::Ui,
    title: &str,
    top: i8,
    bottom: i8,
    body: impl FnOnce(&mut egui::Ui),
) {
    let result = page_frame()
        .inner_margin(egui::Margin {
            left: 25,
            right: 25,
            top,
            bottom,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    ui_style::place_left(
        ui,
        Rect::from_min_size(
            result.response.rect.min + Vec2::new(15.0, 10.0),
            Vec2::new(result.response.rect.width() - 30.0, 20.0),
        ),
        egui::Label::new(ui_style::card_title(title)),
    );
    ui.add_space(15.0);
}
fn button(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    enabled: bool,
    highlight: bool,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 35.0), egui::Sense::hover());
    ui_style::outline_button(ui, rect, label, None, highlight, enabled)
}

#[cfg(test)]
mod management_tests {
    use super::*;
    #[test]
    fn navigating_away_cancels_metadata_and_late_reply_cannot_update_next_instance() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(directory.path());
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let ctx = ui_context();
        let mut old = None;
        let _ = ctx.run(Default::default(), |ctx| {
            old = Some(mod_details(ctx, &first, &[], &[]));
            app.mod_details_finish_frame(ctx);
        });
        let old = old.unwrap();
        assert!(!old.cancel.load(Ordering::Relaxed));
        let _ = ctx.run(Default::default(), |ctx| app.mod_details_finish_frame(ctx));
        assert!(old.cancel.load(Ordering::Relaxed));
        *old.pending.lock().unwrap() = Some(Ok(vec![mods::RemoteModDetails {
            file_name: "stale.jar".into(),
            title: "Stale project".into(),
            ..Default::default()
        }]));
        let _ = ctx.run(Default::default(), |ctx| {
            let next = mod_details(ctx, &second, &[], &[]);
            assert!(next.entries.is_empty());
            assert!(next.error.is_none());
            assert!(!Arc::ptr_eq(&old.pending, &next.pending));
        });
    }

    #[test]
    fn mod_row_uses_remote_title_tags_description_and_preserves_controls_in_narrow_page() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(directory.path());
        let instance = app.settings.game_root.clone();
        std::fs::create_dir_all(instance.join("mods")).unwrap();
        app.local_mods = vec![mods::LocalMod {
            file_name: "fixture.jar.disabled".into(),
            path: instance.join("mods/fixture.jar.disabled"),
            enabled: false,
            name: "Local title".into(),
            version: Some("1.0".into()),
            mod_ids: vec![],
            loader: "fabric".into(),
            error: None,
            metadata: mods::ModMetadata {
                description: "Local fallback".into(),
                ..Default::default()
            },
        }];
        let ctx = ui_context();
        let _ = ctx.run(Default::default(), |ctx| {
            let mut cache = mod_details(ctx, &instance, &app.local_mods, &[]);
            cache.entries = Arc::new(vec![mods::RemoteModDetails {
                file_name: "fixture.jar.disabled".into(),
                title: "Verified project title".into(),
                description: "Remote description is visible and never replaces the file".into(),
                tags: vec!["optimization".into()],
                ..Default::default()
            }]);
            ctx.data_mut(|d| d.insert_temp(mod_details_key(), cache));
        });
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(570.0, 500.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.instance_mods(ui, "fixture", &instance));
            },
        );
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some((text, shape.clip_rect))
                } else {
                    None
                }
            })
            .collect();
        assert!(texts
            .iter()
            .any(|(t, _)| t.galley.text().contains("Verified project")));
        assert!(texts.iter().any(|(t, _)| t.galley.text() == "optimization"));
        let tag = texts
            .iter()
            .find(|(t, _)| t.galley.text() == "optimization")
            .unwrap()
            .0;
        let description = texts
            .iter()
            .find(|(t, _)| t.galley.text().starts_with("fixture.jar"))
            .unwrap()
            .0;
        assert!(tag.pos.x + tag.galley.size().x < description.pos.x);
        println!(
            "tag pos={:?} size={:?}; description pos={:?} size={:?}",
            tag.pos,
            tag.galley.size(),
            description.pos,
            description.galley.size()
        );
        assert!(
            (tag.pos.y + tag.galley.size().y - description.pos.y - description.galley.size().y)
                .abs()
                <= 0.1,
            "the source tag padding and description margin share the same text bottom"
        );
        for (text, clip) in texts.iter().filter(|(text, _)| {
            text.galley.text().contains("Verified") || text.galley.text() == "optimization"
        }) {
            assert!(clip.contains_rect(Rect::from_min_size(text.pos, text.galley.size())));
        }
        assert!(
            !instance.join("mods/fixture.jar.disabled").exists(),
            "rendering cannot create/rename a mod"
        );
    }
    fn ui_context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        fonts.families.insert(
            egui::FontFamily::Name("PCL Bold".into()),
            fonts.families[&egui::FontFamily::Proportional].clone(),
        );
        ctx.set_fonts(fonts);
        ctx
    }
    #[test]
    fn installed_last_row_and_its_hover_settings_button_keep_the_correct_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        for i in 0..40 {
            let id = format!("instance-{i:03}");
            let folder = app.settings.game_root.join("versions").join(&id);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join(format!("{id}.json")),
                serde_json::to_vec(&serde_json::json!({"id":id,"type":"release","libraries":[]}))
                    .unwrap(),
            )
            .unwrap();
            app.versions.push(InstalledVersion {
                id,
                kind: "release".into(),
                required_java: 21,
                error: None,
            });
        }
        let ctx = ui_context();
        // Let the real async catalog resolve fixture metadata, not live game files.
        for _ in 0..100 {
            version_catalog(&ctx, &app.settings.game_root, &app.versions);
            if ctx.data(|data| {
                data.get_temp::<CatalogCache>(egui::Id::new("instance-version-catalog"))
                    .is_some_and(|cache| !cache.loading)
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let draw = |app: &mut Launcher, scroll, events| {
            let mut size = Vec2::ZERO;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 360.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    theme::apply(ctx, &app.settings);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        size = egui::ScrollArea::vertical()
                            .id_salt("version-audit-page")
                            .max_height(260.0)
                            .vertical_scroll_offset(scroll)
                            .show(ui, |ui| app.versions_page(ui))
                            .content_size;
                    });
                },
            );
            (output, size)
        };
        let (_, size) = draw(&mut app, 0.0, vec![]);
        assert!(size.y > 1600.0);
        let offset = size.y - 260.0;
        let (bottom, _) = draw(&mut app, offset, vec![]);
        let last = bottom
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "instance-039" => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .unwrap();
        let click = |pos, pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        for pressed in [true, false] {
            draw(&mut app, offset, click(last.center(), pressed));
        }
        assert_eq!(
            app.settings.selected_version.as_deref(),
            Some("instance-039")
        );
        assert!(!app.version_tools);
        // Keep the mouse over the settings column across several frames before clicking.
        let pos = Pos2::new(586.0, last.center().y + 7.0);
        for _ in 0..3 {
            let (output, _) = draw(&mut app, offset, vec![egui::Event::PointerMoved(pos)]);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.rect.contains(pos) && rect.rect.width() > 300.0
                    && (rect.rect.height()-42.0).abs()<0.1 && rect.fill==theme::palette(&ctx).light
            )), "row highlight must remain under the settings button");
        }
        for pressed in [true, false] {
            draw(&mut app, offset, click(pos, pressed));
        }
        assert!(
            app.version_tools,
            "hover button must not disappear when it becomes the topmost widget"
        );
        assert_eq!(
            app.settings.selected_version.as_deref(),
            Some("instance-039")
        );
    }

    #[test]
    fn hovering_over_mod_checkbox_keeps_it_visible_and_changes_only_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        let instance = app.settings.game_root.clone();
        std::fs::create_dir_all(instance.join("mods")).unwrap();
        let path = instance.join("mods/one.jar");
        std::fs::write(&path, b"fixture").unwrap();
        let other = instance.join("mods/other.jar");
        std::fs::write(&other, b"other").unwrap();
        app.local_mods = vec![mods::LocalMod {
            file_name: "one.jar".into(),
            path: path.clone(),
            enabled: true,
            name: "Fixture Mod".into(),
            version: None,
            mod_ids: vec![],
            loader: "fabric".into(),
            error: None,
            metadata: Default::default(),
        }];
        let ctx = ui_context();
        let draw = |app: &mut Launcher, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 650.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    theme::apply(ctx, &app.settings);
                    egui::CentralPanel::default()
                        .show(ctx, |ui| app.instance_mods(ui, "fixture", &instance));
                },
            )
        };
        let text = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
        };
        let output = draw(&mut app, vec![]);
        let row = text(&output, "Fixture Mod").unwrap();
        let pos = Pos2::new(580.0, row.center().y + 7.0);
        for _ in 0..4 {
            let output = draw(&mut app, vec![egui::Event::PointerMoved(pos)]);
            assert!(
                text(&output, "启用").is_some(),
                "checkbox must remain present under its own pointer"
            );
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.rect.contains(pos) && rect.rect.width() > 300.0
                    && (rect.rect.height()-44.0).abs()<0.1 && rect.fill==theme::palette(&ctx).light
            )), "row highlight must remain under the enable checkbox");
        }
        for pressed in [true, false] {
            draw(
                &mut app,
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
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(instance.join("mods/one.jar.disabled")).unwrap(),
            b"fixture"
        );
        assert_eq!(std::fs::read(other).unwrap(), b"other");
    }

    #[test]
    fn version_preferences_cannot_write_during_root_job() {
        let d = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(d.path());
        let root = app.settings.game_root.clone();
        std::fs::create_dir_all(root.join("versions/v")).unwrap();
        std::fs::write(root.join("versions/v/v.json"), b"{}").unwrap();
        let old = config::InstanceSettings::default();
        config::save_instance_settings(&root, "v", &old).unwrap();
        let path = config::instance_settings_path(&root, "v").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        app.start_download_job("fixture writer", Some("v".into()))
            .unwrap();
        app.save_version_preferences(
            &egui::Context::default(),
            "v",
            &config::InstanceSettings {
                description: "change".into(),
                ..old
            },
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(app.error.is_some());
    }
}
