//! Instance export page, based on PageInstanceExport.xaml.
use super::{version_ui::page_frame, Event, Launcher};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, Rect, Vec2};
use pcl_core::pack_export::{self, ExportSelection, PackExportOptions, PackFormat, ResourceMode};
use std::{io::Write, path::PathBuf};

pub(super) struct PackExportState {
    target: Option<(PathBuf, String)>,
    options: PackExportOptions,
    advanced: bool,
    available: Option<ExportSelection>,
    error: Option<String>,
    base_description: String,
    java_available: bool,
    launcher_error: Option<String>,
}
impl Default for PackExportState {
    fn default() -> Self {
        Self {
            target: None,
            options: PackExportOptions {
                name: String::new(),
                version: String::new(),
                summary: String::new(),
                selection: ExportSelection::default(),
                resource_mode: ResourceMode::PreferHosted,
                ..Default::default()
            },
            advanced: false,
            available: None,
            error: None,
            base_description: String::new(),
            java_available: false,
            launcher_error: None,
        }
    }
}
impl Launcher {
    pub(super) fn pack_export_page(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.settings.selected_version.clone() else {
            return;
        };
        let root = self.settings.game_root.clone();
        if self.pack_export.target.as_ref() != Some(&(root.clone(), id.clone())) {
            self.pack_export = PackExportState {
                target: Some((root.clone(), id.clone())),
                launcher_error: current_launcher()
                    .and_then(|path| pack_export::validate_launcher_export(&path))
                    .err()
                    .map(|error| format!("{error:#}")),
                java_available: pack_export::available_java_roots(&root, &id)
                    .is_ok_and(|paths| !paths.is_empty()),
                base_description: self
                    .versions
                    .iter()
                    .find(|version| version.id == id)
                    .map(|version| super::version_ui::export_version_description(&root, version))
                    .unwrap_or_else(|| id.clone()),
                ..Default::default()
            };
            match pack_export::available_selection(&root, &id) {
                Ok(available) => {
                    apply_visibility(&mut self.pack_export.options.selection, &available);
                    self.pack_export.available = Some(available);
                }
                Err(error) => {
                    self.pack_export.error = Some(format!("读取可导出内容失败：{error:#}"))
                }
            }
        }

        let mutable = self.busy.is_none() && self.game_pid.is_none();
        let mut help = false;
        let state = &mut self.pack_export;
        let Some(available) = state.available.as_ref() else {
            ui.colored_label(
                Color32::DARK_RED,
                state.error.as_deref().unwrap_or("无法读取可导出内容"),
            );
            return;
        };
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_enabled_ui(mutable, |ui| {
            export_card(
                ui,
                "",
                None,
                egui::Margin {
                    left: 22,
                    right: 25,
                    top: 15,
                    bottom: 15,
                },
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let remaining = (ui.available_width() - 200.0).max(100.0);
                        let name_width = remaining / 1.3;
                        ui.add_sized(Vec2::new(65.0, 28.0), egui::Label::new("整合包名称"));
                        ui.add_space(20.0);
                        let name = ui.add_sized(
                            Vec2::new(name_width, 28.0),
                            egui::TextEdit::singleline(&mut state.options.name)
                                .hint_text(&id)
                                .char_limit(100)
                                .margin(Vec2::new(6.0, 5.0)),
                        );
                        if name.gained_focus() && state.options.name.is_empty() {
                            state.options.name = id.clone();
                        }
                        ui.add_space(30.0);
                        ui.add_sized(Vec2::new(65.0, 28.0), egui::Label::new("整合包版本"));
                        ui.add_space(20.0);
                        ui.add_sized(
                            Vec2::new((remaining - name_width).max(50.0), 28.0),
                            egui::TextEdit::singleline(&mut state.options.version)
                                .hint_text("1.0.0")
                                .char_limit(100)
                                .margin(Vec2::new(6.0, 5.0)),
                        );
                    });
                },
            );
            export_card(
                ui,
                "导出内容列表",
                None,
                egui::Margin {
                    left: 25,
                    right: 15,
                    top: 38,
                    bottom: 15,
                },
                |ui| {
                    ui.add_enabled_ui(false, |ui| {
                        checkbox(ui, &mut true, "游戏本体", &state.base_description, false);
                    });
                    let selection = &mut state.options.selection;
                    if available.game_settings {
                        checkbox(
                            ui,
                            &mut selection.game_settings,
                            "游戏本体设置",
                            "键位、音量、视频设置等",
                            true,
                        );
                    }
                    if available.game_personal {
                        checkbox(
                            ui,
                            &mut selection.game_personal,
                            "游戏本体个人信息",
                            "命令历史、已保存的快捷栏",
                            true,
                        );
                    }
                    if available.optifine_settings {
                        checkbox(
                            ui,
                            &mut selection.optifine_settings,
                            "OptiFine 设置",
                            "",
                            true,
                        );
                    }
                    if available.mods {
                        checkbox(ui, &mut selection.mods, "Mod", "模组", false);
                    }
                    if selection.mods {
                        if available.disabled_mods {
                            checkbox(ui, &mut selection.disabled_mods, "已禁用的 Mod", "", true);
                        }
                        if available.pack_data {
                            checkbox(
                                ui,
                                &mut selection.pack_data,
                                "整合包重要数据",
                                "脚本文件、内置资源包、数据包等",
                                true,
                            );
                        }
                        if available.mod_configs {
                            checkbox(ui, &mut selection.mod_configs, "Mod 设置", "", true);
                        }
                        if available.tacz {
                            checkbox(ui, &mut selection.tacz, "TaCZ 枪包", "", true);
                        }
                        if available.paintings {
                            checkbox(ui, &mut selection.paintings, "已上传的沉浸画", "", true);
                        }
                        if available.maps {
                            checkbox(
                                ui,
                                &mut selection.maps,
                                "已绘制的地图",
                                "现有存档、服务器的地图和路标点等",
                                true,
                            );
                        }
                        if available.jei_personal {
                            checkbox(
                                ui,
                                &mut selection.jei_personal,
                                "JEI 个人信息",
                                "物品收藏夹等",
                                true,
                            );
                        }
                        if available.emi_personal {
                            checkbox(
                                ui,
                                &mut selection.emi_personal,
                                "EMI 个人信息",
                                "物品收藏夹、默认配方、合成历史记录等",
                                true,
                            );
                        }
                        if available.patchouli_personal {
                            checkbox(
                                ui,
                                &mut selection.patchouli_personal,
                                "帕秋莉手册个人信息",
                                "教程书的已读记录、书签等",
                                true,
                            );
                        }
                    }
                    if available.resource_packs {
                        checkbox(
                            ui,
                            &mut selection.resource_packs,
                            "资源包",
                            "纹理包/材质包",
                            false,
                        );
                        if selection.resource_packs {
                            pack_item_rows(
                                ui,
                                &available.resource_pack_items,
                                &mut selection.resource_pack_items,
                            );
                        }
                    }
                    if available.shader_packs {
                        checkbox(ui, &mut selection.shader_packs, "光影包", "", false);
                    }
                    if selection.shader_packs && available.shader_settings {
                        checkbox(
                            ui,
                            &mut selection.shader_settings,
                            "光影包设置",
                            "已选光影包的自定义设置",
                            true,
                        );
                    }
                    if selection.shader_packs {
                        pack_item_rows(
                            ui,
                            &available.shader_pack_items,
                            &mut selection.shader_pack_items,
                        );
                    }
                    if available.screenshots {
                        checkbox(ui, &mut selection.screenshots, "截图", "", false);
                    }
                    if available.schematics {
                        checkbox(
                            ui,
                            &mut selection.schematics,
                            "导出的结构",
                            "schematics 文件夹",
                            false,
                        );
                    }
                    if available.replays {
                        checkbox(
                            ui,
                            &mut selection.replays,
                            "录像回放",
                            "Replay Mod 的录像文件",
                            false,
                        );
                    }
                    let mut saves = !selection.worlds.is_empty();
                    if !available.worlds.is_empty()
                        && checkbox(ui, &mut saves, "单人游戏存档", "世界 / 地图", false).changed()
                    {
                        selection.worlds = if saves {
                            available.worlds.clone()
                        } else {
                            Vec::new()
                        };
                    }
                    if saves {
                        for world in &available.worlds {
                            let mut checked = selection.worlds.contains(world);
                            if checkbox(ui, &mut checked, world, "", true).changed() {
                                if checked {
                                    selection.worlds.push(world.clone());
                                } else {
                                    selection.worlds.retain(|value| value != world);
                                }
                            }
                        }
                    }
                    if available.licenses {
                        checkbox(ui, &mut selection.licenses, "协议", "Licence 文件", false);
                    }
                    if available.servers {
                        checkbox(ui, &mut selection.servers, "多人游戏服务器列表", "", false);
                    }
                    if state.java_available {
                        checkbox(
                            ui,
                            &mut state.options.include_java,
                            "版本文件夹中的 Java",
                            "仅复制此版本的运行时；导入后手动选择 Java，不会执行随包程序",
                            false,
                        );
                    }
                    if let Some(error) = &state.launcher_error {
                        ui.add_enabled_ui(false, |ui| {checkbox(ui, &mut false, "PCL-Rust 启动器程序", error, false);});
                    } else {
                    if checkbox(
                        ui,
                        &mut state.options.include_launcher,
                        "PCL-Rust 启动器程序",
                        "当前平台第三方 Rust 版；外层 ZIP 不包含账户/全局设置，公开再分发前请核对程序与字体许可",
                        false,
                    )
                    .changed()
                        && state.options.include_launcher
                    {
                        state.options.format = PackFormat::Mrpack;
                    }
                    }
                },
            );
            export_card(
                ui,
                "高级选项",
                Some(&mut state.advanced),
                egui::Margin {
                    left: 25,
                    right: 23,
                    top: 37,
                    bottom: 20,
                },
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label("导出格式");
                        ui.add_enabled_ui(!state.options.include_launcher, |ui| {
                            ui_style::PclComboBox::from_id_salt("pack-export-format")
                                .width(230.0)
                                .selected_text(state.options.format.label())
                                .show_ui(ui, |ui| {
                                    for format in [
                                        PackFormat::Mrpack,
                                        PackFormat::MultiMc,
                                        PackFormat::Hmcl,
                                        PackFormat::Mcbbs,
                                    ] {
                                        ui.selectable_value(
                                            &mut state.options.format,
                                            format,
                                            format.label(),
                                        );
                                    }
                                });
                        });
                    });
                    if state.options.format != PackFormat::Mrpack {
                        ui.label("此 ZIP 格式直接包含所选资源文件；HMCL 格式目前仅导出原版。");
                    }
                    let mut embed = state.options.resource_mode == ResourceMode::EmbedAll;
                    if embed {
                        ui.add_space(2.0);
                        super::setup_launch_ui::hint(
                            ui,
                            "资源文件将直接放入整合包。公开发布前请确认资源允许再分发。",
                            true,
                        );
                        ui.add_space(8.0);
                    }
                    if checkbox(
                        ui,
                        &mut embed,
                        "打包资源文件，以避免在导入时下载",
                        "",
                        false,
                    )
                    .changed()
                    {
                        state.options.resource_mode = if embed {
                            ResourceMode::EmbedAll
                        } else {
                            ResourceMode::PreferHosted
                        };
                    }
                    ui.add_enabled_ui(false, |ui| {
                        checkbox(ui, &mut true, "仅从 Modrinth 下载资源文件", "", false);
                    });
                    ui.add_space(20.0);
                    super::setup_launch_ui::hint(
                        ui,
                        "可保存当前导出选项，在下次导出时读取配置。\n仅支持当前可用的选项和存档。",
                        false,
                    );
                    ui.add_space(2.0);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 20.0;
                        if button(ui, "读取配置", true).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("导出配置", &["json"])
                                .pick_file()
                            {
                                match read_options(&path).and_then(|value| {
                                    validate_available_options(&value.selection, available)?;
                                    anyhow::ensure!(!value.include_launcher || state.launcher_error.is_none(), "此本机应用不能附带导出，请关闭 include_launcher");
                                    anyhow::ensure!(!value.include_java || state.java_available, "当前版本没有可导出的版本目录 Java，请关闭配置中的 include_java");
                                    Ok(value)
                                }) {
                                    Ok(value) => {
                                        state.options = value;
                                        apply_visibility(&mut state.options.selection, available);
                                        state.error = None;
                                    }
                                    Err(error) => {
                                        state.error = Some(format!("读取配置失败：{error:#}"))
                                    }
                                }
                            }
                        }
                        if button(ui, "保存配置", false).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("导出配置", &["json"])
                                .set_file_name("PCL-Rust-export.json")
                                .save_file()
                            {
                                match save_options(&path, &state.options) {
                                    Ok(()) => state.error = None,
                                    Err(error) => {
                                        state.error = Some(format!("保存配置失败：{error:#}"))
                                    }
                                }
                            }
                        }
                        help = button(ui, "整合包制作指南", false).clicked();
                    });
                },
            );
        });
        if let Some(error) = &state.error {
            ui.add_space(10.0);
            ui.colored_label(Color32::DARK_RED, error);
        }
        ui.add_space(65.0);
        let center = ui.max_rect().center().x;
        let size = Vec2::new(136.0, 42.0);
        let bottom = ui.ctx().content_rect().bottom() - 20.0 - size.y;
        let mut export = false;
        egui::Area::new(egui::Id::new("pack-export-start"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(center - size.x / 2.0, bottom))
            .movable(false)
            .default_size(size)
            .show(ui.ctx(), |ui| {
                ui.set_min_size(size);
                ui.set_max_size(size);
                let response = ui.add_enabled(
                    mutable,
                    egui::Button::new("")
                        .min_size(size)
                        .fill(theme::palette(ui.ctx()).accent)
                        .corner_radius(21)
                        .stroke(egui::Stroke::NONE),
                );
                let color = if mutable {
                    Color32::WHITE
                } else {
                    Color32::from_gray(210)
                };
                self.assets.icon(
                    ui,
                    "pack",
                    Rect::from_min_size(
                        response.rect.min + Vec2::new(21.0, 11.0),
                        Vec2::splat(20.0),
                    ),
                    color,
                );
                ui.painter().text(
                    response.rect.min + Vec2::new(53.0, 20.6),
                    egui::Align2::LEFT_CENTER,
                    "开始导出",
                    egui::FontId::proportional(16.0),
                    color,
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, mutable, "开始导出")
                });
                export = response.clicked();
            });
        if help {
            self.open_local_help("指南/整合包制作 - Public");
        }
        if export {
            self.start_pack_export();
        }
    }

    fn start_pack_export(&mut self) {
        let Some(id) = self.settings.selected_version.clone() else {
            return;
        };
        let options = match effective_options(&self.pack_export.options, &id) {
            Ok(options) => options,
            Err(error) => {
                self.pack_export.error = Some(format!("无法导出：{error:#}"));
                return;
            }
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出整合包")
            .add_filter(
                "整合包",
                &[if options.include_launcher {
                    "zip"
                } else {
                    options.format.extension()
                }],
            )
            .set_file_name(suggested_pack_filename(&options))
            .save_file()
        else {
            return;
        };
        let Some((tx, _cancel)) =
            self.start_download_job_at("正在导出整合包", path.clone(), Some(id.clone()))
        else {
            return;
        };
        self.pack_export.error = None;
        let root = self.settings.game_root.clone();
        let launcher = current_launcher().ok();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let event = pack_export_event(pack_export::export_pack_with_launcher(
                &root,
                &id,
                &path,
                &options,
                launcher.as_deref(),
                &cancel,
                |progress| {
                    let _ = tx.send(Event::Progress(progress));
                },
            ));
            let _ = tx.send(event);
        });
    }
}

