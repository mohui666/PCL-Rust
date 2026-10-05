//! Explicit reversible management; requests are pinned to their original root and instance.
use super::*;
use crate::app::{
    modal_ui::{account_modal_with_options, ModalOptions},
    Event,
};
use std::path::PathBuf;
#[derive(Clone)]
enum Pending {
    Remove {
        root: PathBuf,
        id: String,
        instance: PathBuf,
        names: Vec<String>,
    },
    Reset {
        root: PathBuf,
        id: String,
    },
}
fn key() -> egui::Id {
    egui::Id::new("version-management-confirm")
}
impl Launcher {
    pub(in crate::app) fn confirm_mod_removal(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        instance: &Path,
        names: Vec<String>,
    ) {
        ctx.data_mut(|d| {
            d.insert_temp(
                key(),
                Pending::Remove {
                    root: self.settings.game_root.clone(),
                    id: id.into(),
                    instance: instance.into(),
                    names,
                },
            )
        });
    }
    pub(in crate::app) fn confirm_instance_reset(&mut self, ctx: &egui::Context, id: &str) {
        ctx.data_mut(|d| {
            d.insert_temp(
                key(),
                Pending::Reset {
                    root: self.settings.game_root.clone(),
                    id: id.into(),
                },
            )
        });
    }
    pub(in crate::app) fn version_management_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = ctx.data(|d| d.get_temp::<Pending>(key())) else {
            return;
        };
        let (root,id,message)=match &pending {
            Pending::Remove{root,id,names,..}=>(root,id,format!("移除所选的 {} 个 Mod？文件会移至当前实例的 PCL-Rust/mod-removals 备份目录，可通过“恢复已移除”恢复。同名新文件不会被覆盖。",names.len())),
            Pending::Reset{root,id}=>(root,id,"初始化此版本的独立启动设置？Java、内存、参数、隔离等将恢复默认。不会删除游戏、Mod 或存档；原设置会保留为版本目录内的备份文件，图标、分类、收藏和描述保留。".into()),
        };
        if root != &self.settings.game_root
            || self.settings.selected_version.as_deref() != Some(id)
            || self.game_pid.is_some()
            || self.jobs.conflicts_with(root)
        {
            ctx.data_mut(|d| d.remove::<Pending>(key()));
            return;
        }
        if let Some(action) = account_modal_with_options(
            ctx,
            "version-management-warning",
            "确认操作",
            &message,
            &["确定", "取消"],
            ModalOptions::warning(),
        ) {
            ctx.data_mut(|d| d.remove::<Pending>(key()));
            if action != 0 {
                return;
            }
            let Some((tx, _)) = self.start_download_job("正在处理版本文件", Some(id.clone()))
            else {
                return;
            };
            tx.spawn(move |tx| {
                let cancel = tx.cancel_token();
                let result = match &pending {
                    Pending::Remove {
                        instance, names, ..
                    } => mods::remove_mods(instance, names, &cancel).map(|r| {
                        format!("已移除 {} 个 Mod；备份：{}", r.count, r.backup.display())
                    }),
                    Pending::Reset { root, id } => {
                        config::reset_instance_settings(root, id, &cancel)
                            .map(|p| format!("版本设置已初始化；原设置备份：{}", p.display()))
                    }
                };
                let id = match &pending {
                    Pending::Remove { id, .. } | Pending::Reset { id, .. } => id.clone(),
                };
                let _ = tx.send(match result {
                    Ok(message) => Event::ModsChanged {
                        target: id,
                        message,
                    },
                    Err(e) => Event::download_failed("版本文件操作未完成", e),
                });
            });
        }
    }
    pub(in crate::app) fn restore_instance_preferences(&mut self, id: &str) {
        if self.game_pid.is_some() || self.jobs.conflicts_with(&self.settings.game_root) {
            return;
        }
        let root = self.settings.game_root.clone();
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择当前版本初始化前的设置备份")
            .set_directory(root.join("versions").join(id).join("PCL-Rust"))
            .add_filter("版本设置备份", &["json"])
            .pick_file()
        else {
            return;
        };
        let Some((tx, _)) = self.start_download_job("正在恢复版本设置", Some(id.into()))
        else {
            return;
        };
        let id = id.to_owned();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = config::restore_instance_settings(&root, &id, &path, &cancel);
            let _ = tx.send(match result {
                Ok(()) => Event::ModsChanged {
                    target: id,
                    message: "版本设置已恢复".into(),
                },
                Err(e) => Event::download_failed("版本设置恢复未完成", e),
            });
        });
    }
    pub(in crate::app) fn restore_mod_removal(&mut self, id: &str, instance: &Path) {
        if self.game_pid.is_some() || self.jobs.conflicts_with(&self.settings.game_root) {
            return;
        }
        let Some(backup) = rfd::FileDialog::new()
            .set_title("选择当前实例的 Mod 移除备份")
            .set_directory(instance.join("PCL-Rust/mod-removals"))
            .pick_folder()
        else {
            return;
        };
        let Some((tx, _)) = self.start_download_job("正在恢复 Mod", Some(id.into())) else {
            return;
        };
        let instance = instance.to_owned();
        let id = id.to_owned();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = mods::restore_removed_mods(&instance, &backup, &cancel);
            let _ = tx.send(match result {
                Ok(count) => Event::ModsChanged {
                    target: id,
                    message: format!("已恢复 {count} 个 Mod"),
                },
                Err(e) => Event::download_failed("Mod 恢复未完成", e),
            });
        });
    }
}
pub(in crate::app) fn paint_custom_icon(
    ui: &mut egui::Ui,
    path: Option<&Path>,
    rect: Rect,
) -> bool {
    let Some(path) = path else {
        return false;
    };
    let key = egui::Id::new(("custom-instance-icon", path));
    let cached = ui.data(|d| d.get_temp::<egui::TextureHandle>(key));
    let texture = cached.or_else(|| {
        let meta = std::fs::symlink_metadata(path).ok()?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 4 * 1024 * 1024 {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(256);
        limits.max_image_height = Some(256);
        reader.limits(limits);
        let img = reader.decode().ok()?.to_rgba8();
        let texture = ui.ctx().load_texture(
            format!("instance-icon:{}", path.display()),
            egui::ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        );
        ui.data_mut(|d| d.insert_temp(key, texture.clone()));
        Some(texture)
    });
    if let Some(texture) = texture {
        let pixels = texture.size_vec2();
        let scale = (rect.width() / pixels.x).min(rect.height() / pixels.y);
        let fitted = Rect::from_center_size(rect.center(), pixels * scale);
        ui.painter().image(
            texture.id(),
            fitted,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        true
    } else {
        false
    }
}
