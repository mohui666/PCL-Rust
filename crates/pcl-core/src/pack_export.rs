//! Explicit instance export with no-clobber commit and credential filtering.
//! Additional format adapters share the same selected-file snapshot and hashes.
use crate::{config, install, instances, metadata, model::Progress, resources::ModrinthVersion};
use anyhow::{bail, ensure, Context, Result};
use regex::Regex;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[path = "pack_export_formats.rs"]
mod formats;
pub use formats::PackFormat;
#[path = "pack_export_extras.rs"]
mod extras;
pub use extras::{available_java_roots, export_pack_with_launcher, validate_launcher_export};

const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOTAL: u64 = 20 * 1024 * 1024 * 1024;
const MAX_TEXT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResourceMode {
    /// Query Modrinth; a successful query with no exact match leaves an explicit
    /// unhosted override. Network failure is an error, never an implicit fallback.
    #[default]
    PreferHosted,
    /// Explicit local embedding, without sending any file hashes to a provider.
    EmbedAll,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ExportSelection {
    pub game_settings: bool,
    pub game_personal: bool,
    pub optifine_settings: bool,
    pub mods: bool,
    pub disabled_mods: bool,
    pub mod_configs: bool,
    pub pack_data: bool,
    pub tacz: bool,
    pub paintings: bool,
    pub maps: bool,
    pub jei_personal: bool,
    pub emi_personal: bool,
    pub patchouli_personal: bool,
    /// Master switch; turning it off retains the child selection below.
    pub resource_packs: bool,
    /// Literal immediate resourcepacks/ or texturepacks/ children. Folders end
    /// in `/`. None preserves older configs' all-items behavior; Some([]) is none.
    pub resource_pack_items: Option<Vec<String>>,
    /// Master switch; turning it off retains the child selection below.
    pub shader_packs: bool,
    /// Literal immediate shaderpacks/ children, with the same None/empty semantics.
    pub shader_pack_items: Option<Vec<String>>,
    pub shader_settings: bool,
    pub licenses: bool,
    pub screenshots: bool,
    pub schematics: bool,
    pub replays: bool,
    pub servers: bool,
    /// Explicit immediate children of saves/. Empty means no worlds.
    pub worlds: Vec<String>,
}
impl Default for ExportSelection {
    fn default() -> Self {
        Self {
            game_settings: true,
            game_personal: false,
            optifine_settings: true,
            mods: true,
            disabled_mods: false,
            mod_configs: true,
            pack_data: true,
            tacz: true,
            paintings: true,
            maps: false,
            jei_personal: false,
            emi_personal: false,
            patchouli_personal: false,
            resource_packs: true,
            resource_pack_items: None,
            shader_packs: true,
            shader_pack_items: None,
            shader_settings: true,
            licenses: true,
            screenshots: false,
            schematics: false,
            replays: false,
            servers: false,
            worlds: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackExportOptions {
    pub name: String,
    pub version: String,
    pub summary: String,
    pub selection: ExportSelection,
    pub resource_mode: ResourceMode,
    pub format: PackFormat,
    pub include_java: bool,
    pub include_launcher: bool,
}
impl Default for PackExportOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "1.0.0".into(),
            summary: String::new(),
            selection: ExportSelection::default(),
            resource_mode: ResourceMode::default(),
            format: PackFormat::default(),
            include_java: false,
            include_launcher: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackExportReport {
    pub path: PathBuf,
    pub files: usize,
    pub hosted_files: usize,
    pub override_files: usize,
    pub unhosted_files: Vec<String>,
    pub excluded_sensitive_files: Vec<String>,
    pub bytes: u64,
}

pub fn validate_export_options(options: &PackExportOptions) -> Result<()> {
    for (value, label) in [
        (&options.name, "整合包名称"),
        (&options.version, "整合包版本"),
    ] {
        ensure!(
            !value.trim().is_empty()
                && value.chars().count() <= 100
                && !value.chars().any(char::is_control),
            "{label}必须为 1–100 个字符且不能含控制字符"
        );
    }
    ensure!(
        options.summary.len() <= 32_768 && !options.summary.contains('\0'),
        "整合包简介过长或含 NUL 字符"
    );
    let mut worlds = BTreeSet::new();
    for world in &options.selection.worlds {
        metadata::validate_id(world).context("目标世界名称无效")?;
        ensure!(
            worlds.insert(world.to_lowercase()),
            "目标世界重复或有大小写冲突"
        );
    }
    validate_pack_items(
        options.selection.resource_pack_items.as_deref(),
        &["resourcepacks", "texturepacks"],
    )?;
    validate_pack_items(
        options.selection.shader_pack_items.as_deref(),
        &["shaderpacks"],
    )?;
    Ok(())
}

fn validate_pack_items(items: Option<&[String]>, folders: &[&str]) -> Result<()> {
    let Some(items) = items else { return Ok(()) };
    ensure!(items.len() <= 200_000, "所选资源项目数量过多");
    let mut seen = BTreeSet::new();
    for item in items {
        let path = item.strip_suffix('/').unwrap_or(item);
        metadata::safe_relative(path).context("资源子项路径无效")?;
        let parts: Vec<_> = path.split('/').collect();
        ensure!(
            parts.len() == 2 && folders.contains(&parts[0]),
            "资源子项必须是指定资源目录的直属项目：{item}"
        );
        ensure!(
            item.ends_with('/') || compressed_pack_name(parts[1]),
            "资源子项必须是 ZIP/RAR 文件或以 / 结尾的文件夹：{item}"
        );
        ensure!(
            !resource_blacklisted(parts[1]) && !sensitive_path(path) && !skipped(path),
            "资源子项在不可导出的排除列表中：{item}"
        );
        ensure!(
            seen.insert(path.to_lowercase()),
            "资源子项重复或存在大小写冲突：{item}"
        );
    }
    Ok(())
}

fn compressed_pack_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".rar")
}

/// No network request occurs until the caller explicitly invokes this function.
/// This exporter reads only the chosen instance, never modifies it, and writes a
/// new .mrpack atomically. Unknown resources remain named in the returned report.
pub fn export_pack(
    root: &Path,
    version_id: &str,
    destination: &Path,
    options: &PackExportOptions,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<PackExportReport> {
    ensure!(
        !options.include_launcher,
        "附带启动器时须由桌面端提供当前程序路径"
    );
    export_with(
        root,
        version_id,
        destination,
        options,
        cancel,
        progress,
        lookup_modrinth,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: (u64, u64),
    len: u64,
    modified: SystemTime,
    changed: Option<(i64, i64)>,
}
fn open_source(path: &Path) -> Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100); // O_NOFOLLOW
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000); // O_NOFOLLOW
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .with_context(|| format!("无法安全打开导出文件：{}", path.display()))?;
    handle_stamp(&file)?;
    Ok(file)
}
fn handle_stamp(file: &File) -> Result<Stamp> {
    let meta = file.metadata()?;
    ensure!(meta.is_file(), "导出内容必须是普通文件");
    #[cfg(unix)]
    let (identity, changed) = {
        use std::os::unix::fs::MetadataExt;
        (
            (meta.dev(), meta.ino()),
            Some((meta.ctime(), meta.ctime_nsec())),
        )
    };
    #[cfg(windows)]
    let (identity, changed) = {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Information {
            attributes: u32,
            created: [u32; 2],
            accessed: [u32; 2],
            modified: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                info: *mut Information,
            ) -> i32;
        }
        let mut info: Information = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
            "无法读取导出文件身份"
        );
        ensure!(info.attributes & 0x400 == 0, "导出内容不能是重解析点");
        (
            (
                (info.volume as u64),
                ((info.index_high as u64) << 32) | info.index_low as u64,
            ),
            None,
        )
    };
    #[cfg(not(any(unix, windows)))]
    let (identity, changed) = {
        bail!("此平台不支持安全导出文件身份检查");
    };
    Ok(Stamp {
        identity,
        len: meta.len(),
        modified: meta.modified()?,
        changed,
    })
}
fn file_stamp(path: &Path) -> Result<Stamp> {
    instances::identity(path)?;
    handle_stamp(&open_source(path)?)
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    relative: String,
    stamp: Stamp,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tree {
    directories: BTreeMap<String, instances::Identity>,
    files: Vec<Candidate>,
    excluded: BTreeSet<String>,
}
#[derive(Clone)]
struct HashedFile {
    candidate: Candidate,
    sha1: String,
    sha512: String,
    resource: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackIndex<'a> {
    game: &'static str,
    format_version: u32,
    version_id: &'a str,
    name: &'a str,
    summary: &'a str,
    files: Vec<IndexFile>,
    dependencies: BTreeMap<String, String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    path: String,
    hashes: BTreeMap<String, String>,
    downloads: Vec<String>,
    file_size: u64,
}
#[derive(Clone, Debug)]
struct HostedFile {
    sha1: String,
    sha512: String,
    size: u64,
    url: String,
}

fn export_with(
    root: &Path,
    version_id: &str,
    destination: &Path,
    options: &PackExportOptions,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
    mut lookup: impl FnMut(&[String], &AtomicBool) -> Result<BTreeMap<String, HostedFile>>,
) -> Result<PackExportReport> {
    install::cancelled(cancel)?;
    validate_export_options(options)?;
    metadata::validate_id(version_id)?;
    ensure!(destination.is_absolute(), "整合包保存位置必须是绝对路径");
    ensure!(
        destination
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(options.format.extension())),
        "保存文件扩展名与所选整合包格式不符"
    );
    let parent = destination
        .parent()
        .context("保存位置缺少父目录")?
        .canonicalize()
        .context("保存目录不存在或不可访问")?;
    ensure!(parent.is_dir(), "保存位置的父路径不是目录");
    let output = parent.join(destination.file_name().context("保存位置缺少文件名")?);
    require_absent(&output)?;
    let resolved = metadata::resolve_version(root, version_id)?;
    let minecraft = export_minecraft_id(root, &resolved)?;
    let dependencies = dependencies(&resolved, &minecraft)?;
    let instance = config::instance_game_dir(root, version_id)?;
    ensure!(
        fs::symlink_metadata(&instance)?.is_dir(),
        "游戏实例目录不存在"
    );
    let extra_files = if options.include_java {
        extras::java_files(root, version_id, cancel)?
    } else {
        Vec::new()
    };
    let rules = Rules::new(&options.selection)?;
    let tree = collect(&instance, &rules, cancel)?;
    let mut excluded = tree.excluded.clone();
    let mut hashed = Vec::new();
    let mut total_size = extra_files.iter().map(|file| file.stamp.len).sum::<u64>();
    ensure!(total_size <= MAX_TOTAL, "Java 运行时导出超过 20 GiB 限制");
    for (index, candidate) in tree.files.iter().enumerate() {
        install::cancelled(cancel)?;
        total_size = total_size
            .checked_add(candidate.stamp.len)
            .context("导出内容大小溢出")?;
        ensure!(
            candidate.stamp.len <= MAX_FILE && total_size <= MAX_TOTAL,
            "导出内容超过单文件 2 GiB / 总计 20 GiB 限制"
        );
        progress(Progress {
            completed: index as u64,
            total: tree.files.len() as u64,
            message: format!("检查导出文件：{}", candidate.relative),
            ..Default::default()
        });
        let path = checked_source_path(&instance, &candidate.relative)?;
        let (sha1, sha512, sensitive) = hash_file(&path, &candidate.stamp, cancel, true)?;
        if sensitive {
            excluded.insert(candidate.relative.clone());
            continue;
        }
        hashed.push(HashedFile {
            candidate: candidate.clone(),
            sha1,
            sha512,
            resource: is_hosted_candidate(&candidate.relative),
        });
    }
    extras::check_collisions(
        &extra_files,
        hashed.iter().map(|file| file.candidate.relative.as_str()),
    )?;
    let queries: Vec<_> = hashed
        .iter()
        .filter(|file| file.resource)
        .map(|file| file.sha512.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let hosted = if options.format == PackFormat::Mrpack
        && options.resource_mode == ResourceMode::PreferHosted
        && !queries.is_empty()
    {
        progress(Progress {
            message: "正在查询 Modrinth 托管文件信息".into(),
            ..Default::default()
        });
        lookup(&queries, cancel)?
    } else {
        BTreeMap::new()
    };
    ensure!(
        hosted.keys().all(|key| queries.contains(key)),
        "Modrinth 文件查询返回未请求的哈希"
    );
    let mut index_files = Vec::new();
    let mut overrides = Vec::new();
    let mut unhosted = Vec::new();
    for file in &hashed {
        if let Some(remote) = hosted.get(&file.sha512).filter(|_| file.resource) {
            ensure!(
                remote.sha1.eq_ignore_ascii_case(&file.sha1)
                    && remote.sha512.eq_ignore_ascii_case(&file.sha512)
                    && remote.size == file.candidate.stamp.len,
                "Modrinth 文件信息与本地内容不一致：{}",
                file.candidate.relative
            );
            validate_hosted_url(&remote.url)?;
            index_files.push(IndexFile {
                path: file.candidate.relative.clone(),
                hashes: BTreeMap::from([
                    ("sha1".into(), file.sha1.clone()),
                    ("sha512".into(), file.sha512.clone()),
                ]),
                downloads: vec![remote.url.clone()],
                file_size: file.candidate.stamp.len,
            });
        } else {
            if file.resource
                && options.format == PackFormat::Mrpack
                && options.resource_mode == ResourceMode::PreferHosted
            {
                unhosted.push(file.candidate.relative.clone());
            }
            overrides.push(file);
        }
    }
    let index = PackIndex {
        game: "minecraft",
        format_version: 1,
        version_id: &options.version,
        name: &options.name,
        summary: &options.summary,
        files: index_files,
        dependencies,
    };
    let manifests = formats::manifests(&index, options.format)?;
    ensure!(
        manifests
            .iter()
            .all(|(_, bytes)| bytes.len() <= 8 * 1024 * 1024),
        "整合包索引超过大小限制"
    );
    ensure!(
        overrides.len() + extra_files.len() < 50_000,
        "整合包 ZIP 条目超过 50000 个限制"
    );
    let mut temporary = tempfile::Builder::new()
        .prefix(".pcl-pack-export-")
        .tempfile_in(&parent)
        .context("无法创建整合包临时文件")?;
    {
        let mut archive = ZipWriter::new(temporary.as_file_mut());
        let zip_options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, bytes) in manifests {
            archive.start_file(name, zip_options)?;
            archive.write_all(&bytes)?;
        }
        for (position, file) in overrides.iter().enumerate() {
            install::cancelled(cancel)?;
            archive.start_file(
                format!(
                    "{}{}",
                    options.format.payload_prefix(),
                    file.candidate.relative
                ),
                zip_options,
            )?;
            let path = checked_source_path(&instance, &file.candidate.relative)?;
            let hashes = copy_verified(&path, &file.candidate.stamp, &mut archive, cancel)?;
            ensure!(
                hashes == (file.sha1.clone(), file.sha512.clone()),
                "导出期间文件内容发生变化，未提交整合包：{}",
                file.candidate.relative
            );
            progress(Progress {
                completed: (position + 1) as u64,
                total: overrides.len() as u64,
                message: format!("写入整合包：{}", file.candidate.relative),
                ..Default::default()
            });
        }
        for file in &extra_files {
            archive.start_file(
                format!("{}{}", options.format.payload_prefix(), file.relative),
                zip_options.unix_permissions(file.permissions),
            )?;
            file.copy_to(&mut archive, cancel)?;
        }
        archive.finish()?;
    }
    // Re-enumerate and verify after network/ZIP work. Our hidden temporary file
    // is excluded, so saving beside the instance does not masquerade as a user edit.
    ensure!(
        collect(&instance, &rules, cancel)? == tree,
        "导出期间实例文件或目录发生变化，请重试"
    );
    for file in &hashed {
        let path = checked_source_path(&instance, &file.candidate.relative)?;
        let (sha1, sha512, _) = hash_file(&path, &file.candidate.stamp, cancel, false)?;
        ensure!(
            sha1 == file.sha1 && sha512 == file.sha512,
            "导出期间源文件发生变化，未提交整合包：{}",
            file.candidate.relative
        );
    }
    for file in &extra_files {
        file.verify(cancel)?;
    }
    install::cancelled(cancel)?;
    temporary.as_file().sync_all()?;
    let bytes = temporary.as_file().metadata()?.len();
    install::cancelled(cancel)?;
    temporary
        .persist_noclobber(&output)
        .map_err(|error| anyhow::anyhow!("整合包未提交（目标可能已存在）：{}", error.error))?;
    progress(Progress {
        completed: hashed.len() as u64,
        total: hashed.len() as u64,
        message: "整合包导出完成".into(),
        ..Default::default()
    });
    Ok(PackExportReport {
        path: output,
        files: hashed.len() + extra_files.len(),
        hosted_files: index.files.len(),
        override_files: overrides.len() + extra_files.len(),
        unhosted_files: unhosted,
        excluded_sensitive_files: excluded.into_iter().collect(),
        bytes,
    })
}
fn require_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("无法检查整合包保存位置"),
        Ok(_) => bail!("保存位置已存在，未覆盖原文件"),
    }
}

