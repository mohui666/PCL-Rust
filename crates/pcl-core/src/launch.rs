use crate::metadata::{
    confined_path, library_artifacts, resolve_version, rules_allow, safe_relative, validate_id,
};
use crate::model::{Platform, Session};
use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub root: PathBuf,
    pub version_id: String,
    pub java: PathBuf,
    pub memory_mb: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct PreLaunchCommand {
    pub label: &'static str,
    pub text: String,
    pub wait: bool,
}

#[derive(Clone, Default)]
pub struct GameWindowOptions {
    pub title: String,
    pub maximize: bool,
}

pub struct LaunchBehavior {
    pub visibility: crate::config::LauncherVisibility,
    pub priority: crate::config::ProcessPriority,
    pub window: GameWindowOptions,
    pub memory_optimize: bool,
    pub auto_chinese: bool,
    pub language_code: String,
    pub high_performance_gpu: bool,
    pub offline_skin: Option<crate::offline_skin::SkinUpdate>,
    pub warnings: Vec<String>,
    pub commands: Vec<PreLaunchCommand>,
    pub command_cwd: PathBuf,
    /// User path values are environment data, never injected as shell syntax.
    pub variables: Vec<(String, String)>,
}
impl LaunchBehavior {
    pub fn environment(&self) -> Vec<(String, String)> {
        self.variables
            .iter()
            .enumerate()
            .map(|(i, (_, value))| (format!("PCL_LAUNCH_{i}"), value.clone()))
            .collect()
    }

    /// Preserve the user's shell program while expanding supported PCL markers
    /// via quoted environment references. Path punctuation cannot become code.
    /// Windows uses cmd /D /V:ON; Unix uses /bin/sh.
    pub fn shell_text(&self, text: &str, windows: bool) -> String {
        let mut output = String::new();
        let mut quote = None;
        let mut offset = 0;
        while offset < text.len() {
            let rest = &text[offset..];
            let ch = rest.chars().next().unwrap();
            if (ch == '\\' && !windows && quote != Some('\''))
                || (ch == '^' && windows && quote != Some('"'))
            {
                output.push(ch);
                offset += ch.len_utf8();
                if let Some(next) = text[offset..].chars().next() {
                    output.push(next);
                    offset += next.len_utf8();
                }
                continue;
            }
            if let Some((index, (marker, _))) = self
                .variables
                .iter()
                .enumerate()
                .find(|(_, (marker, _))| rest.starts_with(marker))
            {
                let reference = if windows {
                    format!("!PCL_LAUNCH_{index}!")
                } else {
                    format!("${{PCL_LAUNCH_{index}}}")
                };
                match quote {
                    Some('"') => output.push_str(&reference),
                    Some('\'') if !windows => output.push_str(&format!("'\"{reference}\"'")),
                    _ => output.push_str(&format!("\"{reference}\"")),
                }
                offset += marker.len();
                continue;
            }
            if ch == '"' && quote != Some('\'') {
                quote = if quote == Some('"') { None } else { Some('"') };
            } else if ch == '\'' && !windows && quote != Some('"') {
                quote = if quote == Some('\'') {
                    None
                } else {
                    Some('\'')
                };
            }
            output.push(ch);
            offset += ch.len_utf8();
        }
        output
    }
}

pub struct LaunchPlan {
    pub java: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub behavior: LaunchBehavior,
    secrets: Vec<String>,
}

impl std::fmt::Debug for LaunchPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchPlan")
            .field("command", &self.redacted_command())
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl LaunchPlan {
    pub(crate) fn sanitized_pre_launch(
        &self,
    ) -> (Vec<PreLaunchCommand>, Vec<(String, String)>, bool) {
        let mut removed = false;
        let mut sanitize = |text: &str| {
            let mut value = text.to_owned();
            for secret in &self.secrets {
                if !secret.is_empty() && secret != "0" && value.contains(secret) {
                    value = value.replace(secret, "F");
                    removed = true;
                }
            }
            value
        };
        let commands = self
            .behavior
            .commands
            .iter()
            .map(|c| PreLaunchCommand {
                label: c.label,
                text: sanitize(&c.text),
                wait: c.wait,
            })
            .collect();
        let variables = self
            .behavior
            .environment()
            .into_iter()
            .map(|(k, v)| (k, sanitize(&v)))
            .collect();
        (commands, variables, removed)
    }

    /// Export only inert credential placeholders, even when a token appears in
    /// a custom argument or an inline `--accessToken=value` form.
    pub(crate) fn sanitized_arguments(&self) -> (Vec<String>, bool) {
        self.sanitize_arguments_with("F")
    }

    fn sanitize_arguments_with(&self, replacement: &str) -> (Vec<String>, bool) {
        let mut removed = false;
        let mut credential_flag: Option<&str> = None;
        let mut arguments = Vec::with_capacity(self.args.len());
        for argument in &self.args {
            let mut safe = argument.clone();
            if let Some(flag) = credential_flag {
                if !is_offline_credential(flag, &safe) {
                    safe = replacement.into();
                    removed = true;
                }
            }
            for flag in ["--accessToken", "--session", "--clientToken"] {
                if let Some(value) = safe.strip_prefix(&format!("{flag}=")) {
                    if !is_offline_credential(flag, value) {
                        safe = format!("{flag}={replacement}");
                        removed = true;
                    }
                }
            }
            for secret in &self.secrets {
                if !secret.is_empty() && secret != "0" && safe.contains(secret) {
                    safe = safe.replace(secret, replacement);
                    removed = true;
                }
            }
            credential_flag = matches!(
                argument.as_str(),
                "--accessToken" | "--session" | "--clientToken"
            )
            .then_some(argument.as_str());
            arguments.push(safe);
        }
        (arguments, removed)
    }

    /// A display string only: execute with Command::args, never via a shell.
    pub fn redacted_command(&self) -> String {
        let quote = |value: &str| {
            if value.is_empty() {
                return "\"\"".to_owned();
            }
            if value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_/.:=+".contains(c))
            {
                value.to_owned()
            } else {
                format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
            }
        };
        let mut parts = vec![quote(&self.java.to_string_lossy())];
        for argument in self.sanitize_arguments_with("<redacted>").0 {
            parts.push(quote(&argument));
        }
        parts.join(" ")
    }
}

fn is_offline_credential(flag: &str, value: &str) -> bool {
    value.is_empty()
        || value == "0"
        || (flag == "--session"
            && value.strip_prefix("token:0:").is_some_and(|uuid| {
                uuid.len() == 32 && uuid.bytes().all(|byte| byte.is_ascii_hexdigit())
            }))
}

pub fn build_plan(
    options: &LaunchOptions,
    session: &Session,
    platform: &Platform,
) -> Result<LaunchPlan> {
    validate_id(&options.version_id)?;
    let instance = crate::config::load_instance_settings(&options.root, &options.version_id)?;
    build_plan_with_instance(options, session, platform, instance, None)
}

/// Build with a caller-selected Java path, preserving all other instance options.
/// The caller must inspect this executable before constructing the plan.
pub fn build_plan_with_explicit_java(
    options: &LaunchOptions,
    session: &Session,
    platform: &Platform,
) -> Result<LaunchPlan> {
    validate_id(&options.version_id)?;
    let mut instance = crate::config::load_instance_settings(&options.root, &options.version_id)?;
    instance.java_path = None;
    build_plan_with_instance(options, session, platform, instance, None)
}