/// Keep success, genuine failure and typed cancellation on the same scoped
/// sender; a failed export must release the write job just like installation.
pub(super) fn pack_export_event(result: anyhow::Result<pack_export::PackExportReport>) -> Event {
    match result {
        Ok(report) => {
            Event::Done(format!(
            "整合包已导出：{}\n资源下载链接：{}；随包文件：{}；未托管资源：{}；排除敏感文件：{}",
            report.path.display(),report.hosted_files,report.override_files,
            report.unhosted_files.len(),report.excluded_sensitive_files.len()
        ))
        }
        Err(error) => Event::download_failed("整合包未导出", error),
    }
}

fn current_launcher() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    Ok(executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(&executable)
        .to_owned())
}

fn suggested_pack_filename(options: &PackExportOptions) -> String {
    let portable = |value: &str| {
        value
            .chars()
            .map(|character| {
                if "/\\:*?\"<>|".contains(character) || character.is_control() {
                    '_'
                } else {
                    character
                }
            })
            .collect::<String>()
    };
    format!(
        "{}-{}.{}",
        portable(&options.name),
        portable(&options.version),
        if options.include_launcher {
            "zip"
        } else {
            options.format.extension()
        }
    )
}

fn apply_visibility(selection: &mut ExportSelection, available: &ExportSelection) {
    macro_rules! keep { ($($field:ident),* $(,)?) => { $(selection.$field &= available.$field;)* }; }
    keep!(
        game_settings,
        game_personal,
        optifine_settings,
        mods,
        disabled_mods,
        mod_configs,
        pack_data,
        tacz,
        paintings,
        maps,
        jei_personal,
        emi_personal,
        patchouli_personal,
        resource_packs,
        shader_packs,
        shader_settings,
        licenses,
        screenshots,
        schematics,
        replays,
        servers
    );
    selection
        .worlds
        .retain(|world| available.worlds.contains(world));
    if selection.resource_pack_items.is_none() {
        selection.resource_pack_items =
            Some(available.resource_pack_items.clone().unwrap_or_default());
    }
    if selection.shader_pack_items.is_none() {
        selection.shader_pack_items = Some(available.shader_pack_items.clone().unwrap_or_default());
    }
}