const GAME_SETTINGS: &str = "options.txt|configureddefaults/";
const GAME_PERSONAL: &str = "hotbar.nbt|command_history.txt";
const OPTIFINE: &str = "optionsof.txt|optionsshaders.txt";
const MODS: &str = "mods/|!mods/*.disabled|!mods/*.old|!mods/.connector/|coremods/|lib/";
const DISABLED_MODS: &str = "mods/*.disabled|mods/*.old";
const MOD_CONFIGS: &str = "config/|!config/jei/world/|!config/worldedit/|config/worldedit/worldedit.properties|!config/spark/|config/spark/config.json|defaultconfigs/|journeymap/config/|journeymap/server/|TrashSlotSaveState.json|customfov.txt|gg.essential.mod/|essential/|!essential/*/|!essential/*.jar*|!essential/screenshot-checksum-caches.json|!essential/microsoft_accounts.json|paragliderSettings.nbt|local/client_config.json|local/ftbl.json|local/client/sidebar_buttons.json|local/client/ftbutilities.cfg|local/client/ftblib.cfg|local/client/xencraft.cfg|liteloader.properties|default_reference.xml|CustomSkinLoader/CustomSkinLoader.json|!config/tacz/custom/";
const PACK_DATA: &str = "hotai/|bansoukou/|addons/|multiblocked/|modpack-update-checker/|global_packs/|global_resource_packs/|global_data_packs/|optional_data_packs/|moonlight-global-datapacks/|maps/|icon.png|mods-resourcepacks/|matmos/|resource_assorts/|resource_assorts.json|patchouli_books/|datapacks/|kubejs*/|!kubejs*/probe/|!kubejs*/exported/|!kubejs*/jsconfig.json|!kubejs*/README.txt|openloader/|worldshape/|resources/|scripts/|structures/|fontfiles/|oresources/|packmenu/|craftpresence/|pointblanks/|template*/|!template*/playerdata/|!template*/stats/";
const TACZ: &str = "tacz/|config/tacz/custom/";
const PAINTINGS: &str = "immersive_paintings/";
const MAPS: &str = "journeymap/data/|xaero/|XaeroWaypoints/|XaeroWorldMap/";
const JEI: &str = "config/jei/world/";
const EMI: &str = "emi.json";
const PATCHOULI: &str = "patchouli_data.json";
const SCREENSHOTS: &str = "screenshots/";
const SCHEMATICS: &str = "schematics/";
const REPLAYS: &str = "replay_recordings/|replay_videos/";
const LICENSES: &str = "LICEN*";
const SERVERS: &str = "servers.dat";

