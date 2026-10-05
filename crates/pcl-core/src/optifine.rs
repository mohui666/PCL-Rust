//! Official OptiFine downloads and isolated installer execution. The publisher
//! provides no SHA1 manifest; locally derived hashes are explicitly marked as such.
use crate::{
    install, java, metadata,
    model::{Artifact, Platform, Progress},
};
use anyhow::{bail, ensure, Context, Result};
use regex::Regex;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptiFineVersion {
    pub minecraft: String,
    pub version: String,
    pub filename: String,
    /// None means explicitly incompatible, empty means no additional restriction.
    pub forge: Option<String>,
    pub preview: bool,
}
impl OptiFineVersion {
    pub fn id(&self) -> String {
        format!("{}-OptiFine_{}", self.minecraft, self.version)
    }
    pub fn compatible_forge(&self, minecraft: &str, forge: &str) -> bool {
        if minecraft != self.minecraft {
            return false;
        }
        match self.forge.as_deref() {
            None => false,
            Some("") => true,
            Some(required) if required.contains('.') => required == forge,
            Some(required) => forge.rsplit('.').next() == Some(required),
        }
    }
    fn validate(&self) -> Result<()> {
        metadata::validate_id(&self.minecraft)?;
        metadata::validate_id(&self.version)?;
        ensure!(
            self.version.starts_with("HD_U_")
                && self
                    .version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "OptiFine 版本名无效"
        );
        let expected = format!(
            "{}OptiFine_{}_{}.jar",
            if self.preview { "preview_" } else { "" },
            self.minecraft,
            self.version
        );
        ensure!(self.filename == expected, "OptiFine 文件名与版本不匹配");
        Ok(())
    }
}