fn validate_available_options(
    selection: &ExportSelection,
    available: &ExportSelection,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        selection
            .worlds
            .iter()
            .all(|world| available.worlds.contains(world)),
        "配置指定的存档在当前版本中不存在，未应用配置"
    );
    for (enabled, selected, candidates, label) in [
        (
            selection.resource_packs,
            &selection.resource_pack_items,
            &available.resource_pack_items,
            "资源包",
        ),
        (
            selection.shader_packs,
            &selection.shader_pack_items,
            &available.shader_pack_items,
            "光影包",
        ),
    ] {
        if !enabled {
            continue;
        }
        if let Some(selected) = selected {
            let candidates = candidates.as_deref().unwrap_or_default();
            anyhow::ensure!(
                selected.iter().all(|item| candidates.contains(item)),
                "配置指定的{label}在当前版本中不存在或类型已改变，未应用配置"
            );
        }
    }
    Ok(())
}

fn pack_item_rows(
    ui: &mut egui::Ui,
    available: &Option<Vec<String>>,
    selected: &mut Option<Vec<String>>,
) {
    let Some(available) = available else {
        return;
    };
    let selected = selected.get_or_insert_with(|| available.clone());
    for item in available {
        let name = item
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(item);
        let duplicate_name = available
            .iter()
            .filter(|other| other.trim_end_matches('/').rsplit('/').next() == Some(name))
            .count()
            > 1;
        let mut description = if item.ends_with('/') {
            "文件夹".to_owned()
        } else {
            String::new()
        };
        if duplicate_name {
            if !description.is_empty() {
                description.push_str(" · ");
            }
            description.push_str(item.split('/').next().unwrap_or_default());
        }
        let mut checked = selected.contains(item);
        if checkbox(ui, &mut checked, name, &description, true).changed() {
            if checked {
                selected.push(item.clone());
            } else {
                selected.retain(|other| other != item);
            }
        }
    }
}