struct Rule {
    expression: Regex,
    allow: bool,
    prefix: String,
}
struct Rules {
    ordered: Vec<Rule>,
    selection: ExportSelection,
    resource_items: Option<BTreeSet<String>>,
    shader_items: Option<BTreeSet<String>>,
}
fn pattern_expression(pattern: &str) -> Result<Regex> {
    let pattern = if pattern.ends_with('/') {
        format!("{pattern}*")
    } else {
        pattern.to_owned()
    };
    Ok(Regex::new(&format!(
        "(?i)^{}$",
        regex::escape(&pattern)
            .replace("\\*", ".*")
            .replace("\\?", ".")
    ))?)
}
fn selection_rules(s: &ExportSelection) -> Vec<(bool, &'static str)> {
    vec![
        (s.game_settings, GAME_SETTINGS),
        (s.game_personal, GAME_PERSONAL),
        (s.optifine_settings, OPTIFINE),
        (s.mods, MODS),
        (s.mods && s.disabled_mods, DISABLED_MODS),
        (s.mods && s.pack_data, PACK_DATA),
        (s.mods && s.mod_configs, MOD_CONFIGS),
        (s.mods && s.tacz, TACZ),
        (s.mods && s.paintings, PAINTINGS),
        (s.mods && s.maps, MAPS),
        (s.mods && s.jei_personal, JEI),
        (s.mods && s.emi_personal, EMI),
        (s.mods && s.patchouli_personal, PATCHOULI),
        (s.licenses, LICENSES),
        (s.screenshots, SCREENSHOTS),
        (s.schematics, SCHEMATICS),
        (s.replays, REPLAYS),
        (s.servers, SERVERS),
    ]
}
impl Rules {
    fn new(selection: &ExportSelection) -> Result<Self> {
        validate_pack_items(
            selection.resource_pack_items.as_deref(),
            &["resourcepacks", "texturepacks"],
        )?;
        validate_pack_items(selection.shader_pack_items.as_deref(), &["shaderpacks"])?;
        let mut ordered = Vec::new();
        for (_, patterns) in selection_rules(selection)
            .into_iter()
            .filter(|(selected, _)| *selected)
        {
            for raw in patterns.split('|') {
                let (allow, pattern) = match raw.strip_prefix('!') {
                    Some(pattern) => (false, pattern),
                    None => (true, raw),
                };
                ordered.push(Rule {
                    expression: pattern_expression(pattern)?,
                    allow,
                    prefix: pattern
                        .split(['*', '?'])
                        .next()
                        .unwrap_or_default()
                        .to_lowercase(),
                });
            }
        }
        for world in &selection.worlds {
            metadata::validate_id(world)?;
            let prefix = format!("saves/{world}/");
            // World names are literal, including regex metacharacters.
            ordered.push(Rule {
                expression: Regex::new(&format!("^{}.*$", regex::escape(&prefix)))?,
                allow: true,
                prefix: prefix.to_lowercase(),
            });
        }
        Ok(Self {
            ordered,
            selection: selection.clone(),
            resource_items: selection
                .resource_pack_items
                .as_ref()
                .map(|items| items.iter().cloned().collect()),
            shader_items: selection
                .shader_pack_items
                .as_ref()
                .map(|items| items.iter().cloned().collect()),
        })
    }
    fn pack_group(&self, folder: &str) -> Option<&Option<BTreeSet<String>>> {
        match folder {
            "resourcepacks" | "texturepacks" if self.selection.resource_packs => {
                Some(&self.resource_items)
            }
            "shaderpacks" if self.selection.shader_packs => Some(&self.shader_items),
            _ => None,
        }
    }
    fn pack_selected(&self, folder: &str, name: &str, directory: bool) -> bool {
        if resource_blacklisted(name) {
            return false;
        }
        let Some(items) = self.pack_group(folder) else {
            return false;
        };
        items.as_ref().is_none_or(|items| {
            items.contains(&format!(
                "{folder}/{name}{}",
                if directory { "/" } else { "" }
            ))
        })
    }
    fn relevant_directory(&self, relative: &str) -> bool {
        let directory = format!("{}/", relative.to_lowercase());
        let potential = |rule: &Rule| {
            rule.allow
                && (rule.prefix.starts_with(&directory) || directory.starts_with(&rule.prefix))
        };
        // An excluded subtree is not traversed unless a later positive rule
        // explicitly selects something below it (e.g. worldedit.properties).
        let start = self
            .ordered
            .iter()
            .enumerate()
            .rev()
            .find(|(_, rule)| rule.expression.is_match(&directory) && !rule.allow)
            .map_or(0, |(index, _)| index + 1);
        let static_match = self.ordered[start..].iter().any(potential);
        let parts: Vec<_> = relative.split('/').collect();
        let folder = parts[0].to_ascii_lowercase();
        let dynamic = if let Some(name) = parts.get(1) {
            self.pack_selected(&folder, name, true)
        } else {
            self.pack_group(&folder)
                .is_some_and(|items| items.as_ref().is_none_or(|items| !items.is_empty()))
        };
        static_match || dynamic
    }
    fn allows(&self, relative: &str, root: &Path) -> bool {
        let mut allow = false;
        for rule in &self.ordered {
            if rule.expression.is_match(relative) {
                allow = rule.allow;
            }
        }
        let parts: Vec<_> = relative.split('/').collect();
        let folder = parts[0].to_ascii_lowercase();
        if parts.len() >= 2 {
            allow |= if parts.len() > 2 {
                self.pack_selected(&folder, parts[1], true)
            } else {
                compressed_pack_name(parts[1]) && self.pack_selected(&folder, parts[1], false)
            };
            if folder == "shaderpacks"
                && self.selection.shader_settings
                && parts.len() == 2
                && parts[1].to_lowercase().ends_with(".txt")
            {
                let pack = &relative[..relative.len() - 4];
                // Settings apply only to a pack included in the same export.
                allow |= fs::symlink_metadata(root.join(pack)).is_ok_and(|meta| {
                    !meta.file_type().is_symlink()
                        && (meta.is_dir() || meta.is_file() && compressed_pack_name(pack))
                        && self.pack_selected(
                            &folder,
                            &parts[1][..parts[1].len() - 4],
                            meta.is_dir(),
                        )
                });
            }
        }
        allow
    }
}
fn resource_blacklisted(name: &str) -> bool {
    [
        "quark programmer art.zip",
        "+ euphoriapatches_",
        "pcl2 skin.zip",
    ]
    .iter()
    .any(|part| name.to_lowercase().contains(part))
}

// Must never be overridden by a user content selection. This covers known
// launcher/account files; content scanning below catches common secret fields.
fn without_backup_suffix(mut name: &str) -> &str {
    loop {
        let before = name;
        for suffix in [".bak", ".backup", ".old", ".disabled", ".orig", "~"] {
            if let Some(value) = name.strip_suffix(suffix) {
                name = value;
                break;
            }
        }
        if before == name {
            return name;
        }
    }
}
fn sensitive_path(relative: &str) -> bool {
    let lower = relative.to_lowercase();
    lower.split('/').any(|part| {
        let part = without_backup_suffix(part);
        matches!(
            part,
            "pcl"
                | "pcl-rust"
                | "accounts.json"
                | "microsoft_accounts.json"
                | "launcher_accounts.json"
                | "launcher_profiles.json"
                | "credentials.json"
                | "credentials.toml"
                | "keyring.json"
                | "auth.json"
                | "tokens.json"
                | "access_token"
                | "refresh_token"
                | "usercache.json"
                | "usernamecache.json"
                | ".env"
        ) || part.starts_with(".env.")
    })
}
fn skipped(relative: &str) -> bool {
    let lower = relative.to_lowercase();
    let top = lower.split('/').next().unwrap_or_default();
    matches!(top, "assets" | "versions" | "libraries")
        || lower.split('/').any(|part| {
            matches!(
                part,
                ".git"
                    | ".fabric"
                    | "structurecachev1"
                    | "avatar-cache"
                    | "cosmetic-cache"
                    | ".ds_store"
            ) || part.starts_with(".pcl-pack-export-")
        })
        || lower.ends_with(".log")
        || lower.ends_with(".dat_old")
        || lower.ends_with(".bakacoreinfo")
        || matches!(lower.as_str(), "hmclversion.cfg" | "log4j2.xml")
}
fn plain_directory(path: &Path) -> Result<instances::Identity> {
    let id = instances::identity(path)?;
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "导出源需要普通文件夹：{}",
        path.display()
    );
    Ok(id)
}
fn checked_source_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = metadata::safe_relative(relative)?;
    plain_directory(root)?;
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        instances::identity(&path).context("导出内容包含链接、重解析点或特殊文件")?;
    }
    Ok(path)
}

fn check_selected_pack_sources(
    root: &Path,
    selection: &ExportSelection,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut groups: BTreeMap<&str, Vec<(&str, bool)>> = BTreeMap::new();
    for (enabled, items) in [
        (selection.resource_packs, &selection.resource_pack_items),
        (selection.shader_packs, &selection.shader_pack_items),
    ] {
        if !enabled {
            continue;
        }
        for item in items.iter().flatten() {
            let path = item.strip_suffix('/').unwrap_or(item);
            let (folder, name) = path.split_once('/').context("资源子项缺少所属目录")?;
            groups
                .entry(folder)
                .or_default()
                .push((name, item.ends_with('/')));
        }
    }
    for (folder, items) in groups {
        install::cancelled(cancel)?;
        let parent = checked_source_path(root, folder)
            .with_context(|| format!("所选资源目录已缺失或变更：{folder}"))?;
        plain_directory(&parent)?;
        let mut actual_names = BTreeSet::new();
        for entry in fs::read_dir(&parent)? {
            install::cancelled(cancel)?;
            actual_names.insert(entry?.file_name());
            ensure!(actual_names.len() <= 200_000, "资源目录项目数量过多");
        }
        for (name, directory) in items {
            install::cancelled(cancel)?;
            ensure!(
                actual_names.contains(std::ffi::OsStr::new(name)),
                "所选资源已缺失或名称发生变化：{folder}/{name}"
            );
            let path = checked_source_path(root, &format!("{folder}/{name}"))?;
            if directory {
                plain_directory(&path).context("所选资源文件夹类型已变更")?;
                ensure!(
                    fs::read_dir(&path)?.next().transpose()?.is_some(),
                    "所选资源文件夹已为空：{folder}/{name}"
                );
            } else {
                file_stamp(&path).context("所选资源文件类型已变更")?;
            }
        }
    }
    Ok(())
}

fn collect(root: &Path, rules: &Rules, cancel: &AtomicBool) -> Result<Tree> {
    check_selected_pack_sources(root, &rules.selection, cancel)?;
    for world in &rules.selection.worlds {
        let world = checked_source_path(root, &format!("saves/{world}"))?;
        plain_directory(&world)?;
        file_stamp(&world.join("level.dat")).context("选择的世界没有普通 level.dat 文件")?;
    }
    let mut tree = Tree {
        directories: BTreeMap::new(),
        files: Vec::new(),
        excluded: BTreeSet::new(),
    };
    let mut names = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, relative)) = pending.pop() {
        install::cancelled(cancel)?;
        tree.directories
            .insert(relative.clone(), plain_directory(&directory)?);
        for entry in fs::read_dir(&directory)? {
            install::cancelled(cancel)?;
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("导出目录包含非 UTF-8 文件名"))?;
            let relative = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if skipped(&relative) {
                continue;
            }
            if sensitive_path(&relative) {
                tree.excluded.insert(relative);
                continue;
            }
            let meta = entry.file_type()?;
            let relevant = rules.allows(&relative, root) || rules.relevant_directory(&relative);
            if !relevant {
                continue;
            }
            metadata::safe_relative(&relative)?;
            instances::identity(&entry.path())
                .context("选定导出内容包含符号链接、重解析点或特殊文件")?;
            ensure!(
                names.insert(relative.to_lowercase()),
                "导出路径存在 Windows 大小写冲突：{relative}"
            );
            ensure!(names.len() <= 200_000, "导出文件数量超过 200000 个限制");
            if meta.is_dir() {
                if rules.relevant_directory(&relative) {
                    pending.push((entry.path(), relative));
                }
            } else if rules.allows(&relative, root) {
                tree.files.push(Candidate {
                    relative,
                    stamp: file_stamp(&entry.path())?,
                });
            }
        }
    }
    tree.files
        .sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(tree)
}

