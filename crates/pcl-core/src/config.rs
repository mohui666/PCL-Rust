use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Non-secret, per-version settings. Missing values retain global launch choices.
/// Old Rust instances keep their existing data directory; imported PCL versions
/// use the version directory without moving or deleting any user data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstanceSettings {
    pub isolated: bool,
    pub java_path: Option<PathBuf>,
    /// None preserves pre-selection-mode settings (a saved path means Specific).
    pub java_mode: Option<crate::java_selection::JavaSelectionMode>,
    pub java_range: String,
    pub memory_mb: Option<u32>,
    pub memory_auto: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fullscreen: bool,
    pub window_mode: Option<WindowMode>,
    pub custom_info: String,
    pub jvm_arguments: String,
    pub game_arguments: String,
    pub gc_mode: Option<GcMode>,
    pub server: String,
    pub login_requirement: LoginRequirement,
    pub description: String,
    pub favorite: bool,
    pub hidden: bool,
    pub display_icon: String,
    pub display_category: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginRequirement {
    #[default]
    Any,
    Microsoft,
    Offline,
}

impl Default for InstanceSettings {
    fn default() -> Self {
        Self {
            isolated: true,
            java_path: None,
            java_mode: None,
            java_range: String::new(),
            memory_mb: None,
            memory_auto: false,
            width: None,
            height: None,
            fullscreen: false,
            window_mode: None,
            custom_info: String::new(),
            jvm_arguments: String::new(),
            game_arguments: String::new(),
            gc_mode: None,
            server: String::new(),
            login_requirement: LoginRequirement::Any,
            description: String::new(),
            favorite: false,
            hidden: false,
            display_icon: String::new(),
            display_category: String::new(),
        }
    }
}

pub fn instance_settings_path(root: &Path, id: &str) -> Result<PathBuf> {
    crate::metadata::validate_id(id)?;
    crate::metadata::confined_path(
        root,
        &Path::new("versions")
            .join(id)
            .join("PCL-Rust/instance.json"),
    )
}

pub fn load_instance_settings(root: &Path, id: &str) -> Result<InstanceSettings> {
    let path = instance_settings_path(root, id)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstanceSettings::default());
        }
        Err(error) => return Err(error).context("读取版本设置失败"),
    };
    let settings = serde_json::from_slice(&bytes).context("版本设置不是有效的 UTF-8 JSON")?;
    validate_instance_settings(&settings)?;
    Ok(settings)
}

pub fn validate_instance_settings(settings: &InstanceSettings) -> Result<()> {
    if !matches!(
        settings.display_icon.as_str(),
        "" | "block-cobblestone"
            | "block-command"
            | "block-gold"
            | "block-grass"
            | "block-path"
            | "block-forge"
            | "block-redstone"
            | "block-lamp-on"
            | "block-lamp-off"
            | "block-egg"
            | "block-fabric"
            | "block-neoforge"
    ) {
        bail!("版本图标设置无效");
    }
    if !matches!(
        settings.display_category.as_str(),
        "" | "mod" | "normal" | "old" | "april"
    ) {
        bail!("版本分类设置无效");
    }
    if settings
        .java_path
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        bail!("Java 路径必须是绝对路径");
    }
    if settings.java_range.len() > 100 || settings.java_range.contains('\0') {
        bail!("Java 版本区间过长或含无效字符");
    }
    if settings.java_mode == Some(crate::java_selection::JavaSelectionMode::VersionRange) {
        crate::java_selection::JavaRange::parse(&settings.java_range)?;
    }
    if settings
        .memory_mb
        .is_some_and(|memory| !(128..=262_144).contains(&memory))
    {
        bail!("版本内存必须介于 128 和 262144 MB 之间");
    }
    if [settings.width, settings.height]
        .into_iter()
        .flatten()
        .any(|value| !(1..=16_384).contains(&value))
    {
        bail!("游戏窗口宽高必须介于 1 和 16384 之间");
    }
    for (name, value) in [
        ("Java 虚拟机参数", &settings.jvm_arguments),
        ("游戏参数", &settings.game_arguments),
        ("自定义信息", &settings.custom_info),
        ("版本描述", &settings.description),
    ] {
        if value.len() > 16_384 || value.contains('\0') {
            bail!("{name}过长或含无效字符");
        }
    }
    crate::launch::split_legacy_arguments(&settings.jvm_arguments)
        .context("Java 虚拟机参数格式错误")?;
    crate::launch::split_legacy_arguments(&settings.game_arguments).context("游戏参数格式错误")?;
    if !settings.server.is_empty() {
        parse_server_address(&settings.server)?;
    }
    Ok(())
}

