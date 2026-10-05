use super::{
    modal_ui,
    xaml_ui::{self, Action, Node, Origin, Renderer},
    Launcher, Page,
};
use anyhow::{bail, Context, Result};
use eframe::egui;
use pcl_core::config;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
};

type DocumentResult = Result<(Vec<Node>, Origin)>;

#[derive(Default)]
pub(super) struct HomeState {
    key: Option<(u8, u8, String)>,
    nodes: Arc<Vec<Node>>,
    origin: Origin,
    renderer: Renderer,
    pending: Option<mpsc::Receiver<DocumentResult>>,
    error: Option<String>,
    pub(super) message: Option<(String, String)>,
    actions: VecDeque<Action>,
    confirmation: Option<Action>,
    executing: Option<mpsc::Receiver<Result<String>>>,
    variables: HashMap<String, String>,
    variables_loaded: bool,
}
#[derive(Serialize, Deserialize)]
struct Cached {
    url: String,
    content: String,
}

pub(super) const PRESETS:&[(u8,&str,&str)]=&[
    (0,"你知道吗？",""),(1,"回声洞",""),(2,"Minecraft 新闻（最亮的信标）","https://mcnews.meloong.com"),
    (4,"每日整合包推荐（wkea）","https://pclsub.sodamc.com/"),(5,"Minecraft 皮肤推荐（wkea）","https://forgepixel.com/pcl_sub_file"),
    (6,"OpenBMCLAPI 仪表盘 Lite","https://pcl-bmcl.milu.ink/"),(9,"PCL 新功能说明书","https://raw.gitcode.com/WForst-Breeze/WhatsNewPCL/raw/main/Custom.xaml"),
    (11,"杂志主页（CreeperIsASpy）","https://gh-proxy.com/https://github.com/Neclyon/Magazine-Homepage-PCL/raw/main/output/Custom.xaml"),
    (12,"PCL GitHub 仪表盘","https://ddf.pcl-community.top/Custom.xaml"),(13,"Minecraft 更新摘要","https://raw.gitcode.com/ENC_Euphony/PCL-AI-Summary-HomePage/raw/master/Custom.xaml"),
    (14,"今日新闻热点","https://pcl.wyc-w.top/index.xaml"),(15,"Minecraft 芝士站","https://www.xxag.top/mkss"),(16,"整合包推荐引擎","https://qawsedrftgyhujiko.fun/pcl2/Custom.xaml"),
    (17,"Bangumi 番剧主页","https://bangumi.p.kaphia.qzz.io"),(18,"Bilibili 热门","https://bilibili.p.kaphia.qzz.io"),(19,"Music 云音乐热门","https://cloudmusic.p.kaphia.qzz.io")];

