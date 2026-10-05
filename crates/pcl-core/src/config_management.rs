//! Registration and scoped resets never remove game data or execute imported settings.
use super::*;
use std::sync::atomic::AtomicBool;

fn checked_root(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("游戏目录必须是绝对路径");
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        bail!("游戏目录必须是普通目录");
    }
    path.canonicalize().context("无法解析游戏目录")
}
fn roots(settings: &Settings) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for path in std::iter::once(&settings.game_root).chain(&settings.game_roots) {
        let path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !result.contains(&path) {
            result.push(path);
        }
    }
    result
}
/// Rename is a display label, matching PCL's folder registration semantics.
pub fn register_game_root(
    settings_path: &Path,
    settings: &Settings,
    path: &Path,
    label: &str,
) -> Result<Settings> {
    let path = checked_root(path)?;
    let mut next = settings.clone();
    next.game_roots = roots(settings);
    if !next.game_roots.contains(&path) {
        next.game_roots.push(path.clone());
    }
    next.game_root_names.insert(path, label.trim().into());
    save_settings(settings_path, &next)?;
    Ok(next)
}
pub fn rename_game_root(
    settings_path: &Path,
    settings: &Settings,
    path: &Path,
    label: &str,
) -> Result<Settings> {
    let path = checked_root(path)?;
    if !roots(settings).contains(&path) {
        bail!("该游戏目录未登记");
    }
    register_game_root(settings_path, settings, &path, label)
}
pub fn create_game_root(
    settings_path: &Path,
    settings: &Settings,
    path: &Path,
    label: &str,
) -> Result<Settings> {
    if !path.is_absolute() {
        bail!("新目录必须是绝对路径");
    }
    let parent = path.parent().context("新目录没有父目录")?;
    checked_root(parent)?;
    fs::create_dir(path).context("新目录已存在或无法创建，未使用已有数据")?;
    let result = (|| {
        fs::create_dir(path.join("versions"))?;
        register_game_root(settings_path, settings, path, label)
    })();
    if result.is_err() {
        let _ = fs::remove_dir(path.join("versions"));
        let _ = fs::remove_dir(path);
    }
    result
}
pub fn unregister_game_root(
    settings_path: &Path,
    settings: &Settings,
    path: &Path,
) -> Result<Settings> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let mut next = settings.clone();
    next.game_roots = roots(settings);
    if !next.game_roots.contains(&path) {
        bail!("该目录未登记");
    }
    next.game_roots.retain(|p| p != &path);
    if next.game_roots.is_empty() {
        bail!("请先添加另一游戏目录，再移除最后一项登记");
    }
    next.game_root_names
        .retain(|key, _| key.canonicalize().unwrap_or_else(|_| key.clone()) != path);
    if settings
        .game_root
        .canonicalize()
        .unwrap_or_else(|_| settings.game_root.clone())
        == path
    {
        next.game_root = next.game_roots[0].clone();
        next.selected_version = None;
    }
    save_settings(settings_path, &next)?;
    Ok(next)
}
pub fn game_root_label(settings: &Settings, path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    settings
        .game_root_names
        .get(&canonical)
        .cloned()
        .unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
}
/// Keep presentation metadata; reset only the independent launch controls.
pub fn reset_instance_settings(root: &Path, id: &str, cancel: &AtomicBool) -> Result<PathBuf> {
    crate::install::cancelled(cancel)?;
    let path = instance_settings_path(root, id)?;
    let original = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("无法备份原设置"),
    };
    let previous: InstanceSettings = serde_json::from_slice(original.as_deref().unwrap_or(b"{}"))?;
    validate_instance_settings(&previous)?;
    let next = InstanceSettings {
        description: previous.description,
        favorite: previous.favorite,
        hidden: previous.hidden,
        display_icon: previous.display_icon,
        custom_icon: previous.custom_icon,
        display_category: previous.display_category,
        ..Default::default()
    };
    let parent = path.parent().context("配置目录无效")?;
    fs::create_dir_all(parent)?;
    let mut backup = tempfile::Builder::new()
        .prefix("instance-before-reset-")
        .suffix(".json")
        .tempfile_in(parent)?;
    let original_bytes = original.as_deref().unwrap_or(b"{}");
    backup.write_all(original_bytes)?;
    backup.as_file().sync_all()?;
    crate::install::cancelled(cancel)?;
    let current = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if current != original {
        bail!("设置在初始化前已改变，请重新操作");
    }
    let backup = backup.keep().map_err(|e| e.error)?.1;
    save_instance_settings(root, id, &next)
        .with_context(|| format!("初始化失败，原设置副本保留于 {}", backup.display()))?;
    Ok(backup)
}
/// Import a user-selected image as a bounded, normalized immutable icon, preserving its source.
pub fn import_instance_icon(root: &Path, id: &str, source: &Path) -> Result<String> {
    use image::ImageReader;
    use sha2::{Digest, Sha256};
    let meta = fs::symlink_metadata(source)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4 * 1024 * 1024 {
        bail!("请选择不超过 4 MiB 的常用图片");
    }
    let bytes = fs::read(source)?;
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .context("无法识别图标格式")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .context("图标不是有效的 PNG、JPEG、GIF 或 WebP 图片")?;
    let image = image.thumbnail(256, 256);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png)?;
    let bytes = encoded.into_inner();
    let name = format!("{:x}.png", Sha256::digest(&bytes));
    let path = instance_icon_path(root, id, &name)?;
    if path.try_exists()? {
        if fs::read(&path)? != bytes {
            bail!("已有图标文件内容不匹配，未覆盖");
        }
        return Ok(name);
    }
    let parent = path.parent().context("图标目录无效")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path).map_err(|e| e.error)?;
    Ok(name)
}
pub fn instance_icon_path(root: &Path, id: &str, name: &str) -> Result<PathBuf> {
    crate::metadata::validate_id(id)?;
    crate::metadata::validate_id(name)?;
    if !name.ends_with(".png") {
        bail!("图标文件必须为 PNG");
    }
    crate::metadata::confined_path(
        root,
        &Path::new("versions")
            .join(id)
            .join("PCL-Rust/icons")
            .join(name),
    )
}

