use super::*;
use anyhow::{bail, ensure};
use pcl_core::{
    accounts, auth, crash, forge, install, instances, java, java_download, loaders, mod_updates,
    modpack, mods, pack_export, packs, resources,
};

pub(super) fn execute(cx: &RuntimeContext, command: Action) -> Result<Value> {
    cx.check_cancel()?;
    let progress = |p| cx.progress(p);
    let cancel = &*cx.cancel;
    match command {
        Action::Doctor => Ok(json!({"platform":cx.platform,"root":cx.root,
            "java":java::discover_java_with_cancel(cancel)?.runtimes.iter().map(java_value).collect::<Vec<_>>(),
            "versions":metadata::list_installed(&cx.root)?})),
        Action::List => value(metadata::list_installed(&cx.root)?),
        Action::Manifest => value(install::fetch_manifest()?),
        Action::Install { version } => {
            install::install_version(&cx.root, &version, &cx.platform, cancel, progress)?;
            config::initialize_instance_settings(&cx.root, &version, cx.settings.default_isolation)?;
            Ok(json!({"installed":version,"root":cx.root}))
        }
        Action::Repair { version, java } => {
            install::repair_version_with_java(&cx.root, &version, java.as_deref(), &cx.platform, cancel, progress)?;
            Ok(json!({"repaired":version}))
        }
        Action::Loaders { kind, minecraft } => match kind {
            Loader::Fabric | Loader::Quilt => value(loaders::list_loader_versions(meta_loader(kind)?, &minecraft, cancel)?),
            Loader::Forge | Loader::Neoforge => value(forge::list_versions(forge_loader(kind)?, &minecraft, cancel)?),
            Loader::Liteloader => value(loaders::liteloader::list_versions(&minecraft, cancel)?),
            Loader::Optifine => value(loaders::optifine::list_versions(&minecraft, cancel)?),
        },
        Action::InstallLoader { kind, minecraft, loader, java, parent } => {
            install_loader(cx, kind, &minecraft, &loader, java.as_deref(), parent.as_deref())
        }
        Action::Mods { version } => local_mods(cx, &version),
        Action::Mod { command } => mod_command(cx, command),
        Action::Search { query, kind, provider, minecraft, loader, offset, limit } => {
            value(resources::search_resources(kind.into(), &resources::SearchOptions {
                query, minecraft, loader, offset, limit, sort: cx.settings.resource_sort,
                provider: match provider { Provider::Modrinth => resources::ResourceProvider::Modrinth,
                    Provider::Curseforge => resources::ResourceProvider::CurseForge },
            }, "", cancel)?)
        }
        Action::Resource { command } => resource_command(cx, command),
        Action::Java { command } => match command {
            JavaAction::List => value(java::discover_java_with_cancel(cancel)?.runtimes.iter().map(java_value).collect::<Vec<_>>()),
            JavaAction::Inspect { path } => Ok(java_value(&java::inspect_java_with_cancel(&path, cancel)?)),
            JavaAction::Runtimes => value(java_download::list_runtimes(&cx.platform, cancel)?.iter().map(|j|
                json!({"platform":j.platform,"component":j.component,"version":j.version,"major":j.major})).collect::<Vec<_>>()),
            JavaAction::Install { component, directory } => {
                let directory = directory.unwrap_or_else(java_download::runtime_root);
                ensure!(directory.is_absolute(), "Java --directory 必须为绝对路径");
                let target = java_download::list_runtimes(&cx.platform, cancel)?.into_iter()
                    .find(|j| j.component == component).context("官方清单没有该 component，请先运行 java runtimes")?;
                Ok(java_value(&java_download::download_runtime(&directory, &target, cancel, progress)?))
            }
        },
        Action::InspectPack { pack } => value(modpack::inspect_mrpack(&pack)?),
        Action::InstallPack { pack, instance, optional, java } => {
            let id = packs::install_pack_with_java(&cx.root, &pack, &instance, optional, java.as_deref(), &cx.platform, cancel, progress)?;
            Ok(json!({"installed":id}))
        }
        Action::ExportPack { version, output, options, format, launcher } => {
            let mut options = options.as_deref().map(read_json::<pack_export::PackExportOptions>).transpose()?
                .unwrap_or_else(|| pack_export::PackExportOptions {name:version.clone(),format:format.into(),..Default::default()});
            if launcher.is_some() { options.include_launcher = true; }
            value(pack_export::export_pack_with_launcher(&cx.root, &version, &output, &options, launcher.as_deref(), cancel, progress)?)
        }
        Action::Plan(args) => {
            let (plan, _) = game::plan(cx, args, false)?;
            Ok(json!({"command":plan.redacted_command(),"cwd":plan.cwd,"warnings":plan.behavior.warnings}))
        }
        Action::Launch(args) => {
            let (plan, session) = game::plan(cx, args, true)?;
            game::run(cx, plan, session)
        }
        Action::ExportScript { launch, output } => {
            let format = match output.extension().and_then(|ext|ext.to_str()) {
                Some(ext) if ext.eq_ignore_ascii_case("bat") => pcl_core::launch_script::ScriptFormat::WindowsBatch,
                Some(ext) if ext.eq_ignore_ascii_case("command") => pcl_core::launch_script::ScriptFormat::MacCommand,
                _ => bail!("启动脚本扩展名必须为 .command 或 .bat"),
            };
            let (plan, _) = game::plan(cx, launch, false)?;
            let report = pcl_core::launch_script::export_launch_script_with_cancel(&output, &plan, format, cancel)?;
            Ok(json!({"path":report.path,"bytes":report.bytes,"runnable_without_credentials":report.runnable_without_credentials}))
        }
        Action::Accounts => value(accounts::load_accounts()?),
        Action::Login { client_id } => {
            let client_id = client_id.unwrap_or_else(|| cx.settings.microsoft_client_id.clone());
            let code = auth::begin_device_login(&client_id)?;
            if cx.output.json {
                cx.output.value(json!({"type":"device_code","verification_uri":code.verification_uri,
                    "user_code":code.user_code,"expires_in":code.expires_in}));
            } else {
                cx.output.event("device_code", &format!("在其他浏览器打开 {}，输入 {}（{} 秒内有效）",code.verification_uri,code.user_code,code.expires_in));
            }
            let account = accounts::complete_device_login_and_save(&client_id, &code, cancel, |s| cx.output.event("login", &s))?;
            value(account.account)
        }
        Action::Logout { account } => {
            accounts::remove_account(&account)?;
            Ok(json!({"removed_account":account}))
        }
        Action::Instance { command } => match command {
            InstanceAction::Show { version } => Ok(json!({"version":metadata::resolve_version(&cx.root, &version)?,
                "game_dir":cx.instance(&version)?,"settings":redacted(value(config::load_instance_settings(&cx.root, &version)?)?)})),
            InstanceAction::Settings { version, apply } => {
                cx.instance(&version)?;
                if let Some(path) = apply { config::save_instance_settings(&cx.root, &version, &read_json(&path)?)?; }
                Ok(redacted(value(config::load_instance_settings(&cx.root, &version)?)?))
            }
            InstanceAction::Rename { version, new_name } => {
                instances::rename_version_with_cancel(&cx.root, &version, &new_name, cancel)?;
                Ok(json!({"renamed":version,"version":new_name}))
            }
            InstanceAction::Trash { version, apply } => {
                let preview = pcl_core::deletion::preview_version_delete_with_cancel(&cx.root, &version, cancel)?;
                if !apply {
                    return Ok(json!({"version":preview.version_id,"version_directory":preview.version_directory,
                        "instance_directory":preview.instance_directory,"preserved_shared_directories":preview.preserved_shared_directories,
                        "file_count":preview.file_count,"total_bytes":preview.total_bytes,
                        "dependent_versions":preview.dependent_versions.iter().map(|d|json!({"version":d.version_id,"field":d.field,"reference":d.reference})).collect::<Vec<_>>()}));
                }
                let report = pcl_core::deletion::trash_version(&cx.root, &version, &preview, cancel).map_err(|e| {
                    if e.cancelled { anyhow::Error::new(OperationCancelled).context(e.to_string()) }
                    else { anyhow::anyhow!(e.to_string()) }
                })?;
                Ok(json!({"trashed":report.trashed,"remaining":report.remaining,"uncertain":report.uncertain,"trash_locations":report.trash_locations}))
            }
        },
        Action::Settings { apply } => {
            if let Some(path) = apply {
                let settings: config::Settings = read_json(&path)?;
                config::save_settings(&cx.config_path, &settings)?;
                return Ok(json!({"saved":cx.config_path}));
            }
            Ok(redacted(value(&cx.settings)?))
        }
        Action::Logs { instance, file, export } => {
            let report = if let Some(file) = file { crash::import(&file, &[])? }
                else { crash::collect(&cx.instance(instance.as_deref().context("缺少 --instance")?)?, None, &[], &[])? };
            if let Some(path) = export { return Ok(json!({"exported":crash::export(&report, &path)?,"report":report})); }
            value(report)
        }
    }
}