pub(super) fn folder(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("homepage")
}
pub(super) fn generate_tutorial(settings_path: &Path) -> Result<PathBuf> {
    let folder = folder(settings_path);
    fs::create_dir_all(&folder)?;
    let target = folder.join("Custom.xaml");
    let mut file = tempfile::NamedTempFile::new_in(&folder)?;
    file.write_all(r#"<!-- PCL Rust 示例：在文本编辑器中修改后点击“刷新主页”。 -->
<local:MyCard Title="我的主页" Margin="0,0,0,15">
  <StackPanel Margin="25,40,23,15">
    <TextBlock Text="当前版本：{name} · {date}" FontSize="16" Margin="0,0,0,12" />
    <local:MyButton Text="PCL Rust 项目主页" EventType="打开网页" EventData="https://github.com/mohui666/PCL-Rust" />
  </StackPanel>
</local:MyCard>"#.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&target)
        .map_err(|error| error.error)
        .context("Custom.xaml 已存在，未覆盖；请先备份或编辑原文件")?;
    Ok(target)
}
impl Launcher {
    pub(super) fn refresh_custom_home(&mut self) {
        self.home.key = None;
        self.home.error = None;
        self.home.renderer.clear();
    }
    pub(super) fn custom_home_page(&mut self, ui: &mut egui::Ui) {
        let key = (
            self.settings.ui_custom_type,
            self.settings.ui_custom_preset,
            self.settings.ui_custom_net.clone(),
        );
        if self.home.key.as_ref() != Some(&key) {
            self.home.key = Some(key.clone());
            self.home.pending = None;
            self.home.nodes = Arc::new(Vec::new());
            self.home.error = None;
            self.home.renderer.clear();
            let directory = folder(&self.settings_path);
            self.home.origin = Origin {
                directory: Some(directory.clone()),
                url: None,
            };
            let result = match key.0 {
                0 => Ok(()),
                1 => read_local(&directory.join("Custom.xaml"))
                    .map(|nodes| self.home.nodes = Arc::new(nodes)),
                2 | 3 => {
                    let url = if key.0 == 2 {
                        key.2.clone()
                    } else {
                        PRESETS
                            .iter()
                            .find(|(id, _, _)| *id == key.1)
                            .map(|(_, _, url)| url.to_string())
                            .unwrap_or_default()
                    };
                    if key.0 == 3 && key.1 <= 1 {
                        let (title, text) = if key.1 == 0 {
                            ("你知道吗？","可以在设置中选择 Java、调整背景图片、导出整合包。主页按钮仅在点击时执行。此为 PCL Rust 使用提示。")
                        } else {
                            ("回声洞","原版回声洞句库未在固定上游公开，当前无法取得原始内容。可选择联网主页或编辑本地 Custom.xaml。")
                        };
                        xaml_ui::parse(&format!("<local:MyCard Title='{}' Margin='0,0,0,15'><TextBlock Text='{}'/></local:MyCard>",title,xaml_ui::escape_xml(text))).map(|nodes|self.home.nodes=Arc::new(nodes))
                    } else {
                        let values = self.public_home_values();
                        let url = xaml_ui::replace(&url, &values);
                        self.begin_network_home(&url, &directory, ui.ctx())
                    }
                }
                _ => Err(anyhow::anyhow!("未知主页模式")),
            };
            if let Err(error) = result {
                self.home.error = Some(format!("主页读取失败：{error:#}"));
            }
        }
        if let Some(receiver) = &self.home.pending {
            match receiver.try_recv() {
                Ok(Ok((nodes, origin))) => {
                    self.home.nodes = Arc::new(nodes);
                    self.home.origin = origin;
                    self.home.pending = None;
                    self.home.error = None;
                    self.home.renderer.clear();
                }
                Ok(Err(error)) => {
                    self.home.pending = None;
                    self.home.error = Some(format!("主页更新失败：{error:#}"));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.home.pending = None;
                    self.home.error = Some("主页读取任务提前结束".into());
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if self.home.pending.is_some() && self.home.nodes.is_empty() {
            ui.label("正在获取主页……");
        }
        if let Some(error) = &self.home.error {
            ui.colored_label(egui::Color32::DARK_RED, error);
            if ui.button("重新读取主页").clicked() {
                self.refresh_custom_home();
            }
        }
        let nodes = Arc::clone(&self.home.nodes);
        let values = self.custom_values();
        let actions = self
            .home
            .renderer
            .render(ui, &nodes, &self.home.origin, &values);
        self.home.actions.extend(actions);
    }
    fn begin_network_home(
        &mut self,
        url: &str,
        directory: &Path,
        ctx: &egui::Context,
    ) -> Result<()> {
        if url.is_empty() {
            bail!("请先在个性化设置填写下载地址");
        }
        if url.contains('{') || url.contains('}') {
            bail!("主页网址含未知或仅限本地的替换标记；网址仅替换日期、时间和公开版本信息");
        }
        let url = xaml_ui::http_url(url)?.to_string();
        let cache = directory.join("network-cache.json");
        if let Ok(bytes) = read_bounded(&cache, xaml_ui::MAX_DOCUMENT + 8192) {
            if let Ok(cached) = serde_json::from_slice::<Cached>(&bytes) {
                if cached.url == url {
                    if let Ok(nodes) = xaml_ui::parse(&cached.content) {
                        self.home.nodes = Arc::new(nodes);
                        self.home.origin = Origin {
                            directory: None,
                            url: Some(url.clone()),
                        };
                    }
                }
            }
        }
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("pcl-custom-home".into())
            .spawn(move || {
                let result = (|| {
                    let bytes = xaml_ui::fetch_bytes(&url, xaml_ui::MAX_DOCUMENT)?;
                    let content = String::from_utf8(bytes).context("联网主页必须使用 UTF-8")?;
                    let nodes = xaml_ui::parse(&content)?;
                    if let Some(parent) = cache.parent() {
                        fs::create_dir_all(parent)?;
                        let mut file = tempfile::NamedTempFile::new_in(parent)?;
                        serde_json::to_writer(
                            &mut file,
                            &Cached {
                                url: url.clone(),
                                content,
                            },
                        )?;
                        file.as_file().sync_all()?;
                        file.persist(&cache).map_err(|error| error.error)?;
                    }
                    Ok((
                        nodes,
                        Origin {
                            directory: None,
                            url: Some(url),
                        },
                    ))
                })();
                let _ = sender.send(result);
                repaint.request_repaint();
            })?;
        self.home.pending = Some(receiver);
        Ok(())
    }
    fn public_home_values(&self) -> HashMap<String, String> {
        let now = chrono::Local::now();
        HashMap::from([
            ("pcl_version".into(), env!("CARGO_PKG_VERSION").into()),
            ("pcl_version_branch".into(), "Rust 第三方重构".into()),
            ("pcl_build_type".into(), "Rust".into()),
            ("date".into(), now.format("%Y/%-m/%-d").to_string()),
            ("time".into(), now.format("%H:%M:%S").to_string()),
        ])
    }
    pub(super) fn custom_values(&self) -> HashMap<String, String> {
        let mut values = self.public_home_values();
        let selected = self.settings.selected_version.as_deref().unwrap_or("");
        values.insert("name".into(), selected.into());
        values.insert("version".into(), selected.into());
        for (key, value) in &self.home.variables {
            values.insert(format!("variable:{key}"), value.clone());
        }
        if self.home.origin.url.is_none() {
            values.insert(
                "path".into(),
                folder(&self.settings_path).to_string_lossy().into_owned(),
            );
            values.insert(
                "minecraft".into(),
                self.settings.game_root.to_string_lossy().into_owned(),
            );
        }
        for (key, value) in [
            ("UiLauncherTheme", self.settings.ui_theme.to_string()),
            ("UiMusicVolume", self.settings.ui_music_volume.to_string()),
            (
                "UiBackgroundOpacity",
                self.settings.ui_background_opacity.to_string(),
            ),
        ] {
            values.insert(format!("setup:{key}"), value);
        }
        values
    }
    pub(super) fn queue_custom_actions(&mut self, actions: impl IntoIterator<Item = Action>) {
        self.home.actions.extend(actions);
    }
    pub(super) fn custom_content_dialogs(&mut self, ctx: &egui::Context) {
        if !self.home.variables_loaded {
            self.home.variables_loaded = true;
            let path = folder(&self.settings_path).join("variables.json");
            if path.exists() {
                match read_bounded(&path, 64 * 1024).and_then(|bytes| {
                    serde_json::from_slice::<HashMap<String, String>>(&bytes)
                        .context("主页变量配置格式错误")
                }) {
                    Ok(values) => self.home.variables = values,
                    Err(error) => {
                        self.home.message = Some(("主页变量读取失败".into(), format!("{error:#}")))
                    }
                }
            }
        }
        if let Some(receiver) = &self.home.executing {
            match receiver.try_recv() {
                Ok(result) => {
                    self.home.executing = None;
                    match result {
                        Ok(message) => self.status = message,
                        Err(error) => {
                            self.home.message = Some(("事件执行失败".into(), format!("{error:#}")))
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.home.executing = None;
                    self.home.message = Some(("事件执行失败".into(), "后台任务提前结束".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100))
                }
            }
        }
        if self.home.confirmation.is_none()
            && self.home.message.is_none()
            && self.home.executing.is_none()
        {
            if let Some(action) = self.home.actions.pop_front() {
                if matches!(
                    action.kind.as_str(),
                    "打开文件"
                        | "执行命令"
                        | "下载文件"
                        | "修改设置"
                        | "写入设置"
                        | "修改变量"
                        | "写入变量"
                ) {
                    self.home.confirmation = Some(action);
                } else if let Err(error) = self.perform_custom_action(action, ctx) {
                    self.home.message = Some(("事件执行失败".into(), format!("{error:#}")));
                }
            }
        }
        if let Some(action) = self.home.confirmation.clone() {
            let caption = format!(
                "主页请求执行“{}”：\n{}\n\n仅在确认目标及参数后继续。",
                action.kind, action.data
            );
            if let Some(index) = modal_ui::account_modal_with_options(
                ctx,
                "custom-event-confirm",
                "执行主页操作",
                &caption,
                &["执行", "取消"],
                modal_ui::ModalOptions::warning(),
            ) {
                self.home.confirmation = None;
                if index == 0 {
                    if let Err(error) = self.perform_custom_action(action, ctx) {
                        self.home.message = Some(("事件执行失败".into(), format!("{error:#}")));
                    }
                } else {
                    self.home.actions.clear();
                }
            }
        }
        if let Some((title, message)) = self.home.message.clone() {
            if modal_ui::account_modal(ctx, "custom-content-message", &title, &message, &["确定"])
                .is_some()
            {
                self.home.message = None;
            }
        }
    }
    fn perform_custom_action(&mut self, action: Action, ctx: &egui::Context) -> Result<()> {
        let (arg0, arg1) = action.data.split_once('|').unwrap_or((&action.data, ""));
        match action.kind.as_str() {
            "打开网页" | "OpenWebsite" => {
                xaml_ui::http_url(&action.data)?;
                webbrowser::open(&action.data)?;
            }
            "复制文本" => {
                ctx.copy_text(action.data);
                self.status = "已复制文本".into();
            }
            "刷新主页" | "刷新页面" => self.refresh_custom_home(),
            "刷新帮助" => self.refresh_help(),
            "打开帮助" => self.open_help_content(arg0, ctx)?,
            "弹出窗口" => {
                if arg1.is_empty() {
                    bail!("弹出窗口需要 标题|正文");
                }
                self.home.message = Some((arg0.replace("\\n", "\n"), arg1.replace("\\n", "\n")));
            }
            "弹出提示" => self.status = arg0.replace("\\n", "\n"),
            "切换页面" => {
                self.task_view = false;
                self.version_tools = false;
                self.version_view = false;
                self.page = match arg0 {
                    "0" | "Launch" | "启动" => Page::Launch,
                    "1" | "Download" | "下载" => Page::Download,
                    "2" | "Setup" | "设置" => Page::Settings,
                    "3" | "Other" | "更多" => Page::More,
                    _ => bail!("未知或已排除的页面：{arg0}"),
                };
            }
            "启动游戏" => {
                if !arg1.is_empty() {
                    bail!("此主页启动事件带有服务器参数；请在版本设置中保存服务器后再启动");
                }
                if !matches!(arg0, "\\current" | "") {
                    if !self.versions.iter().any(|version| version.id == arg0) {
                        bail!("未找到已安装版本：{arg0}");
                    }
                    self.settings.selected_version = Some(arg0.into());
                }
                self.launch(false);
            }
            "导入整合包" | "安装整合包" => {
                self.open_local_pack_import();
            }
            "修改变量" | "写入变量" => {
                if arg0.is_empty() || arg0.len() > 128 || arg1.len() > 4096 {
                    bail!("变量名称或内容长度无效");
                }
                let path = folder(&self.settings_path).join("variables.json");
                let mut next = self.home.variables.clone();
                next.insert(arg0.into(), arg1.into());
                if serde_json::to_vec(&next)?.len() > 64 * 1024 {
                    bail!("主页变量总量超过 64 KiB，未写入");
                }
                fs::create_dir_all(path.parent().unwrap())?;
                let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
                serde_json::to_writer(&mut file, &next)?;
                file.as_file().sync_all()?;
                file.persist(&path).map_err(|error| error.error)?;
                self.home.variables = next;
                self.status = "主页变量已保存".into();
            }
            "修改设置" | "写入设置" => {
                let mut next = self.settings.clone();
                match arg0 {
                    "UiMusicVolume" => next.ui_music_volume = arg1.parse()?,
                    "UiLauncherTheme" => next.ui_theme = arg1.parse()?,
                    "UiCustomType" => next.ui_custom_type = arg1.parse()?,
                    "UiCustomNet" => next.ui_custom_net = arg1.into(),
                    "UiBackgroundOpacity" => next.ui_background_opacity = arg1.parse()?,
                    _ => bail!("此设置尚未开放给主页事件：{arg0}"),
                };
                config::save_settings(&self.settings_path, &next)?;
                self.settings = next;
                self.status = "主页设置操作已保存".into();
            }
            "下载文件" => {
                let url = xaml_ui::http_url(arg0)?.to_string();
                let suggested = if arg1.is_empty() {
                    "download.bin"
                } else {
                    arg1
                };
                let Some(path) = rfd::FileDialog::new().set_file_name(suggested).save_file() else {
                    return Ok(());
                };
                let (sender, receiver) = mpsc::channel();
                let repaint = ctx.clone();
                std::thread::Builder::new()
                    .name("pcl-page-download".into())
                    .spawn(move || {
                        let result = (|| {
                            let bytes = xaml_ui::fetch_bytes(&url, 64 * 1024 * 1024)?;
                            let parent = path.parent().context("下载目标没有父目录")?;
                            let mut file = tempfile::NamedTempFile::new_in(parent)?;
                            file.write_all(&bytes)?;
                            file.as_file().sync_all()?;
                            file.persist_noclobber(&path)
                                .map_err(|error| error.error)
                                .context("目标文件已存在或无法写入，未覆盖")?;
                            Ok(format!(
                                "下载完成：{}（{} 字节）",
                                path.display(),
                                bytes.len()
                            ))
                        })();
                        let _ = sender.send(result);
                        repaint.request_repaint();
                    })?;
                self.home.executing = Some(receiver);
            }
            "打开文件" | "执行命令" => {
                if arg0.starts_with("http:") || arg0.starts_with("https:") {
                    bail!("请先下载并检查文件；主页不能直接执行联网地址");
                }
                let path = PathBuf::from(arg0);
                let path = if path.is_absolute() {
                    path
                } else {
                    folder(&self.settings_path).join(pcl_core::metadata::safe_relative(arg0)?)
                };
                if !path.exists() {
                    bail!("目标不存在：{}", path.display());
                }
                if action.kind == "打开文件" && arg1.is_empty() {
                    open_file(&path)?;
                    self.status = "已请求系统打开文件".into();
                } else {
                    let arguments = split_arguments(arg1)?;
                    let (sender, receiver) = mpsc::channel();
                    let repaint = ctx.clone();
                    std::thread::Builder::new()
                        .name("pcl-page-command".into())
                        .spawn(move || {
                            let result = std::process::Command::new(&path)
                                .args(arguments)
                                .current_dir(path.parent().unwrap_or(Path::new(".")))
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status()
                                .with_context(|| format!("无法执行 {}", path.display()))
                                .and_then(|status| {
                                    if status.success() {
                                        Ok("主页程序已正常退出".into())
                                    } else {
                                        bail!("主页程序退出状态：{status}")
                                    }
                                });
                            let _ = sender.send(result);
                            repaint.request_repaint();
                        })?;
                    self.home.executing = Some(receiver);
                }
            }
            _ => bail!("未支持或已按用户要求移除的事件：{}", action.kind),
        }
        Ok(())
    }
}
fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("内容文件必须是普通文件");
    }
    if meta.len() > max as u64 {
        bail!("内容文件过大");
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        bail!("内容文件过大");
    }
    Ok(bytes)
}
fn read_local(path: &Path) -> Result<Vec<Node>> {
    xaml_ui::parse(
        &String::from_utf8(read_bounded(path, xaml_ui::MAX_DOCUMENT)?)
            .context("本地主页必须使用 UTF-8")?,
    )
}
fn split_arguments(value: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for ch in value.chars() {
        if matches!(ch, '\'' | '"') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            } else {
                current.push(ch);
            }
            started = true;
        } else if ch.is_whitespace() && quote.is_none() {
            if started {
                result.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            if ch == '\0' {
                bail!("参数包含 NUL");
            }
            current.push(ch);
            started = true;
        }
    }
    if quote.is_some() {
        bail!("参数引号未闭合");
    }
    if started {
        result.push(current);
    }
    Ok(result)
}
fn open_file(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg(path)
            .status()
            .context("系统打开失败")?
            .success()
            .then_some(())
            .context("系统拒绝打开文件")
    }
    #[cfg(target_os = "windows")]
    {
        // Explorer receives one path argument, never a shell expression.
        std::process::Command::new("explorer.exe")
            .arg(path)
            .spawn()
            .context("系统打开失败")?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Explicit read-only smoke against the fixed upstream news-home endpoint"]
    fn live_fixed_news_home_fetches_and_parses_without_actions() {
        let url = PRESETS.iter().find(|(id, _, _)| *id == 2).unwrap().2;
        let bytes = xaml_ui::fetch_bytes(url, xaml_ui::MAX_DOCUMENT).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let nodes = xaml_ui::parse(&text).unwrap();
        assert!(!nodes.is_empty());
        println!("HTTP home smoke: URL={url}; UTF-8 bytes={}; root controls={}; no events clicked or programs run", text.len(), nodes.len());
    }
    #[test]
    fn pending_network_home_cannot_replace_new_blank_selection() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(directory.path());
        app.home.key = Some((2, 0, "https://example.org/old.xaml".into()));
        let (sender, receiver) = mpsc::channel();
        app.home.pending = Some(receiver);
        sender
            .send(Ok((
                xaml_ui::parse("<TextBlock Text='old result'/>").unwrap(),
                Origin {
                    directory: None,
                    url: Some("https://example.org/old.xaml".into()),
                },
            )))
            .unwrap();
        app.settings.ui_custom_type = 0;
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.custom_home_page(ui));
        });
        assert!(app.home.nodes.is_empty());
        assert!(app.home.pending.is_none());
        assert!(app.home.origin.url.is_none());
    }
    #[test]
    fn program_event_stops_at_confirmation_without_starting_worker() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(directory.path());
        app.queue_custom_actions([Action {
            kind: "执行命令".into(),
            data: "never-run-fixture|argument".into(),
        }]);
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.custom_content_dialogs(ctx)
        });
        assert_eq!(app.home.confirmation.as_ref().unwrap().kind, "执行命令");
        assert!(app.home.executing.is_none());
        assert!(app.home.actions.is_empty());
    }
    #[test]
    fn teaching_file_is_parseable_and_never_clobbers_user_home() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let path = generate_tutorial(&settings).unwrap();
        assert!(!read_local(&path).unwrap().is_empty());
        let original = fs::read(&path).unwrap();
        assert!(generate_tutorial(&settings).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }
    #[test]
    fn custom_arguments_keep_shell_metacharacters_literal_and_validate_quotes() {
        assert_eq!(
            split_arguments("\"hello world\" '$HOME;rm' C:\\Games\\Java").unwrap(),
            ["hello world", "$HOME;rm", "C:\\Games\\Java"]
        );
        assert!(split_arguments("\"unclosed").is_err());
    }
}
