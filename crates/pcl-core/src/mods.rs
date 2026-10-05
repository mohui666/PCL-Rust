//! Local JAR inspection and reversible enable/disable operations; no mod is executed.
use crate::metadata::validate_id;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::ZipArchive;

#[path = "mods_removal.rs"]
mod removal;
pub use removal::{remove_mods, restore_removed_mods, ModRemoval};

#[path = "mods_import.rs"]
mod imports;
pub use imports::{import_mods, imported_mod_name};

const METADATA_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalMod {
    pub file_name: String,
    pub path: PathBuf,
    pub enabled: bool,
    pub name: String,
    pub version: Option<String>,
    pub mod_ids: Vec<String>,
    pub loader: String,
    pub error: Option<String>,
}

fn jar_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".jar", ".jar.disabled", ".litemod", ".litemod.disabled"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn mods_directory(instance: &Path, create: bool) -> Result<PathBuf> {
    if !instance.is_absolute() {
        bail!("实例目录必须是绝对路径");
    }
    if create {
        fs::create_dir_all(instance).context("创建实例目录失败")?;
    }
    let root = instance.canonicalize().context("实例目录不存在")?;
    let mods = root.join("mods");
    match fs::symlink_metadata(&mods) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!("mods 目录不能是符号链接"),
        Ok(metadata) if !metadata.is_dir() => bail!("mods 路径不是目录"),
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir(&mods)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(mods)
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Option<String>> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if entry.size() > METADATA_LIMIT {
        bail!("Mod 元数据超过 1 MiB 限制");
    }
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > METADATA_LIMIT {
        bail!("Mod 元数据超过 1 MiB 限制");
    }
    Ok(Some(
        String::from_utf8(bytes).context("Mod 元数据不是 UTF-8")?,
    ))
}

fn inspect(path: &Path, name: &str) -> Result<LocalMod> {
    let mut result = LocalMod {
        file_name: name.into(),
        path: path.into(),
        enabled: !name.to_ascii_lowercase().ends_with(".disabled"),
        name: name
            .trim_end_matches(".disabled")
            .trim_end_matches(".jar")
            .into(),
        version: None,
        mod_ids: vec![],
        loader: "unknown".into(),
        error: None,
    };
    let mut archive = ZipArchive::new(File::open(path)?).context("文件不是有效的 JAR/ZIP")?;
    if archive.len() > 100_000 {
        bail!("JAR 条目数量超过限制");
    }
    if let Some(text) = read_entry(&mut archive, "fabric.mod.json")? {
        let metadata: Value = serde_json::from_str(&text).context("Fabric 元数据无效")?;
        let id = metadata["id"].as_str().context("Fabric 元数据缺少 id")?;
        result.mod_ids.push(id.into());
        result.name = metadata["name"].as_str().unwrap_or(id).into();
        result.version = metadata["version"].as_str().map(str::to_owned);
        result.loader = "fabric".into();
    } else if let Some(text) = read_entry(&mut archive, "litemod.json")? {
        let metadata: Value = serde_json::from_str(&text)?;
        result.loader = "liteloader".into();
        result.name = metadata["name"].as_str().unwrap_or(name).into();
        result.version = metadata["version"].as_str().map(str::to_owned);
    } else {
        for (entry, loader) in [
            ("META-INF/neoforge.mods.toml", "neoforge"),
            ("META-INF/mods.toml", "forge"),
        ] {
            let Some(text) = read_entry(&mut archive, entry)? else {
                continue;
            };
            let metadata: toml::Value =
                toml::from_str(&text).context("Forge/NeoForge 元数据无效")?;
            let mods = metadata
                .get("mods")
                .and_then(toml::Value::as_array)
                .context("Mod 元数据缺少 [[mods]]")?;
            for item in mods {
                let id = item
                    .get("modId")
                    .and_then(toml::Value::as_str)
                    .context("Mod 元数据缺少 modId")?;
                result.mod_ids.push(id.into());
            }
            if let Some(first) = mods.first() {
                result.name = first
                    .get("displayName")
                    .and_then(toml::Value::as_str)
                    .or_else(|| first.get("modId").and_then(toml::Value::as_str))
                    .unwrap_or(name)
                    .into();
                result.version = first
                    .get("version")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned);
            }
            if result.version.as_deref() == Some("${file.jarVersion}") {
                result.version =
                    read_entry(&mut archive, "META-INF/MANIFEST.MF")?.and_then(|text| {
                        text.lines().find_map(|line| {
                            line.strip_prefix("Implementation-Version:")
                                .map(|value| value.trim().to_owned())
                        })
                    });
            }
            result.loader = loader.into();
            break;
        }
    }
    Ok(result)
}

pub fn list_mods(instance_dir: &Path) -> Result<Vec<LocalMod>> {
    if !instance_dir.is_absolute() {
        bail!("实例目录必须是绝对路径");
    }
    if !instance_dir.exists() {
        return Ok(vec![]);
    }
    let directory = mods_directory(instance_dir, false)?;
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut output = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !jar_name(&name) {
            continue;
        }
        let file_type = entry.file_type()?;
        let result = if file_type.is_symlink() {
            Err(anyhow::anyhow!("拒绝读取符号链接 Mod"))
        } else if !file_type.is_file() {
            continue;
        } else {
            inspect(&entry.path(), &name)
        };
        output.push(match result {
            Ok(value) => value,
            Err(error) => LocalMod {
                file_name: name.clone(),
                path: entry.path(),
                enabled: !name.to_ascii_lowercase().ends_with(".disabled"),
                name,
                version: None,
                mod_ids: vec![],
                loader: "unknown".into(),
                error: Some(format!("{error:#}")),
            },
        });
    }
    output.sort_by_key(|item| item.file_name.to_ascii_lowercase());
    Ok(output)
}

