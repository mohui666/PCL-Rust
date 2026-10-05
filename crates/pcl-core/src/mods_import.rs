//! Staged local drop/import, never replacing a file or deleting the source.
use super::*;
use crate::{install::cancelled, instances::identity};
use std::sync::atomic::AtomicBool;
pub fn imported_mod_name(name: &str) -> Result<String> {
    validate_id(name)?;
    let lower = name.to_ascii_lowercase();
    let mut result = if lower.ends_with(".disabled") {
        name[..name.len() - 9].to_owned()
    } else if lower.ends_with(".old") {
        name[..name.len() - 4].to_owned()
    } else {
        name.into()
    };
    if !result.contains('.') {
        result.push_str(".jar");
    }
    if ![".jar", ".litemod"]
        .iter()
        .any(|suffix| result.to_ascii_lowercase().ends_with(suffix))
    {
        bail!("请拖入 JAR 或 LiteLoader Mod 文件");
    }
    validate_id(&result)?;
    Ok(result)
}
pub fn import_mods(instance: &Path, sources: &[PathBuf], cancel: &AtomicBool) -> Result<usize> {
    cancelled(cancel)?;
    if sources.is_empty() || sources.len() > 200 {
        bail!("每次请选择 1 至 200 个 Mod");
    }
    let instance = instance.canonicalize()?;
    let directory = mods_directory(&instance, false)?;
    let mut names = std::collections::HashSet::new();
    let mut plan = Vec::new();
    for source in sources {
        let meta = fs::symlink_metadata(source)?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 512 * 1024 * 1024 {
            bail!("Mod 必须为不超过 512 MiB 的普通文件");
        }
        let name = imported_mod_name(
            source
                .file_name()
                .and_then(|n| n.to_str())
                .context("Mod 文件名无效")?,
        )?;
        if !names.insert(name.to_lowercase()) {
            bail!("所选文件安装后重名：{name}");
        }
        let target = directory.join(&name);
        if source.canonicalize()? == target {
            continue;
        }
        if directory.exists()
            && fs::read_dir(&directory)?.any(|e| {
                e.is_ok_and(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(&name))
            })
        {
            bail!("同名 Mod 已存在，未覆盖：{name}");
        }
        plan.push((
            source.clone(),
            name,
            identity(source)?,
            meta.len(),
            meta.modified()?,
        ));
    }
    if plan.is_empty() {
        return Ok(0);
    }
    let stage = tempfile::Builder::new()
        .prefix(".pcl-mod-import-")
        .tempdir_in(&instance)?;
    for (source, name, stamp, size, modified) in &plan {
        cancelled(cancel)?;
        let mut input = File::open(source)?;
        let mut output = File::create(stage.path().join(name))?;
        let mut copied = 0;
        let mut buffer = [0; 65536];
        loop {
            cancelled(cancel)?;
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            if copied > *size {
                bail!("Mod 在复制时发生改变");
            }
            output.write_all(&buffer[..n])?;
        }
        output.sync_all()?;
        let after = fs::symlink_metadata(source)?;
        if identity(source)? != *stamp
            || after.len() != *size
            || after.modified()? != *modified
            || copied != *size
        {
            bail!("Mod 在复制时发生改变");
        }
        inspect(&stage.path().join(name), name)?;
    }
    cancelled(cancel)?;
    let directory = mods_directory(&instance, true)?;
    let directory_id = identity(&directory)?;
    let mut committed = Vec::new();
    let result = (|| {
        for (_, name, ..) in &plan {
            cancelled(cancel)?;
            if identity(&directory)? != directory_id {
                bail!("Mod 目标目录已改变");
            }
            if fs::read_dir(&directory)?.any(|e| {
                e.is_ok_and(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
            }) {
                bail!("同名 Mod 已出现，未覆盖：{name}");
            }
            let source = stage.path().join(name);
            let target = directory.join(name);
            let stamp = identity(&source)?;
            fs::hard_link(source, &target)?;
            committed.push((target, stamp));
        }
        cancelled(cancel)
    })();
    if let Err(error) = result {
        let mut retained = Vec::new();
        for (path, stamp) in committed.iter().rev() {
            if identity(path).ok() != Some(*stamp) || fs::remove_file(path).is_err() {
                retained.push(path.display().to_string());
            }
        }
        if !retained.is_empty() {
            return Err(error.context(format!(
                "未能回滚已改变文件，已保留：{}",
                retained.join("；")
            )));
        }
        return Err(error);
    }
    Ok(committed.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn jar(path: &Path) {
        let mut z = zip::ZipWriter::new(File::create(path).unwrap());
        z.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(br#"{"id":"fixture","version":"1"}"#).unwrap();
        z.finish().unwrap();
    }
    #[test]
    fn batch_preflight_preserves_sources_and_existing_and_handles_disabled() {
        let d = tempfile::tempdir().unwrap();
        let instance = d.path().join("instance");
        fs::create_dir(&instance).unwrap();
        let a = d.path().join("a.jar.disabled");
        jar(&a);
        let before = fs::read(&a).unwrap();
        assert_eq!(
            import_mods(&instance, std::slice::from_ref(&a), &AtomicBool::new(false)).unwrap(),
            1
        );
        assert_eq!(fs::read(&a).unwrap(), before);
        assert_eq!(fs::read(instance.join("mods/a.jar")).unwrap(), before);
        assert!(import_mods(&instance, &[a], &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn invalid_second_mod_and_cancel_commit_nothing() {
        let d = tempfile::tempdir().unwrap();
        let instance = d.path().join("instance");
        fs::create_dir(&instance).unwrap();
        let a = d.path().join("a.jar");
        jar(&a);
        let b = d.path().join("b.jar");
        fs::write(&b, b"broken").unwrap();
        assert!(import_mods(&instance, &[a.clone(), b], &AtomicBool::new(false)).is_err());
        assert!(!instance.join("mods").exists());
        assert!(import_mods(&instance, &[a], &AtomicBool::new(true)).is_err());
        assert!(!instance.join("mods").exists());
    }
}