/// PCL lists immediate ZIP/RAR files, then nonempty folders (newest first).
/// Values remain literal paths so two same-named packs in different roots differ.
fn available_pack_items(root: &Path, folders: &[&str]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for folder in folders {
        let path = root.join(folder);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("读取资源目录失败"),
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        checked_source_path(root, folder)?;
        let mut files = Vec::new();
        let mut directories = Vec::new();
        let mut names = BTreeSet::new();
        let mut count = 0;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            count += 1;
            ensure!(count <= 200_000, "资源目录项目数量过多");
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("资源项目名称不是 UTF-8"))?;
            let relative = format!("{folder}/{name}");
            if resource_blacklisted(&name) || sensitive_path(&relative) || skipped(&relative) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                continue;
            }
            if kind.is_file() && !compressed_pack_name(&name) {
                continue;
            }
            metadata::safe_relative(&relative)?;
            instances::identity(&entry.path())?;
            ensure!(
                names.insert(name.to_lowercase()),
                "资源项目存在 Windows 大小写冲突：{relative}"
            );
            if kind.is_dir() {
                if fs::read_dir(entry.path())?.next().transpose()?.is_some() {
                    directories.push((entry.metadata()?.modified()?, format!("{relative}/")));
                }
            } else {
                files.push(relative);
            }
        }
        files.sort_by_key(|path| (!path.to_lowercase().ends_with(".zip"), path.clone()));
        directories.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        result.extend(files);
        result.extend(directories.into_iter().map(|(_, path)| path));
    }
    Ok(result)
}

