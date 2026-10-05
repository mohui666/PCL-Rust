//! Manifest-only adapters. Archived launcher settings and executable commands
//! are never interpreted. All payloads still pass the shared ZIP preflight.
use super::*;
use serde_json::{json, Value};

pub(super) struct Manifest {
    pub index: Value,
    pub index_path: String,
    pub override_prefixes: Vec<String>,
    pub format: &'static str,
    pub warnings: Vec<String>,
    pub curseforge: Vec<CurseForgeFile>,
    pub embedded_hashes: BTreeMap<String, String>,
}
#[derive(Clone)]
pub(super) struct CurseForgeFile {
    pub project: u64,
    pub file: u64,
    pub required: bool,
}
fn text(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut entry = archive.by_name(name)?;
    anyhow::ensure!(entry.size() <= MAX_INDEX, "整合包元数据超过 8 MiB");
    let mut bytes = Vec::new();
    entry.by_ref().take(MAX_INDEX + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_INDEX, "整合包元数据超过 8 MiB");
    String::from_utf8(bytes).context("整合包元数据必须为 UTF-8")
}
fn insert(dependencies: &mut BTreeMap<String, String>, name: &str, value: &str) -> Result<()> {
    validate_id(value)?;
    anyhow::ensure!(
        dependencies.insert(name.into(), value.into()).is_none(),
        "整合包重复声明组件：{name}"
    );
    Ok(())
}
pub(super) fn read(archive: &mut ZipArchive<File>) -> Result<Manifest> {
    let mut candidates = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name();
        safe_relative(name.trim_end_matches('/'))?;
        if !entry.is_dir()
            && name.split('/').count() <= 2
            && matches!(
                name.rsplit('/').next(),
                Some(
                    "modrinth.index.json"
                        | "mmc-pack.json"
                        | "modpack.json"
                        | "mcbbs.packmeta"
                        | "manifest.json"
                )
            )
        {
            candidates.push(name.to_owned());
        }
    }
    // A root manifest owns the archive. A payload named overrides/manifest.json
    // is ordinary game data, not a second instance manifest.
    if candidates.iter().any(|path| !path.contains('/')) {
        candidates.retain(|path| !path.contains('/'));
    }
    // MCBBS and MMC explicitly win over their compatibility manifest.json.
    let priority = |name: &str| match name.rsplit('/').next().unwrap_or("") {
        "mcbbs.packmeta" => 0,
        "mmc-pack.json" => 1,
        "modrinth.index.json" => 2,
        "modpack.json" => 3,
        _ => 4,
    };
    candidates.sort_by_key(|path| priority(path));
    if candidates.is_empty() {
        return read_game_zip(archive);
    }
    let index_path = candidates[0].clone();
    let prefix = index_path
        .rsplit_once('/')
        .map_or("".into(), |(prefix, _)| format!("{prefix}/"));
    anyhow::ensure!(
        candidates.iter().all(|path| path.starts_with(&prefix)
            && path.rsplit_once('/').map_or("", |(parent, _)| parent)
                == prefix.trim_end_matches('/')),
        "压缩包包含多个实例或嵌套清单，请分别导入"
    );
    let value: Value = serde_json::from_str(&text(archive, &index_path)?)?;
    let filename = index_path.rsplit('/').next().unwrap();
    if filename == "modrinth.index.json" {
        return Ok(Manifest {
            index: value,
            index_path,
            override_prefixes: vec![
                format!("{prefix}overrides/"),
                format!("{prefix}client-overrides/"),
            ],
            format: "mrpack",
            warnings: Vec::new(),
            curseforge: Vec::new(),
            embedded_hashes: BTreeMap::new(),
        });
    }
    let mut dependencies = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut curseforge = Vec::new();
    let mut external_files = Vec::new();
    let mut embedded_hashes = BTreeMap::new();
    let mut name = value["name"].as_str().unwrap_or("导入的整合包").to_owned();
    let version = value["version"]
        .as_str()
        .or_else(|| value["versionId"].as_str())
        .unwrap_or("1.0.0");
    let (format, folder) = match filename {
        "mmc-pack.json" => {
            anyhow::ensure!(
                value["formatVersion"].as_u64() == Some(1),
                "只支持 MMC formatVersion=1"
            );
            let cfg_path = format!("{prefix}instance.cfg");
            let has_cfg = archive.file_names().any(|name| name == cfg_path);
            let cfg = if has_cfg {
                Some(text(archive, &cfg_path)?)
            } else {
                None
            };
            if let Some(cfg) = cfg {
                for line in cfg.lines() {
                    if let Some(value) = line.trim().strip_prefix("name=") {
                        if !value.trim().is_empty() {
                            name = value.trim().into();
                        }
                    }
                }
                warnings.push("仅导入游戏组件和文件；MMC 的账号、Java路径、启动命令及启动器设置不执行、不导入。".into());
            }
            for component in value["components"]
                .as_array()
                .context("MMC 缺少 components")?
            {
                let uid = component["uid"].as_str().context("MMC 组件缺少 uid")?;
                let key = match uid {
                    "net.minecraft" => "minecraft",
                    "net.minecraftforge" => "forge",
                    "net.neoforged" => "neoforge",
                    "net.fabricmc.fabric-loader" => "fabric-loader",
                    "org.quiltmc.quilt-loader" => "quilt-loader",
                    "com.mumfrey.liteloader" => "liteloader",
                    "optifine.OptiFine" => "optifine",
                    value if value.starts_with("org.lwjgl") => continue,
                    _ => bail!("尚不支持 MMC 组件 {uid}，未开始安装"),
                };
                insert(
                    &mut dependencies,
                    key,
                    component["version"].as_str().context("MMC 组件缺少版本")?,
                )?;
            }
            ("MultiMC / Prism", ".minecraft/")
        }
        "modpack.json" => {
            insert(
                &mut dependencies,
                "minecraft",
                value["gameVersion"]
                    .as_str()
                    .context("HMCL 缺少 gameVersion")?,
            )?;
            let pack_path = format!("{prefix}minecraft/pack.json");
            if archive.file_names().any(|name| name == pack_path) {
                let profile: Value = serde_json::from_str(&text(archive, &pack_path)?)?;
                let mc = dependencies["minecraft"].clone();
                for (key, version) in super::profile::components(&profile, &mc)? {
                    insert(&mut dependencies, &key, &version)?;
                }
                warnings.push("依据 pack.json 识别游戏组件并从发行方重建；包内启动命令、凭据和自定义下载地址不执行。".into());
            }
            ("HMCL", "minecraft/")
        }
        "mcbbs.packmeta" | "manifest.json" if value.get("addons").is_some() => {
            for addon in value["addons"]
                .as_array()
                .context("MCBBS addons 必须为数组")?
            {
                let name = addon["id"].as_str().context("MCBBS 组件缺少 id")?;
                let key = match name {
                    "game" => "minecraft",
                    "forge" => "forge",
                    "neoforge" => "neoforge",
                    "fabric" => "fabric-loader",
                    "quilt" => "quilt-loader",
                    "optifine" => "optifine",
                    "liteloader" => "liteloader",
                    _ => bail!("尚不支持 MCBBS 组件 {name}，未开始安装"),
                };
                insert(
                    &mut dependencies,
                    key,
                    addon["version"].as_str().context("MCBBS 组件缺少版本")?,
                )?;
            }
            if value.get("launchInfo").is_some() {
                warnings
                    .push("MCBBS 附带的启动参数不自动导入；请检查后在版本设置中手动配置。".into());
            }
            let mut seen = HashSet::new();
            for file in value["files"].as_array().into_iter().flatten() {
                match file["type"].as_str() {
                    Some("addon") => {
                        let path = file["path"].as_str().context("MCBBS 内嵌文件缺少 path")?;
                        safe_relative(path)?;
                        let archived = format!("{prefix}overrides/{path}");
                        let hash = file["hash"].as_str().context("MCBBS 文件缺少 SHA1")?;
                        anyhow::ensure!(
                            hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                            "MCBBS 文件 SHA1 无效"
                        );
                        if !archive.file_names().any(|name| name == archived) {
                            let api = value["fileApi"]
                                .as_str()
                                .context("MCBBS 外部文件缺少 fileApi")?;
                            let base =
                                Url::parse(&format!("{}/overrides/", api.trim_end_matches('/')))?;
                            let url = base.join(path)?;
                            validate_download_url(url.as_str())?;
                            external_files.push(json!({"path":path,"hashes":{"sha1":hash},"downloads":[url.as_str()]}));
                            continue;
                        }
                        anyhow::ensure!(
                            archive.by_name(&archived)?.size() <= MAX_FILE,
                            "MCBBS 内嵌文件过大"
                        );
                        anyhow::ensure!(
                            embedded_hashes
                                .insert(path.to_owned(), hash.to_ascii_lowercase())
                                .is_none(),
                            "MCBBS 重复声明内嵌文件"
                        );
                    }
                    Some("curse") => {
                        let project = file["projectID"]
                            .as_u64()
                            .context("MCBBS CurseForge 项目 ID 无效")?;
                        let id = file["fileID"]
                            .as_u64()
                            .context("MCBBS CurseForge 文件 ID 无效")?;
                        anyhow::ensure!(
                            (1..=u32::MAX as u64).contains(&project)
                                && (1..=u32::MAX as u64).contains(&id)
                                && seen.insert(project),
                            "MCBBS CurseForge ID 越界或重复"
                        );
                        curseforge.push(CurseForgeFile {
                            project,
                            file: id,
                            required: true,
                        });
                    }
                    _ => bail!("MCBBS 文件类型未知"),
                }
            }
            if !curseforge.is_empty() {
                warnings.push("MCBBS 外部 Mod 将通过 CurseForge 官方 API 解析并校验；包内直链不代替 API 下载许可。".into());
            }
            ("MCBBS", "overrides/")
        }
        "manifest.json" => {
            anyhow::ensure!(
                value["manifestType"] == "minecraftModpack" && value["manifestVersion"] == 1,
                "只支持 CurseForge minecraftModpack manifestVersion=1"
            );
            insert(
                &mut dependencies,
                "minecraft",
                value["minecraft"]["version"]
                    .as_str()
                    .context("CurseForge 缺少 Minecraft 版本")?,
            )?;
            let loaders = value["minecraft"]["modLoaders"]
                .as_array()
                .context("CurseForge 缺少 modLoaders")?;
            let selected = loaders
                .iter()
                .filter(|loader| loader["primary"].as_bool() == Some(true) || loaders.len() == 1)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                selected.len() <= 1 && (loaders.is_empty() || selected.len() == 1),
                "CurseForge 加载器主选项不明确"
            );
            for loader in selected {
                let (name, version) = loader["id"]
                    .as_str()
                    .context("CurseForge 加载器缺少 id")?
                    .split_once('-')
                    .context("CurseForge 加载器标识无效")?;
                let key = match name {
                    "forge" => "forge",
                    "neoforge" => "neoforge",
                    "fabric" => "fabric-loader",
                    "quilt" => "quilt-loader",
                    "optifine" => "optifine",
                    "liteloader" => "liteloader",
                    _ => bail!("尚不支持 CurseForge 加载器 {name}，未开始下载"),
                };
                insert(&mut dependencies, key, version)?;
            }
            let mut projects = HashSet::new();
            for file in value["files"].as_array().context("CurseForge 缺少 files")? {
                let project = file["projectID"]
                    .as_u64()
                    .context("CurseForge projectID 无效")?;
                let id = file["fileID"].as_u64().context("CurseForge fileID 无效")?;
                anyhow::ensure!(
                    (1..=u32::MAX as u64).contains(&project) && (1..=u32::MAX as u64).contains(&id),
                    "CurseForge 项目/文件 ID 超出范围"
                );
                anyhow::ensure!(projects.insert(project), "CurseForge 清单重复声明同一项目");
                curseforge.push(CurseForgeFile {
                    project,
                    file: id,
                    required: file
                        .get("required")
                        .map_or(Ok(true), |v| v.as_bool().context("required 必须为布尔值"))?,
                });
            }
            anyhow::ensure!(curseforge.len() <= 20_000, "CurseForge 文件数量过多");
            warnings.push("外部文件将通过 CurseForge 官方 API 解析，需要已配置 API Key；不允许 API 下载的项目会明确中止。".into());
            let folder = value["overrides"].as_str().unwrap_or("overrides");
            safe_relative(folder)?;
            // Return here because the folder is manifest-owned rather than static.
            return Ok(Manifest {
                index: json!({"formatVersion":1,"game":"minecraft","name":name,"versionId":version,"files":[],"dependencies":dependencies}),
                index_path,
                override_prefixes: vec![format!("{prefix}{}/", folder.trim_end_matches('/'))],
                format: "CurseForge",
                warnings,
                curseforge,
                embedded_hashes,
            });
        }
        _ => bail!("整合包格式无法识别"),
    };
    anyhow::ensure!(
        dependencies.contains_key("minecraft"),
        "整合包未声明 Minecraft 版本"
    );
    Ok(Manifest {
        index: json!({"formatVersion":1,"game":"minecraft","name":name,"versionId":version,"summary":value["description"],"files":external_files,"dependencies":dependencies}),
        index_path,
        override_prefixes: vec![format!("{prefix}{folder}")],
        format,
        warnings,
        curseforge,
        embedded_hashes,
    })
}

