//! Complete .mrpack installation: dependency preflight, files, then no-clobber version registration.
use crate::{
    install,
    loaders::{self, LoaderKind},
    metadata::{confined_path, library_artifacts, resolve_version, safe_relative, validate_id},
    model::{Platform, Progress},
    modpack::{self, ModpackInfo},
};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Debug)]
struct Dependencies {
    minecraft: String,
    loader: Option<(PackLoader, String)>,
}
#[derive(Debug, Clone, Copy)]
enum PackLoader {
    Meta(LoaderKind),
    Forge(crate::forge::ForgeKind),
}

fn dependencies(info: &ModpackInfo) -> Result<Dependencies> {
    let minecraft = info
        .dependencies
        .get("minecraft")
        .context("整合包缺少 Minecraft 依赖")?
        .clone();
    validate_id(&minecraft)?;
    let mut loader = None;
    for (name, version) in &info.dependencies {
        validate_id(version)?;
        let kind = match name.as_str() {
            "minecraft" => continue,
            "fabric-loader" => PackLoader::Meta(LoaderKind::Fabric),
            "quilt-loader" => PackLoader::Meta(LoaderKind::Quilt),
            "forge" => PackLoader::Forge(crate::forge::ForgeKind::Forge),
            "neoforge" => PackLoader::Forge(crate::forge::ForgeKind::NeoForge),
            _ => bail!(
                "暂不支持整合包依赖 {name}；当前仅支持 Minecraft 加一个 Fabric、Quilt、Forge 或 NeoForge，未开始下载"
            ),
        };
        if loader.is_some() {
            bail!("整合包同时声明多个加载器，未开始下载");
        }
        loader = Some((kind, version.clone()));
    }
    Ok(Dependencies { minecraft, loader })
}

fn absent(path: &Path, description: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("{description}已存在，不会覆盖已有数据：{}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("无法检查{description}")),
    }
}

fn destinations(root: &Path, id: &str) -> Result<(PathBuf, PathBuf)> {
    validate_id(id)?;
    let version_relative = safe_relative(&format!("versions/{id}/{id}.json"))?;
    let instance_relative = safe_relative(&format!("instances/{id}"))?;
    let version = install::safe_target(root, &version_relative)?;
    let instance = install::safe_target(root, &instance_relative)?;
    if root.exists() {
        confined_path(root, &version_relative)?;
        confined_path(root, &instance_relative)?;
    }
    absent(&version, "目标版本 JSON")?;
    absent(&instance, "目标实例目录")?;
    let natives = install::safe_target(root, &safe_relative(&format!("versions/{id}/natives"))?)?;
    absent(&natives, "目标原生库目录")?;
    Ok((version, instance))
}

fn register_version(
    root: &Path,
    id: &str,
    parent: &str,
    kind: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    validate_id(id)?;
    validate_id(parent)?;
    if id.eq_ignore_ascii_case(parent) {
        bail!("整合包实例不能与父版本同名");
    }
    install::cancelled(cancel)?;
    let relative = safe_relative(&format!("versions/{id}/{id}.json"))?;
    let path = install::safe_target(root, &relative)?;
    confined_path(root, &relative)?;
    let directory = path.parent().context("版本路径没有父目录")?;
    fs::create_dir_all(directory)?;
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(
        &mut staged,
        &json!({ "id": id, "inheritsFrom": parent, "type": kind }),
    )?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    install::cancelled(cancel)?;
    install::safe_target(root, &relative)?;
    staged
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .context("无法登记整合包版本（目标冲突或写入失败，未覆盖已有版本）")?;
    Ok(())
}