/// A shallow availability scan matching PCL's two-level UI visibility check.
/// This does not hash/read file contents, query services, or recurse into worlds.
pub fn available_selection(root: &Path, id: &str) -> Result<ExportSelection> {
    metadata::validate_id(id)?;
    let version = metadata::resolve_version(root, id)?;
    let instance = config::instance_game_dir(root, id)?;
    plain_directory(&instance)?;
    let libraries = version["libraries"].as_array().cloned().unwrap_or_default();
    let names: Vec<_> = libraries
        .iter()
        .filter_map(|library| library["name"].as_str())
        .collect();
    let modable = game_property(&version, "--fml.neoForgeVersion")?.is_some()
        || game_property(&version, "--fml.forgeVersion")?.is_some()
        || names.iter().any(|name| {
            [
                "net.fabricmc:fabric-loader:",
                "org.quiltmc:quilt-loader:",
                "net.minecraftforge:forge:",
                "net.neoforged:neoforge:",
                "com.mumfrey:liteloader:",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        });
    let optifine = names
        .iter()
        .any(|name| name.to_lowercase().contains("optifine"));
    let mut entries = Vec::new();
    let mut pending = vec![(instance.clone(), String::new(), 0)];
    while let Some((directory, prefix, depth)) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("实例包含非 UTF-8 文件名"))?;
            let relative = format!("{prefix}{name}");
            if skipped(&relative) || sensitive_path(&relative) {
                continue;
            }
            let meta = entry.file_type()?;
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                instances::identity(&entry.path())?;
                if fs::read_dir(entry.path())?.next().is_some() {
                    entries.push(format!("{relative}/"));
                }
                if depth < 1 {
                    pending.push((entry.path(), format!("{relative}/"), depth + 1));
                }
            } else if meta.is_file() {
                entries.push(relative);
            }
            ensure!(
                entries.len() <= 200_000,
                "实例文件数量过多，无法读取可见选项"
            );
        }
    }
    let visible = |patterns: &str| -> Result<bool> {
        for pattern in patterns
            .split('|')
            .filter(|pattern| !pattern.starts_with('!'))
        {
            let regex = pattern_expression(pattern)?;
            if entries.iter().any(|entry| regex.is_match(entry)) {
                return Ok(true);
            }
            if pattern.matches('/').count() >= 2 && !pattern.contains(['*', '?']) {
                let relative = pattern.trim_end_matches('/');
                let path = instance.join(metadata::safe_relative(relative)?);
                if fs::symlink_metadata(&path).is_ok() {
                    checked_source_path(&instance, relative)?;
                    if pattern.ends_with('/') {
                        if fs::read_dir(path)?.next().is_some() {
                            return Ok(true);
                        }
                    } else if path.is_file() {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    };
    let mut worlds = Vec::new();
    let saves = instance.join("saves");
    if fs::symlink_metadata(&saves).is_ok() {
        plain_directory(&saves)?;
        for entry in fs::read_dir(saves)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("世界名称不是 UTF-8"))?;
            if metadata::validate_id(&name).is_err() || !entry.file_type()?.is_dir() {
                continue;
            }
            plain_directory(&entry.path())?;
            if file_stamp(&entry.path().join("level.dat")).is_ok() {
                worlds.push(name);
            }
        }
    }
    worlds.sort();
    Ok(ExportSelection {
        game_settings: visible(GAME_SETTINGS)?,
        game_personal: visible(GAME_PERSONAL)?,
        optifine_settings: optifine && visible(OPTIFINE)?,
        mods: modable && visible(MODS)?,
        disabled_mods: modable && visible(DISABLED_MODS)?,
        mod_configs: modable && visible(MOD_CONFIGS)?,
        pack_data: modable && visible(PACK_DATA)?,
        tacz: modable && visible(TACZ)?,
        paintings: modable && visible(PAINTINGS)?,
        maps: modable && visible(MAPS)?,
        jei_personal: modable && visible(JEI)?,
        emi_personal: modable && visible(EMI)?,
        patchouli_personal: modable && visible(PATCHOULI)?,
        resource_packs: visible("resourcepacks/|texturepacks/")?,
        resource_pack_items: Some(available_pack_items(
            &instance,
            &["resourcepacks", "texturepacks"],
        )?),
        shader_packs: (modable || optifine) && visible("shaderpacks/")?,
        shader_pack_items: Some(available_pack_items(&instance, &["shaderpacks"])?),
        shader_settings: (modable || optifine) && visible("shaderpacks/*.txt")?,
        licenses: visible(LICENSES)?,
        screenshots: visible(SCREENSHOTS)?,
        schematics: visible(SCHEMATICS)?,
        replays: visible(REPLAYS)?,
        servers: visible(SERVERS)?,
        worlds,
    })
}

fn hash_file(
    path: &Path,
    expected: &Stamp,
    cancel: &AtomicBool,
    inspect_secrets: bool,
) -> Result<(String, String, bool)> {
    let text = inspect_secrets
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let lower = name.to_lowercase();
                let name = without_backup_suffix(&lower);
                name.rsplit_once('.').is_some_and(|(_, extension)| {
                    matches!(
                        extension,
                        "json"
                            | "json5"
                            | "toml"
                            | "yaml"
                            | "yml"
                            | "ini"
                            | "properties"
                            | "cfg"
                            | "txt"
                            | "xml"
                    )
                })
            });
    ensure!(
        !text || expected.len <= MAX_TEXT,
        "文本配置超过 16 MiB，无法完整检查凭证：{}",
        path.display()
    );
    let mut contents = Vec::new();
    let (sha1, sha512) = stream_file(path, expected, cancel, |bytes| {
        if text {
            contents.extend_from_slice(bytes);
        }
        Ok(())
    })?;
    let sensitive=text && Regex::new(r#"(?i)[\"']?\b(access[_-]?token|refresh[_-]?token|client[_-]?token|session[_-]?token|client[_-]?secret|authorization|password|api[_-]?key|secret)\b[\"']?\s*[:=]\s*[^\s,}\]]"#)?.is_match(&String::from_utf8_lossy(&contents));
    Ok((sha1, sha512, sensitive))
}
fn copy_verified(
    path: &Path,
    expected: &Stamp,
    writer: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<(String, String)> {
    stream_file(path, expected, cancel, |bytes| {
        writer.write_all(bytes)?;
        Ok(())
    })
}
fn stream_file(
    path: &Path,
    expected: &Stamp,
    cancel: &AtomicBool,
    consume: impl FnMut(&[u8]) -> Result<()>,
) -> Result<(String, String)> {
    ensure!(
        file_stamp(path)? == *expected,
        "导出期间源文件已变化：{}",
        path.display()
    );
    stream_opened(open_source(path)?, path, expected, cancel, consume)
}
fn stream_opened(
    mut file: File,
    path: &Path,
    expected: &Stamp,
    cancel: &AtomicBool,
    mut consume: impl FnMut(&[u8]) -> Result<()>,
) -> Result<(String, String)> {
    ensure!(
        handle_stamp(&file)? == *expected,
        "打开的文件与导出快照不一致"
    );
    let (mut sha1, mut sha512) = (Sha1::new(), Sha512::new());
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        install::cancelled(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .context("导出文件大小溢出")?;
        ensure!(
            total <= expected.len && total <= MAX_FILE,
            "导出期间文件大小变化或超出限制"
        );
        sha1.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
        consume(&buffer[..count])?;
    }
    ensure!(
        total == expected.len
            && handle_stamp(&file)? == *expected
            && file_stamp(path)? == *expected,
        "导出期间源文件已变化：{}",
        path.display()
    );
    Ok((
        format!("{:x}", sha1.finalize()),
        format!("{:x}", sha512.finalize()),
    ))
}
fn is_hosted_candidate(relative: &str) -> bool {
    let lower = relative.to_lowercase();
    [".zip", ".rar", ".jar", ".disabled", ".old"]
        .iter()
        .any(|extension| lower.ends_with(extension))
        && ["mods", "packs", "openloader", "resource"]
            .iter()
            .any(|part| lower.contains(part))
}
fn game_property(version: &serde_json::Value, name: &str) -> Result<Option<String>> {
    let mut result = None;
    for pair in version
        .pointer("/arguments/game")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .windows(2)
    {
        if pair[0].as_str() == Some(name) {
            let value = pair[1].as_str().context("加载器版本参数必须是字符串")?;
            metadata::validate_id(value)?;
            ensure!(
                result.as_deref().is_none_or(|old| old == value),
                "加载器参数 {name} 互相冲突"
            );
            result = Some(value.to_owned());
        }
    }
    Ok(result)
}
fn export_minecraft_id(root: &Path, version: &serde_json::Value) -> Result<String> {
    let jar_id = version["_pcl_jar_id"]
        .as_str()
        .context("无法定位游戏客户端 JAR")?;
    metadata::validate_id(jar_id)?;
    let path = checked_source_path(root, &format!("versions/{jar_id}/{jar_id}.jar"))?;
    let mut jar = zip::ZipArchive::new(open_source(&path)?)
        .context("客户端 JAR 无效，无法确认 Minecraft 依赖")?;
    let mut bytes = Vec::new();
    let minecraft=match jar.by_name("version.json") {
        Ok(entry)=>{
            ensure!(entry.size()<=1024*1024,"客户端 version.json 过大");
            entry.take(1024*1024+1).read_to_end(&mut bytes)?;
            ensure!(bytes.len()<=1024*1024,"客户端 version.json 过大");
            let value:serde_json::Value=serde_json::from_slice(&bytes).context("客户端 version.json 格式无效")?;
            value["id"].as_str().context("客户端 version.json 缺少版本 ID")?.to_owned()
        },
        Err(zip::result::ZipError::FileNotFound)=>game_property(version,"--fml.mcVersion")?.context("旧版客户端没有 version.json，当前无法确认原始 Minecraft ID；未将本地名称当作官方版本导出")?,
        Err(error)=>return Err(error).context("读取客户端版本失败"),
    };
    metadata::validate_id(&minecraft)?;
    if let Some(argument) = game_property(version, "--fml.mcVersion")? {
        ensure!(
            argument == minecraft,
            "加载器的 Minecraft 版本与客户端 JAR 不一致"
        );
    }
    Ok(minecraft)
}
fn dependencies(version: &serde_json::Value, minecraft: &str) -> Result<BTreeMap<String, String>> {
    metadata::validate_id(minecraft)?;
    let mut result = BTreeMap::from([("minecraft".into(), minecraft.into())]);
    let mut loader = None;
    let mut add = |key: &str, value: &str| -> Result<()> {
        ensure!(
            loader.as_deref().is_none_or(|existing| existing == key),
            "版本包含多个不同 Mod 加载器，无法导出为明确依赖"
        );
        loader = Some(key.to_owned());
        metadata::validate_id(value)?;
        if let Some(old) = result.insert(key.into(), value.into()) {
            ensure!(old == value, "版本包含互相冲突的加载器版本");
        }
        Ok(())
    };
    for library in version["libraries"].as_array().into_iter().flatten() {
        let Some(name) = library["name"].as_str() else {
            continue;
        };
        let parts: Vec<_> = name.split(':').collect();
        if parts.len() < 3 {
            continue;
        }
        let key = match (parts[0], parts[1]) {
            ("net.fabricmc", "fabric-loader") => "fabric-loader",
            ("org.quiltmc", "quilt-loader") => "quilt-loader",
            ("net.minecraftforge", "forge") => "forge",
            ("net.neoforged", "neoforge" | "forge") => "neoforge",
            ("optifine", _) | ("com.mumfrey", "liteloader") => {
                bail!("mrpack 导出暂不支持 OptiFine / LiteLoader 依赖，未创建不完整整合包")
            }
            _ => continue,
        };
        let value = if key == "forge" || (parts[0] == "net.neoforged" && parts[1] == "forge") {
            parts[2]
                .strip_prefix(&format!("{minecraft}-"))
                .context("Forge 依赖与 Minecraft 版本不一致")?
        } else {
            parts[2]
        };
        add(key, value)?;
    }
    if let Some(neo) = game_property(version, "--fml.neoForgeVersion")? {
        add("neoforge", &neo)?;
    }
    if let Some(forge) = game_property(version, "--fml.forgeVersion")? {
        // Early NeoForge retained the Forge argument name and changed its group.
        let neo = game_property(version, "--fml.forgeGroup")?.as_deref() == Some("net.neoforged");
        add(if neo { "neoforge" } else { "forge" }, &forge)?;
    }
    if loader.is_none() {
        ensure!(
            matches!(
                version["mainClass"].as_str(),
                Some("net.minecraft.client.main.Main" | "net.minecraft.client.Minecraft")
            ),
            "未知的游戏启动方式，无法安全推断整合包依赖"
        );
    }
    Ok(result)
}

fn trusted(url: &Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}
fn validate_hosted_url(value: &str) -> Result<()> {
    let url = Url::parse(value).context("托管文件 URL 无效")?;
    ensure!(
        trusted(&url, "cdn.modrinth.com")
            && url.query().is_none()
            && url.path().starts_with("/data/"),
        "托管文件不是 Modrinth 官方 CDN HTTPS 地址"
    );
    Ok(())
}
fn lookup_modrinth(hashes: &[String], cancel: &AtomicBool) -> Result<BTreeMap<String, HostedFile>> {
    let client = Client::builder()
        .user_agent(concat!(
            "PCL-Rust/",
            env!("CARGO_PKG_VERSION"),
            " (third-party launcher; pack export)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() < 3 && trusted(attempt.url(), "api.modrinth.com") {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()?;
    let mut results = BTreeMap::new();
    for chunk in hashes.chunks(100) {
        install::cancelled(cancel)?;
        let mut response = client
            .post("https://api.modrinth.com/v2/version_files")
            .json(&serde_json::json!({"hashes":chunk,"algorithm":"sha512"}))
            .send()
            .map_err(|_| anyhow::anyhow!("Modrinth 文件查询网络失败；未自动改为打包所有文件"))?;
        ensure!(
            response.status().is_success(),
            "Modrinth 文件查询失败：HTTP {}",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        let mut buffer = [0; 32 * 1024];
        loop {
            install::cancelled(cancel)?;
            let count = response
                .read(&mut buffer)
                .map_err(|_| anyhow::anyhow!("读取 Modrinth 文件查询响应失败"))?;
            if count == 0 {
                break;
            }
            ensure!(
                bytes.len() + count <= 8 * 1024 * 1024,
                "Modrinth 文件查询响应过大"
            );
            bytes.extend_from_slice(&buffer[..count]);
        }
        let versions: BTreeMap<String, ModrinthVersion> =
            serde_json::from_slice(&bytes).context("Modrinth 文件查询响应格式无效")?;
        for (hash, version) in versions {
            ensure!(chunk.contains(&hash), "Modrinth 返回未请求的文件哈希");
            let file = version
                .files
                .into_iter()
                .find(|file| {
                    file.hashes
                        .get("sha512")
                        .is_some_and(|value| value.eq_ignore_ascii_case(&hash))
                })
                .context("Modrinth 返回的版本没有所查询的文件")?;
            validate_hosted_url(&file.url)?;
            let sha1 = file
                .hashes
                .get("sha1")
                .context("托管文件缺少 SHA1")?
                .clone();
            ensure!(
                sha1.len() == 40 && sha1.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "托管文件 SHA1 格式无效"
            );
            results.insert(
                hash.clone(),
                HostedFile {
                    sha1,
                    sha512: hash,
                    size: file.size,
                    url: file.url,
                },
            );
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::atomic::Ordering;
    use zip::ZipArchive;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            "versions/1.21.1/1.21.1.json",
            serde_json::to_vec(
                &json!({"id":"1.21.1","mainClass":"net.minecraft.client.main.Main","libraries":[]}),
            )
            .unwrap(),
        );
        put(dir.path(),"versions/pack/pack.json",serde_json::to_vec(&json!({"id":"pack","inheritsFrom":"1.21.1","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"}]})).unwrap());
        let mut jar =
            ZipWriter::new(File::create(dir.path().join("versions/1.21.1/1.21.1.jar")).unwrap());
        jar.start_file("version.json", SimpleFileOptions::default())
            .unwrap();
        jar.write_all(br#"{"id":"1.21.1"}"#).unwrap();
        jar.finish().unwrap();
        fs::create_dir_all(dir.path().join("instances/pack")).unwrap();
        dir
    }
    fn put(root: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn options() -> PackExportOptions {
        PackExportOptions {
            name: "Fixture 整合包".into(),
            ..Default::default()
        }
    }
    fn archive(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut output = BTreeMap::new();
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            output.insert(entry.name().to_owned(), bytes);
        }
        output
    }
    fn empty_lookup(_: &[String], _: &AtomicBool) -> Result<BTreeMap<String, HostedFile>> {
        Ok(BTreeMap::new())
    }
    fn no_temporary_files(parent: &Path) {
        assert!(fs::read_dir(parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pcl-pack-export-")
        }));
    }
    fn hosted(bytes: &[u8]) -> HostedFile {
        HostedFile {
            sha1: format!("{:x}", Sha1::digest(bytes)),
            sha512: format!("{:x}", Sha512::digest(bytes)),
            size: bytes.len() as u64,
            url: "https://cdn.modrinth.com/data/fixture/versions/v/file.jar".into(),
        }
    }

    #[test]
    fn defaults_preserve_selected_content_and_exclude_secrets_personal_and_caches() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        let included = [
            "options.txt",
            "mods/test.jar",
            "config/test.toml",
            "config/worldedit/worldedit.properties",
            "scripts/recipe.zs",
            "kubejs/startup_scripts/test.js",
            "resourcepacks/pack.zip",
            "resourcepacks/unpacked/assets/test.json",
            "shaderpacks/test.zip",
            "shaderpacks/test.zip.txt",
            "LICENSE.txt",
            "tacz/gun.json",
        ];
        for relative in included {
            put(&instance, relative, "safe=yes");
        }
        let excluded = [
            "mods/old.jar.disabled",
            "mods/.connector/cache.jar",
            "config/jei/world/one.json",
            "config/worldedit/history.dat",
            "kubejs/probe/metadata.json",
            "essential/microsoft_accounts.json",
            "screenshots/private.png",
            "hotbar.nbt",
            "servers.dat",
            "logs/latest.log",
            "PCL-Rust/settings.json",
            "config/accounts.json",
            "config/coolmod.json",
            "saves/World/level.dat",
        ];
        for relative in excluded {
            put(
                &instance,
                relative,
                if relative == "config/coolmod.json" {
                    "{\"accessToken\":\"synthetic-secret\"}"
                } else {
                    "private"
                },
            );
        }
        let output = dir.path().join("default.mrpack");
        let report = export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        let files = archive(&output);
        for relative in included {
            assert!(
                files.contains_key(&format!("overrides/{relative}")),
                "missing {relative}"
            );
            assert_eq!(fs::read(instance.join(relative)).unwrap(), b"safe=yes");
        }
        for relative in excluded {
            assert!(
                !files.contains_key(&format!("overrides/{relative}")),
                "unexpected {relative}"
            );
            assert!(instance.join(relative).is_file());
        }
        assert!(report
            .excluded_sensitive_files
            .contains(&"config/coolmod.json".into()));
        assert!(report
            .excluded_sensitive_files
            .contains(&"config/accounts.json".into()));
        assert!(report.unhosted_files.contains(&"mods/test.jar".into()));
        let info = crate::modpack::inspect_mrpack(&output).unwrap();
        assert_eq!(info.minecraft, "1.21.1");
        assert_eq!(info.dependencies["fabric-loader"], "0.16.0");
        no_temporary_files(dir.path());
    }

    #[test]
    fn hosted_exact_file_preserves_local_name_and_hashes_while_unknown_is_explicit_override() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        put(&instance, "mods/renamed.jar", b"hosted-fixture");
        put(&instance, "mods/private.jar", b"unknown-fixture");
        put(&instance, "config/test.cfg", b"enabled=true");
        let remote = hosted(b"hosted-fixture");
        let output = dir.path().join("hosted.mrpack");
        let report = export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            |hashes, _| {
                assert_eq!(hashes.len(), 2);
                assert!(hashes.contains(&remote.sha512));
                Ok(BTreeMap::from([(remote.sha512.clone(), remote.clone())]))
            },
        )
        .unwrap();
        let files = archive(&output);
        let index: Value = serde_json::from_slice(&files["modrinth.index.json"]).unwrap();
        assert_eq!(index["files"][0]["path"], "mods/renamed.jar");
        assert_eq!(index["files"][0]["hashes"]["sha1"], remote.sha1);
        assert_eq!(index["files"][0]["hashes"]["sha512"], remote.sha512);
        assert!(!files.contains_key("overrides/mods/renamed.jar"));
        assert_eq!(files["overrides/mods/private.jar"], b"unknown-fixture");
        assert_eq!(report.hosted_files, 1);
        assert_eq!(report.override_files, 2);
        assert_eq!(report.unhosted_files, vec!["mods/private.jar"]);
        crate::modpack::inspect_mrpack(&output).unwrap();
    }

    #[test]
    fn explicit_embedding_never_queries_and_worlds_require_exact_selection() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        put(&instance, "mods/test.jar", b"local");
        put(&instance, "saves/World [1]/level.dat", b"nbt");
        put(&instance, "saves/World [1]/region/r.0.0.mca", b"region");
        put(&instance, "saves/Other/level.dat", b"other");
        put(&instance, "hotbar.nbt", b"hotbar");
        let mut options = options();
        options.resource_mode = ResourceMode::EmbedAll;
        options.selection.worlds = vec!["World [1]".into()];
        options.selection.game_personal = true;
        let output = dir.path().join("embedded.mrpack");
        let report = export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            |_, _| panic!("EmbedAll must not send hashes"),
        )
        .unwrap();
        let files = archive(&output);
        assert!(files.contains_key("overrides/saves/World [1]/region/r.0.0.mca"));
        assert!(files.contains_key("overrides/hotbar.nbt"));
        assert!(!files.contains_key("overrides/saves/Other/level.dat"));
        assert!(report.unhosted_files.is_empty());
        options.selection.worlds = vec!["Missing".into()];
        assert!(export_with(
            dir.path(),
            "pack",
            &dir.path().join("missing.mrpack"),
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup
        )
        .is_err());
    }

    #[test]
    fn network_failure_hash_mismatch_and_untrusted_url_never_commit() {
        let dir = fixture();
        put(
            &dir.path().join("instances/pack"),
            "mods/test.jar",
            b"original",
        );
        let original = hosted(b"original");
        for mode in 0..4 {
            let output = dir.path().join(format!("failure-{mode}.mrpack"));
            let result = export_with(
                dir.path(),
                "pack",
                &output,
                &options(),
                &AtomicBool::new(false),
                |_| {},
                |_, _| {
                    if mode == 0 {
                        bail!("simulated offline");
                    }
                    let mut remote = original.clone();
                    match mode {
                        1 => remote.sha1 = "0".repeat(40),
                        2 => remote.url = "https://attacker.invalid/file.jar".into(),
                        _ => remote.size += 1,
                    }
                    Ok(BTreeMap::from([(remote.sha512.clone(), remote)]))
                },
            );
            assert!(result.is_err());
            assert!(!output.exists());
            no_temporary_files(dir.path());
        }
        assert_eq!(
            fs::read(dir.path().join("instances/pack/mods/test.jar")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn cancellation_after_lookup_preserves_type_and_never_commits() {
        let dir = fixture();
        put(
            &dir.path().join("instances/pack"),
            "mods/test.jar",
            b"original",
        );
        let cancel = AtomicBool::new(false);
        let output = dir.path().join("cancel.mrpack");
        let error = export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &cancel,
            |_| {},
            |_, cancel| {
                cancel.store(true, Ordering::Relaxed);
                Ok(BTreeMap::new())
            },
        )
        .unwrap_err();
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<crate::model::OperationCancelled>()
                .is_some()
        }));
        assert!(!output.exists());
        no_temporary_files(dir.path());
    }

    #[test]
    fn source_mutation_and_new_file_during_lookup_abort_without_reverting_user_changes() {
        for add_new in [false, true] {
            let dir = fixture();
            let instance = dir.path().join("instances/pack");
            put(&instance, "mods/test.jar", b"original");
            let output = dir.path().join("changed.mrpack");
            let result = export_with(
                dir.path(),
                "pack",
                &output,
                &options(),
                &AtomicBool::new(false),
                |_| {},
                |_, _| {
                    put(
                        &instance,
                        if add_new {
                            "mods/new.jar"
                        } else {
                            "mods/test.jar"
                        },
                        b"new user content",
                    );
                    Ok(BTreeMap::new())
                },
            );
            assert!(result.is_err());
            assert!(!output.exists());
            assert_eq!(
                fs::read(instance.join(if add_new {
                    "mods/new.jar"
                } else {
                    "mods/test.jar"
                }))
                .unwrap(),
                b"new user content"
            );
            no_temporary_files(dir.path());
        }
    }

    #[test]
    fn existing_and_racing_destination_remain_untouched() {
        let dir = fixture();
        let output = dir.path().join("present.mrpack");
        fs::write(&output, b"existing").unwrap();
        assert!(export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            |_, _| panic!("no network before target rejection")
        )
        .is_err());
        assert_eq!(fs::read(&output).unwrap(), b"existing");
        let race = dir.path().join("race.mrpack");
        put(
            &dir.path().join("instances/pack"),
            "mods/test.jar",
            b"original",
        );
        assert!(export_with(
            dir.path(),
            "pack",
            &race,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            |_, _| {
                fs::write(&race, b"other writer")?;
                Ok(BTreeMap::new())
            }
        )
        .is_err());
        assert_eq!(fs::read(race).unwrap(), b"other writer");
        no_temporary_files(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn selected_symlinks_and_symlink_worlds_are_refused() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        put(dir.path(), "private.txt", b"secret");
        fs::create_dir_all(instance.join("config")).unwrap();
        symlink(
            dir.path().join("private.txt"),
            instance.join("config/linked.txt"),
        )
        .unwrap();
        let output = dir.path().join("unsafe.mrpack");
        assert!(export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            empty_lookup
        )
        .is_err());
        assert!(!output.exists());
        fs::remove_file(instance.join("config/linked.txt")).unwrap();
        put(dir.path(), "external-world/level.dat", b"world");
        fs::create_dir_all(instance.join("saves")).unwrap();
        symlink(
            dir.path().join("external-world"),
            instance.join("saves/Linked"),
        )
        .unwrap();
        let mut options = options();
        options.selection.worlds = vec!["Linked".into()];
        assert!(export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup
        )
        .is_err());
        symlink(dir.path().join("private.txt"), &output).unwrap();
        assert!(export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup
        )
        .is_err());
        assert_eq!(fs::read(dir.path().join("private.txt")).unwrap(), b"secret");
    }

    #[test]
    fn loader_dependencies_are_exact_and_unknown_or_conflicting_profiles_fail() {
        for (coordinate, key, value) in [
            (
                "net.fabricmc:fabric-loader:0.16.0",
                "fabric-loader",
                "0.16.0",
            ),
            ("org.quiltmc:quilt-loader:0.26.4", "quilt-loader", "0.26.4"),
            (
                "net.minecraftforge:forge:1.21.1-52.1.16",
                "forge",
                "52.1.16",
            ),
            ("net.neoforged:neoforge:21.1.255", "neoforge", "21.1.255"),
        ] {
            let profile = json!({"_pcl_jar_id":"1.21.1","libraries":[{"name":coordinate}]});
            assert_eq!(dependencies(&profile, "1.21.1").unwrap()[key], value);
        }
        let conflicts = json!({"_pcl_jar_id":"1.21.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.0"},{"name":"net.neoforged:neoforge:21.1.255"}]});
        assert!(dependencies(&conflicts, "1.21.1").is_err());
        assert!(dependencies(
            &json!({"_pcl_jar_id":"1.21.1","mainClass":"custom.Unknown","libraries":[]}),
            "1.21.1"
        )
        .is_err());
        assert!(dependencies(&json!({"_pcl_jar_id":"1.21.1","libraries":[{"name":"net.minecraftforge:forge:1.20.1-47.1.0"}]}), "1.21.1").is_err());
        assert!(dependencies(&json!({"_pcl_jar_id":"1.21.1","libraries":[{"name":"optifine:OptiFine:1.21.1_HD_U_J1"}]}), "1.21.1").is_err());
    }

    #[test]
    fn available_options_are_shallow_and_respect_loader_requirements() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        put(&instance, "options.txt", b"settings");
        put(&instance, "config/mod/options.toml", b"config");
        put(&instance, "saves/World/level.dat", b"world");
        put(&instance, "resourcepacks/a.zip", b"pack");
        put(&instance, "mods/a.jar", b"mod");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // A recursive scan would visit and reject this path. Availability
            // must only inspect the direct world's level.dat metadata.
            symlink("missing", instance.join("saves/World/deep-link")).unwrap();
        }
        let available = available_selection(dir.path(), "pack").unwrap();
        assert!(
            available.game_settings
                && available.mod_configs
                && available.resource_packs
                && available.mods
        );
        assert!(!available.game_personal && !available.maps && !available.optifine_settings);
        assert_eq!(available.worlds, vec!["World"]);
        put(dir.path(), "versions/1.21.1/mods/a.jar", b"stray mod");
        assert!(!available_selection(dir.path(), "1.21.1").unwrap().mods);
    }

    #[test]
    fn config_validation_rejects_unsupported_fields_and_nonportable_world_names() {
        let mut options = options();
        for world in ["../outside", "C:drive", "CON", "trailing."] {
            options.selection.worlds = vec![world.into()];
            assert!(validate_export_options(&options).is_err());
        }
        options.selection.worlds = vec!["World".into(), "world".into()];
        assert!(validate_export_options(&options).is_err());
        assert!(serde_json::from_value::<PackExportOptions>(
            json!({"name":"test","include_credentials":true})
        )
        .is_err());
        assert!(serde_json::from_value::<PackExportOptions>(
            json!({"name":"test","selection":{"extra_paths":["/private"]}})
        )
        .is_err());
        let roundtrip: PackExportOptions =
            serde_json::from_slice(&serde_json::to_vec(&super::tests::options()).unwrap()).unwrap();
        assert_eq!(roundtrip.selection, ExportSelection::default());
    }

    #[test]
    fn pack_items_list_immediate_archives_and_nonempty_folders_without_blacklisted_items() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        for name in [
            "resourcepacks/A [1].ZIP",
            "resourcepacks/B.rar",
            "resourcepacks/notes.txt",
            "resourcepacks/folder/assets/test.json",
            "resourcepacks/Quark Programmer Art.zip",
            "resourcepacks/PCL2 Skin.zip",
            "resourcepacks/+ EuphoriaPatches_test/data.txt",
            "texturepacks/A [1].ZIP",
            "shaderpacks/Example.zip",
            "shaderpacks/Example.zip.txt",
            "shaderpacks/unpacked/shaders/test.fsh",
        ] {
            put(&instance, name, b"safe");
        }
        fs::create_dir_all(instance.join("resourcepacks/empty")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("A [1].ZIP", instance.join("resourcepacks/link.zip")).unwrap();
        let available = available_selection(dir.path(), "pack").unwrap();
        assert_eq!(
            available.resource_pack_items.unwrap(),
            vec![
                "resourcepacks/A [1].ZIP",
                "resourcepacks/B.rar",
                "resourcepacks/folder/",
                "texturepacks/A [1].ZIP",
            ]
        );
        assert_eq!(
            available.shader_pack_items.unwrap(),
            vec!["shaderpacks/Example.zip", "shaderpacks/unpacked/"]
        );
    }

    #[test]
    fn explicit_resource_items_are_literal_and_shader_settings_follow_only_selected_packs() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        let included = [
            "resourcepacks/A [1].zip",
            "texturepacks/A [1].zip",
            "resourcepacks/unpacked/assets/safe.json",
            "shaderpacks/Selected.zip",
            "shaderpacks/Selected.zip.txt",
            "shaderpacks/folder/shaders/main.fsh",
            "shaderpacks/folder.txt",
        ];
        let excluded = [
            "resourcepacks/A 1.zip",
            "resourcepacks/Other.rar",
            "resourcepacks/unpacked/accounts.json.bak",
            "resourcepacks/unpacked/private.json",
            "shaderpacks/Other.zip",
            "shaderpacks/Other.zip.txt",
            "shaderpacks/orphan.zip.txt",
        ];
        for relative in included.into_iter().chain(excluded) {
            put(
                &instance,
                relative,
                if relative.ends_with("private.json") {
                    br#"{"refresh_token":"synthetic-secret"}"#.as_slice()
                } else {
                    b"safe"
                },
            );
        }
        let mut options = options();
        options.resource_mode = ResourceMode::EmbedAll;
        options.selection.resource_pack_items = Some(vec![
            "resourcepacks/A [1].zip".into(),
            "texturepacks/A [1].zip".into(),
            "resourcepacks/unpacked/".into(),
        ]);
        options.selection.shader_pack_items = Some(vec![
            "shaderpacks/Selected.zip".into(),
            "shaderpacks/folder/".into(),
        ]);
        let output = dir.path().join("selected.mrpack");
        let report = export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            |_, _| panic!("explicit embedding must not query"),
        )
        .unwrap();
        let files = archive(&output);
        for relative in included {
            assert_eq!(files[&format!("overrides/{relative}")], b"safe");
        }
        for relative in excluded {
            assert!(!files.contains_key(&format!("overrides/{relative}")));
            assert!(instance.join(relative).exists());
        }
        assert!(report
            .excluded_sensitive_files
            .contains(&"resourcepacks/unpacked/private.json".into()));
        options.selection.shader_settings = false;
        let output = dir.path().join("no-settings.mrpack");
        export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        let files = archive(&output);
        assert!(files.contains_key("overrides/shaderpacks/Selected.zip"));
        assert!(!files.contains_key("overrides/shaderpacks/Selected.zip.txt"));
        assert!(!files.contains_key("overrides/shaderpacks/folder.txt"));
    }

    #[test]
    fn legacy_export_configs_keep_all_items_but_explicit_empty_and_master_switch_are_respected() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        put(&instance, "resourcepacks/A.zip", b"safe");
        put(&instance, "shaderpacks/A.zip", b"safe");
        put(&instance, "shaderpacks/A.zip.txt", b"safe");
        let mut options: PackExportOptions = serde_json::from_value(json!({
            "name":"old export", "selection":{"resource_packs":true,"shader_packs":true},
            "resource_mode":"EmbedAll"
        }))
        .unwrap();
        assert!(options.selection.resource_pack_items.is_none());
        assert!(options.selection.shader_pack_items.is_none());
        let output = dir.path().join("old.mrpack");
        export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        assert_eq!(archive(&output).len(), 4);
        options.selection.resource_pack_items = Some(vec![]);
        options.selection.shader_pack_items = Some(vec![]);
        let output = dir.path().join("empty.mrpack");
        export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        assert_eq!(archive(&output).len(), 1);
        options.selection.resource_packs = false;
        options.selection.resource_pack_items = Some(vec!["resourcepacks/Missing.zip".into()]);
        let saved = serde_json::to_vec(&options).unwrap();
        let restored: PackExportOptions = serde_json::from_slice(&saved).unwrap();
        assert_eq!(restored.selection, options.selection);
        let output = dir.path().join("master-off.mrpack");
        export_with(
            dir.path(),
            "pack",
            &output,
            &restored,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        assert_eq!(archive(&output).len(), 1);
    }

    #[test]
    fn selected_resource_paths_reject_escape_ambiguous_case_and_non_pack_children() {
        let mut options = options();
        for path in [
            "../outside.zip",
            "resourcepacks/../outside.zip",
            "resourcepacks/deep/pack.zip",
            "shaderpacks/a.zip",
            "resourcepacks/a.zip/extra/",
            "resourcepacks/notes.txt",
            "resourcepacks/PCL2 Skin.zip",
            "resourcepacks/accounts.json/",
            "resourcepacks/CON.zip",
            "resourcepacks/a.zip//",
            "resourcepacks/a*.zip",
            "resourcepacks\\a.zip",
        ] {
            options.selection.resource_pack_items = Some(vec![path.into()]);
            assert!(validate_export_options(&options).is_err(), "{path}");
        }
        options.selection.resource_pack_items = Some(vec![
            "resourcepacks/A.zip".into(),
            "resourcepacks/a.zip".into(),
        ]);
        assert!(validate_export_options(&options).is_err());
        options.selection.resource_pack_items = Some(vec![
            "resourcepacks/A.zip".into(),
            "texturepacks/A.zip".into(),
        ]);
        validate_export_options(&options).unwrap();
    }

    #[test]
    fn selected_resource_missing_type_changes_and_links_fail_without_output() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        let mut options = options();
        options.resource_mode = ResourceMode::EmbedAll;
        options.selection.resource_pack_items = Some(vec!["resourcepacks/pack.zip".into()]);
        let output = dir.path().join("missing.mrpack");
        let run = |options: &PackExportOptions| {
            export_with(
                dir.path(),
                "pack",
                &output,
                options,
                &AtomicBool::new(false),
                |_| {},
                empty_lookup,
            )
        };
        assert!(run(&options).is_err());
        fs::create_dir_all(instance.join("resourcepacks/pack.zip")).unwrap();
        put(&instance, "resourcepacks/pack.zip/data", b"safe");
        assert!(run(&options).is_err());
        fs::remove_dir_all(instance.join("resourcepacks/pack.zip")).unwrap();
        put(&instance, "resourcepacks/pack.zip", b"safe");
        options.selection.resource_pack_items = Some(vec!["resourcepacks/pack.zip/".into()]);
        assert!(run(&options).is_err());
        options.selection.resource_pack_items = Some(vec!["resourcepacks/PACK.zip".into()]);
        assert!(run(&options).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("pack.zip", instance.join("resourcepacks/link.zip"))
                .unwrap();
            options.selection.resource_pack_items = Some(vec!["resourcepacks/link.zip".into()]);
            assert!(run(&options).is_err());
        }
        assert!(!output.exists());
        assert_eq!(
            fs::read(instance.join("resourcepacks/pack.zip")).unwrap(),
            b"safe"
        );
        no_temporary_files(dir.path());
    }

    #[test]
    fn selected_pack_or_associated_settings_change_during_lookup_never_commits() {
        for remove_pack in [false, true] {
            let dir = fixture();
            let instance = dir.path().join("instances/pack");
            put(&instance, "shaderpacks/pack.zip", b"safe");
            put(&instance, "shaderpacks/pack.zip.txt", b"setting=before");
            let mut options = options();
            options.selection.shader_pack_items = Some(vec!["shaderpacks/pack.zip".into()]);
            let output = dir.path().join("changed.mrpack");
            let result = export_with(
                dir.path(),
                "pack",
                &output,
                &options,
                &AtomicBool::new(false),
                |_| {},
                |_, _| {
                    if remove_pack {
                        fs::remove_file(instance.join("shaderpacks/pack.zip")).unwrap();
                    } else {
                        put(&instance, "shaderpacks/pack.zip.txt", b"setting=after");
                    }
                    Ok(BTreeMap::new())
                },
            );
            assert!(result.is_err());
            assert!(!output.exists());
            assert_eq!(
                fs::read(instance.join("shaderpacks/pack.zip.txt")).unwrap(),
                if remove_pack {
                    b"setting=before".as_slice()
                } else {
                    b"setting=after"
                }
            );
            no_temporary_files(dir.path());
        }
    }

    #[test]
    fn isolation_setting_selects_the_same_game_directory_as_launch() {
        let dir = fixture();
        put(dir.path(), "options.txt", b"shared settings");
        put(
            &dir.path().join("instances/pack"),
            "options.txt",
            b"isolated settings",
        );
        config::save_instance_settings(
            dir.path(),
            "pack",
            &config::InstanceSettings {
                isolated: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut options = options();
        options.resource_mode = ResourceMode::EmbedAll;
        let output = dir.path().join("shared.mrpack");
        export_with(
            dir.path(),
            "pack",
            &output,
            &options,
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        assert_eq!(
            archive(&output)["overrides/options.txt"],
            b"shared settings"
        );
        assert_eq!(
            fs::read(dir.path().join("instances/pack/options.txt")).unwrap(),
            b"isolated settings"
        );
    }

    #[test]
    fn credential_backups_and_common_oauth_secret_fields_are_excluded() {
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        let sensitive = [
            ("config/accounts.json.bak", r#"{"refresh_token":"fixture"}"#),
            (
                "config/credentials.json.old",
                r#"{"refresh_token":"fixture"}"#,
            ),
            ("config/discord.json", r#"{"client_secret":"fixture"}"#),
            ("config/oauth.toml.backup", r#"clientSecret = "fixture""#),
            ("config/session.cfg", r#"session_token="fixture""#),
        ];
        for (name, contents) in sensitive {
            put(&instance, name, contents);
        }
        let output = dir.path().join("secrets.mrpack");
        let report = export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            |_, _| panic!("no resource query"),
        )
        .unwrap();
        let files = archive(&output);
        for (name, _) in sensitive {
            assert!(
                report.excluded_sensitive_files.contains(&name.into()),
                "{name}"
            );
            assert!(!files.contains_key(&format!("overrides/{name}")));
        }
    }

    #[test]
    fn substituted_open_handle_is_rejected_before_any_bytes_reach_hash_consumer() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let other = dir.path().join("other.txt");
        fs::write(&source, b"ordinary").unwrap();
        fs::write(&other, b"private!").unwrap();
        let expected = file_stamp(&source).unwrap();
        let mut consumed = 0;
        assert!(stream_opened(
            open_source(&other).unwrap(),
            &source,
            &expected,
            &AtomicBool::new(false),
            |bytes| {
                consumed += bytes.len();
                Ok(())
            }
        )
        .is_err());
        assert_eq!(consumed, 0);
    }

    #[test]
    fn renamed_vanilla_uses_embedded_client_id_and_neoforge_arguments_are_exact() {
        let dir = fixture();
        fs::create_dir_all(dir.path().join("versions/Renamed Game")).unwrap();
        fs::copy(
            dir.path().join("versions/1.21.1/1.21.1.jar"),
            dir.path().join("versions/Renamed Game/Renamed Game.jar"),
        )
        .unwrap();
        let renamed =
            json!({"_pcl_jar_id":"Renamed Game","mainClass":"net.minecraft.client.main.Main"});
        let minecraft = export_minecraft_id(dir.path(), &renamed).unwrap();
        assert_eq!(minecraft, "1.21.1");
        assert_eq!(
            dependencies(&renamed, &minecraft).unwrap()["minecraft"],
            "1.21.1"
        );
        let neo = json!({"_pcl_jar_id":"1.21.1","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher","arguments":{"game":["--fml.neoForgeVersion","21.1.255","--fml.mcVersion","1.21.1"]},"libraries":[]});
        assert_eq!(
            dependencies(&neo, &export_minecraft_id(dir.path(), &neo).unwrap()).unwrap()
                ["neoforge"],
            "21.1.255"
        );
        let mut conflicting = neo.clone();
        conflicting["libraries"] = json!([{"name":"net.neoforged:neoforge:21.1.254"}]);
        assert!(dependencies(&conflicting, "1.21.1").is_err());
        conflicting["arguments"]["game"] = json!(["--fml.mcVersion", "1.20.1"]);
        assert!(export_minecraft_id(dir.path(), &conflicting).is_err());
        put(dir.path(),"versions/pack/pack.json",serde_json::to_vec(&json!({"id":"pack","inheritsFrom":"1.21.1","arguments":{"game":["--fml.neoForgeVersion","21.1.255","--fml.mcVersion","1.21.1"]}})).unwrap());
        put(&dir.path().join("instances/pack"), "mods/test.jar", b"test");
        assert!(available_selection(dir.path(), "pack").unwrap().mods);
        let mut old = ZipWriter::new(
            File::create(dir.path().join("versions/Renamed Game/Renamed Game.jar")).unwrap(),
        );
        old.start_file("legacy.class", SimpleFileOptions::default())
            .unwrap();
        old.write_all(b"old").unwrap();
        old.finish().unwrap();
        assert!(export_minecraft_id(dir.path(), &renamed).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn excluded_cache_subtrees_are_not_traversed_but_later_positive_rules_work() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let instance = dir.path().join("instances/pack");
        for folder in [
            "mods/.connector",
            "kubejs/probe",
            "config/jei/world",
            "config/worldedit/history",
        ] {
            fs::create_dir_all(instance.join(folder)).unwrap();
            symlink("missing", instance.join(folder).join("link")).unwrap();
        }
        put(
            &instance,
            "config/worldedit/worldedit.properties",
            b"enabled=true",
        );
        let output = dir.path().join("pruned.mrpack");
        export_with(
            dir.path(),
            "pack",
            &output,
            &options(),
            &AtomicBool::new(false),
            |_| {},
            empty_lookup,
        )
        .unwrap();
        let files = archive(&output);
        assert_eq!(files.len(), 2);
        assert!(files.contains_key("overrides/config/worldedit/worldedit.properties"));
    }
    #[test]
    fn common_zip_formats_roundtrip_selected_payload_without_network() {
        let root = fixture();
        put(root.path(), "instances/pack/options.txt", b"music:0.5");
        for format in [PackFormat::MultiMc, PackFormat::Mcbbs] {
            let output = root.path().join(format!("{format:?}.zip"));
            let mut value = options();
            value.format = format;
            export_with(
                root.path(),
                "pack",
                &output,
                &value,
                &AtomicBool::new(false),
                |_| {},
                |_, _| panic!("embedded formats must not query hosted services"),
            )
            .unwrap();
            let info = crate::modpack::inspect_mrpack(&output).unwrap();
            assert_eq!(info.dependencies["fabric-loader"], "0.16.0");
            let target = root.path().join(format!("import-{format:?}"));
            crate::modpack::import_mrpack(&output, &target, false, &AtomicBool::new(false), |_| {})
                .unwrap();
            assert_eq!(fs::read(target.join("options.txt")).unwrap(), b"music:0.5");
            assert!(!target.join("instance.cfg").exists());
        }
        let mut value = options();
        value.format = PackFormat::Hmcl;
        let output = root.path().join("hmcl.zip");
        assert!(export_with(
            root.path(),
            "pack",
            &output,
            &value,
            &AtomicBool::new(false),
            |_| {},
            |_, _| panic!()
        )
        .is_err());
        assert!(!output.exists());
        put(
            root.path(),
            "versions/pack/pack.json",
            br#"{"id":"pack","inheritsFrom":"1.21.1"}"#,
        );
        export_with(
            root.path(),
            "pack",
            &output,
            &value,
            &AtomicBool::new(false),
            |_| {},
            |_, _| panic!(),
        )
        .unwrap();
        assert_eq!(
            crate::modpack::inspect_mrpack(&output).unwrap().format,
            "HMCL"
        );
    }
    #[test]
    fn explicit_version_java_and_launcher_bundle_roundtrip_without_executing_programs() {
        let root = fixture();
        put(root.path(), "instances/pack/options.txt", b"music:0.5");
        put(
            root.path(),
            "versions/pack/jre/release",
            b"JAVA_VERSION=\"21.0.7\"",
        );
        put(
            root.path(),
            "versions/pack/jre/bin/java",
            b"not an executable fixture",
        );
        put(
            root.path(),
            "versions/pack/jre/legal/LICENSE",
            b"fixture licence",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.path().join("versions/pack/jre/bin/java"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let mut settings = config::load_instance_settings(root.path(), "pack").unwrap();
        settings.java_mode = Some(crate::java_selection::JavaSelectionMode::VersionFolder);
        config::save_instance_settings(root.path(), "pack", &settings).unwrap();
        assert_eq!(available_java_roots(root.path(), "pack").unwrap().len(), 1);
        let mut value = options();
        value.include_java = true;
        value.include_launcher = true;
        value.resource_mode = ResourceMode::EmbedAll;
        let launcher = root.path().join("launcher-fixture");
        fs::write(&launcher, b"synthetic launcher, never executed").unwrap();
        let output = root.path().join("bundle.zip");
        export_pack_with_launcher(
            root.path(),
            "pack",
            &output,
            &value,
            Some(&launcher),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let files = archive(&output);
        assert!(files.contains_key("modpack.mrpack"));
        assert!(files.contains_key("使用说明.txt"));
        let info = crate::modpack::inspect_mrpack(&output).unwrap();
        assert!(info.warnings.iter().any(|s| s.contains("不会提取或执行")));
        let target = root.path().join("import");
        crate::modpack::import_mrpack(&output, &target, false, &AtomicBool::new(false), |_| {})
            .unwrap();
        assert_eq!(
            fs::read(target.join("jre/bin/java")).unwrap(),
            b"not an executable fixture"
        );
        assert!(!target.join("PCL-Rust").exists());
        assert!(!target.join("settings.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(target.join("jre/bin/java"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        let before = fs::read(&output).unwrap();
        assert!(export_pack_with_launcher(
            root.path(),
            "pack",
            &output,
            &value,
            Some(&launcher),
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        assert_eq!(fs::read(&output).unwrap(), before);
        let cancelled = root.path().join("cancelled.zip");
        assert!(export_pack_with_launcher(
            root.path(),
            "pack",
            &cancelled,
            &value,
            Some(&launcher),
            &AtomicBool::new(true),
            |_| {}
        )
        .unwrap_err()
        .is::<crate::model::OperationCancelled>());
        assert!(!cancelled.exists());
    }
    #[cfg(unix)]
    #[test]
    fn runtime_links_are_materialized_only_inside_selected_runtime() {
        use std::os::unix::fs::symlink;
        let root = fixture();
        put(
            root.path(),
            "versions/pack/jre/release",
            b"JAVA_VERSION=\"21\"",
        );
        put(root.path(), "versions/pack/jre/bin/java", b"java fixture");
        put(root.path(), "versions/pack/jre/lib/original", b"library");
        symlink("original", root.path().join("versions/pack/jre/lib/link")).unwrap();
        let mut settings = config::load_instance_settings(root.path(), "pack").unwrap();
        settings.java_mode = Some(crate::java_selection::JavaSelectionMode::VersionFolder);
        config::save_instance_settings(root.path(), "pack", &settings).unwrap();
        let mut value = options();
        value.include_java = true;
        value.resource_mode = ResourceMode::EmbedAll;
        let output = root.path().join("java.mrpack");
        export_pack(
            root.path(),
            "pack",
            &output,
            &value,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(archive(&output)["overrides/jre/lib/link"], b"library");
        symlink(
            root.path().join("versions/pack/pack.json"),
            root.path().join("versions/pack/jre/outside"),
        )
        .unwrap();
        let rejected = root.path().join("outside.mrpack");
        assert!(export_pack(
            root.path(),
            "pack",
            &rejected,
            &value,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        assert!(!rejected.exists());
    }
    #[test]
    fn launcher_bundle_rejects_local_only_system_fonts_before_creating_archive() {
        let root = fixture();
        let app = root.path().join("Current.app");
        put(&app, "Contents/Info.plist", b"fixture");
        put(
            &app,
            "Contents/Resources/PingFang-Regular.otf",
            b"local-only fixture font",
        );
        let mut value = options();
        value.include_launcher = true;
        let output = root.path().join("not-redistributable.zip");
        assert!(export_pack_with_launcher(
            root.path(),
            "pack",
            &output,
            &value,
            Some(&app),
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap_err()
        .to_string()
        .contains("仅本机"));
        assert!(!output.exists());
        assert_eq!(
            fs::read(app.join("Contents/Resources/PingFang-Regular.otf")).unwrap(),
            b"local-only fixture font"
        );
    }
}
