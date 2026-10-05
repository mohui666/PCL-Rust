//! Explicit Mod updates: identify by official hashes, prepare in isolation, then
//! move old files to a recoverable backup. Never infer a project from a filename.
use crate::{
    curseforge, install,
    instances::{identity, Identity},
    metadata,
    model::Progress,
    mods,
    resources::{self, ModrinthVersion, ResourceKind},
};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const FILE_LIMIT: u64 = 512 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 20 * 1024 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct ModUpdate {
    pub file_name: String,
    pub name: String,
    pub current_version: String,
    pub new_version: String,
    pub enabled: bool,
    pub source: String,
    version: ModrinthVersion,
}
pub struct UpdatePlan {
    instance: PathBuf,
    instance_identity: Identity,
    mods_identity: Identity,
    minecraft: String,
    loader: String,
    snapshot: Snapshot,
    updates: Vec<ModUpdate>,
    unmatched: Vec<String>,
    issues: Vec<String>,
}
impl UpdatePlan {
    pub fn updates(&self) -> &[ModUpdate] {
        &self.updates
    }
    pub fn unmatched(&self) -> &[String] {
        &self.unmatched
    }
    pub fn issues(&self) -> &[String] {
        &self.issues
    }
    pub fn instance(&self) -> &Path {
        &self.instance
    }
}
#[derive(Debug)]
pub struct UpdateReport {
    pub updated: usize,
    pub added_dependencies: usize,
    pub backup_directory: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: Identity,
    size: u64,
    hash: String,
}
type Snapshot = BTreeMap<String, Stamp>;
fn ordinary_directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "Mod 更新目录不是普通目录：{}",
        path.display()
    );
    Ok(())
}
fn stamp(path: &Path, cancel: &AtomicBool) -> Result<Stamp> {
    let before = identity(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= FILE_LIMIT,
        "Mod 文件类型或大小不安全"
    );
    let mut file = fs::File::open(path)?;
    let mut hash = Sha512::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        install::cancelled(cancel)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        ensure!(size <= meta.len(), "Mod 文件在检查期间增长");
        hash.update(&buffer[..n]);
    }
    ensure!(
        size == meta.len() && identity(path)? == before,
        "Mod 文件在检查期间变更"
    );
    Ok(Stamp {
        identity: before,
        size,
        hash: format!("{:x}", hash.finalize()),
    })
}
fn snapshot(directory: &Path, cancel: &AtomicBool) -> Result<Snapshot> {
    ordinary_directory(directory)?;
    let mut out = BTreeMap::new();
    let mut case_names = BTreeSet::new();
    let mut total = 0;
    for entry in fs::read_dir(directory)? {
        install::cancelled(cancel)?;
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Mod 文件名不是 Unicode"))?;
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".jar") && !lower.ends_with(".jar.disabled") {
            continue;
        }
        metadata::validate_id(&name)?;
        ensure!(
            out.len() < 1000 && case_names.insert(lower),
            "Mod 数量超过限制或文件名存在大小写冲突"
        );
        let item = stamp(&entry.path(), cancel)?;
        total += item.size;
        ensure!(total <= TOTAL_LIMIT, "Mod 总大小超过 20 GiB 检查限制");
        out.insert(name, item);
    }
    Ok(out)
}
fn verify_plan(plan: &UpdatePlan, expected: &Snapshot, cancel: &AtomicBool) -> Result<()> {
    ensure!(
        identity(&plan.instance)? == plan.instance_identity
            && identity(&plan.instance.join("mods"))? == plan.mods_identity,
        "Mod 所属目录已变更，请重新检查更新"
    );
    ensure!(
        snapshot(&plan.instance.join("mods"), cancel)? == *expected,
        "Mod 文件在检查更新后发生变化，请重新检查"
    );
    Ok(())
}
fn file_matches(version: &ModrinthVersion, hash: &str) -> bool {
    version.files.iter().any(|file| {
        file.hashes
            .get("sha512")
            .is_some_and(|h| h.eq_ignore_ascii_case(hash))
    })
}
fn compatible(v: &ModrinthVersion, minecraft: &str, loader: &str) -> bool {
    v.game_versions.iter().any(|s| s == minecraft)
        && v.loaders.iter().any(|s| s == loader)
        && !matches!(
            v.environment.as_deref(),
            Some("server_only" | "dedicated_server_only")
        )
}
/// Network lookups send hashes/fingerprints only, never local paths or file contents.
pub fn check_updates(
    instance: &Path,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
) -> Result<UpdatePlan> {
    check_with(
        instance,
        minecraft,
        loader,
        cancel,
        |existing| {
            let hashes: Vec<_> = existing.values().map(|s| s.hash.clone()).collect();
            let mut found = resources::identify_hashes(&hashes, cancel)?;
            let unmatched: BTreeMap<_, _> = existing
                .iter()
                .filter(|(_, s)| !found.iter().any(|v| file_matches(v, &s.hash)))
                .map(|(name, s)| (instance.join("mods").join(name), s.hash.clone()))
                .collect();
            let mut issues = Vec::new();
            if !unmatched.is_empty() {
                if curseforge::has_api_key()? {
                    found.extend(curseforge::identify_files_with_disabled(
                        &unmatched,
                        ResourceKind::Mod,
                        true,
                        cancel,
                    )?);
                } else {
                    issues
                        .push("未配置 CurseForge API Key；未匹配的文件仅查询了 Modrinth。".into());
                }
            }
            Ok((found, issues))
        },
        |project| resources::list_versions(project, minecraft, loader, cancel),
    )
}
fn check_with(
    instance: &Path,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
    identify: impl FnOnce(&Snapshot) -> Result<(Vec<ModrinthVersion>, Vec<String>)>,
    mut versions: impl FnMut(&str) -> Result<Vec<ModrinthVersion>>,
) -> Result<UpdatePlan> {
    install::cancelled(cancel)?;
    ensure!(
        instance.is_absolute()
            && !minecraft.is_empty()
            && matches!(loader, "fabric" | "quilt" | "forge" | "neoforge"),
        "Mod 更新需要明确的游戏版本和加载器"
    );
    ordinary_directory(instance)?;
    let instance = instance.canonicalize()?;
    let instance_identity = identity(&instance)?;
    let mods_identity = identity(&instance.join("mods"))?;
    let snapshot = snapshot(&instance.join("mods"), cancel)?;
    let (identified, mut issues) = identify(&snapshot)?;
    let local: BTreeMap<_, _> = mods::list_mods(&instance)?
        .into_iter()
        .map(|m| (m.file_name.clone(), m))
        .collect();
    let mut updates = Vec::new();
    let mut unmatched = Vec::new();
    let mut cache = BTreeMap::new();
    for (name, stamp) in &snapshot {
        install::cancelled(cancel)?;
        let Some(current) = identified.iter().find(|v| file_matches(v, &stamp.hash)) else {
            unmatched.push(name.clone());
            continue;
        };
        if identified
            .iter()
            .filter(|v| file_matches(v, &stamp.hash))
            .any(|v| v.project_id != current.project_id)
        {
            issues.push(format!("{name}：同一文件对应多个项目，未自动选择"));
            continue;
        }
        if let Some(error) = local.get(name).and_then(|m| m.error.as_ref()) {
            issues.push(format!("{name}：无法读取 Mod 元数据：{error}"));
            continue;
        }
        if !cache.contains_key(&current.project_id) {
            cache.insert(current.project_id.clone(), versions(&current.project_id)?);
        }
        let next = cache[&current.project_id]
            .iter()
            .filter(|v| {
                v.project_id == current.project_id
                    && v.id != current.id
                    && v.date_published > current.date_published
                    && compatible(v, minecraft, loader)
                    && (current.version_type != "release" || v.version_type == "release")
                    && !file_matches(v, &stamp.hash)
            })
            .max_by(|a, b| a.date_published.cmp(&b.date_published));
        let Some(next) = next else {
            continue;
        };
        resources::resource_file(ResourceKind::Mod, next)?;
        updates.push(ModUpdate {
            file_name: name.clone(),
            name: local
                .get(name)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| name.clone()),
            current_version: current.version_number.clone(),
            new_version: next.version_number.clone(),
            enabled: !name.to_ascii_lowercase().ends_with(".disabled"),
            source: if next.id.starts_with("cf:") {
                "CurseForge"
            } else {
                "Modrinth"
            }
            .into(),
            version: next.clone(),
        });
    }
    let plan = UpdatePlan {
        instance,
        instance_identity,
        mods_identity,
        minecraft: minecraft.into(),
        loader: loader.into(),
        snapshot,
        updates,
        unmatched,
        issues,
    };
    verify_plan(&plan, &plan.snapshot, cancel)?;
    Ok(plan)
}
/// Download and verify every selected update and required dependency before any
/// existing Mod is moved. Successful old files remain in backup_directory.
pub fn apply_updates(
    plan: &UpdatePlan,
    selected: &BTreeSet<String>,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<UpdateReport> {
    apply_with(
        plan,
        selected,
        cancel,
        |shadow, updates| {
            for update in updates {
                install::cancelled(cancel)?;
                let install_plan = resources::plan_mod_install(
                    shadow,
                    &update.version.id,
                    &plan.minecraft,
                    &plan.loader,
                    cancel,
                )?;
                resources::execute_install_plan(&install_plan, cancel, &progress)?;
            }
            // Restore the disabled state before checking dependencies of enabled mods.
            for update in updates.iter().filter(|u| !u.enabled) {
                let name = &resources::resource_file(ResourceKind::Mod, &update.version)?.filename;
                mods::set_mod_enabled(shadow, name, false)?;
            }
            for update in updates.iter().filter(|u| u.enabled) {
                let check = resources::plan_mod_install(
                    shadow,
                    &update.version.id,
                    &plan.minecraft,
                    &plan.loader,
                    cancel,
                )?;
                ensure!(
                    check.resources().iter().all(|r| r.reused),
                    "所选更新依赖被禁用或与其他更新冲突，未替换原 Mod"
                );
            }
            Ok(())
        },
        |_| Ok(()),
    )
}
fn apply_with(
    plan: &UpdatePlan,
    selected: &BTreeSet<String>,
    cancel: &AtomicBool,
    stage_updates: impl FnOnce(&Path, &[&ModUpdate]) -> Result<()>,
    mut after_move: impl FnMut(usize) -> Result<()>,
) -> Result<UpdateReport> {
    install::cancelled(cancel)?;
    ensure!(!selected.is_empty(), "请先选择要更新的 Mod");
    let updates: Vec<_> = plan
        .updates
        .iter()
        .filter(|u| selected.contains(&u.file_name))
        .collect();
    ensure!(
        updates.len() == selected.len(),
        "更新选项与原检查结果不匹配"
    );
    let projects: BTreeSet<_> = updates.iter().map(|u| &u.version.project_id).collect();
    ensure!(
        projects.len() == updates.len(),
        "同一项目存在多份 Mod，请先整理重复文件"
    );
    verify_plan(plan, &plan.snapshot, cancel)?;
    let work = tempfile::tempdir_in(&plan.instance)?;
    let shadow = work.path().join("instance");
    fs::create_dir(&shadow)?;
    fs::create_dir(shadow.join("mods"))?;
    for (name, original) in &plan.snapshot {
        if selected.contains(name) {
            continue;
        }
        install::cancelled(cancel)?;
        let from = plan.instance.join("mods").join(name);
        ensure!(stamp(&from, cancel)? == *original, "Mod 在准备更新时变化");
        let mut input = fs::File::open(&from)?.take(FILE_LIMIT + 1);
        let to = shadow.join("mods").join(name);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)?;
        let copied = std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        ensure!(
            copied == original.size && stamp(&to, cancel)?.hash == original.hash,
            "Mod 在复制更新快照时变化"
        );
    }
    stage_updates(&shadow, &updates)?;
    install::cancelled(cancel)?;
    let staged = snapshot(&shadow.join("mods"), cancel)?;
    for (name, old) in &plan.snapshot {
        if !selected.contains(name) {
            ensure!(
                staged.get(name).is_some_and(|s| s.hash == old.hash),
                "更新计划改动了未选择的 Mod"
            );
        }
    }
    let additions: Vec<_> = staged
        .iter()
        .filter(|(name, _)| !plan.snapshot.contains_key(*name) || selected.contains(*name))
        .collect();
    for update in &updates {
        let f = resources::resource_file(ResourceKind::Mod, &update.version)?;
        let name = format!(
            "{}{}",
            f.filename,
            if update.enabled { "" } else { ".disabled" }
        );
        let actual = staged
            .get(&name)
            .context("准备的更新文件缺失或启用状态不符")?;
        let path = shadow.join("mods").join(name);
        ensure!(actual.size == f.size, "更新文件大小不匹配");
        // CF may publish SHA1 only; the resource downloader verifies it, and this
        // final check repeats that mandatory digest before touching originals.
        let mut file = fs::File::open(path)?;
        let mut sha1 = sha1::Sha1::new();
        let mut buffer = [0; 65536];
        loop {
            install::cancelled(cancel)?;
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            sha1.update(&buffer[..n]);
        }
        ensure!(
            f.hashes
                .get("sha1")
                .is_some_and(|h| h.eq_ignore_ascii_case(&format!("{:x}", sha1.finalize()))),
            "更新文件 SHA1 不匹配"
        );
        if let Some(hash) = f.hashes.get("sha512") {
            ensure!(
                actual.hash.eq_ignore_ascii_case(hash),
                "更新文件 SHA512 不匹配"
            );
        }
    }
    verify_plan(plan, &plan.snapshot, cancel)?;
    let store = child_directory(&plan.instance, "PCL-Rust")?;
    let backups = child_directory(&store, "mod-backups")?;
    let backup = tempfile::Builder::new()
        .prefix("update-")
        .tempdir_in(backups)?
        .keep();
    let backup_identity = identity(&backup)?;
    let mut expected = plan.snapshot.clone();
    let mut old_moves = Vec::new();
    let mut new_moves: Vec<(String, Stamp)> = Vec::new();
    let result = (|| {
        for update in &updates {
            verify_plan(plan, &expected, cancel)?;
            ensure!(identity(&backup)? == backup_identity, "备份目录已变更");
            metadata::rename_directory_no_replace(
                &plan.instance.join("mods").join(&update.file_name),
                &backup.join(&update.file_name),
            )?;
            old_moves.push(update.file_name.clone());
            expected.remove(&update.file_name);
            after_move(old_moves.len() + new_moves.len())?;
        }
        for (name, staged_stamp) in &additions {
            verify_plan(plan, &expected, cancel)?;
            ensure!(
                !fs::read_dir(plan.instance.join("mods"))?
                    .any(|e| e
                        .is_ok_and(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))),
                "更新文件名与已有文件冲突"
            );
            let from = shadow.join("mods").join(name);
            ensure!(
                stamp(&from, cancel)? == **staged_stamp,
                "暂存更新文件已变更"
            );
            metadata::rename_directory_no_replace(&from, &plan.instance.join("mods").join(name))?;
            new_moves.push(((*name).clone(), (*staged_stamp).clone()));
            expected.insert((*name).clone(), (*staged_stamp).clone());
            after_move(old_moves.len() + new_moves.len())?;
        }
        verify_plan(plan, &expected, cancel)
    })();
    if let Err(error) = result {
        let rollback = rollback(plan, &backup, backup_identity, &old_moves, &new_moves);
        return match rollback {
            Ok(()) => Err(error.context("Mod 更新未完成，已恢复本次移动的原文件")),
            Err(rollback) => Err(anyhow::anyhow!(
                "Mod 更新未完成：{error:#}；部分回滚未完成：{rollback:#}。原文件备份保留在 {}",
                backup.display()
            )),
        };
    }
    Ok(UpdateReport {
        updated: updates.len(),
        added_dependencies: additions.len().saturating_sub(updates.len()),
        backup_directory: backup,
    })
}
fn child_directory(parent: &Path, name: &str) -> Result<PathBuf> {
    ordinary_directory(parent)?;
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    ordinary_directory(&path)?;
    Ok(path)
}
fn rollback(
    plan: &UpdatePlan,
    backup: &Path,
    backup_identity: Identity,
    old: &[String],
    new: &[(String, Stamp)],
) -> Result<()> {
    let never_cancel = AtomicBool::new(false);
    ensure!(
        identity(&plan.instance)? == plan.instance_identity
            && identity(&plan.instance.join("mods"))? == plan.mods_identity
            && identity(backup)? == backup_identity,
        "原目录或备份目录被替换，保留全部文件"
    );
    let mut errors = Vec::new();
    for (name, expected) in new.iter().rev() {
        let path = plan.instance.join("mods").join(name);
        match stamp(&path, &never_cancel).and_then(|s| {
            ensure!(s == *expected, "新增文件已被其他程序改动");
            fs::remove_file(&path)?;
            Ok(())
        }) {
            Ok(()) => (),
            Err(e) => errors.push(format!("{name}：{e:#}")),
        }
    }
    for name in old.iter().rev() {
        let from = backup.join(name);
        let result = (|| {
            ensure!(
                stamp(&from, &never_cancel)? == plan.snapshot[name],
                "备份文件已被改动"
            );
            metadata::rename_directory_no_replace(&from, &plan.instance.join("mods").join(name))
        })();
        if let Err(e) = result {
            errors.push(format!("{name}：{e:#}"));
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("；"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, sync::atomic::Ordering};
    fn jar(version: &str) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        write!(zip, "{{\"schemaVersion\":1,\"id\":\"fixture\",\"version\":\"{version}\",\"name\":\"Fixture\"}}").unwrap();
        zip.finish().unwrap().into_inner()
    }
    fn version(id: &str, bytes: &[u8], date: &str) -> ModrinthVersion {
        serde_json::from_value(serde_json::json!({"id":id,"project_id":"fixture","name":id,"version_number":id,
            "version_type":"release","date_published":date,"game_versions":["1.21.1"],"loaders":["fabric"],
            "files":[{"hashes":{"sha512":format!("{:x}",Sha512::digest(bytes)),"sha1":format!("{:x}",sha1::Sha1::digest(bytes))},
            "url":"https://cdn.modrinth.com/data/fixture/new.jar","filename":"new.jar","primary":true,"size":bytes.len(),"file_type":null}]})).unwrap()
    }
    fn fixture(disabled: bool) -> (tempfile::TempDir, UpdatePlan, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("mods")).unwrap();
        let old = jar("1");
        let new = jar("2");
        let name = if disabled {
            "renamed.jar.DISABLED"
        } else {
            "renamed.jar"
        };
        fs::write(dir.path().join("mods").join(name), &old).unwrap();
        fs::write(dir.path().join("mods/user-note.txt"), b"untouched").unwrap();
        let plan = check_with(
            dir.path(),
            "1.21.1",
            "fabric",
            &AtomicBool::new(false),
            |_| Ok((vec![version("v1", &old, "2026-01-01")], vec![])),
            |_| Ok(vec![version("v2", &new, "2026-01-02")]),
        )
        .unwrap();
        (dir, plan, new)
    }
    fn selection(plan: &UpdatePlan) -> BTreeSet<String> {
        plan.updates.iter().map(|u| u.file_name.clone()).collect()
    }
    fn stage(shadow: &Path, bytes: &[u8], disabled: bool) -> Result<()> {
        fs::write(
            shadow.join("mods").join(if disabled {
                "new.jar.disabled"
            } else {
                "new.jar"
            }),
            bytes,
        )?;
        Ok(())
    }
    #[test]
    fn identifies_renamed_disabled_file_by_hash_and_preserves_state_and_backup() {
        let (dir, plan, new) = fixture(true);
        assert_eq!(plan.updates.len(), 1);
        assert!(!plan.updates[0].enabled);
        let old = fs::read(dir.path().join("mods/renamed.jar.DISABLED")).unwrap();
        let report = apply_with(
            &plan,
            &selection(&plan),
            &AtomicBool::new(false),
            |s, _| stage(s, &new, true),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(report.added_dependencies, 0);
        assert_eq!(
            fs::read(report.backup_directory.join("renamed.jar.DISABLED")).unwrap(),
            old
        );
        assert_eq!(
            fs::read(dir.path().join("mods/new.jar.disabled")).unwrap(),
            new
        );
        assert_eq!(
            fs::read(dir.path().join("mods/user-note.txt")).unwrap(),
            b"untouched"
        );
    }
    #[test]
    fn candidate_cannot_cross_loader_or_release_channel_and_unknown_is_reported() {
        let (dir, _, new) = fixture(false);
        let old = jar("1");
        fs::write(dir.path().join("mods/unknown.jar"), jar("unmatched")).unwrap();
        let plan = check_with(
            dir.path(),
            "1.21.1",
            "fabric",
            &AtomicBool::new(false),
            |_| Ok((vec![version("v1", &old, "2026-01-01")], vec![])),
            |_| {
                let mut wrong = version("wrong", &new, "2026-01-03");
                wrong.loaders = vec!["forge".into()];
                let mut beta = version("beta", &new, "2026-01-04");
                beta.version_type = "beta".into();
                Ok(vec![wrong, beta])
            },
        )
        .unwrap();
        assert!(plan.updates.is_empty());
        assert_eq!(plan.unmatched, ["unknown.jar"]);
    }
    #[test]
    fn changed_source_or_new_unselected_file_aborts_before_download() {
        let (dir, plan, new) = fixture(false);
        fs::write(dir.path().join("mods/renamed.jar"), &new).unwrap();
        assert!(apply_with(
            &plan,
            &selection(&plan),
            &AtomicBool::new(false),
            |_, _| panic!("must not stage"),
            |_| Ok(())
        )
        .is_err());
        assert_eq!(fs::read(dir.path().join("mods/renamed.jar")).unwrap(), new);
    }
    #[test]
    fn same_name_unknown_destination_is_not_overwritten_and_old_restored() {
        let (dir, plan, new) = fixture(false);
        fs::write(dir.path().join("mods/new.jar"), b"unknown").unwrap();
        // A file appearing after the check invalidates the whole original snapshot.
        assert!(apply_with(
            &plan,
            &selection(&plan),
            &AtomicBool::new(false),
            |s, _| stage(s, &new, false),
            |_| Ok(())
        )
        .is_err());
        assert_eq!(
            fs::read(dir.path().join("mods/new.jar")).unwrap(),
            b"unknown"
        );
        assert!(dir.path().join("mods/renamed.jar").is_file());
    }
    #[test]
    fn corrupt_prepared_file_never_moves_original() {
        let (dir, plan, _) = fixture(false);
        assert!(apply_with(
            &plan,
            &selection(&plan),
            &AtomicBool::new(false),
            |s, _| stage(s, b"corrupt", false),
            |_| Ok(())
        )
        .is_err());
        assert_eq!(
            fs::read(dir.path().join("mods/renamed.jar")).unwrap(),
            jar("1")
        );
    }
    #[test]
    fn cancellation_after_move_restores_original_and_retains_typed_cause() {
        let (dir, plan, new) = fixture(false);
        let cancel = AtomicBool::new(false);
        let error = apply_with(
            &plan,
            &selection(&plan),
            &cancel,
            |s, _| stage(s, &new, false),
            |_| {
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.is::<crate::model::OperationCancelled>());
        assert_eq!(
            fs::read(dir.path().join("mods/renamed.jar")).unwrap(),
            jar("1")
        );
        assert!(!dir.path().join("mods/new.jar").exists());
        // A failed attempt does not mutate plan file identities: retry is possible.
        cancel.store(false, Ordering::Relaxed);
        apply_with(
            &plan,
            &selection(&plan),
            &cancel,
            |s, _| stage(s, &new, false),
            |_| Ok(()),
        )
        .unwrap();
    }
    #[test]
    fn rollback_never_deletes_concurrent_user_edit_and_reports_retained_backup() {
        let (dir, plan, new) = fixture(false);
        let error = apply_with(
            &plan,
            &selection(&plan),
            &AtomicBool::new(false),
            |s, _| stage(s, &new, false),
            |n| {
                if n == 2 {
                    fs::write(dir.path().join("mods/new.jar"), b"user edit")?;
                    bail!("injected write failure");
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("部分回滚未完成"));
        assert_eq!(
            fs::read(dir.path().join("mods/new.jar")).unwrap(),
            b"user edit"
        );
        assert_eq!(
            fs::read(dir.path().join("mods/renamed.jar")).unwrap(),
            jar("1")
        );
    }
    #[test]
    #[cfg(unix)]
    fn symlink_mod_or_directory_is_rejected() {
        use std::os::unix::fs::symlink;
        let (dir, _, _) = fixture(false);
        symlink(
            dir.path().join("mods/renamed.jar"),
            dir.path().join("mods/link.jar"),
        )
        .unwrap();
        assert!(snapshot(&dir.path().join("mods"), &AtomicBool::new(false)).is_err());
    }
}