fn checkbox(
    ui: &mut egui::Ui,
    value: &mut bool,
    title: &str,
    description: &str,
    indent: bool,
) -> egui::Response {
    let (mut rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), egui::Sense::hover());
    if indent {
        rect.min.x += 30.0;
    }
    ui_style::place_left(ui, rect, |ui: &mut egui::Ui| {
        ui_style::checkbox(ui, value, title, description)
    })
}
fn button(ui: &mut egui::Ui, title: &str, highlight: bool) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(140.0, 35.0), egui::Sense::hover());
    ui_style::outline_button(ui, rect, title, None, highlight, true)
}
fn effective_options(
    value: &PackExportOptions,
    instance: &str,
) -> anyhow::Result<PackExportOptions> {
    let mut result = value.clone();
    if result.name.is_empty() {
        result.name = instance.into();
    }
    if result.version.is_empty() {
        result.version = "1.0.0".into();
    }
    pack_export::validate_export_options(&result)?;
    Ok(result)
}
fn export_card(
    ui: &mut egui::Ui,
    title: &str,
    mut open: Option<&mut bool>,
    margin: egui::Margin,
    body: impl FnOnce(&mut egui::Ui),
) {
    let expanded = open.as_deref().copied().unwrap_or(true);
    let result = page_frame()
        .inner_margin(if expanded { margin } else { egui::Margin::ZERO })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if expanded {
                body(ui);
            } else {
                ui.set_min_height(40.0);
            }
        });
    let rect = result.response.rect;
    if !title.is_empty() {
        ui_style::place_left(
            ui,
            Rect::from_min_size(
                rect.min + Vec2::new(15.0, 12.0),
                Vec2::new(rect.width() - 55.0, 18.0),
            ),
            egui::Label::new(ui_style::card_title(title)),
        );
    }
    if let Some(open) = open.as_mut() {
        let response = ui.interact(
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), 37.0)),
            ui.id().with(("export-card", title)),
            egui::Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), title)
        });
        if response.clicked() {
            **open = !**open;
        }
        ui_style::card_chevron(ui, rect, expanded, theme::palette(ui.ctx()).text);
    }
    ui.add_space(15.0);
}
fn read_options(path: &std::path::Path) -> anyhow::Result<PackExportOptions> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 131_072,
        "导出配置超过 128 KiB"
    );
    let value = serde_json::from_slice(&std::fs::read(path)?)?;
    effective_options(&value, "export")?;
    Ok(value)
}
fn save_options(path: &std::path::Path, value: &PackExportOptions) -> anyhow::Result<()> {
    effective_options(value, "export")?;
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("配置路径无效"))?,
    )?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_resource_selection_expands_once_and_missing_explicit_items_are_rejected() {
        let available = ExportSelection {
            resource_pack_items: Some(vec![
                "resourcepacks/A.zip".into(),
                "texturepacks/A.zip".into(),
            ]),
            shader_pack_items: Some(vec!["shaderpacks/B/".into()]),
            ..Default::default()
        };
        let mut selection = ExportSelection::default();
        apply_visibility(&mut selection, &available);
        assert_eq!(selection.resource_pack_items, available.resource_pack_items);
        selection.resource_pack_items = Some(Vec::new());
        selection.resource_packs = false;
        apply_visibility(&mut selection, &available);
        assert_eq!(selection.resource_pack_items, Some(Vec::new()));
        assert!(validate_available_options(&selection, &available).is_ok());
        selection.shader_pack_items = Some(vec!["shaderpacks/B.zip".into()]);
        selection.shader_packs = true;
        assert!(validate_available_options(&selection, &available).is_err());
        assert_eq!(
            selection.shader_pack_items,
            Some(vec!["shaderpacks/B.zip".into()])
        );
        selection.shader_packs = false;
        assert!(validate_available_options(&selection, &available).is_ok());
    }
    #[test]
    fn blank_export_fields_use_visible_defaults_but_whitespace_is_invalid() {
        let mut options = PackExportState::default().options;
        let resolved = effective_options(&options, "我的版本").unwrap();
        assert_eq!(resolved.name, "我的版本");
        assert_eq!(resolved.version, "1.0.0");
        assert!(options.name.is_empty());
        options.name = "  ".into();
        assert!(effective_options(&options, "我的版本").is_err());
    }
    #[test]
    fn checkbox_rows_start_at_the_same_left_edge_with_exact_child_indent() {
        let ctx = egui::Context::default();
        let mut rects = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(656., 300.))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.set_width(640.0);
                for (title, description, indent) in [
                    ("A", "", false),
                    ("Longer", "inline description", false),
                    ("Child", "", true),
                ] {
                    rects.push(checkbox(ui, &mut true, title, description, indent).rect);
                }
            });
        });
        assert_eq!(rects[0].left(), rects[1].left());
        assert_eq!(rects[2].left() - rects[0].left(), 30.0);
        assert!((rects[1].center().y - rects[0].center().y - 26.0).abs() < 0.01);
        assert!(
            (rects[0].width() - 640.0).abs() < 0.01,
            "row bounds: {rects:?}"
        );
        assert_eq!(rects[0].right(), rects[2].right());
        assert_eq!(rects[0].height(), 26.0);
    }
    #[test]
    fn export_configuration_roundtrips_and_never_clobbers_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.json");
        let options = PackExportOptions {
            name: "测试整合包".into(),
            version: "1.2".into(),
            selection: ExportSelection {
                worlds: vec!["Only selected world".into()],
                ..Default::default()
            },
            resource_mode: ResourceMode::EmbedAll,
            ..Default::default()
        };
        save_options(&path, &options).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let restored = read_options(&path).unwrap();
        assert_eq!(restored.name, options.name);
        assert_eq!(restored.selection, options.selection);
        assert_eq!(restored.resource_mode, ResourceMode::EmbedAll);
        assert!(save_options(&path, &options).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(
            &path,
            b"{\"name\":\"Pack\",\"selection\":{\"worlds\":[\"../outside\"]}}",
        )
        .unwrap();
        assert!(read_options(&path).is_err());
    }
}