pub(super) fn java_value(runtime: &java::JavaRuntime) -> Value {
    json!({"path":runtime.path,"major":runtime.major,"architecture":runtime.architecture})
}
fn meta_loader(kind: Loader) -> Result<loaders::LoaderKind> {
    match kind {
        Loader::Fabric => Ok(loaders::LoaderKind::Fabric),
        Loader::Quilt => Ok(loaders::LoaderKind::Quilt),
        _ => bail!("需要 Fabric 或 Quilt"),
    }
}
fn forge_loader(kind: Loader) -> Result<forge::ForgeKind> {
    match kind {
        Loader::Forge => Ok(forge::ForgeKind::Forge),
        Loader::Neoforge => Ok(forge::ForgeKind::NeoForge),
        _ => bail!("需要 Forge 或 NeoForge"),
    }
}

fn install_loader(
    cx: &RuntimeContext,
    kind: Loader,
    minecraft: &str,
    loader: &str,
    java: Option<&Path>,
    parent: Option<&str>,
) -> Result<Value> {
    ensure!(
        parent.is_none() || matches!(kind, Loader::Liteloader | Loader::Optifine),
        "--parent 仅适用于 LiteLoader / OptiFine"
    );
    let cancel = &*cx.cancel;
    let progress = |p| cx.progress(p);
    let id = match kind {
        Loader::Fabric | Loader::Quilt => loaders::install_loader(
            &cx.root,
            meta_loader(kind)?,
            minecraft,
            loader,
            &cx.platform,
            cancel,
            progress,
        )?,
        Loader::Forge | Loader::Neoforge => {
            let java = java.context(
                "安装 Forge / NeoForge 需要 --java 绝对路径；可先运行 java list 或 java install",
            )?;
            forge::install_forge(
                &cx.root,
                forge_loader(kind)?,
                minecraft,
                loader,
                java,
                &cx.platform,
                cancel,
                progress,
            )?
        }
        Loader::Liteloader => loaders::liteloader::install_liteloader(
            &cx.root,
            minecraft,
            loader,
            parent,
            &cx.platform,
            cancel,
            progress,
        )?,
        Loader::Optifine => {
            let entry = loaders::optifine::list_versions(minecraft, cancel)?
                .into_iter()
                .find(|entry| entry.version == loader)
                .context("官方列表没有该 OptiFine 版本")?;
            if let Some(parent) = parent {
                let resolved = metadata::resolve_version(&cx.root, parent)?;
                if let Some(forge) = resolved["libraries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|lib| lib["name"].as_str())
                    .find_map(|name| {
                        name.strip_prefix(&format!("net.minecraftforge:forge:{minecraft}-"))
                    })
                {
                    loaders::optifine::install_forge_mod(
                        &cx.root,
                        parent,
                        &entry,
                        forge.split(':').next().unwrap_or(forge),
                        cancel,
                    )?;
                } else {
                    loaders::optifine::install_fabric_mod(&cx.root, parent, &entry, cancel)?;
                }
                parent.into()
            } else {
                loaders::optifine::install_optifine(
                    &cx.root,
                    &entry,
                    java.context("OptiFine 安装需要 --java")?,
                    &cx.platform,
                    cancel,
                    progress,
                )?
            }
        }
    };
    config::initialize_instance_settings(&cx.root, &id, cx.settings.default_isolation)?;
    Ok(json!({"installed":id}))
}

/// Infer filters from installed metadata, never from arbitrary command-line claims.
pub(super) fn target_info(cx: &RuntimeContext, id: &str) -> Result<(String, String)> {
    let resolved = metadata::resolve_version(&cx.root, id)?;
    let minecraft = resolved["_pcl_jar_id"]
        .as_str()
        .context("无法识别实例的 Minecraft 版本")?
        .to_owned();
    let libraries: Vec<_> = resolved["libraries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|lib| lib["name"].as_str())
        .collect();
    let loader = [
        ("net.neoforged:neoforge:", "neoforge"),
        ("net.neoforged.fancymodloader:loader:", "neoforge"),
        ("net.minecraftforge:forge:", "forge"),
        ("org.quiltmc:quilt-loader:", "quilt"),
        ("net.fabricmc:fabric-loader:", "fabric"),
        ("com.mumfrey:liteloader:", "liteloader"),
    ]
    .into_iter()
    .find(|(prefix, _)| libraries.iter().any(|lib| lib.starts_with(prefix)))
    .map_or("", |(_, kind)| kind);
    Ok((minecraft, loader.into()))
}

fn local_mods(cx: &RuntimeContext, id: &str) -> Result<Value> {
    let directory = cx.instance(id)?;
    if !directory.exists() {
        return Ok(json!([]));
    }
    value(mods::list_mods(&directory)?)
}
fn mod_command(cx: &RuntimeContext, command: ModAction) -> Result<Value> {
    let cancel = &*cx.cancel;
    match command {
        ModAction::List { instance } => local_mods(cx, &instance),
        ModAction::Import { instance, files } => {
            Ok(json!({"imported":mods::import_mods(&cx.instance(&instance)?, &files, cancel)?}))
        }
        ModAction::Enable { instance, file } => {
            Ok(json!({"path":mods::set_mod_enabled(&cx.instance(&instance)?, &file, true)?}))
        }
        ModAction::Disable { instance, file } => {
            Ok(json!({"path":mods::set_mod_enabled(&cx.instance(&instance)?, &file, false)?}))
        }
        ModAction::Remove { instance, files } => {
            let removed = mods::remove_mods(&cx.instance(&instance)?, &files, cancel)?;
            Ok(json!({"removed":removed.count,"backup":removed.backup}))
        }
        ModAction::Restore { instance, backup } => Ok(
            json!({"restored":mods::restore_removed_mods(&cx.instance(&instance)?, &backup, cancel)?}),
        ),
        ModAction::Updates {
            instance,
            apply,
            files,
        } => {
            let (minecraft, loader) = target_info(cx, &instance)?;
            ensure!(!loader.is_empty(), "实例未安装 Mod 加载器");
            let plan =
                mod_updates::check_updates(&cx.instance(&instance)?, &minecraft, &loader, cancel)?;
            if apply {
                let selected = if files.is_empty() {
                    plan.updates().iter().map(|u| u.file_name.clone()).collect()
                } else {
                    files.into_iter().collect()
                };
                let report =
                    mod_updates::apply_updates(&plan, &selected, cancel, |p| cx.progress(p))?;
                return Ok(
                    json!({"updated":report.updated,"added_dependencies":report.added_dependencies,"backup":report.backup_directory}),
                );
            }
            Ok(
                json!({"updates":plan.updates().iter().map(|u|json!({"file":u.file_name,"name":u.name,"current":u.current_version,"new":u.new_version,"enabled":u.enabled,"source":u.source})).collect::<Vec<_>>(),
                "unmatched":plan.unmatched(),"issues":plan.issues()}),
            )
        }
    }
}

fn resource_command(cx: &RuntimeContext, command: ResourceAction) -> Result<Value> {
    let cancel = &*cx.cancel;
    let progress = |p| cx.progress(p);
    match command {
        ResourceAction::Project { project } => value(resources::get_project(&project, cancel)?),
        ResourceAction::Versions {
            project,
            kind,
            minecraft,
            loader,
        } => value(resources::list_resource_versions(
            kind.into(),
            &project,
            &minecraft,
            &loader,
            cancel,
        )?),
        ResourceAction::Download {
            version,
            output,
            kind,
        } => {
            let version = resources::get_version(&version, cancel)?;
            resources::save_resource_version(kind.into(), &version, &output, cancel, progress)?;
            Ok(json!({"downloaded":output,"version":version.id}))
        }
        ResourceAction::Install {
            version,
            instance,
            kind,
            world,
            dry_run,
            optional,
            java,
        } => {
            ensure!(
                kind == Kind::DataPack || world.is_none(),
                "--world 仅适用于数据包"
            );
            ensure!(
                kind != Kind::DataPack || world.is_some(),
                "数据包需要 --world 世界目录名"
            );
            ensure!(
                kind == Kind::Modpack || (!optional && java.is_none()),
                "--optional / --java 仅适用于整合包"
            );
            metadata::validate_id(&instance)?;
            if kind == Kind::Modpack {
                ensure!(
                    !dry_run,
                    "整合包请先用 resource download 和 inspect-pack 检查"
                );
                let selected = resources::get_version(&version, cancel)?;
                let pack = resources::download_modpack(
                    &selected.project_id,
                    &selected.id,
                    cancel,
                    progress,
                )?;
                let id = packs::install_pack_with_java(
                    &cx.root,
                    pack.path(),
                    &instance,
                    optional,
                    java.as_deref(),
                    &cx.platform,
                    cancel,
                    progress,
                )?;
                return Ok(json!({"installed":id}));
            }
            let directory = cx.instance(&instance)?;
            let (minecraft, loader) = target_info(cx, &instance)?;
            ensure!(
                kind != Kind::Mod || !loader.is_empty(),
                "目标版本没有 Mod 加载器，请先 install-loader"
            );
            let world = world
                .map(|name| {
                    metadata::validate_id(&name)?;
                    metadata::confined_path(&directory, &PathBuf::from("saves").join(name))
                })
                .transpose()?;
            let plan = if kind == Kind::Mod {
                resources::plan_mod_install(&directory, &version, &minecraft, &loader, cancel)?
            } else {
                let selected = resources::get_version(&version, cancel)?;
                resources::plan_resource_install(
                    &resources::ResourceInstall {
                        kind: kind.into(),
                        instance: &directory,
                        world: world.as_deref(),
                        project_id: &selected.project_id,
                        version_id: &version,
                        minecraft: &minecraft,
                    },
                    cancel,
                )?
            };
            if dry_run {
                return Ok(
                    json!({"instance":instance,"resources":plan.resources().iter().map(|r|
                json!({"project":r.project,"version":r.version,"filename":r.filename,"reused":r.reused})).collect::<Vec<_>>()}),
                );
            }
            let paths = resources::execute_install_plan(&plan, cancel, progress)?;
            Ok(json!({"instance":instance,"installed":paths}))
        }
    }
}