pub fn save_instance_settings(root: &Path, id: &str, settings: &InstanceSettings) -> Result<()> {
    validate_instance_settings(settings)?;
    crate::metadata::validate_id(id)?;
    let version = crate::metadata::confined_path(
        root,
        &Path::new("versions").join(id).join(format!("{id}.json")),
    )?;
    if !version.is_file() {
        bail!("版本不存在，未写入设置");
    }
    let path = instance_settings_path(root, id)?;
    save_document(&path, settings)
}

/// Snapshot the global isolation policy for a newly registered game version.
/// Existing preferences (including concurrently created ones) are never replaced.
/// Pack installation deliberately keeps its own isolated data directory.
pub fn initialize_instance_settings(root: &Path, id: &str, policy: IsolationPolicy) -> Result<()> {
    let path = instance_settings_path(root, id)?;
    if path.try_exists()? {
        load_instance_settings(root, id)?;
        return Ok(());
    }
    let version = crate::metadata::resolve_version(root, id)?;
    let settings = InstanceSettings {
        isolated: policy.isolates(
            version_is_modded(&version),
            version["type"].as_str() == Some("release"),
        ),
        ..InstanceSettings::default()
    };
    let parent = path.parent().context("版本设置目录无效")?;
    fs::create_dir_all(parent).context("创建版本设置目录失败")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut staged, &settings)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(&path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            load_instance_settings(root, id).map(|_| ())
        }
        Err(error) => Err(error.error).context("保存新版本隔离设置失败"),
    }
}

fn version_is_modded(version: &Value) -> bool {
    version["libraries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|library| library["name"].as_str())
        .any(|name| {
            [
                "net.minecraftforge:forge:",
                "net.neoforged:neoforge:",
                "net.fabricmc:fabric-loader:",
                "org.quiltmc:quilt-loader:",
                "com.mumfrey:liteloader:",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        })
}

/// Resolve every consumer (launch, local mods, remote files and directory links)
/// through the same isolation setting. Never fall back on malformed settings.
pub fn instance_game_dir(root: &Path, id: &str) -> Result<PathBuf> {
    let settings = load_instance_settings(root, id)?;
    if !settings.isolated {
        return root.canonicalize().context("Minecraft 根目录不存在");
    }
    let old = crate::metadata::confined_path(root, &Path::new("instances").join(id))?;
    if old.is_dir() {
        return Ok(old);
    }
    if old.try_exists()? {
        bail!("已有实例数据路径不是文件夹：{}", old.display());
    }
    crate::metadata::confined_path(root, &Path::new("versions").join(id))
}

#[derive(Clone, Copy, Debug)]
pub struct MemorySnapshot {
    pub total_mb: u64,
    pub available_mb: u64,
}

pub fn memory_snapshot() -> Result<MemorySnapshot> {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let total_mb = system.total_memory() / 1_048_576;
    let available_mb = (system.available_memory() / 1_048_576).min(total_mb);
    if total_mb == 0 {
        bail!("未能读取系统物理内存");
    }
    Ok(MemorySnapshot {
        total_mb,
        available_mb,
    })
}

pub fn automatic_memory_mb(root: &Path, id: &str, memory: MemorySnapshot) -> Result<u32> {
    let version = crate::metadata::resolve_version(root, id)?;
    let libraries = version
        .get("libraries")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let modable = version_is_modded(&version);
    let optifine = libraries
        .iter()
        .filter_map(|library| library["name"].as_str())
        .any(|name| name.to_ascii_lowercase().starts_with("optifine:optifine:"));
    let mut count = 0;
    if modable {
        let directory = instance_game_dir(root, id)?.join("mods");
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.context("读取 Mod 文件夹失败")?;
                    if entry.file_type()?.is_file()
                        && entry
                            .path()
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                ["jar", "zip", "litemod"]
                                    .iter()
                                    .any(|value| extension.eq_ignore_ascii_case(value))
                            })
                    {
                        count += 1;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("读取 Mod 文件夹失败"),
        }
    }
    Ok(auto_memory_from_inputs(
        memory.available_mb,
        modable,
        optifine,
        count,
    ))
}