pub fn set_mod_enabled(instance_dir: &Path, file_name: &str, enabled: bool) -> Result<PathBuf> {
    validate_id(file_name)?;
    if !jar_name(file_name) {
        bail!("只能启用或禁用 .jar / .jar.disabled 文件");
    }
    let directory = mods_directory(instance_dir, false)?;
    let source = directory.join(file_name);
    let metadata = fs::symlink_metadata(&source).context("Mod 文件不存在")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("Mod 必须是普通文件，不能是符号链接");
    }
    let currently_enabled = !file_name.to_ascii_lowercase().ends_with(".disabled");
    if currently_enabled == enabled {
        return Ok(source);
    }
    let target_name = if enabled {
        file_name[..file_name.len() - 9].to_owned()
    } else {
        format!("{file_name}.disabled")
    };
    let target = directory.join(target_name);
    // hard_link creates a destination exclusively on macOS and Windows. Unlike rename,
    // it cannot replace a conflicting user file. The original bytes remain untouched.
    fs::hard_link(&source, &target).context("切换失败：目标已存在或文件系统不支持安全链接")?;
    if let Err(error) = fs::remove_file(&source) {
        let rollback = fs::remove_file(&target);
        if rollback.is_err() {
            bail!("原 Mod 删除失败且回滚失败；两个路径均已保留，请检查目录：{error}");
        }
        return Err(error).context("切换失败，已保留原 Mod 文件");
    }
    Ok(target)
}

pub fn import_mod(instance_dir: &Path, source: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(source).context("无法读取待导入 Mod")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("待导入 Mod 必须是普通文件");
    }
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .context("Mod 文件名无效")?;
    validate_id(name)?;
    if !name.to_ascii_lowercase().ends_with(".jar") {
        bail!("请选择 .jar Mod 文件");
    }
    inspect(source, name)?;
    let directory = mods_directory(instance_dir, true)?;
    let target = directory.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        bail!("同名 Mod 已存在，未覆盖原文件");
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    std::io::copy(&mut File::open(source)?, &mut temporary)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&target)
        .context("Mod 导入冲突，未覆盖原文件")?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn jar(path: &Path, entries: &[(&str, &str)]) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        for (name, body) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    #[test]
    fn detects_fabric_forge_neoforge_and_preserves_corrupt_entry() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("mods");
        fs::create_dir(&directory).unwrap();
        jar(
            &directory.join("fabric.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"example","name":"Fabric Example","version":"1.0"}"#,
            )],
        );
        jar(
            &directory.join("forge.jar"),
            &[
                (
                    "META-INF/mods.toml",
                    "[[mods]]\nmodId='forge_example'\ndisplayName='Forge Example'\nversion='${file.jarVersion}'",
                ),
                ("META-INF/MANIFEST.MF", "Implementation-Version: 2.0\n"),
            ],
        );
        jar(
            &directory.join("neo.jar.disabled"),
            &[(
                "META-INF/neoforge.mods.toml",
                "[[mods]]\nmodId='neo_example'\nversion='3.0'",
            )],
        );
        fs::write(directory.join("broken.jar"), b"bad").unwrap();
        let values = list_mods(temp.path()).unwrap();
        assert_eq!(values.len(), 4);
        assert!(values
            .iter()
            .any(|v| v.loader == "fabric" && v.name == "Fabric Example"));
        assert!(values
            .iter()
            .any(|v| v.loader == "forge" && v.version.as_deref() == Some("2.0")));
        assert!(values.iter().any(|v| v.loader == "neoforge" && !v.enabled));
        assert!(values
            .iter()
            .any(|v| v.file_name == "broken.jar" && v.error.is_some()));
    }
    #[test]
    fn toggle_never_overwrites_conflicts_and_round_trip_preserves_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("mods");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("a.jar"), b"original").unwrap();
        fs::write(directory.join("a.jar.disabled"), b"other").unwrap();
        assert!(set_mod_enabled(temp.path(), "a.jar", false).is_err());
        assert_eq!(fs::read(directory.join("a.jar")).unwrap(), b"original");
        fs::remove_file(directory.join("a.jar.disabled")).unwrap();
        set_mod_enabled(temp.path(), "a.jar", false).unwrap();
        set_mod_enabled(temp.path(), "a.jar.disabled", true).unwrap();
        assert_eq!(fs::read(directory.join("a.jar")).unwrap(), b"original");
        assert!(set_mod_enabled(temp.path(), "../a.jar", false).is_err());
    }
    #[test]
    fn importing_is_a_copy_and_conflicts_preserve_both_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("example.jar");
        jar(
            &source,
            &[("fabric.mod.json", r#"{"id":"example","version":"1"}"#)],
        );
        let original = fs::read(&source).unwrap();
        let instance = temp.path().join("instance");
        let target = import_mod(&instance, &source).unwrap();
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::read(&target).unwrap(), original);
        assert!(import_mod(&instance, &source).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_mod_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("mods")).unwrap();
        fs::write(temp.path().join("outside.jar"), b"untouched").unwrap();
        std::os::unix::fs::symlink(
            temp.path().join("outside.jar"),
            temp.path().join("mods/link.jar"),
        )
        .unwrap();
        assert!(set_mod_enabled(temp.path(), "link.jar", false).is_err());
        assert!(list_mods(temp.path()).unwrap()[0].error.is_some());
    }
}