/// Desktop entry point. Apply global choices first; explicit version choices
/// take precedence, except Java which has already been selected and inspected.
/// `java_major` must describe the authoritative executable in `options`.
pub fn build_plan_with_settings(
    options: &LaunchOptions,
    session: &Session,
    platform: &Platform,
    settings: &crate::config::Settings,
    java_major: u32,
) -> Result<LaunchPlan> {
    build_plan_with_settings_and_viewport(options, session, platform, settings, java_major, None)
}

pub fn build_plan_with_settings_and_viewport(
    options: &LaunchOptions,
    session: &Session,
    platform: &Platform,
    settings: &crate::config::Settings,
    java_major: u32,
    launcher_size: Option<(u32, u32)>,
) -> Result<LaunchPlan> {
    build_plan_with_overrides(
        options,
        session,
        platform,
        settings,
        java_major,
        LaunchOverrides {
            launcher_size,
            server: None,
            memory_mb: None,
        },
    )
}
#[derive(Default)]
pub struct LaunchOverrides<'a> {
    pub launcher_size: Option<(u32, u32)>,
    pub server: Option<&'a str>,
    /// Explicit CLI memory takes precedence without changing persisted settings.
    pub memory_mb: Option<u32>,
}
pub fn build_plan_with_overrides(
    options: &LaunchOptions,
    session: &Session,
    platform: &Platform,
    settings: &crate::config::Settings,
    java_major: u32,
    overrides: LaunchOverrides<'_>,
) -> Result<LaunchPlan> {
    let launcher_size = overrides.launcher_size;
    crate::config::validate_settings(settings)?;
    let mut instance = crate::config::load_instance_settings(&options.root, &options.version_id)?;
    if let Some(server) = overrides.server {
        crate::config::parse_server_address(server)?;
        instance.server = server.into();
    }
    // Selection already resolved the instance's Java mode. Do not replace the
    // inspected executable if the saved path changed while selection was running.
    instance.java_path = None;
    if instance.game_window_title.is_empty() {
        instance
            .game_window_title
            .clone_from(&settings.game_window_title);
    }
    instance.memory_optimize = Some(instance.memory_optimize.unwrap_or(settings.memory_optimize));
    instance.disable_java_wrapper |= settings.disable_java_wrapper;
    instance.disable_lwjgl_unsafe_agent |= settings.disable_lwjgl_unsafe_agent;
    let mut options = options.clone();
    options.memory_mb = settings.memory_mb;
    options.width = 854;
    options.height = 480;
    if !instance.memory_auto && instance.memory_mb.is_none() && settings.memory_auto {
        instance.memory_auto = true;
    }
    if let Some(memory) = overrides.memory_mb {
        anyhow::ensure!(
            (256..=262_144).contains(&memory),
            "内存必须为 256–262144 MiB"
        );
        instance.memory_auto = false;
        instance.memory_mb = Some(memory);
    }
    if instance.custom_info.is_empty() {
        instance.custom_info.clone_from(&settings.custom_info);
    }
    if instance.jvm_arguments.is_empty() {
        instance.jvm_arguments.clone_from(&settings.jvm_arguments);
    }
    if instance.game_arguments.is_empty() {
        instance.game_arguments.clone_from(&settings.game_arguments);
    }
    let window_mode = instance.window_mode.unwrap_or_else(|| {
        if instance.fullscreen {
            crate::config::WindowMode::Fullscreen
        } else if instance.width.is_some() || instance.height.is_some() {
            crate::config::WindowMode::Custom
        } else {
            settings.window_mode
        }
    });
    instance.fullscreen = window_mode == crate::config::WindowMode::Fullscreen;
    match window_mode {
        crate::config::WindowMode::Default => {}
        crate::config::WindowMode::Fullscreen => instance.fullscreen = true,
        crate::config::WindowMode::Custom => {
            options.width = settings.width.max(100);
            options.height = settings.height.max(100);
        }
        crate::config::WindowMode::LauncherSize => {
            let (width, height) =
                launcher_size.context("跟随窗口尺寸需要当前启动器窗口，命令行请使用自定义尺寸")?;
            anyhow::ensure!(
                (100..=16_384).contains(&width) && (100..=16_384).contains(&height),
                "启动器窗口尺寸超出有效范围"
            );
            options.width = width;
            options.height = height;
            instance.width = None;
            instance.height = None;
            instance.window_mode = Some(crate::config::WindowMode::Custom);
        }
        crate::config::WindowMode::Maximized => {
            instance.window_mode = Some(crate::config::WindowMode::Maximized);
        }
    }
    let gc = instance.gc_mode.unwrap_or(settings.gc_mode);
    let mut plan = build_plan_with_instance(
        &options,
        session,
        platform,
        instance,
        Some((gc, java_major)),
    )?;
    plan.behavior.visibility = settings.launcher_visibility;
    plan.behavior.priority = settings.process_priority;
    plan.behavior.high_performance_gpu = settings.prefer_high_performance_gpu;
    plan.behavior.auto_chinese = settings.system.auto_chinese;
    if !settings.pre_launch_command.trim().is_empty() {
        plan.behavior.commands.insert(
            0,
            PreLaunchCommand {
                label: "全局",
                text: settings.pre_launch_command.clone(),
                wait: settings.pre_launch_wait,
            },
        );
    }
    Ok(plan)
}