// Direct port of PageInstanceSetup.GetRam's four allocation bands (GiB).
pub fn auto_memory_from_inputs(
    available_mb: u64,
    modable: bool,
    optifine: bool,
    count: u32,
) -> u32 {
    let count = f64::from(count);
    let (minimum, target1, target2, target3) = if modable {
        (
            0.5 + count / 150.0,
            1.5 + count / 90.0,
            2.7 + count / 50.0,
            4.5 + count / 25.0,
        )
    } else if optifine {
        (0.5, 1.5, 3.0, 5.0)
    } else {
        (0.5, 1.5, 2.5, 4.0)
    };
    let mut available = (available_mb as f64 / 1024.0 * 10.0).round_ties_even() / 10.0;
    let mut allocated: f64 = 0.0;
    for (delta, fraction) in [
        (target1, 1.0),
        (target2 - target1, 0.7),
        (target3 - target2, 0.4),
        (target3, 0.15),
    ] {
        allocated += (available * fraction).min(delta);
        available -= delta / fraction;
        if available < 0.1 {
            break;
        }
    }
    ((allocated.max(minimum) * 10.0).round_ties_even() / 10.0 * 1024.0)
        .round()
        .clamp(512.0, 65_536.0) as u32
}

/// Minecraft accepts DNS names, IPv4 and bracketed IPv6. Return a separately
/// validated port for old clients; never treat this field as command text.
pub fn parse_server_address(server: &str) -> Result<(String, Option<u16>)> {
    if server.is_empty()
        || server.len() > 255
        || server
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '"' | '\''))
    {
        bail!("服务器地址无效，请输入主机名或 IP，可附加 :端口");
    }
    let (host, port) = if server.starts_with('[') {
        let end = server.find(']').context("IPv6 地址缺少右方括号")?;
        let host = &server[1..end];
        host.parse::<std::net::Ipv6Addr>()
            .context("IPv6 地址无效")?;
        let rest = &server[end + 1..];
        let port = if rest.is_empty() {
            None
        } else {
            Some(rest.strip_prefix(':').context("服务器端口格式无效")?)
        };
        (host, port)
    } else {
        let mut parts = server.split(':');
        let host = parts.next().unwrap_or_default();
        let port = parts.next();
        if parts.next().is_some() {
            bail!("IPv6 地址请使用 [地址]:端口 格式");
        }
        if host.is_empty()
            || !host
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            bail!("服务器主机名无效");
        }
        (host, port)
    };
    let port = port
        .map(|port| {
            port.parse::<u16>()
                .context("服务器端口必须介于 1 和 65535 之间")
        })
        .transpose()?;
    if port == Some(0) {
        bail!("服务器端口必须介于 1 和 65535 之间");
    }
    Ok((host.into(), port))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    #[default]
    Default,
    Fullscreen,
    LauncherSize,
    Custom,
    Maximized,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationPolicy {
    Off,
    Modded,
    NonRelease,
    ModdedOrNonRelease,
    #[default]
    All,
}

