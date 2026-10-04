use super::{Event, Launcher, MUTED};
use crate::theme;
use crate::ui_style;
use anyhow::Context;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};
use pcl_core::{
    config,
    forge::{self, ForgeKind},
    install,
    loaders::{self, LoaderKind, LoaderVersion},
    metadata,
    model::Platform,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallKind {
    Forge,
    NeoForge,
    Fabric,
    Quilt,
}
impl InstallKind {
    fn label(self) -> &'static str {
        match self {
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
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
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Forge => "block-forge",
            Self::NeoForge => "block-neoforge",
            Self::Fabric => "block-fabric",
            Self::Quilt => "mod",
        }
    }
}
impl Launcher {
    fn load_loader_versions(&mut self, kind: InstallKind, minecraft: String) {
        let Some((tx, cancel)) = self.start_job("正在获取可用加载器") else {
            return;
        };
        self.loader_versions.clear();
        std::thread::spawn(move || {
            let result = if let Some(kind) = kind.meta_kind() {
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
            let _ = tx.send(match result {
                Ok(versions) => Event::LoaderVersions(minecraft, kind, versions),
                Err(error) => Event::Error(format!("加载器列表获取失败：{error:#}")),
            });
        });
    }
    fn install_selected_loader(
        &mut self,
        kind: InstallKind,
        minecraft: String,
        loader: String,
        instance_id: String,
    ) {
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
        if kind.forge_kind().is_some() && java.is_none() {
            self.error=Some(format!("{} 安装处理器需要 Java。请先在设置中选择与 Minecraft {minecraft} 对应的 Java 安装目录。",kind.label()));
            self.page = super::Page::Settings;
            return;
        }
        let Some((tx, cancel)) = self.start_download_job(
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
        std::thread::spawn(move || {
            let progress = |p| {
                let _ = tx.send(Event::Progress(p));
            };
            let result = if let Some(kind) = kind.meta_kind() {
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
                forge::install_forge(
                    &root,
                    kind.forge_kind().unwrap(),
                    &minecraft,
                    &loader,
                    java.as_ref().unwrap(),
                    &Platform::current(),
                    &cancel,
                    progress,
                )
            };
            let result = result.and_then(|id| {
                install::register_instance_id(
                    &root,
                    &id,
                    &instance_id,
                    &Platform::current(),
                    &cancel,
                )
            });
            let result = result.and_then(|id| {
                if !existed {
                    config::initialize_instance_settings(&root, &id, policy)
                        .context("新版本已登记，但默认隔离设置未保存")?;
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
        let Some((tx, cancel)) = self.start_download_job(
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
        std::thread::spawn(move || {
            let progress = |p| {
                let _ = tx.send(Event::Progress(p));
            };
            let result = if minecraft == instance_id {
                install::install_version(&root, &minecraft, &Platform::current(), &cancel, progress)
                    .map(|()| minecraft)
            } else {
                install::install_vanilla_instance(
                    &root,
                    &minecraft,
                    &instance_id,
                    &Platform::current(),
                    &cancel,
                    progress,
                )
            };
            let result = result.and_then(|id| {
                if !existed {
                    config::initialize_instance_settings(&root, &id, policy)
                        .context("新版本已登记，但默认隔离设置未保存")?;
                }
                Ok(id)
            });
            let _ = tx.send(match result {
                Ok(id) => Event::Installed(id),
                Err(error) => Event::download_failed("实例安装未完成", error),
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
                    ui.add_enabled_ui(self.busy.is_none(), |ui| {
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
            component_frame().show(ui, |ui| {
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 40.0),
                    egui::Sense::hover(),
                );
                response.on_hover_text("OptiFine 安装尚未迁移，当前不可选择。");
                ui_style::place_left(
                    ui,
                    Rect::from_min_size(rect.min + Vec2::new(15.0, 12.0), Vec2::new(110.0, 18.0)),
                    egui::Label::new(ui_style::card_title("OptiFine")).halign(egui::Align::Min),
                );
                ui_style::place_left(
                    ui,
                    Rect::from_min_size(
                        rect.min + Vec2::new(132.0, 11.0),
                        Vec2::new(rect.width() - 147.0, 18.0),
                    ),
                    egui::Label::new(RichText::new("无 · 暂未支持").color(MUTED))
                        .halign(egui::Align::Min),
                );
            });
            ui.add_space(12.0);
            egui::CollapsingHeader::new(RichText::new("更多组件（Quilt）").size(12.0).color(MUTED))
                .id_salt("additional-install-components")
                .show(ui, |ui| {
                    self.install_component_card(ui, InstallKind::Quilt, &minecraft, &mut open);
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
        let source_id = if let (Some(kind), Some(version)) =
            (self.loader_kind, self.loader_version.as_deref())
        {
            kind.default_id(minecraft, version)
        } else {
            minecraft.to_owned()
        };
        let existing_forge = self
            .loader_kind
            .is_some_and(|kind| kind.forge_kind().is_some())
            && std::fs::symlink_metadata(self.settings.game_root.join("versions").join(&source_id))
                .is_ok();
        if existing_forge {
            anyhow::bail!("此 Forge / NeoForge 版本已安装；当前尚不支持复用它创建新名称，请在版本列表选择已有实例");
        }
        if self.loader_kind.is_some() && self.install_name == minecraft {
            anyhow::bail!("名称与原版相同，请为加载器实例使用独立名称");
        }
        install::validate_instance_id(&self.settings.game_root, &source_id, &self.install_name)
    }

    pub(super) fn install_footer(&mut self, ctx: &egui::Context) {
        let Some(minecraft) = self.download_selection.clone() else {
            return;
        };
        let ready = self.busy.is_none()
            && self.game_pid.is_none()
            && self.install_selection_validation(&minecraft).is_ok();
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
        let selected = self.loader_kind == Some(kind);
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
                self.loader_version.as_deref().unwrap_or("可以添加")
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
                    self.loader_kind = None;
                    self.loader_version = None;
                    cleared = true;
                    if !self.install_name_edited {
                        self.install_name = minecraft.to_owned();
                    }
                }
            }
            if response.clicked() && !cleared && self.busy.is_none() {
                if expanded {
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
                        if self.loader_versions.is_empty() {
                            ui.label(if self.busy.is_some() {
                                "正在获取版本列表…"
                            } else {
                                "此 Minecraft 版本暂无可安装版本。"
                            });
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
                                                && self.loader_version.as_deref()
                                                    == Some(&version.version),
                                            label,
                                        )
                                        .clicked()
                                        && self.busy.is_none()
                                    {
                                        self.loader_kind = Some(kind);
                                        self.loader_version = Some(version.version.clone());
                                        self.loader_expanded = None;
                                        if !self.install_name_edited {
                                            self.install_name =
                                                kind.default_id(minecraft, &version.version);
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