/// Only portable game content is taken from an unstructured game ZIP. A launcher,
/// account database, scripts, libraries and arbitrary version JSON never become
/// active launcher configuration.
pub(super) fn game_payload(path: &str) -> bool {
    matches!(
        path.split('/').next().unwrap_or(""),
        "mods"
            | "config"
            | "defaultconfigs"
            | "resourcepacks"
            | "shaderpacks"
            | "saves"
            | "screenshots"
            | "kubejs"
            | "scripts"
    ) || matches!(
        path,
        "options.txt"
            | "optionsof.txt"
            | "optionsshaders.txt"
            | "servers.dat"
            | "servers.dat_old"
            | "icon.png"
    )
}
fn read_game_zip(archive: &mut ZipArchive<File>) -> Result<Manifest> {
    let mut versions = BTreeMap::<String, (String, String, Value)>::new();
    let names = archive.file_names().map(str::to_owned).collect::<Vec<_>>();
    for path in names {
        let parts = path.split('/').collect::<Vec<_>>();
        if parts.len() < 3 || parts[parts.len() - 3] != "versions" {
            continue;
        }
        let id = parts[parts.len() - 2];
        if parts[parts.len() - 1] != format!("{id}.json") {
            continue;
        }
        validate_id(id)?;
        let value: Value = serde_json::from_str(&text(archive, &path)?)?;
        anyhow::ensure!(
            value["id"].as_str() == Some(id),
            "通用 ZIP 版本 ID 与文件夹不符"
        );
        let prefix = parts[..parts.len() - 3].join("/");
        let prefix = if prefix.is_empty() {
            prefix
        } else {
            format!("{prefix}/")
        };
        anyhow::ensure!(
            versions.insert(id.into(), (path, prefix, value)).is_none(),
            "通用 ZIP 包含重名实例"
        );
    }
    anyhow::ensure!(
        !versions.is_empty(),
        "压缩包缺少受支持的整合包清单或 versions/<版本>/<版本>.json"
    );
    let parents = versions
        .values()
        .filter_map(|(_, _, v)| v["inheritsFrom"].as_str())
        .collect::<HashSet<_>>();
    let leaves = versions
        .keys()
        .filter(|id| !parents.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    anyhow::ensure!(
        leaves.len() == 1,
        "通用 ZIP 包含多个独立版本，请分别导出后导入"
    );
    let leaf = &leaves[0];
    let (index_path, prefix, _) = &versions[leaf];
    let mut current = leaf.clone();
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let minecraft = loop {
        anyhow::ensure!(
            seen.insert(current.clone()) && seen.len() <= 65,
            "通用 ZIP 继承循环或过深"
        );
        let Some((_, _, profile)) = versions.get(&current) else {
            break current;
        };
        chain.push(profile);
        if let Some(parent) = profile["inheritsFrom"].as_str() {
            validate_id(parent)?;
            current = parent.into();
            continue;
        }
        break profile["jar"].as_str().unwrap_or(&current).to_owned();
    };
    validate_id(&minecraft)?;
    let mut dependencies = BTreeMap::from([("minecraft".into(), minecraft.clone())]);
    for profile in chain {
        for (key, value) in super::profile::components(profile, &minecraft)? {
            if let Some(old) = dependencies.insert(key.clone(), value.clone()) {
                anyhow::ensure!(old == value, "通用 ZIP 的 {key} 版本冲突");
            }
        }
    }
    Ok(Manifest {
        index: json!({"formatVersion":1,"game":"minecraft","name":leaf,"versionId":"1.0.0","files":[],"dependencies":dependencies}),
        index_path: index_path.clone(),
        override_prefixes: vec![prefix.clone(), format!("{prefix}versions/{leaf}/")],
        format: "通用游戏 ZIP",
        warnings: vec![
            "仅导入唯一实例的游戏文件并重建官方组件；不运行附带启动器、脚本或账号配置。".into(),
        ],
        curseforge: Vec::new(),
        embedded_hashes: BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(path: &Path, files: &[(&str, &[u8])]) {
        let mut archive = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in files {
            archive
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
    }
    #[test]
    fn wrapped_mmc_preserves_payload_but_never_imports_launcher_commands() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("mmc.zip");
        fixture(&pack,&[("Pack/mmc-pack.json",br#"{"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.21.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.16.5"}]}"#),
            ("Pack/instance.cfg",b"name=My Pack\nOverrideCommands=true\nPreLaunchCommand=must-not-run\n"),("Pack/.minecraft/config/a.txt",b"config")]);
        let info = inspect_mrpack(&pack).unwrap();
        assert_eq!(info.name, "My Pack");
        assert_eq!(info.dependencies["fabric-loader"], "0.16.5");
        assert_eq!(info.warnings.len(), 1);
        let destination = temp.path().join("instance");
        import_mrpack(&pack, &destination, false, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(
            fs::read(destination.join("config/a.txt")).unwrap(),
            b"config"
        );
        assert!(!destination.join("instance.cfg").exists());
    }
    #[test]
    fn hmcl_and_mcbbs_embedded_files_use_shared_no_clobber_import() {
        for (name,json,payload) in [("modpack.json",br#"{"name":"HMCL","version":"1","gameVersion":"1.21.1"}"#.as_slice(),"minecraft/config/a.txt"),
            ("mcbbs.packmeta",br#"{"name":"MCBBS","version":"1","addons":[{"id":"game","version":"1.21.1"},{"id":"quilt","version":"0.29.0"}]}"#.as_slice(),"overrides/config/a.txt")] {
            let temp=tempfile::tempdir().unwrap();let pack=temp.path().join("pack.zip");fixture(&pack,&[(name,json),(payload,b"selected")]);
            let destination=temp.path().join("instance");import_mrpack(&pack,&destination,false,&AtomicBool::new(false),|_|{}).unwrap();
            assert_eq!(fs::read(destination.join("config/a.txt")).unwrap(),b"selected");
            assert!(import_mrpack(&pack,&destination,false,&AtomicBool::new(false),|_|{}).is_err());
            assert_eq!(fs::read(destination.join("config/a.txt")).unwrap(),b"selected");
        }
    }
    #[test]
    fn ambiguous_nested_or_unknown_components_fail_before_extraction() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("bad.zip");
        for files in [
            vec![
                ("a/modpack.json", br#"{"gameVersion":"1.21.1"}"#.as_slice()),
                ("b/modpack.json", br#"{"gameVersion":"1.20.1"}"#.as_slice()),
            ],
            vec![(
                "mmc-pack.json",
                br#"{"formatVersion":1,"components":[{"uid":"unknown.launcher","version":"1"}]}"#
                    .as_slice(),
            )],
            vec![
                ("modpack.json", br#"{"gameVersion":"1.21.1"}"#.as_slice()),
                ("minecraft/../outside", b"bad".as_slice()),
            ],
        ] {
            fixture(&pack, &files);
            assert!(inspect_mrpack(&pack).is_err());
        }
    }
    #[test]
    fn hmcl_components_rebuilt_without_importing_launch_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("hmcl.zip");
        fixture(&pack,&[("modpack.json",br#"{"name":"HMCL","version":"1","gameVersion":"1.20.1"}"#),
            ("minecraft/pack.json",br#"{"id":"old","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.5"}],"javaArgs":"-javaagent:bad.jar"}"#),
            ("minecraft/config/a.txt",b"selected")]);
        let info = inspect_mrpack(&pack).unwrap();
        assert_eq!(info.dependencies["fabric-loader"], "0.16.5");
        let dest = temp.path().join("target");
        import_mrpack(&pack, &dest, false, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(dest.join("config/a.txt").exists());
        assert!(!dest.join("pack.json").exists());
    }
    #[test]
    fn universal_zip_rebuilds_single_leaf_and_skips_account_and_launcher_files() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("game.zip");
        fixture(&pack,&[("Game/.minecraft/versions/Fabric/Fabric.json",br#"{"id":"Fabric","inheritsFrom":"1.21.1","libraries":[{"name":"net.fabricmc:fabric-loader:0.16.5"}]}"#),
            ("Game/.minecraft/config/a.txt",b"shared"),("Game/.minecraft/versions/Fabric/config/a.txt",b"isolated"),
            ("Game/.minecraft/launcher_accounts.json",b"secret"),("Game/launcher.exe",b"program")]);
        let info = inspect_mrpack(&pack).unwrap();
        assert_eq!(info.minecraft, "1.21.1");
        assert_eq!(info.dependencies["fabric-loader"], "0.16.5");
        let dest = temp.path().join("target");
        import_mrpack(&pack, &dest, false, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(fs::read(dest.join("config/a.txt")).unwrap(), b"isolated");
        assert!(!dest.join("launcher_accounts.json").exists());
        assert!(!dest.join("versions").exists());
    }
    #[test]
    fn mcbbs_external_curse_ids_and_embedded_integrity_are_checked() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("mcbbs.zip");
        let body=serde_json::to_vec(&json!({"name":"test","version":"1","addons":[{"id":"game","version":"1.21.1"}],"files":[{"type":"curse","projectID":306612,"fileID":123},{"type":"addon","path":"config/a.txt","hash":format!("{:x}",Sha1::digest(b"keep"))}]})).unwrap();
        fixture(
            &pack,
            &[
                ("mcbbs.packmeta", &body),
                ("overrides/config/a.txt", b"keep"),
            ],
        );
        let info = inspect_mrpack(&pack).unwrap();
        assert_eq!(info.files, 2);
        fixture(
            &pack,
            &[
                ("mcbbs.packmeta", &body),
                ("overrides/config/a.txt", b"bad"),
            ],
        );
        assert!(inspect_mrpack(&pack).is_ok());
        let body=serde_json::to_vec(&json!({"name":"test","version":"1","addons":[{"id":"game","version":"1.21.1"}],"files":[{"type":"addon","path":"config/a.txt","hash":format!("{:x}",Sha1::digest(b"keep"))}]})).unwrap();
        fixture(
            &pack,
            &[
                ("mcbbs.packmeta", &body),
                ("overrides/config/a.txt", b"bad"),
            ],
        );
        let target = temp.path().join("failed");
        assert!(import_mrpack(&pack, &target, false, &AtomicBool::new(false), |_| {}).is_err());
        assert!(!target.exists());
    }
    #[test]
    fn a_payload_manifest_is_not_a_second_pack() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("payload.mrpack");
        fixture(&pack,&[("modrinth.index.json",br#"{"formatVersion":1,"game":"minecraft","name":"fixture","versionId":"1","dependencies":{"minecraft":"1.21.1"},"files":[]}"#),("overrides/manifest.json",b"game data")]);
        let target = dir.path().join("instance");
        super::super::import_mrpack(&pack, &target, false, &AtomicBool::new(false), |_| {})
            .unwrap();
        assert_eq!(
            fs::read(target.join("manifest.json")).unwrap(),
            b"game data"
        );
    }
}