/// Restore only a previously selected backup in this exact version's settings directory.
pub fn restore_instance_settings(
    root: &Path,
    id: &str,
    backup: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    crate::install::cancelled(cancel)?;
    let target = instance_settings_path(root, id)?;
    let parent = target
        .parent()
        .context("版本设置目录无效")?
        .canonicalize()?;
    let meta = fs::symlink_metadata(backup)?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 1024 * 1024 {
        bail!("恢复文件必须是不超过 1 MiB 的普通 JSON");
    }
    let backup = backup.canonicalize()?;
    if backup.parent() != Some(parent.as_path())
        || !backup
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("instance-before-reset-") && s.ends_with(".json"))
    {
        bail!("只能恢复该版本自身的初始化备份");
    }
    let settings: InstanceSettings = serde_json::from_slice(&fs::read(backup)?)?;
    validate_instance_settings(&settings)?;
    crate::install::cancelled(cancel)?;
    save_instance_settings(root, id, &settings)
}
/// Reset the chosen launcher preferences, preserving registered roots and account-related input.
pub fn reset_launcher_settings(
    path: &Path,
    settings: &Settings,
    all: bool,
) -> Result<(Settings, PathBuf)> {
    let original = fs::read(path).context("读取原设置失败，未初始化")?;
    let saved: Settings = serde_json::from_slice(&original)?;
    validate_settings(&saved)?;
    if &saved != settings {
        bail!("磁盘设置已改变，请重新加载后初始化");
    }
    let settings = &saved;
    let mut next = if all {
        Settings {
            game_root: settings.game_root.clone(),
            game_roots: settings.game_roots.clone(),
            game_root_names: settings.game_root_names.clone(),
            selected_version: settings.selected_version.clone(),
            microsoft_client_id: settings.microsoft_client_id.clone(),
            offline_name: settings.offline_name.clone(),
            offline_history: settings.offline_history.clone(),
            offline_skin_mode: settings.offline_skin_mode,
            offline_skin_name: settings.offline_skin_name.clone(),
            offline_skin_path: settings.offline_skin_path.clone(),
            offline_skin_slim: settings.offline_skin_slim,
            ..Default::default()
        }
    } else {
        settings.clone()
    };
    if !all {
        next.system = Default::default();
        next.downloads = Default::default();
    }
    let parent = path.parent().context("设置目录无效")?;
    let mut backup = tempfile::Builder::new()
        .prefix("settings-before-reset-")
        .suffix(".json")
        .tempfile_in(parent)?;
    backup.write_all(&original)?;
    backup.as_file().sync_all()?;
    if fs::read(path)? != original {
        bail!("设置已改变，请重新初始化");
    }
    let backup = backup.keep().map_err(|e| e.error)?.1;
    save_settings(path, &next)
        .with_context(|| format!("初始化失败，原设置保留于 {}", backup.display()))?;
    Ok((next, backup))
}
#[cfg(test)]
mod tests {
    #[test]
    fn jpeg_icon_is_normalized_without_altering_source() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("wide.jpg");
        image::RgbImage::from_pixel(400, 100, image::Rgb([12, 30, 70]))
            .save(&source)
            .unwrap();
        let before = fs::read(&source).unwrap();
        let name = import_instance_icon(d.path(), "v", &source).unwrap();
        let output = image::open(instance_icon_path(d.path(), "v", &name).unwrap()).unwrap();
        assert_eq!((output.width(), output.height()), (256, 64));
        assert_eq!(fs::read(source).unwrap(), before);
    }

    #[test]
    fn reset_rejects_stale_launcher_snapshot_without_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("settings.json");
        let old = Settings {
            game_root: d.path().to_owned(),
            ..Default::default()
        };
        let new = Settings {
            ui_theme: 4,
            ..old.clone()
        };
        save_settings(&path, &new).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(reset_launcher_settings(&path, &old, true).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn icon_import_is_bounded_immutable_and_preserves_input() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source.png");
        let original = image::RgbaImage::from_pixel(32, 32, image::Rgba([1, 2, 3, 255]));
        original.save(&source).unwrap();
        let before = fs::read(&source).unwrap();
        let name = import_instance_icon(d.path(), "v", &source).unwrap();
        let path = instance_icon_path(d.path(), "v", &name).unwrap();
        assert!(path.is_file());
        assert_eq!(import_instance_icon(d.path(), "v", &source).unwrap(), name);
        assert_eq!(fs::read(&source).unwrap(), before);
        assert!(instance_icon_path(d.path(), "v", "../x.png").is_err());
        fs::write(&path, b"unrelated").unwrap();
        assert!(import_instance_icon(d.path(), "v", &source).is_err());
        assert_eq!(fs::read(path).unwrap(), b"unrelated");
    }
    #[test]
    fn launcher_reset_keeps_roots_account_input_and_exact_backup() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("settings.json");
        let settings = Settings {
            game_root: d.path().to_owned(),
            offline_name: "FixturePlayer".into(),
            microsoft_client_id: "fixture-public-client".into(),
            ui_theme: 5,
            ..Default::default()
        };
        save_settings(&path, &settings).unwrap();
        let before = fs::read(&path).unwrap();
        let (next, backup) = reset_launcher_settings(&path, &settings, true).unwrap();
        assert_eq!(next.game_root, settings.game_root);
        assert_eq!(next.offline_name, settings.offline_name);
        assert_eq!(next.microsoft_client_id, settings.microsoft_client_id);
        assert_eq!(next.ui_theme, Settings::default().ui_theme);
        assert_eq!(fs::read(backup).unwrap(), before);
    }

    use super::*;
    #[test]
    fn rename_and_unregister_are_registration_only_and_failed_save_keeps_data() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("keep"), b"original").unwrap();
        let settings = Settings {
            game_root: a.clone(),
            ..Default::default()
        };
        let p = d.path().join("settings.json");
        let s = register_game_root(&p, &settings, &b, "第二目录").unwrap();
        let s = rename_game_root(&p, &s, &a, "显示名").unwrap();
        assert_eq!(game_root_label(&s, &a), "显示名");
        assert!(a.exists());
        let s = unregister_game_root(&p, &s, &a).unwrap();
        assert_eq!(s.game_root, b.canonicalize().unwrap());
        assert_eq!(fs::read(a.join("keep")).unwrap(), b"original");
        assert!(unregister_game_root(&p, &s, &b).is_err());
        let target = d.path().join("new");
        assert!(create_game_root(d.path(), &s, &target, "new").is_err());
        assert!(!target.exists());
    }
    #[test]
    fn reset_preserves_game_and_display_and_cancel_leaves_config_exact() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("versions/v")).unwrap();
        fs::write(d.path().join("versions/v/save.bin"), b"save").unwrap();
        fs::write(d.path().join("versions/v/v.json"), b"{}").unwrap();
        let previous = InstanceSettings {
            jvm_arguments: "-Dexample=true".into(),
            favorite: true,
            description: "mine".into(),
            ..Default::default()
        };
        save_instance_settings(d.path(), "v", &previous).unwrap();
        let p = instance_settings_path(d.path(), "v").unwrap();
        let old = fs::read(&p).unwrap();
        assert!(reset_instance_settings(d.path(), "v", &AtomicBool::new(true)).is_err());
        assert_eq!(fs::read(&p).unwrap(), old);
        let backup = reset_instance_settings(d.path(), "v", &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), old);
        let next = load_instance_settings(d.path(), "v").unwrap();
        assert!(next.jvm_arguments.is_empty() && next.favorite);
        assert_eq!(next.description, "mine");
        restore_instance_settings(d.path(), "v", &backup, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            load_instance_settings(d.path(), "v").unwrap().jvm_arguments,
            previous.jvm_arguments
        );
        assert_eq!(
            fs::read(d.path().join("versions/v/save.bin")).unwrap(),
            b"save"
        );
    }
}