fn prepare_natives(
    root: &Path,
    id: &str,
    parent: &serde_json::Value,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<()> {
    let natives: Vec<_> = library_artifacts(parent, platform)?
        .into_iter()
        .filter(|artifact| artifact.native)
        .collect();
    if natives.is_empty() {
        return Ok(());
    }
    let target = install::safe_target(root, &safe_relative(&format!("versions/{id}/natives"))?)?;
    fs::create_dir_all(target.parent().context("原生库目录没有父目录")?)?;
    fs::create_dir(&target).context("整合包原生库目录已存在或无法创建；不会覆盖已有目录")?;
    for artifact in natives {
        install::cancelled(cancel)?;
        let source = install::safe_target(root, &artifact.relative_path)?;
        if !install::cache_valid(&source, &artifact, cancel)? {
            bail!(
                "父版本原生库缺失或校验失败：{}",
                artifact.relative_path.display()
            );
        }
        install::extract_natives(&source, &target, &artifact.excludes, cancel)?;
    }
    Ok(())
}

/// Install a new pack instance. Existing version JSONs and instance directories are never reused.
/// Unsupported dependencies are rejected before downloading or creating the game root.
/// Files remain available, with an explicit error, if final version registration fails.
pub fn install_pack(
    root: &Path,
    pack: &Path,
    instance_id: &str,
    include_optional: bool,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    install_pack_with_java(
        root,
        pack,
        instance_id,
        include_optional,
        None,
        platform,
        cancel,
        progress,
    )
}

/// A caller-selected Java is used only for official Forge/NeoForge processors.
/// Without one, inspect installed Java runtimes; never auto-download or launch a game.
#[allow(clippy::too_many_arguments)]
pub fn install_pack_with_java(
    root: &Path,
    pack: &Path,
    instance_id: &str,
    include_optional: bool,
    java: Option<&Path>,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    install_with(
        root,
        pack,
        instance_id,
        include_optional,
        platform,
        cancel,
        &progress,
        |dependency| {
            if let Some((PackLoader::Meta(kind), version)) = &dependency.loader {
                loaders::install_loader(
                    root,
                    *kind,
                    &dependency.minecraft,
                    version,
                    platform,
                    cancel,
                    &progress,
                )
            } else if let Some((PackLoader::Forge(kind), version)) = &dependency.loader {
                ensure_forge_version(
                    root,
                    *kind,
                    &dependency.minecraft,
                    version,
                    java,
                    platform,
                    cancel,
                    &progress,
                )
            } else {
                let parent = install::safe_target(
                    root,
                    &safe_relative(&format!("versions/{0}/{0}.json", dependency.minecraft))?,
                )?;
                if !parent.exists() {
                    install::install_version(
                        root,
                        &dependency.minecraft,
                        platform,
                        cancel,
                        &progress,
                    )?;
                }
                progress(Progress {
                    message: format!("只读校验原版 {}", dependency.minecraft),
                    completed: 0,
                    total: 0,
                    ..Default::default()
                });
                install::verify_vanilla_parent(root, &dependency.minecraft, platform, cancel)?;
                Ok(dependency.minecraft.clone())
            }
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub fn ensure_forge_version(
    root: &Path,
    kind: crate::forge::ForgeKind,
    minecraft: &str,
    version: &str,
    java: Option<&Path>,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: &(impl Fn(Progress) + Sync),
) -> Result<String> {
    let id = match kind {
        crate::forge::ForgeKind::Forge => format!("{minecraft}-forge-{version}"),
        crate::forge::ForgeKind::NeoForge => format!("neoforge-{version}"),
    };
    let profile = install::safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
    if profile.exists() {
        let resolved = resolve_version(root, &id)?;
        let receipt = &resolved["_pcl_forge_install"];
        let kind_name = match kind {
            crate::forge::ForgeKind::Forge => "forge",
            crate::forge::ForgeKind::NeoForge => "neoforge",
        };
        let coordinate = match kind {
            crate::forge::ForgeKind::Forge => {
                format!("net.minecraftforge:forge:{minecraft}-{version}")
            }
            crate::forge::ForgeKind::NeoForge => format!("net.neoforged:neoforge:{version}"),
        };
        let declared = resolved["libraries"].as_array().is_some_and(|libraries| {
            libraries.iter().any(|library| {
                library["name"].as_str().is_some_and(|name| {
                    name == coordinate || name.starts_with(&format!("{coordinate}:"))
                })
            })
        });
        anyhow::ensure!(
            receipt["kind"].as_str() == Some(kind_name)
                && receipt["minecraft"].as_str() == Some(minecraft)
                && (receipt["loader"].as_str() == Some(version)
                    || receipt["loader"].is_null() && declared),
            "既有加载器来源/版本不匹配，未覆盖用户版本"
        );
        install::repair_version_with_java(root, &id, java, platform, cancel, progress)?;
        return Ok(id);
    }
    let base = install::safe_target(
        root,
        &PathBuf::from(format!("versions/{minecraft}/{minecraft}.json")),
    )?;
    if !base.exists() {
        install::install_version(root, minecraft, platform, cancel, progress)?;
    }
    let parent = install::verify_vanilla_parent(root, minecraft, platform, cancel)?;
    let required = parent["javaVersion"]["majorVersion"].as_u64().unwrap_or(8) as u32;
    let runtime = if let Some(path) = java {
        crate::java::inspect_java_with_cancel(path, cancel)?
    } else {
        crate::java::discover_java_with_cancel(cancel)?
            .runtimes
            .into_iter()
            .find(|runtime| runtime.major == required && runtime.architecture == platform.arch)
            .with_context(|| {
                format!(
                    "整合包的 {} 安装器需要 Java {required}；请先在设置中选择或下载 Java",
                    kind.label()
                )
            })?
    };
    crate::forge::install_forge(
        root,
        kind,
        minecraft,
        version,
        &runtime.path,
        platform,
        cancel,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn install_with(
    root: &Path,
    pack: &Path,
    instance_id: &str,
    include_optional: bool,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: &(impl Fn(Progress) + Sync),
    ensure_dependencies: impl FnOnce(&Dependencies) -> Result<String>,
) -> Result<String> {
    install::cancelled(cancel)?;
    validate_id(instance_id)?;
    if !root.is_absolute() {
        bail!("Minecraft 根目录必须是绝对路径");
    }
    let info = modpack::inspect_mrpack(pack)?;
    let dependency = dependencies(&info)?;
    if instance_id.eq_ignore_ascii_case(&dependency.minecraft) {
        bail!("整合包实例名称不能与 Minecraft 父版本相同");
    }
    destinations(root, instance_id)?;
    progress(Progress {
        message: format!("准备整合包 {} 的游戏依赖", info.name),
        completed: 0,
        total: 0,
        ..Default::default()
    });
    let parent_id = ensure_dependencies(&dependency)?;
    install::cancelled(cancel)?;
    validate_id(&parent_id)?;
    if instance_id.eq_ignore_ascii_case(&parent_id) {
        bail!("整合包实例名称不能与加载器父版本相同");
    }
    let parent = resolve_version(root, &parent_id).context("整合包父版本未安装就绪")?;
    let kind = parent["type"].as_str().unwrap_or("release").to_owned();
    // Recheck after potentially lengthy dependency downloads, then atomically reserve the instance.
    let (_, instance) = destinations(root, instance_id)?;
    fs::create_dir_all(instance.parent().context("实例目录没有父目录")?)?;
    install::safe_target(root, &safe_relative(&format!("instances/{instance_id}"))?)?;
    fs::create_dir(&instance).context("实例目录已被创建或无法创建；未覆盖已有数据")?;
    let imported = modpack::import_mrpack(pack, &instance, include_optional, cancel, |event| {
        if event.completed == event.total {
            progress(Progress {
                message: "整合包文件已导入，准备登记实例".into(),
                ..event
            });
        } else {
            progress(event);
        }
    });
    let imported = match imported {
        Ok(info) => info,
        Err(error) => {
            // Remove only our empty reservation. Never recursively delete imported/user data.
            let _ = fs::remove_dir(&instance);
            return Err(error).context("整合包文件导入失败，尚未登记版本");
        }
    };
    let finish = (|| {
        install::cancelled(cancel)?;
        if imported.dependencies != info.dependencies {
            bail!("整合包源文件在依赖检查之后发生变化，依赖不一致");
        }
        prepare_natives(root, instance_id, &parent, platform, cancel)?;
        progress(Progress {
            message: "登记整合包实例".into(),
            completed: 0,
            total: 1,
            ..Default::default()
        });
        register_version(root, instance_id, &parent_id, &kind, cancel)
    })();
    if let Err(error) = finish {
        let action = if error.chain().any(|cause| {
            cause
                .downcast_ref::<crate::model::OperationCancelled>()
                .is_some()
        }) {
            "整合包安装已取消，文件已保留"
        } else {
            "整合包文件已保留"
        };
        return Err(error).with_context(|| {
            format!(
                "{action}在 {}，但版本尚未登记；未删除实例数据",
                instance.display()
            )
        });
    }
    progress(Progress {
        message: format!("整合包 {instance_id} 安装完成"),
        completed: 1,
        total: 1,
        ..Default::default()
    });
    Ok(instance_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
    };
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn fixture_pack(path: &Path, dependencies: serde_json::Value) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        zip.start_file("modrinth.index.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(json!({"formatVersion":1,"game":"minecraft","versionId":"1.0","name":"Fixture Pack","files":[],"dependencies":dependencies}).to_string().as_bytes()).unwrap();
        zip.start_file(
            "overrides/config/settings.txt",
            SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"pack setting").unwrap();
        zip.finish().unwrap();
    }

    fn parent(root: &Path) -> Result<String> {
        fs::create_dir_all(root.join("versions/1.21.1"))?;
        fs::write(root.join("versions/1.21.1/1.21.1.json"), json!({"id":"1.21.1","type":"release","mainClass":"net.minecraft.client.main.Main","arguments":{"game":[]},"libraries":[]}).to_string())?;
        Ok("1.21.1".into())
    }

    #[test]
    fn unsupported_and_multiple_loaders_are_rejected_before_any_installation() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture.mrpack");
        let root = temp.path().join("not-created");
        for dependency in [
            json!({"minecraft":"1.21.1","forge":"52.0.1","neoforge":"21.1.1"}),
            json!({"minecraft":"1.21.1","unknown-loader":"1"}),
            json!({"minecraft":"1.21.1","fabric-loader":"0.19.5","quilt-loader":"0.29.0"}),
        ] {
            fixture_pack(&pack, dependency);
            let error = install_with(
                &root,
                &pack,
                "new-pack",
                false,
                &Platform::current(),
                &AtomicBool::new(false),
                &|_| {},
                |_| panic!("dependency installer must not run"),
            )
            .unwrap_err();
            assert!(error.to_string().contains("未开始下载"));
            assert!(!root.exists());
        }
    }

    #[test]
    fn forge_and_neoforge_dependencies_reach_the_real_dependency_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("test.mrpack");
        for (key, version) in [("forge", "52.0.1"), ("neoforge", "21.1.1")] {
            let root = temp.path().join(key);
            fixture_pack(&pack, json!({"minecraft":"1.21.1",key:version}));
            let error = install_with(
                &root,
                &pack,
                "pack",
                false,
                &Platform::current(),
                &AtomicBool::new(false),
                &|_| {},
                |dependency| {
                    assert!(matches!(dependency.loader, Some((PackLoader::Forge(_), _))));
                    bail!("fixture reached official processor dispatch")
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("processor dispatch"));
            assert!(!root.exists());
        }
    }
    #[test]
    fn existing_instance_or_version_is_never_reused_or_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture.mrpack");
        fixture_pack(&pack, json!({"minecraft":"1.21.1"}));
        for relative in ["instances/new-pack", "versions/new-pack/new-pack.json"] {
            let root = temp.path().join(if relative.starts_with("instances") {
                "instance-case"
            } else {
                "version-case"
            });
            let destination = root.join(relative);
            if relative.starts_with("instances") {
                fs::create_dir_all(&destination).unwrap();
                fs::write(destination.join("save.dat"), b"user save").unwrap();
            } else {
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::write(&destination, b"user version").unwrap();
            }
            assert!(install_with(
                &root,
                &pack,
                "new-pack",
                false,
                &Platform::current(),
                &AtomicBool::new(false),
                &|_| {},
                |_| panic!("dependency installer must not run")
            )
            .is_err());
            if destination.is_dir() {
                assert_eq!(
                    fs::read(destination.join("save.dat")).unwrap(),
                    b"user save"
                );
            } else {
                assert_eq!(fs::read(destination).unwrap(), b"user version");
            }
        }
    }

    #[test]
    fn cancellation_before_and_after_dependency_preparation_creates_no_instance() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture.mrpack");
        let root = temp.path().join("game");
        fixture_pack(&pack, json!({"minecraft":"1.21.1"}));
        let cancel = AtomicBool::new(true);
        assert!(install_with(
            &root,
            &pack,
            "new-pack",
            false,
            &Platform::current(),
            &cancel,
            &|_| {},
            |_| panic!("must not run")
        )
        .is_err());
        assert!(!root.exists());
        cancel.store(false, Ordering::Relaxed);
        assert!(install_with(
            &root,
            &pack,
            "new-pack",
            false,
            &Platform::current(),
            &cancel,
            &|_| {},
            |_| {
                cancel.store(true, Ordering::Relaxed);
                parent(&root)
            }
        )
        .is_err());
        assert!(!root.join("instances/new-pack").exists());
        assert!(!root.join("versions/new-pack/new-pack.json").exists());
    }

    #[test]
    fn imports_overrides_then_registers_inherited_instance_without_touching_parent() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture.mrpack");
        let root = temp.path().join("game");
        fixture_pack(&pack, json!({"minecraft":"1.21.1"}));
        parent(&root).unwrap();
        let parent_file = root.join("versions/1.21.1/1.21.1.json");
        let before = fs::read(&parent_file).unwrap();
        let events = Mutex::new(Vec::new());
        let id = install_with(
            &root,
            &pack,
            "new-pack",
            false,
            &Platform::current(),
            &AtomicBool::new(false),
            &|event| events.lock().unwrap().push(event.message),
            |dep| {
                assert_eq!(dep.minecraft, "1.21.1");
                Ok("1.21.1".into())
            },
        )
        .unwrap();
        assert_eq!(id, "new-pack");
        assert_eq!(
            fs::read(root.join("instances/new-pack/config/settings.txt")).unwrap(),
            b"pack setting"
        );
        let version: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("versions/new-pack/new-pack.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            version,
            json!({"id":"new-pack","inheritsFrom":"1.21.1","type":"release"})
        );
        assert_eq!(fs::read(parent_file).unwrap(), before);
        assert!(events.lock().unwrap().last().unwrap().contains("安装完成"));
    }

    #[test]
    fn registration_race_preserves_both_existing_json_and_imported_files() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture.mrpack");
        let root = temp.path().join("game");
        fixture_pack(&pack, json!({"minecraft":"1.21.1"}));
        let target = root.join("versions/new-pack/new-pack.json");
        let error = install_with(
            &root,
            &pack,
            "new-pack",
            false,
            &Platform::current(),
            &AtomicBool::new(false),
            &|event| {
                if event.message == "登记整合包实例" {
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    fs::write(&target, b"concurrent user version").unwrap();
                }
            },
            |_| parent(&root),
        )
        .unwrap_err();
        assert!(error.to_string().contains("文件已保留"));
        assert!(error.to_string().contains("版本尚未登记"));
        assert_eq!(fs::read(target).unwrap(), b"concurrent user version");
        assert_eq!(
            fs::read(root.join("instances/new-pack/config/settings.txt")).unwrap(),
            b"pack setting"
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_parent_symlink_escape_is_rejected_before_installation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("game");
        fs::create_dir(&root).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("instances")).unwrap();
        let pack = temp.path().join("fixture.mrpack");
        fixture_pack(&pack, json!({"minecraft":"1.21.1"}));
        assert!(install_with(
            &root,
            &pack,
            "new-pack",
            false,
            &Platform::current(),
            &AtomicBool::new(false),
            &|_| {},
            |_| panic!("must not run")
        )
        .is_err());
        assert!(!outside.path().join("new-pack").exists());
    }

    #[test]
    fn cancellation_after_import_keeps_files_but_does_not_register_a_version() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture.mrpack");
        let root = temp.path().join("game");
        fixture_pack(&pack, json!({"minecraft":"1.21.1"}));
        let cancel = AtomicBool::new(false);
        let error = install_with(
            &root,
            &pack,
            "new-pack",
            false,
            &Platform::current(),
            &cancel,
            &|event| {
                if event.message == "整合包文件已导入，准备登记实例" {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            |_| parent(&root),
        )
        .unwrap_err();
        assert!(error.to_string().contains("取消"));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<crate::model::OperationCancelled>()
                .is_some()
        }));
        assert!(error.to_string().contains("版本尚未登记"));
        assert!(root
            .join("instances/new-pack/config/settings.txt")
            .is_file());
        assert!(!root.join("versions/new-pack/new-pack.json").exists());
    }
    #[test]
    fn existing_forge_can_supply_named_instances_without_rewriting_user_profile() {
        use sha1::{Digest, Sha1};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let put = |path: &str, bytes: &[u8]| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        };
        let file = |bytes: &[u8], url: &str| json!({"url":url,"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()});
        let index = br#"{"objects":{}}"#;
        let mut asset = file(index, "https://piston-meta.mojang.com/index.json");
        asset["id"] = json!("fixture");
        put("assets/indexes/fixture.json", index);
        put("versions/1.21.1/1.21.1.jar", b"client");
        put("versions/1.21.1/1.21.1.json",&serde_json::to_vec(&json!({"id":"1.21.1","libraries":[],"downloads":{"client":file(b"client","https://piston-data.mojang.com/client.jar")},"assetIndex":asset})).unwrap());
        let id = "1.21.1-forge-52.0.16";
        let coordinate = "net.minecraftforge:forge:1.21.1-52.0.16";
        let libpath = "net/minecraftforge/forge/1.21.1-52.0.16/forge-1.21.1-52.0.16.jar";
        put(&format!("libraries/{libpath}"), b"forge fixture");
        let mut download = file(
            b"forge fixture",
            &format!("https://maven.minecraftforge.net/{libpath}"),
        );
        download["path"] = json!(libpath);
        let profile = json!({"id":id,"inheritsFrom":"1.21.1","arguments":{"jvm":["-Duser.custom=true"]},"libraries":[{"name":coordinate,"downloads":{"artifact":download}}],"_pcl_forge_install":{"kind":"forge","minecraft":"1.21.1","loader":"52.0.16","files":[]}});
        let bytes = serde_json::to_vec(&profile).unwrap();
        let profile_path = format!("versions/{id}/{id}.json");
        put(&profile_path, &bytes);
        put(&format!("versions/{id}/natives/user-file"), b"keep");
        assert_eq!(
            ensure_forge_version(
                root,
                crate::forge::ForgeKind::Forge,
                "1.21.1",
                "52.0.16",
                None,
                &Platform::current(),
                &AtomicBool::new(false),
                &|_| {}
            )
            .unwrap(),
            id
        );
        assert_eq!(fs::read(root.join(&profile_path)).unwrap(), bytes);
        assert_eq!(
            fs::read(root.join(format!("versions/{id}/natives/user-file"))).unwrap(),
            b"keep"
        );
        let mut changed = profile;
        changed["_pcl_forge_install"]["loader"] = json!("other");
        let changed = serde_json::to_vec(&changed).unwrap();
        put(&profile_path, &changed);
        assert!(ensure_forge_version(
            root,
            crate::forge::ForgeKind::Forge,
            "1.21.1",
            "52.0.16",
            None,
            &Platform::current(),
            &AtomicBool::new(false),
            &|_| {}
        )
        .is_err());
        assert_eq!(fs::read(root.join(profile_path)).unwrap(), changed);
    }
}