fn approved(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("optifine.net")
        && url.port().is_none_or(|p| p == 443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "/downloads" | "/adloadx" | "/downloadx")
        && url
            .query_pairs()
            .all(|(k, v)| matches!(k.as_ref(), "f" | "x") && !v.contains(['\n', '\r', '\0']))
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("PCL-Rust/0.1 (third-party launcher)")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() < 3 && approved(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("拒绝非 OptiFine 官方地址重定向")
            }
        }))
        .build()?)
}
fn read(client: &Client, url: &Url, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    ensure!(approved(url), "OptiFine 地址不符合官方 HTTPS 限制");
    install::cancelled(cancel)?;
    // Do not include a signed download URL in user-visible errors.
    let mut response = client
        .get(url.clone())
        .send()
        .map_err(|_| anyhow::anyhow!("无法连接 OptiFine 官方源"))?;
    ensure!(
        response.status().is_success(),
        "OptiFine 官方源返回 HTTP {}",
        response.status().as_u16()
    );
    ensure!(approved(response.url()), "OptiFine 响应来自非官方地址");
    ensure!(
        response.content_length().is_none_or(|size| size <= limit),
        "OptiFine 响应过大"
    );
    let mut bytes = Vec::new();
    let mut buffer = [0; 65536];
    loop {
        install::cancelled(cancel)?;
        let count = response
            .read(&mut buffer)
            .context("读取 OptiFine 响应失败")?;
        if count == 0 {
            break;
        }
        ensure!(
            bytes.len() as u64 + count as u64 <= limit,
            "OptiFine 响应过大"
        );
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}
fn parse_versions(html: &str) -> Result<Vec<OptiFineVersion>> {
    let rows = Regex::new(r"(?s)<tr\b[^>]*>(.*?)</tr>")?;
    let files = Regex::new(r#"(?:preview_)?OptiFine_[A-Za-z0-9_.]+\.jar"#)?;
    let forge = Regex::new(r#"class=['"]colForge['"][^>]*>([^<]*)"#)?;
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for row in rows.captures_iter(html) {
        let Some(file) = files.find(&row[1]) else {
            continue;
        };
        let filename = file.as_str();
        if !seen.insert(filename.to_owned()) {
            continue;
        }
        let clean = filename
            .strip_prefix("preview_")
            .unwrap_or(filename)
            .strip_prefix("OptiFine_")
            .unwrap()
            .strip_suffix(".jar")
            .unwrap();
        let Some((minecraft, patch)) = clean.split_once("_HD_U_") else {
            continue;
        };
        let required = forge
            .captures(&row[1])
            .context("OptiFine 官方行缺少 Forge 兼容信息")?[1]
            .trim()
            .replace("Forge", "")
            .replace('#', "")
            .trim()
            .to_owned();
        let entry = OptiFineVersion {
            minecraft: minecraft.into(),
            version: format!("HD_U_{patch}"),
            filename: filename.into(),
            forge: (!required.contains("N/A")).then_some(required),
            preview: filename.starts_with("preview_"),
        };
        entry.validate()?;
        output.push(entry);
    }
    ensure!(
        !output.is_empty(),
        "OptiFine 官方列表未解析出版本；未将异常响应当成空列表"
    );
    Ok(output)
}
pub fn list_versions(minecraft: &str, cancel: &AtomicBool) -> Result<Vec<OptiFineVersion>> {
    metadata::validate_id(minecraft)?;
    let bytes = read(
        &client()?,
        &Url::parse("https://optifine.net/downloads")?,
        4 * 1024 * 1024,
        cancel,
    )?;
    Ok(
        parse_versions(std::str::from_utf8(&bytes).context("OptiFine 列表编码无效")?)?
            .into_iter()
            .filter(|v| v.minecraft == minecraft)
            .collect(),
    )
}
fn download(entry: &OptiFineVersion, cancel: &AtomicBool) -> Result<Vec<u8>> {
    entry.validate()?;
    ensure!(
        list_versions(&entry.minecraft, cancel)?
            .iter()
            .any(|current| current == entry),
        "所选 OptiFine 不再匹配官方列表，请刷新后重试"
    );
    let client = client()?;
    let mut page = Url::parse("https://optifine.net/adloadx")?;
    page.query_pairs_mut().append_pair("f", &entry.filename);
    let html = read(&client, &page, 4 * 1024 * 1024, cancel)?;
    let pattern = Regex::new(r#"downloadx\?f=[^"'<>\s]+"#)?;
    let link = pattern
        .find(std::str::from_utf8(&html)?)
        .context("OptiFine 官方页面没有下载链接")?
        .as_str()
        .replace("&amp;", "&");
    let url = Url::parse("https://optifine.net/")?.join(&link)?;
    let names = url
        .query_pairs()
        .filter(|(key, _)| key == "f")
        .map(|(_, v)| v.into_owned())
        .collect::<Vec<_>>();
    ensure!(
        names == [entry.filename.clone()],
        "OptiFine 下载链接文件名不匹配"
    );
    let bytes = read(&client, &url, 64 * 1024 * 1024, cancel)?;
    ensure!(bytes.len() >= 64 * 1024, "OptiFine 下载文件过小");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .context("OptiFine 官方响应不是有效 JAR")?;
    ensure!(
        archive.by_name("optifine/OptiFineTweaker.class").is_ok()
            || archive.by_name("optifine/Installer.class").is_ok(),
        "JAR 缺少 OptiFine 入口"
    );
    Ok(bytes)
}
fn put_new(root: &Path, artifact: &Artifact, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
    install::cancelled(cancel)?;
    let path = install::safe_target(root, &artifact.relative_path)?;
    if fs::symlink_metadata(&path).is_ok() {
        ensure!(
            install::cache_valid(&path, artifact, cancel)?,
            "已有文件冲突，未覆盖：{}",
            artifact.relative_path.display()
        );
        return Ok(());
    }
    let parent = path.parent().context("文件没有父目录")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    install::cancelled(cancel)?;
    install::safe_target(root, &artifact.relative_path)?;
    temp.persist_noclobber(path)
        .map_err(|e| e.error)
        .context("OptiFine 文件提交冲突，未覆盖旧文件")?;
    Ok(())
}
fn artifact(path: PathBuf, bytes: &[u8]) -> Artifact {
    Artifact {
        relative_path: path,
        url: String::new(),
        sha1: Some(format!("{:x}", Sha1::digest(bytes))),
        size: Some(bytes.len() as u64),
        native: false,
        excludes: vec![],
    }
}
fn installer_java(bytes: &[u8]) -> Result<u32> {
    let mut jar = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut header = [0; 8];
    jar.by_name("optifine/Installer.class")?
        .read_exact(&mut header)?;
    ensure!(
        header[..4] == [0xca, 0xfe, 0xba, 0xbe],
        "OptiFine Installer.class 文件头无效"
    );
    let class = u16::from_be_bytes([header[6], header[7]]);
    ensure!(
        (49..=100).contains(&class),
        "OptiFine Installer.class 版本不支持"
    );
    Ok(u32::from(class) - 44)
}
fn needs_installer(minecraft: &str) -> bool {
    !minecraft.starts_with("1.")
        || minecraft
            .split('.')
            .nth(1)
            .and_then(|minor| minor.parse::<u32>().ok())
            .is_none_or(|minor| minor >= 14)
}
fn run_installer(java: &Path, jar: &Path, home: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Command::new(java)
        .arg(format!("-Duser.home={}", home.display()))
        .arg("-Djava.awt.headless=true")
        .arg("-cp")
        .arg(jar)
        .arg("optifine.Installer")
        .current_dir(home)
        .env("HOME", home)
        .env("APPDATA", home)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .context("无法运行 OptiFine 官方安装器")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let mut tail = String::new();
            for file in [&mut stdout, &mut stderr] {
                let length = file.metadata()?.len();
                file.seek(SeekFrom::Start(length.saturating_sub(8192)))?;
                file.take(8192).read_to_string(&mut tail).ok();
            }
            bail!("OptiFine 安装器失败 ({status})：{}", tail.trim());
        }
        let interrupted = install::cancelled(cancel).err();
        if interrupted.is_some()
            || started.elapsed() > Duration::from_secs(600)
            || stdout.metadata()?.len() > 8 * 1024 * 1024
            || stderr.metadata()?.len() > 8 * 1024 * 1024
        {
            child.kill().context("无法终止本次 OptiFine 安装器")?;
            child.wait()?;
            if let Some(error) = interrupted {
                return Err(error);
            }
            bail!("OptiFine 安装器超过时间或输出限制，已终止本次安装器");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn validate_profile(profile: &Value, entry: &OptiFineVersion) -> Result<()> {
    ensure!(
        profile["id"].as_str() == Some(entry.id().as_str())
            && profile["inheritsFrom"].as_str() == Some(&entry.minecraft),
        "OptiFine 安装器版本/父版本不匹配"
    );
    ensure!(
        profile["mainClass"] == "net.minecraft.launchwrapper.Launch",
        "OptiFine 安装器返回不支持的主类"
    );
    let expected = format!("optifine:OptiFine:{}_{}", entry.minecraft, entry.version);
    ensure!(
        profile["libraries"]
            .as_array()
            .is_some_and(|libraries| libraries.iter().any(|library| library["name"] == expected)),
        "OptiFine 安装器未声明所选版本的支持库"
    );
    Ok(())
}

/// Reuse an already installed publisher profile only after checking its receipt
/// and every declared library. Existing metadata is never rewritten.
pub fn ensure_optifine(
    root: &Path,
    entry: &OptiFineVersion,
    java: &Path,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    let id = entry.id();
    let path = install::safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
    if !path.exists() {
        return install_optifine(root, entry, java, platform, cancel, progress);
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    validate_profile(&value, entry)?;
    ensure!(
        value
            .pointer("/_pcl_optifine_install/filename")
            .and_then(Value::as_str)
            == Some(&entry.filename),
        "既有 OptiFine 版本缺少匹配安装收据"
    );
    install::repair_version_with_java(root, &id, Some(java), platform, cancel, progress)?;
    Ok(id)
}

/// Installs into a fresh isolated home, checks the emitted profile and libraries,
/// then publishes only declared files. Existing versions are never overwritten.
pub fn install_optifine(
    root: &Path,
    entry: &OptiFineVersion,
    java_path: &Path,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    entry.validate()?;
    install::cancelled(cancel)?;
    ensure!(root.is_absolute(), "游戏目录必须是绝对路径");
    let id = entry.id();
    let target = install::safe_target(root, &PathBuf::from(format!("versions/{id}")))?;
    ensure!(!target.exists(), "OptiFine 目标版本已存在，未覆盖用户配置");
    progress(Progress {
        message: "下载 OptiFine 官方安装文件（官方未提供校验清单）".into(),
        ..Default::default()
    });
    let bytes = download(entry, cancel)?;
    let modern = needs_installer(&entry.minecraft);
    if modern {
        let java = java::inspect_java_with_cancel(java_path, cancel)?;
        ensure!(
            java.major >= installer_java(&bytes)?,
            "所选 Java 低于 OptiFine 安装器所需版本"
        );
        java::validate_architecture(&java, platform)?;
    }
    let base = install::safe_target(
        root,
        &PathBuf::from(format!("versions/{0}/{0}.json", entry.minecraft)),
    )?;
    if !base.exists() {
        install::install_version(root, &entry.minecraft, platform, cancel, &progress)?;
    }
    let parent = install::verify_vanilla_parent(root, &entry.minecraft, platform, cancel)?;
    let stage = tempfile::tempdir()?;
    let mc = stage.path().join(if platform.os == "osx" {
        "Library/Application Support/minecraft"
    } else {
        ".minecraft"
    });
    let base_dir = mc.join("versions").join(&entry.minecraft);
    fs::create_dir_all(&base_dir)?;
    fs::copy(&base, base_dir.join(format!("{}.json", entry.minecraft)))?;
    fs::copy(
        install::safe_target(
            root,
            &PathBuf::from(format!("versions/{0}/{0}.jar", entry.minecraft)),
        )?,
        base_dir.join(format!("{}.jar", entry.minecraft)),
    )?;
    fs::write(mc.join("launcher_profiles.json"), br#"{"profiles":{}}"#)?;
    let mut profile = if modern {
        let jar = stage.path().join("OptiFine.jar");
        fs::write(&jar, &bytes)?;
        progress(Progress {
            message: "运行 OptiFine 官方安装器".into(),
            ..Default::default()
        });
        run_installer(java_path, &jar, stage.path(), cancel)?;
        let path = install::safe_target(&mc, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
        let bytes = fs::read(path).context("OptiFine 安装器未生成预期版本 JSON")?;
        ensure!(bytes.len() <= 1024 * 1024, "OptiFine profile 过大");
        serde_json::from_slice::<Value>(&bytes)?
    } else {
        let coordinate = format!("{}_{}", entry.minecraft, entry.version);
        let relative = PathBuf::from(format!(
            "libraries/optifine/OptiFine/{coordinate}/OptiFine-{coordinate}.jar"
        ));
        let path = mc.join(&relative);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, &bytes)?;
        let mut value = json!({"id":id,"inheritsFrom":entry.minecraft,"type":"release","mainClass":"net.minecraft.launchwrapper.Launch",
            "libraries":[{"name":format!("optifine:OptiFine:{coordinate}")},{"name":"net.minecraft:launchwrapper:1.12"}]});
        if let Some(arguments) = parent["minecraftArguments"].as_str() {
            value["minecraftArguments"] =
                json!(format!("{arguments} --tweakClass optifine.OptiFineTweaker"));
        } else {
            value["arguments"] = json!({"game":["--tweakClass","optifine.OptiFineTweaker"]});
        }
        value
    };
    validate_profile(&profile, entry)?;
    let mut pending = Vec::new();
    for library in profile["libraries"]
        .as_array_mut()
        .context("OptiFine profile 缺少支持库")?
    {
        let name = library["name"].as_str().context("OptiFine 库缺少名称")?;
        ensure!(
            name.starts_with("optifine:") || name.starts_with("net.minecraft:launchwrapper:"),
            "OptiFine profile 含未知支持库：{name}"
        );
        let artifacts =
            metadata::library_artifacts(&json!({"libraries":[library.clone()]}), platform)?;
        ensure!(artifacts.len() == 1, "OptiFine 支持库声明无效");
        let mut a = artifacts[0].clone();
        let path = install::safe_target(&mc, &a.relative_path)?;
        if path.is_file() {
            let payload = fs::read(path)?;
            ensure!(payload.len() <= 64 * 1024 * 1024, "OptiFine 生成文件过大");
            a = artifact(a.relative_path, &payload);
            pending.push((a.clone(), payload));
            library["_pcl_checksum_source"] = json!("local-derived-official-installer");
        } else {
            ensure!(
                name.starts_with("net.minecraft:launchwrapper:"),
                "OptiFine 安装器缺少生成库"
            );
            let hash = super::parse_checksum(&install::request_bytes(
                &install::http_client()?,
                &format!("{}.sha1", a.url),
                None,
                None,
                cancel,
            )?)?;
            a.sha1 = Some(hash);
            install::download_artifact(&install::http_client()?, &mc, &a, cancel)?;
            let payload = fs::read(mc.join(&a.relative_path))?;
            a.size = Some(payload.len() as u64);
            pending.push((a.clone(), payload));
            library["_pcl_checksum_source"] = json!("official-sha1");
        }
        let relative = a
            .relative_path
            .strip_prefix("libraries")?
            .to_string_lossy()
            .replace('\\', "/");
        library["downloads"] =
            json!({"artifact":{"path":relative,"url":a.url,"sha1":a.sha1,"size":a.size}});
    }
    for (a, _) in &pending {
        let path = install::safe_target(root, &a.relative_path)?;
        if path.exists() {
            ensure!(
                install::cache_valid(&path, a, cancel)?,
                "OptiFine 支持库与既有文件冲突"
            );
        }
    }
    for (a, bytes) in &pending {
        put_new(root, a, bytes, cancel)?;
    }
    profile["_pcl_optifine_install"] = json!({"filename":entry.filename,"sha1":format!("{:x}",Sha1::digest(&bytes)),"checksumSource":"local-derived-official-https"});
    super::commit_profile(
        root,
        &id,
        &profile,
        &metadata::library_artifacts(&parent, platform)?,
        cancel,
    )?;
    progress(Progress {
        message: format!("OptiFine {} 安装完成", entry.version),
        completed: 1,
        total: 1,
        ..Default::default()
    });
    Ok(id)
}

/// Installs the unmodified official jar as a mod only for a listed Forge pairing.
pub fn install_forge_mod(
    root: &Path,
    instance_id: &str,
    entry: &OptiFineVersion,
    forge: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    metadata::validate_id(instance_id)?;
    entry.validate()?;
    ensure!(
        entry.compatible_forge(&entry.minecraft, forge),
        "官方列表未声明该 Forge 与 OptiFine 兼容"
    );
    let resolved = metadata::resolve_version(root, instance_id)?;
    let expected = format!("net.minecraftforge:forge:{}-{forge}", entry.minecraft);
    ensure!(
        resolved["libraries"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v["name"]
                .as_str()
                .is_some_and(|name| name == expected || name.starts_with(&format!("{expected}:")))),
        "目标实例并非所选 Forge 版本"
    );
    let game = crate::config::instance_game_dir(root, instance_id)?;
    let relative = PathBuf::from("mods").join(&entry.filename);
    let target = install::safe_target(&game, &relative)?;
    ensure!(!target.exists(), "目标 OptiFine Mod 已存在，未覆盖");
    let bytes = download(entry, cancel)?;
    let a = artifact(relative, &bytes);
    put_new(&game, &a, &bytes, cancel)?;
    Ok(target)
}

/// The OptiFabric author's installation route: both unmodified jars in mods.
/// A bridge must already be present; it is obtained through its official project
/// API by the caller, never by constructing an unapproved CDN URL.
pub fn install_fabric_mod(
    root: &Path,
    instance_id: &str,
    entry: &OptiFineVersion,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    entry.validate()?;
    let resolved = metadata::resolve_version(root, instance_id)?;
    ensure!(
        resolved["_pcl_jar_id"].as_str() == Some(&entry.minecraft)
            && resolved["libraries"]
                .as_array()
                .is_some_and(|libs| libs.iter().any(|library| library["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("net.fabricmc:fabric-loader:")))),
        "目标不是对应的 Fabric 游戏版本"
    );
    let game = crate::config::instance_game_dir(root, instance_id)?;
    let mods = crate::mods::list_mods(&game)?;
    ensure!(
        mods.iter()
            .any(|m| m.enabled && m.mod_ids.iter().any(|id| id == "optifabric")),
        "请先安装兼容的 OptiFabric 桥接"
    );
    let relative = PathBuf::from("mods").join(&entry.filename);
    let target = install::safe_target(&game, &relative)?;
    ensure!(!target.exists(), "目标 OptiFine 文件已存在，未覆盖");
    let bytes = download(entry, cancel)?;
    let a = artifact(relative, &bytes);
    put_new(&game, &a, &bytes, cancel)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_profile_must_contain_the_requested_optifine_library() {
        let entry = OptiFineVersion {
            minecraft: "1.21.1".into(),
            version: "HD_U_J1".into(),
            filename: "OptiFine_1.21.1_HD_U_J1.jar".into(),
            forge: None,
            preview: false,
        };
        let mut profile = json!({"id":entry.id(),"inheritsFrom":"1.21.1",
            "mainClass":"net.minecraft.launchwrapper.Launch",
            "libraries":[{"name":"net.minecraft:launchwrapper:1.12"}]});
        assert!(validate_profile(&profile, &entry).is_err());
        profile["libraries"][0]["name"] = json!("optifine:OptiFine:1.21.1_HD_U_J0");
        assert!(validate_profile(&profile, &entry).is_err());
        profile["libraries"][0]["name"] = json!("optifine:OptiFine:1.21.1_HD_U_J1");
        assert!(validate_profile(&profile, &entry).is_ok());
        profile["inheritsFrom"] = json!("1.21");
        assert!(validate_profile(&profile, &entry).is_err());
    }
    #[test]
    fn class_header_drives_java_requirement_and_year_versions_use_installer() {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(
            "optifine/Installer.class",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 65])
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert_eq!(installer_java(&bytes).unwrap(), 21);
        assert!(!needs_installer("1.13.2"));
        assert!(needs_installer("1.14"));
        assert!(needs_installer("26.2"));
    }
    #[test]
    fn official_rows_keep_pairing_preview_and_exact_compatibility() {
        let html = r#"<tr><td><a href="adloadx?f=OptiFine_1.21.1_HD_U_J1.jar">x</a></td><td class='colForge'>Forge 52.0.16</td></tr><tr><td>preview_OptiFine_1.12.2_HD_U_G6_pre1.jar</td><td class='colForge'>Forge #2847</td></tr><tr><td>OptiFine_1.13.2_HD_U_G5.jar</td><td class='colForge'>Forge N/A</td></tr>"#;
        let entries = parse_versions(html).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].compatible_forge("1.21.1", "52.0.16"));
        assert!(!entries[0].compatible_forge("1.21.1", "52.0.17"));
        assert!(entries[1].preview);
        assert!(entries[1].compatible_forge("1.12.2", "14.23.5.2847"));
        assert!(!entries[2].compatible_forge("1.13.2", "25.0.0"));
        assert!(parse_versions("<html>network error</html>").is_err());
    }
    #[test]
    fn no_clobber_and_cancel_preserve_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let a = artifact(PathBuf::from("libraries/a.jar"), b"one");
        put_new(root.path(), &a, b"one", &AtomicBool::new(false)).unwrap();
        assert!(put_new(
            root.path(),
            &artifact(a.relative_path.clone(), b"two"),
            b"two",
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(
            fs::read(root.path().join(&a.relative_path)).unwrap(),
            b"one"
        );
        assert!(put_new(
            root.path(),
            &artifact(PathBuf::from("new.jar"), b"two"),
            b"two",
            &AtomicBool::new(true)
        )
        .is_err());
        assert!(!root.path().join("new.jar").exists());
    }
    #[test]
    fn only_publisher_download_routes_and_safe_identifiers_are_accepted() {
        for url in [
            "http://optifine.net/downloads",
            "https://evil.example/downloads",
            "https://optifine.net/downloadx?token=secret",
            "https://user@optifine.net/downloads",
        ] {
            assert!(!approved(&Url::parse(url).unwrap()));
        }
        assert!(approved(
            &Url::parse("https://optifine.net/downloadx?f=OptiFine_1.21.1_HD_U_J1.jar&x=abc")
                .unwrap()
        ));
        let mut entry = OptiFineVersion {
            minecraft: "1.21.1".into(),
            version: "HD_U_J1".into(),
            filename: "OptiFine_1.21.1_HD_U_J1.jar".into(),
            forge: None,
            preview: false,
        };
        assert!(entry.validate().is_ok());
        entry.filename = "../bad.jar".into();
        assert!(entry.validate().is_err());
    }
}