impl IsolationPolicy {
    pub fn isolates(self, modded: bool, release: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Modded => modded,
            Self::NonRelease => !release,
            Self::ModdedOrNonRelease => modded || !release,
            Self::All => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcMode {
    PreferZgc,
    PreferGenerationalZgc,
    G1,
    TunedG1,
    #[default]
    Custom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub game_root: PathBuf,
    pub game_roots: Vec<PathBuf>,
    pub ui_background_folder: Option<PathBuf>,
    pub ui_background_colorful: bool,
    pub ui_theme: u8,
    pub ui_theme_hue: f32,
    pub ui_theme_saturation: f32,
    /// Original custom slider value (0..40), with neutral brightness at 20.
    pub ui_theme_lightness: f32,
    pub ui_theme_gradient: f32,
    pub java_path: Option<PathBuf>,
    pub java_priority: Vec<PathBuf>,
    pub java_excluded: Vec<PathBuf>,
    pub memory_mb: u32,
    pub memory_auto: bool,
    pub custom_info: String,
    pub jvm_arguments: String,
    pub game_arguments: String,
    pub window_mode: WindowMode,
    pub width: u32,
    pub height: u32,
    pub default_isolation: IsolationPolicy,
    pub gc_mode: GcMode,
    pub offline_name: String,
    pub offline_history: Vec<String>,
    pub selected_version: Option<String>,
    pub microsoft_client_id: String,
    pub show_snapshots: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            game_root: dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("pcl-rust")
                .join("game"),
            game_roots: Vec::new(),
            ui_background_folder: None,
            ui_background_colorful: true,
            ui_theme: 0,
            ui_theme_hue: 180.0,
            ui_theme_saturation: 80.0,
            ui_theme_lightness: 20.0,
            ui_theme_gradient: 90.0,
            java_path: None,
            java_priority: Vec::new(),
            java_excluded: Vec::new(),
            memory_mb: 4096,
            memory_auto: false,
            custom_info: String::new(),
            jvm_arguments: String::new(),
            game_arguments: String::new(),
            window_mode: WindowMode::Default,
            width: 854,
            height: 480,
            default_isolation: IsolationPolicy::All,
            gc_mode: GcMode::Custom,
            offline_name: "Player".into(),
            offline_history: vec!["Player".into()],
            selected_version: None,
            microsoft_client_id: String::new(),
            show_snapshots: false,
        }
    }
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("pcl-rust")
        .join("settings.json")
}

pub fn validate_settings(settings: &Settings) -> Result<()> {
    if settings.ui_theme > 14 {
        bail!("主题编号必须介于 0 和 14 之间");
    }
    for (name, value, maximum) in [
        ("主题色调", settings.ui_theme_hue, 360.0),
        ("主题饱和度", settings.ui_theme_saturation, 100.0),
        ("主题亮度", settings.ui_theme_lightness, 40.0),
        ("主题色调渐变", settings.ui_theme_gradient, 180.0),
    ] {
        if !value.is_finite() || !(0.0..=maximum).contains(&value) {
            bail!("{name}必须介于 0 和 {maximum} 之间");
        }
    }
    if !settings.game_root.is_absolute() {
        bail!("游戏目录必须是绝对路径");
    }
    if settings.game_roots.iter().any(|path| !path.is_absolute()) {
        bail!("已保存的游戏目录必须是绝对路径");
    }
    if settings
        .ui_background_folder
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        bail!("背景图片文件夹必须是绝对路径");
    }
    if !(512..=65_536).contains(&settings.memory_mb) {
        bail!("内存必须介于 512 和 65536 MB 之间");
    }
    if let Some(path) = &settings.java_path {
        if !path.is_absolute() {
            bail!("手动指定的 Java 路径必须是绝对路径");
        }
    }
    if settings
        .java_priority
        .iter()
        .chain(&settings.java_excluded)
        .any(|path| !path.is_absolute())
    {
        bail!("Java 优先列表和排除列表必须使用绝对路径");
    }
    validate_instance_settings(&InstanceSettings {
        custom_info: settings.custom_info.clone(),
        jvm_arguments: settings.jvm_arguments.clone(),
        game_arguments: settings.game_arguments.clone(),
        width: Some(settings.width),
        height: Some(settings.height),
        ..InstanceSettings::default()
    })?;
    Ok(())
}

pub fn load_settings(path: &Path) -> Result<Settings> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(error).context("读取设置失败"),
    };
    let settings: Settings = serde_json::from_slice(&bytes).context("设置不是有效的 UTF-8 JSON")?;
    validate_settings(&settings)?;
    Ok(settings)
}

/// Validate before touching disk, merge unknown keys, then atomically replace
/// the destination with a flushed temporary file in the same directory.
pub fn save_settings(path: &Path, settings: &Settings) -> Result<()> {
    validate_settings(settings)?;
    save_document(path, settings)
}

