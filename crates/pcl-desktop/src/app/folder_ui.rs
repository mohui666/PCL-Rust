use super::{modal_ui, Launcher};
use eframe::egui;
use pcl_core::config;
use std::path::PathBuf;
#[derive(Default)]
pub(super) struct FolderUi {
    pub(super) pending: Option<(PathBuf, bool)>,
    pub(super) name: String,
}
impl Launcher {
    pub(super) fn new_game_folder(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("新建 Minecraft 文件夹")
            .set_file_name(".minecraft")
            .save_file()
        else {
            return;
        };
        if self.jobs.conflicts_with(&path) {
            self.error = Some("此目录正在执行任务".into());
            return;
        }
        match config::create_game_root(&self.settings_path, &self.settings, &path, "") {
            Ok(next) => {
                self.settings = next;
                self.status = "游戏文件夹已创建".into();
                self.refresh_versions();
            }
            Err(e) => self.error = Some(format!("新建游戏文件夹失败：{e:#}")),
        }
    }
    pub(super) fn folder_dialog(&mut self, ctx: &egui::Context) {
        let Some((path, remove)) = self.folder_ui.pending.clone() else {
            return;
        };
        let action = if remove {
            modal_ui::account_modal(
                ctx,
                "remove-game-root",
                "移除文件夹",
                &format!(
                    "从列表移除 {}？\n不会删除该文件夹中的游戏文件。",
                    path.display()
                ),
                &["移除", "取消"],
            )
        } else {
            modal_ui::account_input_modal(
                ctx,
                "rename-game-root",
                "重命名文件夹",
                &format!("为 {} 设置显示名称。", path.display()),
                &mut self.folder_ui.name,
                &["确定", "取消"],
            )
        };
        if let Some(action) = action {
            self.folder_ui.pending = None;
            if action != 0 {
                return;
            }
            if self.jobs.conflicts_with(&path) || self.game_pid.is_some() {
                self.error = Some("请等待此目录的任务或游戏结束后再更改".into());
                return;
            }
            let result = if remove {
                config::unregister_game_root(&self.settings_path, &self.settings, &path)
            } else {
                config::rename_game_root(
                    &self.settings_path,
                    &self.settings,
                    &path,
                    &self.folder_ui.name,
                )
            };
            match result {
                Ok(next) => {
                    self.settings = next;
                    self.root_text = self.settings.game_root.display().to_string();
                    self.invalidate_root_views();
                    self.refresh_versions();
                    self.status = if remove {
                        "已从列表移除文件夹"
                    } else {
                        "文件夹显示名称已修改"
                    }
                    .into();
                }
                Err(e) => self.error = Some(format!("文件夹管理失败：{e:#}")),
            }
        }
    }
}
