//! FormMain.FileDrag: files only. No authentication URI handling or directory sorting.
use super::*;
use crate::app::{
    hint_ui::HintKind,
    modal_ui::{account_modal_with_options, ModalOptions},
    Event,
};
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, PartialEq)]
enum DropKind {
    Mods,
    Pack,
    Home,
    Logs,
}
fn classify(paths: &[PathBuf]) -> anyhow::Result<DropKind> {
    anyhow::ensure!(
        !paths.is_empty() && paths.len() <= 200,
        "每次请拖入 1 至 200 个文件"
    );
    for path in paths {
        let meta = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            path.is_absolute() && meta.is_file() && !meta.file_type().is_symlink(),
            "请拖入已解压的普通文件，而非文件夹或链接"
        );
    }
    let ext = |path: &Path| {
        path.extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    if paths
        .iter()
        .all(|p| matches!(ext(p).as_str(), "jar" | "litemod" | "disabled" | "old"))
    {
        return Ok(DropKind::Mods);
    }
    anyhow::ensure!(paths.len() == 1, "整合包、主页或日志一次只接受一个文件");
    match ext(&paths[0]).as_str() {
        "zip" | "mrpack" | "rar" => Ok(DropKind::Pack),
        "xaml" => Ok(DropKind::Home),
        "log" | "txt" => Ok(DropKind::Logs),
        _ => anyhow::bail!("无法确定此文件的拖放操作；支持 Mod、整合包、XAML 主页和日志"),
    }
}
#[derive(Clone)]
struct ModDrop {
    root: PathBuf,
    id: String,
    instance: PathBuf,
    paths: Vec<PathBuf>,
}
type PackResult = Result<pcl_core::modpack::ModpackInfo, String>;
#[derive(Clone)]
struct PackDrop {
    root: PathBuf,
    path: PathBuf,
    result: Arc<Mutex<Option<PackResult>>>,
}
fn mod_key() -> egui::Id {
    egui::Id::new("pending-file-drop-mods")
}
fn pack_key() -> egui::Id {
    egui::Id::new("pending-file-drop-pack")
}
impl Launcher {
    pub(in crate::app) fn handle_file_drop(&mut self, ctx: &egui::Context) {
        if let Some(pending) = ctx.data(|d| d.get_temp::<PackDrop>(pack_key())) {
            let ready = pending.result.lock().ok().and_then(|mut r| r.take());
            if let Some(result) = ready {
                ctx.data_mut(|d| d.remove::<PackDrop>(pack_key()));
                if pending.root == self.settings.game_root {
                    match result {
                        Ok(info) => {
                            self.pack_id = format!("{}-{}", info.name, info.version_id)
                                .chars()
                                .map(|c| {
                                    if c.is_alphanumeric() || "-_.".contains(c) {
                                        c
                                    } else {
                                        '-'
                                    }
                                })
                                .take(80)
                                .collect::<String>()
                                .trim_matches('.')
                                .into();
                            self.pack_info = Some((pending.path, info));
                            self.open_local_pack_import();
                            self.status = "已读取拖入的整合包，请确认新实例名称后开始安装".into();
                        }
                        Err(error) => {
                            if pending
                                .path
                                .extension()
                                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                            {
                                self.import_crash_path(ctx, pending.path);
                            } else {
                                self.error = Some(format!(
                                    "整合包读取失败：{error}。RAR 文件请解压后重新压缩为 ZIP。"
                                ));
                            }
                        }
                    }
                } else {
                    self.push_hint(HintKind::Info, "游戏目录已切换，请重新拖入文件");
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        if let Some(pending) = ctx.data(|d| d.get_temp::<ModDrop>(mod_key())) {
            if pending.root != self.settings.game_root
                || self.settings.selected_version.as_deref() != Some(pending.id.as_str())
            {
                ctx.data_mut(|d| d.remove::<ModDrop>(mod_key()));
            } else if let Some(action) = account_modal_with_options(
                ctx,
                "mod-drop-confirm",
                "Mod 安装确认",
                &format!(
                    "将 {} 个文件作为 Mod 安装到 {}？已有同名文件不会被覆盖，来源文件保留。",
                    pending.paths.len(),
                    pending.id
                ),
                &["确定", "取消"],
                ModalOptions::default(),
            ) {
                ctx.data_mut(|d| d.remove::<ModDrop>(mod_key()));
                if action == 0 {
                    self.install_dropped_mods(pending);
                }
            }
        }
        let files = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        if files.is_empty() {
            return;
        }
        if self.busy.is_some()
            || ctx.data(|d| {
                d.get_temp::<PackDrop>(pack_key()).is_some()
                    || d.get_temp::<ModDrop>(mod_key()).is_some()
            })
        {
            self.push_hint(HintKind::Info, "请先完成当前文件操作，再拖入文件");
            return;
        }
        let paths: Option<Vec<_>> = files.into_iter().map(|f| f.path).collect();
        let Some(paths) = paths else {
            self.error = Some("请先将文件保存到磁盘或解压后再拖入".into());
            return;
        };
        let kind = match classify(&paths) {
            Ok(kind) => kind,
            Err(e) => {
                self.error = Some(format!("文件拖入失败：{e:#}"));
                return;
            }
        };
        match kind {
            DropKind::Home => self.import_home_file(ctx, &paths[0]),
            DropKind::Logs => self.import_crash_path(ctx, paths[0].clone()),
            DropKind::Pack => {
                let path = paths[0].clone();
                let result = Arc::new(Mutex::new(None));
                ctx.data_mut(|d| {
                    d.insert_temp(
                        pack_key(),
                        PackDrop {
                            root: self.settings.game_root.clone(),
                            path: path.clone(),
                            result: result.clone(),
                        },
                    )
                });
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let info =
                        pcl_core::modpack::inspect_mrpack(&path).map_err(|e| format!("{e:#}"));
                    if let Ok(mut target) = result.lock() {
                        *target = Some(info);
                    }
                    ctx.request_repaint();
                });
                self.status = "正在读取拖入的整合包…".into();
            }
            DropKind::Mods => {
                let Some(id) = self.settings.selected_version.clone() else {
                    self.error = Some("请先选择可安装 Mod 的版本".into());
                    return;
                };
                if self.version_view
                    || !self.versions.iter().find(|v| v.id == id).is_some_and(|v| {
                        version_presentation(&self.settings.game_root, v).modable == Some(true)
                    })
                {
                    self.error = Some("请先选择可安装 Mod 的版本".into());
                    return;
                }
                let instance = match config::instance_game_dir(&self.settings.game_root, &id) {
                    Ok(path) => path,
                    Err(e) => {
                        self.error = Some(e.to_string());
                        return;
                    }
                };
                let pending = ModDrop {
                    root: self.settings.game_root.clone(),
                    id,
                    instance,
                    paths,
                };
                if self.version_tools && self.tools_tab == 1 {
                    self.install_dropped_mods(pending);
                } else {
                    ctx.data_mut(|d| d.insert_temp(mod_key(), pending));
                }
            }
        }
    }
    fn install_dropped_mods(&mut self, pending: ModDrop) {
        if self.game_pid.is_some() || self.jobs.conflicts_with(&pending.root) {
            self.error = Some("请先关闭游戏并等待当前游戏目录的写入结束".into());
            return;
        }
        let Some((tx, _)) = self.start_download_job("正在安装拖入的 Mod", Some(pending.id.clone()))
        else {
            return;
        };
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = (|| {
                std::fs::create_dir_all(&pending.instance)?;
                anyhow::ensure!(
                    config::instance_game_dir(&pending.root, &pending.id)? == pending.instance,
                    "实例目录已改变"
                );
                mods::import_mods(&pending.instance, &pending.paths, &cancel)
            })();
            let _ = tx.send(match result {
                Ok(count) => Event::ModsChanged {
                    target: pending.id,
                    message: format!("已安装 {count} 个拖入的 Mod"),
                },
                Err(e) => Event::download_failed("Mod 拖入安装未完成", e),
            });
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drag_routes_only_original_file_types_and_never_directories() {
        let d = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let p = d.path().join(name);
            std::fs::write(&p, b"fixture").unwrap();
            p
        };
        assert_eq!(
            classify(&[file("a.JAR"), file("b.litemod.old")]).unwrap(),
            DropKind::Mods
        );
        assert_eq!(classify(&[file("pack.mrpack")]).unwrap(), DropKind::Pack);
        assert_eq!(classify(&[file("home.xaml")]).unwrap(), DropKind::Home);
        assert!(classify(&[file("a.log"), file("b.zip")]).is_err());
        assert!(classify(&[d.path().into()]).is_err());
        assert!(classify(&[file("run.exe")]).is_err());
    }
}
