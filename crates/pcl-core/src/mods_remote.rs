//! Visible local mods acquire project metadata only after a content-hash match.
//! Requests contain hashes/fingerprints, never paths or archive contents.
use super::LocalMod;
use crate::{
    curseforge,
    install::cancelled,
    instances,
    resources::{self, ModrinthProject, ModrinthVersion, ResourceKind, ResourceProvider},
    wiki,
};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha512};
use std::{collections::BTreeMap, fs, io::Read, path::Path, sync::atomic::AtomicBool};

#[derive(Clone, Debug, Default)]
pub struct RemoteModDetails {
    pub file_name: String,
    pub title: String,
    pub original_title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub project_url: String,
    pub wiki_url: Option<String>,
    pub version: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub icon: Option<Vec<u8>>,
    pub error: Option<String>,
}
fn thumbnail(bytes: &[u8]) -> Result<Vec<u8>> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let (width, height) = reader.into_dimensions()?;
    ensure!(
        width > 0 && height > 0 && width <= 1024 && height <= 1024,
        "项目图标尺寸超过 1024 像素"
    );
    let icon = image::load_from_memory(bytes)?.thumbnail(96, 96);
    let mut output = std::io::Cursor::new(Vec::new());
    icon.write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}

/// Cheap identity/size/mtime key for UI cache invalidation, including same-name replacements.
pub fn remote_snapshot(instance: &Path, mods: &[LocalMod]) -> Result<String> {
    ensure!(mods.len() <= 4096, "Mod 文件超过 4096 个，无法读取详情");
    let directory = super::mods_directory(instance, false)?;
    let mut hash = Sha512::new();
    for item in mods {
        crate::metadata::validate_id(&item.file_name)?;
        let path = directory.join(&item.file_name);
        ensure!(
            item.path == path || item.path.canonicalize().ok().as_deref() == Some(path.as_path()),
            "Mod 列表与实例目录不一致"
        );
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Mod 不是普通文件"
        );
        hash.update(format!(
            "{}:{:?}:{}:{:?}",
            item.file_name,
            instances::identity(&path)?,
            metadata.len(),
            metadata.modified()?
        ));
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn content_hash(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let identity = instances::identity(path)?;
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink() && before.len() <= 512 * 1024 * 1024,
        "Mod 文件类型或大小不受支持"
    );
    let mut f = fs::File::open(path)?;
    let mut hash = Sha512::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        cancelled(cancel)?;
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        ensure!(size <= 512 * 1024 * 1024, "Mod 在读取时超出大小上限");
        hash.update(&buffer[..n]);
    }
    let after = fs::symlink_metadata(path)?;
    ensure!(
        identity == instances::identity(path)?
            && size == before.len()
            && before.len() == after.len()
            && before.modified()? == after.modified()?,
        "Mod 在读取期间改变，请刷新列表"
    );
    Ok(format!("{:x}", hash.finalize()))
}
fn matched<'a>(versions: &'a [ModrinthVersion], hash: &str) -> Result<Option<&'a ModrinthVersion>> {
    let found: Vec<_> = versions
        .iter()
        .filter(|v| {
            v.files.iter().any(|f| {
                f.hashes
                    .get("sha512")
                    .is_some_and(|h| h.eq_ignore_ascii_case(hash))
            })
        })
        .collect();
    if found.iter().any(|v| {
        found
            .first()
            .is_some_and(|first| first.project_id != v.project_id)
    }) {
        bail!("同一文件摘要对应多个项目，未自动选择");
    }
    Ok(found.first().copied())
}
fn details(name: &str, version: &ModrinthVersion, p: &ModrinthProject) -> Result<RemoteModDetails> {
    ensure!(
        p.id == version.project_id && p.project_type == "mod",
        "远端项目与已校验的 Mod 文件不一致"
    );
    let provider = if p.id.starts_with("cf:") {
        ResourceProvider::CurseForge
    } else {
        ResourceProvider::Modrinth
    };
    let translation = wiki::find(provider, &p.slug);
    let project_url = if provider == ResourceProvider::CurseForge {
        format!("https://www.curseforge.com/minecraft/mc-mods/{}", p.slug)
    } else {
        format!("https://modrinth.com/mod/{}", p.id)
    };
    let mut tags: Vec<_> = p
        .categories
        .iter()
        .filter(|s| !s.is_empty())
        .take(16)
        .cloned()
        .collect();
    if p.client_side == "unsupported" {
        tags.push("仅服务端".into());
    } else if p.server_side == "unsupported" {
        tags.push("仅客户端".into());
    }
    Ok(RemoteModDetails {
        file_name: name.into(),
        title: translation
            .and_then(|e| e.chinese.clone())
            .unwrap_or_else(|| p.title.clone()),
        original_title: p.title.clone(),
        description: p.description.clone(),
        tags,
        project_url,
        wiki_url: translation.map(|e| e.url()),
        version: version.version_number.clone(),
        game_versions: version.game_versions.clone(),
        loaders: version.loaders.clone(),
        ..Default::default()
    })
}
pub fn load_remote_details(
    instance: &Path,
    mods: &[LocalMod],
    cancel: &AtomicBool,
) -> Result<Vec<RemoteModDetails>> {
    cancelled(cancel)?;
    let before = remote_snapshot(instance, mods)?;
    let mut hashes = BTreeMap::new();
    let mut size = 0u64;
    for item in mods
        .iter()
        .filter(|m| m.error.is_none() && m.metadata.inspected)
    {
        cancelled(cancel)?;
        size += fs::metadata(&item.path)?.len();
        ensure!(size <= 20 * 1024 * 1024 * 1024, "Mod 总量超过详情读取限制");
        hashes.insert(item.path.clone(), content_hash(&item.path, cancel)?);
    }
    let mut versions =
        resources::identify_hashes(&hashes.values().cloned().collect::<Vec<_>>(), cancel)?;
    let unmatched: BTreeMap<_, _> = hashes
        .iter()
        .filter(|(_, h)| matched(&versions, h).ok().flatten().is_none())
        .map(|(p, h)| (p.clone(), h.clone()))
        .collect();
    if !unmatched.is_empty() && curseforge::has_api_key()? {
        versions.extend(curseforge::identify_files_with_disabled(
            &unmatched,
            ResourceKind::Mod,
            true,
            cancel,
        )?);
    }
    let mut projects = BTreeMap::new();
    let mut result = Vec::new();
    for item in mods {
        cancelled(cancel)?;
        let Some(hash) = hashes.get(&item.path) else {
            continue;
        };
        let mut entry = RemoteModDetails {
            file_name: item.file_name.clone(),
            ..Default::default()
        };
        let lookup = (|| {
            let version =
                matched(&versions, hash)?.context("未找到内容摘要匹配的远端项目；保留本地信息")?;
            if !projects.contains_key(&version.project_id) {
                projects.insert(
                    version.project_id.clone(),
                    resources::get_project(&version.project_id, cancel)?,
                );
            }
            let project = &projects[&version.project_id];
            let mut value = details(&item.file_name, version, project)?;
            if let Some(url) = &project.icon_url {
                match resources::fetch_project_icon(url, cancel).and_then(|icon| thumbnail(&icon)) {
                    Ok(icon) => value.icon = Some(icon),
                    Err(error) => {
                        cancelled(cancel)?;
                        value.error = Some(format!("项目图标未载入：{error:#}"));
                    }
                }
            }
            anyhow::Ok(value)
        })();
        match lookup {
            Ok(value) => entry = value,
            Err(error) => {
                cancelled(cancel)?;
                entry.error = Some(format!("{error:#}"));
            }
        }
        result.push(entry);
    }
    ensure!(
        before == remote_snapshot(instance, mods)?,
        "Mod 列表在获取详情期间改变，请刷新后重试"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translated_project_requires_verified_identity() {
        let p:ModrinthProject=serde_json::from_value(serde_json::json!({"id":"A","slug":"sodium","title":"Sodium","description":"Official description","body":"","project_type":"mod","categories":["optimization"],"downloads":1,"updated":"","source_url":null,"icon_url":null})).unwrap();
        let mut v:ModrinthVersion=serde_json::from_value(serde_json::json!({"id":"V","project_id":"A","name":"Sodium","version_number":"0.6","version_type":"release","date_published":"","game_versions":["1.21"],"loaders":["fabric"],"files":[]})).unwrap();
        let d = details("unrelated-name.jar", &v, &p).unwrap();
        assert_eq!(d.description, "Official description");
        assert_eq!(d.tags, ["optimization"]);
        assert!(d.wiki_url.is_some());
        assert!(d.title.contains('钠'));
        v.project_id = "B".into();
        assert!(details("x.jar", &v, &p).is_err());
    }
    #[test]
    fn cancelled_lookup_never_touches_paths_or_network() {
        assert!(load_remote_details(
            Path::new("/not-a-real-instance"),
            &[],
            &AtomicBool::new(true)
        )
        .unwrap_err()
        .is::<crate::model::OperationCancelled>());
    }
    #[test]
    fn snapshot_changes_when_same_name_is_replaced_and_rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("mods")).unwrap();
        let p = dir.path().join("mods/a.jar");
        fs::write(&p, b"one").unwrap();
        let mut list = super::super::list_mods(dir.path()).unwrap();
        let old = remote_snapshot(dir.path(), &list).unwrap();
        fs::remove_file(&p).unwrap();
        fs::write(&p, b"replacement").unwrap();
        assert_ne!(old, remote_snapshot(dir.path(), &list).unwrap());
        list[0].path = dir.path().join("elsewhere.jar");
        assert!(remote_snapshot(dir.path(), &list).is_err());
    }
}