fn save_document(path: &Path, settings: &impl Serialize) -> Result<()> {
    let mut document = match fs::read(path) {
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).context("原设置文件损坏，未覆盖原文件")?;
            value
                .as_object()
                .cloned()
                .context("原设置文件不是 JSON 对象，未覆盖原文件")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => return Err(error).context("读取原设置失败，未覆盖原文件"),
    };
    let current = serde_json::to_value(settings).context("无法编码设置")?;
    document.extend(current.as_object().context("设置必须是 JSON 对象")?.clone());
    let mut bytes = serde_json::to_vec_pretty(&document).context("无法编码设置")?;
    bytes.push(b'\n');

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("创建设置目录失败")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("创建临时设置文件失败")?;
    temporary.write_all(&bytes).context("写入设置失败")?;
    temporary.as_file().sync_all().context("刷新设置文件失败")?;
    temporary.persist(path).context("原子替换设置文件失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_settings_keep_old_defaults_and_round_trip_custom_values() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        fs::write(&path, br#"{"future":{"keep":true}}"#).unwrap();
        let mut settings = load_settings(&path).unwrap();
        assert_eq!(
            (
                settings.ui_theme,
                settings.ui_theme_hue,
                settings.ui_theme_saturation,
                settings.ui_theme_lightness,
                settings.ui_theme_gradient
            ),
            (0, 180.0, 80.0, 20.0, 90.0)
        );
        settings.ui_theme = 14;
        settings.ui_theme_hue = 360.0;
        settings.ui_theme_saturation = 63.5;
        settings.ui_theme_lightness = 8.0;
        settings.ui_theme_gradient = 180.0;
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path).unwrap(), settings);
        let saved = fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(value["future"]["keep"], true);
        for (hue, saturation, lightness, gradient) in [
            (f32::NAN, 80.0, 20.0, 90.0),
            (180.0, f32::INFINITY, 20.0, 90.0),
            (361.0, 80.0, 20.0, 90.0),
            (180.0, -1.0, 20.0, 90.0),
            (180.0, 80.0, 41.0, 90.0),
            (180.0, 80.0, 20.0, 181.0),
        ] {
            let invalid = Settings {
                ui_theme_hue: hue,
                ui_theme_saturation: saturation,
                ui_theme_lightness: lightness,
                ui_theme_gradient: gradient,
                ..settings.clone()
            };
            assert!(save_settings(&path, &invalid).is_err());
            assert_eq!(fs::read(&path).unwrap(), saved);
        }
        settings.ui_theme = 15;
        assert!(save_settings(&path, &settings).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
    }

    #[test]
    fn new_version_isolation_policy_is_snapshotted_without_replacing_preferences() {
        let root = tempfile::tempdir().unwrap();
        for (id, version_type, modded, expected) in [
            ("release", "release", false, false),
            ("snapshot", "snapshot", false, true),
            ("fabric", "release", true, true),
        ] {
            let folder = root.path().join("versions").join(id);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(format!("{id}.json")),serde_json::json!({
                "id":id,"type":version_type,"mainClass":"Main", "libraries": if modded {vec![serde_json::json!({"name":"net.fabricmc:fabric-loader:0.19.5"})]} else {vec![]}
            }).to_string()).unwrap();
            initialize_instance_settings(root.path(), id, IsolationPolicy::ModdedOrNonRelease)
                .unwrap();
            assert_eq!(
                load_instance_settings(root.path(), id).unwrap().isolated,
                expected
            );
            let before = fs::read(instance_settings_path(root.path(), id).unwrap()).unwrap();
            initialize_instance_settings(root.path(), id, IsolationPolicy::Off).unwrap();
            assert_eq!(
                fs::read(instance_settings_path(root.path(), id).unwrap()).unwrap(),
                before
            );
        }
        assert!(!IsolationPolicy::Off.isolates(true, false));
        assert!(IsolationPolicy::All.isolates(false, true));
        assert!(IsolationPolicy::Modded.isolates(true, true));
        assert!(!IsolationPolicy::NonRelease.isolates(true, true));
    }

    #[test]
    fn automatic_memory_matches_original_allocation_bands() {
        assert_eq!(auto_memory_from_inputs(0, false, false, 0), 512);
        assert_eq!(auto_memory_from_inputs(1024, false, false, 0), 1024);
        assert_eq!(auto_memory_from_inputs(2048, false, false, 0), 1843);
        assert_eq!(auto_memory_from_inputs(65_536, false, false, 0), 8192);
        assert!(
            auto_memory_from_inputs(16_384, true, false, 150)
                > auto_memory_from_inputs(16_384, false, false, 0)
        );
        assert!(
            auto_memory_from_inputs(8192, false, true, 0)
                > auto_memory_from_inputs(8192, false, false, 0)
        );
    }

    #[test]
    fn instance_settings_preserve_unknown_keys_and_reject_corruption() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("versions/test")).unwrap();
        fs::write(directory.path().join("versions/test/test.json"), b"{}").unwrap();
        let path = instance_settings_path(directory.path(), "test").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"future":{"x":42}}"#).unwrap();
        let mut settings = load_instance_settings(directory.path(), "test").unwrap();
        settings.memory_mb = Some(3072);
        settings.java_mode = Some(crate::java_selection::JavaSelectionMode::VersionRange);
        settings.java_range = "[8.0.81,8.0.141]".into();
        save_instance_settings(directory.path(), "test", &settings).unwrap();
        let original = fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(value["future"]["x"], 42);
        assert_eq!(
            load_instance_settings(directory.path(), "test")
                .unwrap()
                .java_range,
            "[8.0.81,8.0.141]"
        );
        settings.java_range = "(21,17)".into();
        assert!(save_instance_settings(directory.path(), "test", &settings).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        settings.java_range = "[8.0.81,8.0.141]".into();
        settings.jvm_arguments = "\"unclosed".into();
        assert!(save_instance_settings(directory.path(), "test", &settings).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::write(&path, b"bad json").unwrap();
        assert!(instance_game_dir(directory.path(), "test").is_err());
        assert!(
            save_instance_settings(directory.path(), "test", &InstanceSettings::default()).is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"bad json");
        assert!(
            save_instance_settings(directory.path(), "missing", &InstanceSettings::default())
                .is_err()
        );
        assert!(instance_settings_path(directory.path(), "../outside").is_err());
    }

    #[test]
    fn server_address_parsing_preserves_ipv6_and_rejects_urls_and_bad_ports() {
        assert_eq!(
            parse_server_address("[2001:db8::1]:25565").unwrap(),
            ("2001:db8::1".into(), Some(25565))
        );
        assert_eq!(
            parse_server_address("localhost").unwrap(),
            ("localhost".into(), None)
        );
        for value in [
            "https://example.com",
            "x:0",
            "x:65536",
            "x:",
            "x --demo",
            "::1",
            "[x]:2",
            "a/b",
            "a\\b",
            "a\0b",
        ] {
            assert!(parse_server_address(value).is_err(), "{value:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn instance_settings_reject_symlink_escape_without_touching_target() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("versions/test")).unwrap();
        fs::write(root.path().join("versions/test/test.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("versions/test/PCL-Rust"))
            .unwrap();
        assert!(save_instance_settings(root.path(), "test", &InstanceSettings::default()).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn invalid_config_does_not_overwrite_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        save_settings(&path, &settings).unwrap();
        let original = fs::read(&path).unwrap();
        settings.memory_mb = 128;
        assert!(save_settings(&path, &settings).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        settings.memory_mb = 4096;
        settings.game_root = PathBuf::from("relative/game");
        assert!(save_settings(&path, &settings).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn corrupt_config_is_never_silently_reset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{corrupt").unwrap();
        assert!(load_settings(&path).is_err());
        assert!(save_settings(&path, &Settings::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{corrupt");
    }

    #[test]
    fn save_preserves_unknown_fields_and_round_trips_unicode_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, br#"{"future_option":{"enabled":true}}"#).unwrap();
        let mut settings = load_settings(&path).unwrap();
        settings.game_root = directory.path().join("我的世界");
        save_settings(&path, &settings).unwrap();
        let loaded = load_settings(&path).unwrap();
        assert_eq!(loaded.game_root, settings.game_root);
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["future_option"]["enabled"], true);
        assert!(value.get("access_token").is_none());
    }

    #[test]
    fn load_rejects_invalid_memory_and_defaults_missing_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, br#"{"memory_mb":65537}"#).unwrap();
        assert!(load_settings(&path).is_err());
        fs::write(&path, b"{}").unwrap();
        let settings = load_settings(&path).unwrap();
        assert_eq!(settings.memory_mb, 4096);
        assert_eq!(settings.offline_name, "Player");
        assert!(settings.game_root.is_absolute());
    }
}