fn build_plan_with_instance(
    options: &LaunchOptions,
    session: &Session,
    platform: &Platform,
    mut instance: crate::config::InstanceSettings,
    gc: Option<(crate::config::GcMode, u32)>,
) -> Result<LaunchPlan> {
    validate_id(&options.version_id)?;
    let mut options = options.clone();
    if let Some(mode) = instance.window_mode {
        match mode {
            crate::config::WindowMode::Default | crate::config::WindowMode::Custom => {
                instance.fullscreen = false
            }
            crate::config::WindowMode::Fullscreen => instance.fullscreen = true,
            crate::config::WindowMode::LauncherSize => {
                bail!("跟随窗口尺寸需要当前启动器窗口");
            }
            crate::config::WindowMode::Maximized => {}
        }
    }
    if crate::java_selection::effective_mode(instance.java_mode, instance.java_path.as_deref())
        == crate::java_selection::JavaSelectionMode::Specific
    {
        if let Some(java) = &instance.java_path {
            options.java = java.clone();
        }
    }
    if instance.memory_auto {
        options.memory_mb = crate::config::automatic_memory_mb(
            &options.root,
            &options.version_id,
            crate::config::memory_snapshot()?,
        )?;
    } else if let Some(memory) = instance.memory_mb {
        options.memory_mb = memory;
    }
    if let Some(width) = instance.width {
        options.width = width;
    }
    if let Some(height) = instance.height {
        options.height = height;
    }
    match instance.login_requirement {
        crate::config::LoginRequirement::Microsoft if session.user_type != "msa" => {
            bail!("此版本仅允许正版登录，请先登录微软账号")
        }
        crate::config::LoginRequirement::Offline if session.user_type != "legacy" => {
            bail!("此版本仅允许离线登录，请切换为离线账号")
        }
        _ => {}
    }
    if !(128..=262_144).contains(&options.memory_mb) {
        bail!("游戏内存必须介于 128 和 262144 MiB 之间");
    }
    if options.width == 0
        || options.height == 0
        || options.width > 16_384
        || options.height > 16_384
    {
        bail!("游戏窗口宽高必须介于 1 和 16384 之间");
    }
    if session.username.is_empty() || session.uuid.is_empty() {
        bail!("请先选择有效的游戏账号");
    }
    if !matches!(platform.os.as_str(), "windows" | "linux" | "osx") {
        bail!("不支持的平台：{}", platform.os);
    }
    let root = options
        .root
        .canonicalize()
        .context("Minecraft 根目录不存在")?;
    let java = options
        .java
        .canonicalize()
        .with_context(|| format!("Java 不存在：{}", options.java.display()))?;
    require_file(&java, "Java 可执行文件")?;
    let version = resolve_version(&root, &options.version_id)?;
    let main_class = version
        .get("mainClass")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .context("版本缺少 mainClass")?;
    let jar_id = version
        .get("_pcl_jar_id")
        .and_then(Value::as_str)
        .unwrap_or(&options.version_id);
    validate_id(jar_id)?;
    let main_jar = confined_path(
        &root,
        &safe_relative(&format!("versions/{jar_id}/{jar_id}.jar"))?,
    )?;
    let artifacts = library_artifacts(&version, platform)?;
    let mut classpath = Vec::new();
    let mut missing = Vec::new();
    for artifact in &artifacts {
        let path = confined_path(&root, &artifact.relative_path)?;
        if !path.is_file() {
            missing.push(path.display().to_string());
        }
        if !artifact.native {
            classpath.push(path);
        }
    }
    if !main_jar.is_file() {
        missing.push(main_jar.display().to_string());
    }
    classpath.push(main_jar.clone());
    let natives = confined_path(
        &root,
        &safe_relative(&format!("versions/{}/natives", options.version_id))?,
    )?;
    if artifacts.iter().any(|artifact| artifact.native) && !natives.is_dir() {
        missing.push(natives.display().to_string());
    }
    if !missing.is_empty() {
        bail!(
            "启动文件尚未就绪，请先安装或修复此版本：\n{}",
            missing
                .iter()
                .take(12)
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let cwd = crate::config::instance_game_dir(&root, &options.version_id)?;
    let assets_root = confined_path(&root, Path::new("assets"))?;
    let assets_id = version
        .pointer("/assetIndex/id")
        .and_then(Value::as_str)
        .or_else(|| version.get("assets").and_then(Value::as_str))
        .unwrap_or("legacy");
    validate_id(assets_id)?;
    let mut game_assets = assets_root.clone();
    if version.get("assetIndex").is_some() || version.get("assets").is_some() {
        let index_path = confined_path(
            &root,
            &safe_relative(&format!("assets/indexes/{assets_id}.json"))?,
        )?;
        let index_text = fs::read_to_string(&index_path).with_context(|| {
            format!("缺少资源索引，请先安装或修复版本：{}", index_path.display())
        })?;
        let index: Value = serde_json::from_str(&index_text).context("资源索引 JSON 无效")?;
        if index
            .get("map_to_resources")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            crate::install::prepare_mapped_resources(
                &root,
                &cwd,
                &index,
                &std::sync::atomic::AtomicBool::new(false),
            )?;
            game_assets = confined_path(&root, &cwd.strip_prefix(&root)?.join("resources"))?;
        } else if index
            .get("virtual")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            game_assets = confined_path(
                &root,
                &safe_relative(&format!("assets/virtual/{assets_id}"))?,
            )?;
        }
    }
    let separator = if platform.os == "windows" { ";" } else { ":" };
    let classpath = classpath
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(separator);
    let libraries = confined_path(&root, Path::new("libraries"))?;
    let mut replacements: HashMap<&str, String> = HashMap::from([
        ("auth_player_name", session.username.clone()),
        ("auth_uuid", session.uuid.clone()),
        ("auth_access_token", session.access_token.clone()),
        (
            "auth_session",
            format!("token:{}:{}", session.access_token, session.uuid),
        ),
        ("user_type", session.user_type.clone()),
        ("user_properties", "{}".into()),
        ("profile_properties", "{}".into()),
        ("version_name", options.version_id.clone()),
        (
            "version_type",
            if !instance.custom_info.is_empty() {
                instance.custom_info.clone()
            } else {
                version
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("custom")
                    .to_owned()
            },
        ),
        ("game_directory", cwd.to_string_lossy().into_owned()),
        ("assets_root", assets_root.to_string_lossy().into_owned()),
        ("game_assets", game_assets.to_string_lossy().into_owned()),
        ("assets_index_name", assets_id.to_owned()),
        ("natives_directory", natives.to_string_lossy().into_owned()),
        (
            "library_directory",
            libraries.to_string_lossy().into_owned(),
        ),
        ("classpath", classpath),
        ("classpath_separator", separator.into()),
        ("primary_jar", main_jar.to_string_lossy().into_owned()),
        ("launcher_name", "PCL Rust Third-Party".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
        ("resolution_width", options.width.to_string()),
        ("resolution_height", options.height.to_string()),
        // Minecraft accepts these optional identity hints empty; authentication uses the access token.
        ("clientid", String::new()),
        ("auth_xuid", String::new()),
        ("quickPlayMultiplayer", instance.server.clone()),
        ("quickPlaySingleplayer", String::new()),
        ("quickPlayRealms", String::new()),
    ]);
    // Keep the offline sentinel out of substring replacement: it is not a credential.
    let secrets = if session.access_token.is_empty() || session.access_token == "0" {
        Vec::new()
    } else {
        vec![session.access_token.clone()]
    };
    let features = HashMap::from([
        ("has_custom_resolution".into(), true),
        ("is_demo_user".into(), session.user_type == "demo"),
        (
            "is_quick_play_multiplayer".into(),
            !instance.server.is_empty(),
        ),
    ]);
    let jvm_json = version.pointer("/arguments/jvm");
    let mut jvm = if let Some(arguments) = jvm_json {
        expand_arguments(arguments, platform, &features)?
    } else {
        vec![
            "-Djava.library.path=${natives_directory}".into(),
            "-cp".into(),
            "${classpath}".into(),
        ]
    };
    // Some loader metadata omits vanilla JVM arguments entirely.
    if !jvm.iter().any(|arg| {
        matches!(arg.as_str(), "-cp" | "-classpath" | "--class-path")
            || arg.starts_with("--class-path=")
    }) {
        jvm.extend(["-cp".into(), "${classpath}".into()]);
    }
    if !jvm
        .iter()
        .any(|arg| arg.starts_with("-Djava.library.path="))
    {
        jvm.push("-Djava.library.path=${natives_directory}".into());
    }
    if platform.os == "osx" && !jvm.iter().any(|arg| arg == "-XstartOnFirstThread") {
        jvm.push("-XstartOnFirstThread".into());
    }
    jvm.extend(split_legacy_arguments(&instance.jvm_arguments)?);
    if let Some((mode, major)) = gc {
        apply_gc_arguments(&mut jvm, mode, major, platform)?;
    }
    // Apply the visible memory choice after metadata/custom JVM flags.
    jvm.push(format!("-Xmx{}M", options.memory_mb));
    let mut game = if let Some(arguments) = version.pointer("/arguments/game") {
        expand_arguments(arguments, platform, &features)?
    } else if let Some(arguments) = version.get("minecraftArguments").and_then(Value::as_str) {
        split_legacy_arguments(arguments)?
    } else {
        bail!("版本既没有 arguments.game，也没有 minecraftArguments");
    };
    game.extend(split_legacy_arguments(&instance.game_arguments)?);
    if instance.fullscreen && !game.iter().any(|arg| arg == "--fullscreen") {
        game.push("--fullscreen".into());
    }
    if !instance.server.is_empty() && !game.iter().any(|arg| arg == "--quickPlayMultiplayer") {
        let (host, port) = crate::config::parse_server_address(&instance.server)?;
        game.extend(["--server".into(), host]);
        if let Some(port) = port {
            game.extend(["--port".into(), port.to_string()]);
        }
    }
    let placeholder = Regex::new(r"\$\{([^}]+)\}").unwrap();
    let replace = |argument: String| -> Result<String> {
        for capture in placeholder.captures_iter(&argument) {
            if !replacements.contains_key(&capture[1]) {
                bail!("启动参数含未支持的变量：${{{}}}", &capture[1]);
            }
        }
        Ok(placeholder
            .replace_all(&argument, |capture: &regex::Captures<'_>| {
                replacements[&capture[1]].as_str()
            })
            .into_owned())
    };
    let mut args = jvm.into_iter().map(&replace).collect::<Result<Vec<_>>>()?;
    if let Some(logging) = logging_jvm_argument(&version, &root)? {
        // This is one JVM argument, kept outside the general/game placeholder scope.
        args.push(logging);
    }
    let patch_libraries = version["libraries"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let warnings = crate::launch_patches::apply(
        &root,
        &mut args,
        &patch_libraries,
        gc.map(|(_, major)| major).unwrap_or_else(|| {
            version
                .pointer("/javaVersion/majorVersion")
                .and_then(Value::as_u64)
                .unwrap_or(8) as u32
        }),
        platform,
        crate::launch_patches::PatchOptions {
            disable_java_wrapper: instance.disable_java_wrapper,
            disable_lwjgl_unsafe_agent: instance.disable_lwjgl_unsafe_agent,
        },
    )?;
    args.push(main_class.to_owned());
    args.extend(game.into_iter().map(replace).collect::<Result<Vec<_>>>()?);
    if session.user_type == "demo" && !args.iter().any(|arg| arg == "--demo") {
        args.push("--demo".into());
    }
    if !args.iter().any(|arg| arg == "--width") {
        args.extend([
            "--width".into(),
            options.width.to_string(),
            "--height".into(),
            options.height.to_string(),
        ]);
    }
    if args.iter().any(|argument| argument.contains('\0')) {
        bail!("启动参数包含 NUL 字符");
    }
    replacements.clear();
    fs::create_dir_all(&cwd).context("无法创建游戏实例目录")?;
    fs::create_dir_all(&natives).context("无法创建 natives 目录")?;
    let folder = |path: &Path| {
        let mut value = path.to_string_lossy().into_owned();
        if !value.ends_with(['/', '\\']) {
            value.push(if platform.os == "windows" { '\\' } else { '/' });
        }
        value
    };
    let executable = std::env::current_exe().context("无法定位启动器可执行文件")?;
    let version_path = confined_path(&root, &Path::new("versions").join(&options.version_id))?;
    let variables = vec![
        ("{minecraft}".into(), folder(&root)),
        ("{version_path}".into(), folder(&version_path)),
        ("{verpath}".into(), folder(&version_path)),
        ("{version_indie}".into(), folder(&cwd)),
        ("{verindie}".into(), folder(&cwd)),
        (
            "{java}".into(),
            folder(java.parent().context("Java 缺少父目录")?),
        ),
        ("{name}".into(), options.version_id.clone()),
        ("{version}".into(), jar_id.to_owned()),
        (
            "{path}".into(),
            folder(executable.parent().context("启动器缺少父目录")?),
        ),
        (
            "{path_with_name}".into(),
            executable.to_string_lossy().into_owned(),
        ),
        ("{pcl_version}".into(), env!("CARGO_PKG_VERSION").into()),
    ];
    let mut window_title = instance.game_window_title.clone();
    for (key, value) in &variables {
        window_title = window_title.replace(key, value);
    }
    let window = GameWindowOptions {
        title: window_title,
        maximize: instance.window_mode == Some(crate::config::WindowMode::Maximized),
    };
    let commands = if instance.pre_launch_command.trim().is_empty() {
        Vec::new()
    } else {
        vec![PreLaunchCommand {
            label: "版本",
            text: instance.pre_launch_command,
            wait: instance.pre_launch_wait,
        }]
    };
    Ok(LaunchPlan {
        java,
        args,
        cwd,
        secrets,
        behavior: LaunchBehavior {
            visibility: crate::config::LauncherVisibility::Keep,
            priority: crate::config::ProcessPriority::Normal,
            window,
            memory_optimize: instance.memory_optimize.unwrap_or(false),
            auto_chinese: false,
            language_code: if version["_pcl_jar_id"]
                .as_str()
                .unwrap_or(&options.version_id)
                .strip_prefix("1.")
                .and_then(|v| v.split('.').next())
                .and_then(|v| v.parse::<u32>().ok())
                .is_some_and(|v| v < 11)
            {
                "zh_CN".into()
            } else {
                "zh_cn".into()
            },
            high_performance_gpu: false,
            offline_skin: None,
            warnings,
            commands,
            command_cwd: root,
            variables,
        },
    })
}

/// PCL 2.13.1.1 ModLaunch's GC selection. Keep custom mode untouched; for a
/// managed mode remove competing collector/tuning flags before adding its set.
fn apply_gc_arguments(
    arguments: &mut Vec<String>,
    mode: crate::config::GcMode,
    java_major: u32,
    platform: &Platform,
) -> Result<()> {
    use crate::config::GcMode;
    if mode == GcMode::Custom {
        return Ok(());
    }
    if java_major < 8 {
        bail!("自动 GC 参数要求 Java 8 或更新版本；旧 Java 请使用不指定模式");
    }
    let old_windows = platform.os == "windows" && {
        let mut version = platform
            .version
            .split('.')
            .filter_map(|part| part.parse::<u32>().ok());
        let major = version.next().unwrap_or(0);
        let _minor = version.next();
        let build = version.next().unwrap_or(0);
        major < 10 || (major == 10 && build < 17_763)
    };
    let use_g1 = match mode {
        GcMode::PreferZgc => java_major < 15 || old_windows,
        GcMode::PreferGenerationalZgc => java_major < 21 || old_windows,
        GcMode::G1 | GcMode::TunedG1 => true,
        GcMode::Custom => unreachable!(),
    };
    let managed = Regex::new(r"^-XX:[+-]?(Use\w+GC|ZGenerational|UseCompactObjectHeaders|G1\w+Percent|G1\w+Size|(Max|Min)(GCPauseMillis|HeapFreeRatio))").unwrap();
    arguments.retain(|argument| !managed.is_match(argument));
    if !arguments
        .iter()
        .any(|argument| argument == "-XX:+UnlockExperimentalVMOptions")
    {
        arguments.push("-XX:+UnlockExperimentalVMOptions".into());
    }
    if java_major >= 24 {
        arguments.push("-XX:+UseCompactObjectHeaders".into());
    }
    if use_g1 {
        arguments.extend(
            [
                "-XX:+UseG1GC",
                "-XX:G1NewSizePercent=20",
                "-XX:G1ReservePercent=20",
                "-XX:G1HeapRegionSize=32M",
                "-XX:MaxGCPauseMillis=50",
            ]
            .map(str::to_owned),
        );
        if mode == GcMode::TunedG1 {
            if !arguments
                .iter()
                .any(|argument| argument == "-XX:+PerfDisableSharedMem")
            {
                arguments.push("-XX:+PerfDisableSharedMem".into());
            }
            if java_major == 8 {
                arguments.push("-XX:+ParallelRefProcEnabled".into());
            }
            if java_major >= 12 {
                arguments.extend([
                    "-XX:MinHeapFreeRatio=25".into(),
                    "-XX:MaxHeapFreeRatio=40".into(),
                ]);
            }
        }
    } else {
        arguments.push("-XX:+UseZGC".into());
        if matches!(java_major, 21 | 22) {
            arguments.push("-XX:+ZGenerational".into());
        }
    }
    Ok(())
}

fn logging_jvm_argument(version: &Value, root: &Path) -> Result<Option<String>> {
    let Some(logging) = version.pointer("/logging/client") else {
        return Ok(None);
    };
    let template = logging
        .get("argument")
        .and_then(Value::as_str)
        .context("日志配置缺少 JVM argument")?;
    if template.len() > 4096
        || !template.starts_with("-D")
        || template
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || template.matches("${path}").count() != 1
        || template.replace("${path}", "").contains("${")
    {
        bail!("日志 JVM 参数格式无效：仅支持含一个 ${{path}} 的单个 -D 参数");
    }
    let id = logging
        .pointer("/file/id")
        .and_then(Value::as_str)
        .context("日志配置缺少文件 ID")?;
    validate_id(id)?;
    let path = confined_path(root, &safe_relative(&format!("assets/log_configs/{id}"))?)?;
    require_file(&path, "日志配置（请先安装或修复版本）")?;
    Ok(Some(template.replace("${path}", &path.to_string_lossy())))
}

fn require_file(path: &Path, description: &str) -> Result<()> {
    if !path.is_file() {
        bail!("{description}不是可用文件：{}", path.display());
    }
    Ok(())
}

fn expand_arguments(
    value: &Value,
    platform: &Platform,
    features: &HashMap<String, bool>,
) -> Result<Vec<String>> {
    let mut output = Vec::new();
    for argument in value.as_array().context("启动参数必须是数组")? {
        if let Some(argument) = argument.as_str() {
            output.push(argument.to_owned());
            continue;
        }
        let object = argument
            .as_object()
            .context("启动参数必须是字符串或规则对象")?;
        if !rules_allow(
            object.get("rules").unwrap_or(&Value::Null),
            platform,
            features,
        )? {
            continue;
        }
        match object.get("value").context("参数规则缺少 value")? {
            Value::String(value) => output.push(value.clone()),
            Value::Array(values) => {
                for value in values {
                    output.push(
                        value
                            .as_str()
                            .context("参数 value 数组必须只包含字符串")?
                            .to_owned(),
                    );
                }
            }
            _ => bail!("参数 value 必须是字符串或字符串数组"),
        }
    }
    Ok(output)
}

pub(crate) fn split_legacy_arguments(text: &str) -> Result<Vec<String>> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut present = false;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters
                .peek()
                .is_some_and(|next| matches!(next, '\'' | '"' | '\\')) =>
            {
                current.push(characters.next().unwrap());
                present = true;
            }
            '\'' | '"' if quote == Some(character) => {
                quote = None;
                present = true;
            }
            '\'' | '"' if quote.is_none() => {
                quote = Some(character);
                present = true;
            }
            c if c.is_whitespace() && quote.is_none() => {
                if present {
                    arguments.push(std::mem::take(&mut current));
                    present = false;
                }
            }
            other => {
                current.push(other);
                present = true;
            }
        }
    }
    if quote.is_some() {
        bail!("旧版启动参数包含未闭合的引号");
    }
    if present {
        arguments.push(current);
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(value: Value) -> (tempfile::TempDir, LaunchOptions, Session, Platform) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("versions/test")).unwrap();
        fs::write(root.join("versions/test/test.json"), value.to_string()).unwrap();
        fs::write(root.join("versions/test/test.jar"), b"test fixture jar").unwrap();
        fs::write(root.join("java"), b"test fixture java").unwrap();
        let options = LaunchOptions {
            root: root.to_owned(),
            version_id: "test".into(),
            java: root.join("java"),
            memory_mb: 2048,
            width: 854,
            height: 480,
        };
        let session = Session {
            username: "Player With Space".into(),
            uuid: "0123456789abcdef0123456789abcdef".into(),
            access_token: "private-test-token".into(),
            user_type: "msa".into(),
        };
        let platform = Platform {
            os: "osx".into(),
            arch: "aarch64".into(),
            version: "14.0".into(),
        };
        (directory, options, session, platform)
    }

    #[test]
    fn historical_resources_follow_named_instances_and_changed_isolation() {
        use sha1::{Digest, Sha1};
        let payload = b"historical sound";
        let hash = format!("{:x}", Sha1::digest(payload));
        // Remap has precedence when both historical flags are present, matching
        // McAssetsListGet's map_to_resources then virtual source branches.
        let index = serde_json::to_vec(&json!({"map_to_resources":true,"virtual":true,
            "objects":{"sound/fixture.ogg":{"hash":hash,"size":payload.len()}}}))
        .unwrap();
        let value = json!({"id":"test","mainClass":"Main", "type":"release", "libraries":[],
            "minecraftArguments":"--assetsDir ${game_assets}",
            "downloads":{"client":{"url":"https://piston-data.mojang.com/fixture.jar", "sha1":format!("{:x}",Sha1::digest(b"test fixture jar")),"size":16}},
            "assetIndex":{"id":"old","url":"https://piston-meta.mojang.com/old.json","sha1":format!("{:x}",Sha1::digest(&index)),"size":index.len()}});
        let (_temp, mut options, session, platform) = fixture(value);
        fs::create_dir_all(options.root.join("assets/indexes")).unwrap();
        fs::write(options.root.join("assets/indexes/old.json"), index).unwrap();
        let source = options
            .root
            .join(format!("assets/objects/{}/{hash}", &hash[..2]));
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, payload).unwrap();
        let original = fs::read(options.root.join("versions/test/test.json")).unwrap();
        crate::install::install_vanilla_instance(
            &options.root,
            "test",
            "named",
            &platform,
            &std::sync::atomic::AtomicBool::new(false),
            |_| (),
        )
        .unwrap();
        options.version_id = "named".into();
        let plan = build_plan(&options, &session, &platform).unwrap();
        assert_eq!(
            plan.cwd,
            options.root.canonicalize().unwrap().join("instances/named")
        );
        assert_eq!(
            fs::read(plan.cwd.join("resources/sound/fixture.ogg")).unwrap(),
            payload
        );
        assert!(plan
            .args
            .contains(&plan.cwd.join("resources").to_string_lossy().into_owned()));
        crate::config::save_instance_settings(
            &options.root,
            "named",
            &crate::config::InstanceSettings {
                isolated: false,
                ..Default::default()
            },
        )
        .unwrap();
        let shared = build_plan(&options, &session, &platform).unwrap();
        assert_eq!(shared.cwd, options.root.canonicalize().unwrap());
        assert_eq!(
            fs::read(shared.cwd.join("resources/sound/fixture.ogg")).unwrap(),
            payload
        );
        assert_eq!(fs::read(&source).unwrap(), payload);
        assert_eq!(
            fs::read(options.root.join("versions/test/test.json")).unwrap(),
            original
        );
    }
    #[test]
    fn globals_apply_and_explicit_instance_choices_override_without_changing_files() {
        let (_root, options, session, platform) = fixture(json!({
            "mainClass":"Main", "type":"release", "minecraftArguments":"--versionType ${version_type}"
        }));
        let original = fs::read(options.root.join("versions/test/test.json")).unwrap();
        let mut global = crate::config::Settings {
            game_root: options.root.clone(),
            memory_mb: 5120,
            custom_info: "Global information".into(),
            jvm_arguments: "-Dmessage=\"global words\" -XX:+UseParallelGC".into(),
            game_arguments: "--custom \"global game\"".into(),
            window_mode: crate::config::WindowMode::Custom,
            width: 1280,
            height: 720,
            gc_mode: crate::config::GcMode::PreferGenerationalZgc,
            ..Default::default()
        };
        let plan = build_plan_with_settings(&options, &session, &platform, &global, 21).unwrap();
        for expected in [
            "-Xmx5120M",
            "-Dmessage=global words",
            "Global information",
            "global game",
            "1280",
            "720",
            "-XX:+UseZGC",
            "-XX:+ZGenerational",
        ] {
            assert!(plan.args.contains(&expected.into()), "{expected}");
        }
        assert!(!plan.args.contains(&"-XX:+UseParallelGC".into()));
        crate::config::save_instance_settings(
            &options.root,
            "test",
            &crate::config::InstanceSettings {
                memory_mb: Some(2048),
                width: Some(1024),
                custom_info: "Instance information".into(),
                jvm_arguments: "-Dmessage=instance -XX:+UseParallelGC".into(),
                game_arguments: "--custom instance".into(),
                gc_mode: Some(crate::config::GcMode::Custom),
                ..Default::default()
            },
        )
        .unwrap();
        let plan = build_plan_with_settings(&options, &session, &platform, &global, 21).unwrap();
        for expected in [
            "-Xmx2048M",
            "1024",
            "Instance information",
            "-Dmessage=instance",
            "-XX:+UseParallelGC",
        ] {
            assert!(plan.args.contains(&expected.into()), "{expected}");
        }
        assert!(!plan.args.contains(&"-XX:+UseZGC".into()));
        assert!(!plan.args.contains(&"Global information".into()));
        global.window_mode = crate::config::WindowMode::Fullscreen;
        assert!(
            !build_plan_with_settings(&options, &session, &platform, &global, 21)
                .unwrap()
                .args
                .contains(&"--fullscreen".into())
        );
        global.window_mode = crate::config::WindowMode::Maximized;
        assert!(build_plan_with_settings(&options, &session, &platform, &global, 21).is_ok());
        crate::config::save_instance_settings(
            &options.root,
            "test",
            &crate::config::InstanceSettings::default(),
        )
        .unwrap();
        assert!(
            build_plan_with_settings(&options, &session, &platform, &global, 21)
                .unwrap()
                .behavior
                .window
                .maximize
        );
        global.window_mode = crate::config::WindowMode::Fullscreen;
        assert!(
            build_plan_with_settings(&options, &session, &platform, &global, 21)
                .unwrap()
                .args
                .contains(&"--fullscreen".into())
        );
        crate::config::save_instance_settings(
            &options.root,
            "test",
            &crate::config::InstanceSettings {
                window_mode: Some(crate::config::WindowMode::Default),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            !build_plan_with_settings(&options, &session, &platform, &global, 21)
                .unwrap()
                .args
                .contains(&"--fullscreen".into())
        );
        assert_eq!(
            fs::read(options.root.join("versions/test/test.json")).unwrap(),
            original
        );
    }

    #[test]
    fn managed_gc_obeys_java_and_windows_compatibility_and_keeps_custom_arguments() {
        use crate::config::GcMode;
        let modern = Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            version: "10.0.19045".into(),
        };
        let old = Platform {
            version: "10.0.17134".into(),
            ..modern.clone()
        };
        for (major, mode, platform, expected, generational) in [
            (8, GcMode::PreferZgc, &modern, "-XX:+UseG1GC", false),
            (17, GcMode::PreferZgc, &modern, "-XX:+UseZGC", false),
            (
                17,
                GcMode::PreferGenerationalZgc,
                &modern,
                "-XX:+UseG1GC",
                false,
            ),
            (21, GcMode::PreferZgc, &old, "-XX:+UseG1GC", false),
            (21, GcMode::PreferZgc, &modern, "-XX:+UseZGC", true),
            (23, GcMode::PreferZgc, &modern, "-XX:+UseZGC", false),
            (25, GcMode::TunedG1, &modern, "-XX:+UseG1GC", false),
        ] {
            let mut args = vec![
                "-Dkeep=value".into(),
                "-XX:+UseParallelGC".into(),
                "-XX:G1NewSizePercent=50".into(),
            ];
            apply_gc_arguments(&mut args, mode, major, platform).unwrap();
            assert!(args.contains(&expected.into()));
            assert!(args.contains(&"-Dkeep=value".into()));
            assert!(!args.contains(&"-XX:+UseParallelGC".into()));
            assert!(!args.contains(&"-XX:G1NewSizePercent=50".into()));
            assert_eq!(args.contains(&"-XX:+ZGenerational".into()), generational);
            assert_eq!(
                args.contains(&"-XX:+UseCompactObjectHeaders".into()),
                major >= 24
            );
        }
        let mut args = vec!["-XX:+UseParallelGC".into()];
        apply_gc_arguments(&mut args, GcMode::Custom, 21, &modern).unwrap();
        assert_eq!(args, ["-XX:+UseParallelGC"]);
    }

    #[test]
    fn modern_launch_preserves_argument_boundaries_and_redacts_all_token_occurrences() {
        let (_root, options, session, platform) = fixture(
            json!({"mainClass":"net.minecraft.Main", "arguments": {
                "jvm":["-cp", "${classpath}", "-Dcustom.token=${auth_access_token}"],
                "game":["--username", "${auth_player_name}", "--accessToken", "${auth_access_token}", {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],"value":["--width","${resolution_width}","--height","${resolution_height}"]}]
            }}),
        );
        let plan = build_plan(&options, &session, &platform).unwrap();
        assert!(plan.args.contains(&"Player With Space".into()));
        assert!(plan.args.contains(&"-XstartOnFirstThread".into()));
        assert_eq!(
            plan.args
                .iter()
                .filter(|argument| *argument == "--width")
                .count(),
            1
        );
        assert!(!plan.redacted_command().contains(&session.access_token));
        assert!(!format!("{plan:?}").contains(&session.access_token));
        assert_eq!(
            plan.cwd,
            options.root.canonicalize().unwrap().join("versions/test")
        );
    }

    #[test]
    fn inspected_java_remains_authoritative_when_saved_specific_path_differs() {
        let (_root, options, session, platform) = fixture(json!({
            "mainClass":"Main", "minecraftArguments":"--username ${auth_player_name}"
        }));
        let other = options.root.join("not-inspected-java");
        fs::write(&other, b"fixture, never executed").unwrap();
        let instance = crate::config::InstanceSettings {
            java_mode: Some(crate::java_selection::JavaSelectionMode::Specific),
            java_path: Some(other.clone()),
            memory_mb: Some(3072),
            jvm_arguments: "-Dpreserve=instance".into(),
            ..Default::default()
        };
        crate::config::save_instance_settings(&options.root, "test", &instance).unwrap();
        let global = crate::config::Settings {
            game_root: options.root.clone(),
            ..Default::default()
        };
        for plan in [
            build_plan_with_explicit_java(&options, &session, &platform).unwrap(),
            build_plan_with_settings(&options, &session, &platform, &global, 21).unwrap(),
        ] {
            assert_eq!(plan.java, options.java.canonicalize().unwrap());
            assert!(plan.args.contains(&"-Xmx3072M".into()));
            assert!(plan.args.contains(&"-Dpreserve=instance".into()));
        }
        // The legacy entry point and the saved preference retain their behavior.
        assert_eq!(
            build_plan(&options, &session, &platform).unwrap().java,
            other.canonicalize().unwrap()
        );
        assert_eq!(
            crate::config::load_instance_settings(&options.root, "test")
                .unwrap()
                .java_path,
            Some(other)
        );
    }

    #[test]
    fn per_instance_settings_apply_before_launch_and_keep_old_data() {
        let (_root, options, session, platform) = fixture(json!({
            "mainClass":"Main", "minecraftArguments":"--username ${auth_player_name} --gameDir ${game_directory} --versionType ${version_type}"
        }));
        let old = options.root.join("instances/test");
        fs::create_dir_all(old.join("saves/world")).unwrap();
        fs::write(old.join("saves/world/level.dat"), b"preserve user world").unwrap();
        let java = options.root.join("other-java");
        fs::write(&java, b"fixture").unwrap();
        let mut settings = crate::config::InstanceSettings {
            java_path: Some(java.clone()),
            memory_mb: Some(3072),
            width: Some(1280),
            height: Some(720),
            fullscreen: true,
            custom_info: "Custom information".into(),
            jvm_arguments: "-Dexample=\"two words\"".into(),
            game_arguments: "--example \"value with space\"".into(),
            server: "example.org:25566".into(),
            ..Default::default()
        };
        crate::config::save_instance_settings(&options.root, "test", &settings).unwrap();
        let plan = build_plan(&options, &session, &platform).unwrap();
        assert_eq!(plan.cwd, old.canonicalize().unwrap());
        assert_eq!(plan.java, java.canonicalize().unwrap());
        for mode in [
            crate::java_selection::JavaSelectionMode::Automatic,
            crate::java_selection::JavaSelectionMode::VersionRange,
            crate::java_selection::JavaSelectionMode::VersionFolder,
        ] {
            settings.java_mode = Some(mode);
            settings.java_range = "[17,22)".into();
            crate::config::save_instance_settings(&options.root, "test", &settings).unwrap();
            assert_eq!(
                build_plan(&options, &session, &platform).unwrap().java,
                options.java.canonicalize().unwrap(),
                "a stale specified path must not override {mode:?}"
            );
        }
        settings.java_mode = Some(crate::java_selection::JavaSelectionMode::Specific);
        for argument in [
            "-Xmx3072M",
            "-Dexample=two words",
            "value with space",
            "--fullscreen",
            "1280",
            "720",
            "example.org",
            "25566",
            "Custom information",
        ] {
            assert!(
                plan.args.contains(&argument.to_owned()),
                "missing {argument}"
            );
        }
        settings.isolated = false;
        settings.login_requirement = crate::config::LoginRequirement::Offline;
        crate::config::save_instance_settings(&options.root, "test", &settings).unwrap();
        assert!(build_plan(&options, &session, &platform)
            .unwrap_err()
            .to_string()
            .contains("离线"));
        settings.login_requirement = crate::config::LoginRequirement::Any;
        crate::config::save_instance_settings(&options.root, "test", &settings).unwrap();
        assert_eq!(
            build_plan(&options, &session, &platform).unwrap().cwd,
            options.root.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read(old.join("saves/world/level.dat")).unwrap(),
            b"preserve user world"
        );
    }

    #[test]
    fn modern_quick_play_uses_metadata_rule_without_legacy_server_flags() {
        let (_root, options, session, platform) = fixture(json!({
            "mainClass":"Main", "arguments":{"game":[
                {"rules":[{"action":"allow","features":{"is_quick_play_multiplayer":true}}],"value":["--quickPlayMultiplayer","${quickPlayMultiplayer}"]}
            ]}
        }));
        let settings = crate::config::InstanceSettings {
            server: "[::1]:25565".into(),
            ..Default::default()
        };
        crate::config::save_instance_settings(&options.root, "test", &settings).unwrap();
        let plan = build_plan(&options, &session, &platform).unwrap();
        assert!(plan.args.contains(&"[::1]:25565".into()));
        assert!(!plan.args.contains(&"--server".into()));
    }

    #[test]
    fn mojang_1_21_1_logging_is_a_confined_jvm_argument_before_main_class() {
        // logging.client is copied from the installed, SHA1-verified Mojang 1.21.1 JSON.
        let mut metadata = json!({
            "mainClass": "net.minecraft.client.main.Main",
            "arguments": { "jvm": ["-cp", "${classpath}"], "game": ["--username", "${auth_player_name}"] },
            "logging": { "client": {
                "argument": "-Dlog4j.configurationFile=${path}",
                "file": {
                    "id": "client-1.12.xml",
                    "sha1": "bd65e7d2e3c237be76cfbef4c2405033d7f91521",
                    "size": 888,
                    "url": "https://piston-data.mojang.com/v1/objects/bd65e7d2e3c237be76cfbef4c2405033d7f91521/client-1.12.xml"
                },
                "type": "log4j2-xml"
            } }
        });
        let (_root, options, session, platform) = fixture(metadata.clone());
        assert!(build_plan(&options, &session, &platform)
            .unwrap_err()
            .to_string()
            .contains("日志配置"));
        let logging = options.root.join("assets/log_configs/client-1.12.xml");
        fs::create_dir_all(logging.parent().unwrap()).unwrap();
        fs::write(&logging, "<Configuration/>").unwrap();
        let plan = build_plan(&options, &session, &platform).unwrap();
        let expected = format!(
            "-Dlog4j.configurationFile={}",
            logging.canonicalize().unwrap().display()
        );
        let position = plan.args.iter().position(|arg| arg == &expected).unwrap();
        let main = plan
            .args
            .iter()
            .position(|arg| arg == "net.minecraft.client.main.Main")
            .unwrap();
        assert!(position < main);
        assert_eq!(
            plan.args
                .iter()
                .filter(|arg| arg.starts_with("-Dlog4j.configurationFile="))
                .count(),
            1
        );
        assert!(!plan.args.iter().any(|arg| arg.contains("${path}")));

        let version_path = options.root.join("versions/test/test.json");
        metadata["arguments"]["game"] = json!(["${path}"]);
        fs::write(&version_path, metadata.to_string()).unwrap();
        assert!(build_plan(&options, &session, &platform)
            .unwrap_err()
            .to_string()
            .contains("未支持的变量"));
        metadata["arguments"]["game"] = json!([]);
        metadata["logging"]["client"]["argument"] =
            json!("-Dlog4j.configurationFile=${path}${unknown}");
        fs::write(&version_path, metadata.to_string()).unwrap();
        assert!(build_plan(&options, &session, &platform)
            .unwrap_err()
            .to_string()
            .contains("日志 JVM 参数格式无效"));
        metadata["logging"]["client"]["argument"] = json!("-Dlog4j.configurationFile=${path}");
        metadata["logging"]["client"]["file"]["id"] = json!("../../outside.xml");
        fs::write(&version_path, metadata.to_string()).unwrap();
        assert!(build_plan(&options, &session, &platform).is_err());

        #[cfg(unix)]
        {
            metadata["logging"]["client"]["file"]["id"] = json!("client-1.12.xml");
            fs::write(&version_path, metadata.to_string()).unwrap();
            let outside = tempfile::NamedTempFile::new().unwrap();
            fs::remove_file(&logging).unwrap();
            std::os::unix::fs::symlink(outside.path(), &logging).unwrap();
            assert!(build_plan(&options, &session, &platform)
                .unwrap_err()
                .to_string()
                .contains("符号链接"));
        }
    }

    #[test]
    fn supports_legacy_quoted_arguments_and_rejects_unknown_placeholders() {
        let (_root, options, session, platform) = fixture(
            json!({"mainClass":"Main", "minecraftArguments":"--username ${auth_player_name} --example \"two words\" --empty \"\""}),
        );
        let plan = build_plan(&options, &session, &platform).unwrap();
        assert!(plan.args.contains(&"two words".into()));
        assert!(plan.args.contains(&String::new()));
        assert!(split_legacy_arguments("--bad \"unclosed").is_err());
        fs::write(
            options.root.join("versions/test/test.json"),
            json!({"mainClass":"Main","minecraftArguments":"${unimplemented}"}).to_string(),
        )
        .unwrap();
        assert!(build_plan(&options, &session, &platform)
            .unwrap_err()
            .to_string()
            .contains("unimplemented"));
    }

    #[test]
    fn diagnoses_missing_libraries_without_creating_an_instance() {
        let (_root, options, session, platform) = fixture(
            json!({"mainClass":"Main","libraries":[{"name":"example:missing:1"}],"minecraftArguments":"--username ${auth_player_name}"}),
        );
        let error = build_plan(&options, &session, &platform)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing-1.jar"));
        assert!(!options.root.join("instances/test").exists());
    }

    #[test]
    fn uses_inherited_client_jar_and_platform_classpath_separator() {
        let (_root, mut options, session, mut platform) = fixture(
            json!({"mainClass":"Main","minecraftArguments":"--username ${auth_player_name}"}),
        );
        fs::create_dir_all(options.root.join("versions/child")).unwrap();
        fs::write(
            options.root.join("versions/child/child.json"),
            json!({"inheritsFrom":"test","libraries":[{"name":"example:lib:1"}]}).to_string(),
        )
        .unwrap();
        fs::create_dir_all(options.root.join("libraries/example/lib/1")).unwrap();
        fs::write(
            options.root.join("libraries/example/lib/1/lib-1.jar"),
            b"fixture",
        )
        .unwrap();
        options.version_id = "child".into();
        platform.os = "windows".into();
        let plan = build_plan(&options, &session, &platform).unwrap();
        let position = plan
            .args
            .iter()
            .position(|argument| argument == "-cp")
            .unwrap();
        // The target OS chooses the classpath delimiter; each path keeps the
        // host's native separators (including Windows canonical path prefixes).
        let entries = plan.args[position + 1]
            .split(';')
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                options
                    .root
                    .join("libraries/example/lib/1/lib-1.jar")
                    .canonicalize()
                    .unwrap(),
                options
                    .root
                    .join("versions/test/test.jar")
                    .canonicalize()
                    .unwrap(),
            ]
        );
        assert!(!plan.args.contains(&"-XstartOnFirstThread".into()));
    }
    #[test]
    fn explicit_commands_are_ordered_and_download_metadata_cannot_add_commands() {
        let (_dir, options, session, platform) = fixture(json!({
            "mainClass":"Main", "minecraftArguments":"", "pre_launch_command":"evil metadata",
            "behavior":{"commands":["evil"]}
        }));
        let instance = crate::config::InstanceSettings {
            pre_launch_command: "printf instance".into(),
            pre_launch_wait: false,
            ..Default::default()
        };
        crate::config::save_instance_settings(&options.root, "test", &instance).unwrap();
        let settings = crate::config::Settings {
            pre_launch_command: "printf global".into(),
            pre_launch_wait: true,
            launcher_visibility: crate::config::LauncherVisibility::HideThenRestore,
            process_priority: crate::config::ProcessPriority::Low,
            ..Default::default()
        };
        let plan = build_plan_with_settings(&options, &session, &platform, &settings, 21).unwrap();
        assert_eq!(
            plan.behavior
                .commands
                .iter()
                .map(|c| (c.label, c.text.as_str(), c.wait))
                .collect::<Vec<_>>(),
            [
                ("全局", "printf global", true),
                ("版本", "printf instance", false)
            ]
        );
        assert_eq!(plan.behavior.priority, crate::config::ProcessPriority::Low);
        assert_eq!(
            plan.behavior.visibility,
            crate::config::LauncherVisibility::HideThenRestore
        );
        assert_eq!(
            plan.behavior.command_cwd,
            options.root.canonicalize().unwrap()
        );
        assert!(!plan.redacted_command().contains("printf"));
    }

    #[test]
    fn native_launch_settings_are_effective_without_running_helpers_during_plan() {
        let (_dir, options, session, platform) =
            fixture(json!({"mainClass":"Main","minecraftArguments":""}));
        let instance = crate::config::InstanceSettings {
            game_window_title: "{user} · {date}".into(),
            memory_optimize: Some(false),
            window_mode: Some(crate::config::WindowMode::Maximized),
            ..Default::default()
        };
        crate::config::save_instance_settings(&options.root, "test", &instance).unwrap();
        let settings = crate::config::Settings {
            memory_optimize: true,
            game_window_title: "global".into(),
            prefer_high_performance_gpu: true,
            ..Default::default()
        };
        let plan = build_plan_with_settings(&options, &session, &platform, &settings, 21).unwrap();
        assert!(plan.behavior.window.maximize);
        assert!(!plan.behavior.memory_optimize);
        assert!(plan.behavior.window.title.contains("{date}"));
        assert!(!plan.behavior.window.title.contains("global"));
        assert!(plan.behavior.high_performance_gpu);
        assert!(plan.behavior.auto_chinese);
        assert!(plan.behavior.offline_skin.is_none());
        assert!(!plan.cwd.join("options.txt").exists());
    }

    #[test]
    fn launcher_size_uses_frozen_pixels_instead_of_old_instance_dimensions() {
        let (_dir, options, session, platform) = fixture(
            json!({"mainClass":"Main", "minecraftArguments":"--width ${resolution_width} --height ${resolution_height}"}),
        );
        let instance = crate::config::InstanceSettings {
            window_mode: Some(crate::config::WindowMode::LauncherSize),
            width: Some(111),
            height: Some(222),
            ..Default::default()
        };
        crate::config::save_instance_settings(&options.root, "test", &instance).unwrap();
        let settings = crate::config::Settings::default();
        assert!(build_plan_with_settings(&options, &session, &platform, &settings, 21).is_err());
        let plan = build_plan_with_settings_and_viewport(
            &options,
            &session,
            &platform,
            &settings,
            21,
            Some((1234, 765)),
        )
        .unwrap();
        assert!(plan.args.windows(2).any(|p| p == ["--width", "1234"]));
        assert!(plan.args.windows(2).any(|p| p == ["--height", "765"]));
        assert!(build_plan_with_settings_and_viewport(
            &options,
            &session,
            &platform,
            &settings,
            21,
            Some((0, 1))
        )
        .is_err());
    }
    #[test]
    fn homepage_server_override_is_ephemeral_for_legacy_and_quick_play() {
        for modern in [false, true] {
            let metadata = if modern {
                json!({"mainClass":"Main","arguments":{"game":[{"rules":[{"action":"allow","features":{"is_quick_play_multiplayer":true}}],"value":["--quickPlayMultiplayer","${quickPlayMultiplayer}"]}]}})
            } else {
                json!({"mainClass":"Main","minecraftArguments":"--username ${auth_player_name}"})
            };
            let (_root, options, session, platform) = fixture(metadata);
            let instance = crate::config::InstanceSettings {
                server: "saved.example.org:25566".into(),
                ..Default::default()
            };
            crate::config::save_instance_settings(&options.root, "test", &instance).unwrap();
            let path = crate::config::instance_settings_path(&options.root, "test").unwrap();
            let before = fs::read(&path).unwrap();
            let plan = build_plan_with_overrides(
                &options,
                &session,
                &platform,
                &Default::default(),
                21,
                LaunchOverrides {
                    launcher_size: None,
                    server: Some("once.example.org:25565"),
                    memory_mb: None,
                },
            )
            .unwrap();
            assert!(plan.args.contains(&if modern {
                "once.example.org:25565".into()
            } else {
                "once.example.org".into()
            }));
            assert_eq!(plan.args.contains(&"--quickPlayMultiplayer".into()), modern);
            assert_eq!(fs::read(path).unwrap(), before);
        }
    }
}
