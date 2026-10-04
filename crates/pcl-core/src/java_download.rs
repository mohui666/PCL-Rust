//! User-requested Mojang Java runtimes. The upstream PCL ModJava index is the
//! authority; each manifest and raw file is verified before any Java is executed.
use crate::{
    install, java, metadata,
    model::{Artifact, Platform, Progress},
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Mutex,
    },
};

const INDEX: &str = "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Download {
    url: String,
    sha1: String,
    size: u64,
}
#[derive(Clone, Debug, Deserialize)]
struct Version {
    name: String,
}
#[derive(Clone, Debug, Deserialize)]
struct Entry {
    manifest: Download,
    version: Version,
}

/// Created only from the official runtime index. Callers cannot inject manifests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeDownload {
    pub platform: String,
    pub component: String,
    pub version: String,
    pub major: u32,
    manifest: Download,
}

pub fn runtime_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("pcl-rust/runtime")
}

fn platform_key(platform: &Platform) -> Result<&'static str> {
    match (platform.os.as_str(), platform.arch.as_str()) {
        ("osx", "aarch64" | "arm64") => Ok("mac-os-arm64"),
        ("osx", "x86_64" | "amd64") => Ok("mac-os"),
        ("windows", "x86_64" | "amd64") => Ok("windows-x64"),
        ("windows", "aarch64" | "arm64") => Ok("windows-arm64"),
        ("windows", "x86" | "i686") => Ok("windows-x86"),
        ("linux", "x86_64" | "amd64") => Ok("linux"),
        ("linux", "x86" | "i686") => Ok("linux-i386"),
        _ => bail!(
            "Mojang 没有此平台与架构的 Java 下载列表：{} / {}",
            platform.os,
            platform.arch
        ),
    }
}

fn validate_download(download: &Download, manifest: bool) -> Result<()> {
    let url = install::validate_url(&download.url)?;
    let host = if manifest {
        "piston-meta.mojang.com"
    } else {
        "piston-data.mojang.com"
    };
    ensure!(
        url.host_str() == Some(host),
        "Java 仅允许 Mojang 官方清单与文件服务"
    );
    install::expected_hash(Some(&download.sha1))?;
    ensure!(
        (download.size > 0 || !manifest)
            && download.size
                <= if manifest {
                    16 * 1024 * 1024
                } else {
                    1024 * 1024 * 1024
                },
        "Java 下载大小超出限制"
    );
    Ok(())
}

fn parse_index(bytes: &[u8], platform: &str) -> Result<Vec<RuntimeDownload>> {
    let index: BTreeMap<String, BTreeMap<String, Vec<Entry>>> =
        serde_json::from_slice(bytes).context("Java 官方清单格式错误")?;
    let entries = index.get(platform).context("官方 Java 清单没有当前平台")?;
    let mut downloads = Vec::new();
    for (component, versions) in entries {
        if component == "minecraft-java-exe" {
            continue;
        }
        metadata::validate_id(component)?;
        // The official list is ordered, newest available entry first.
        let Some(entry) = versions.first() else {
            continue;
        };
        metadata::validate_id(&entry.version.name)?;
        validate_download(&entry.manifest, true)?;
        let major = entry
            .version
            .name
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("")
            .parse::<u32>()
            .context("Java 清单版本号无效")?;
        ensure!(major > 0, "Java 清单版本号无效");
        downloads.push(RuntimeDownload {
            platform: platform.into(),
            component: component.clone(),
            version: entry.version.name.clone(),
            major,
            manifest: entry.manifest.clone(),
        });
    }
    downloads.sort_by(|a, b| {
        b.major
            .cmp(&a.major)
            .then_with(|| a.component.cmp(&b.component))
    });
    ensure!(!downloads.is_empty(), "官方清单没有此平台可下载的完整 Java");
    Ok(downloads)
}

pub fn list_runtimes(platform: &Platform, cancel: &AtomicBool) -> Result<Vec<RuntimeDownload>> {
    let key = platform_key(platform)?;
    let data = install::request_bytes(&install::http_client()?, INDEX, None, None, cancel)?;
    parse_index(&data, key)
}

#[derive(Deserialize)]
struct Manifest {
    files: BTreeMap<String, RuntimeFile>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RuntimeFile {
    File {
        downloads: RawDownload,
        #[serde(default)]
        executable: bool,
    },
    Directory,
    Link {
        target: String,
    },
}
#[derive(Deserialize)]
struct RawDownload {
    raw: Download,
}

struct Plan {
    artifacts: Vec<Artifact>,
    directories: Vec<PathBuf>,
    executables: Vec<PathBuf>,
    links: Vec<(PathBuf, String)>,
    java: PathBuf,
}

fn link_destination(path: &Path, target: &str) -> Result<PathBuf> {
    ensure!(
        !target.is_empty() && !target.starts_with('/') && !target.contains('\\'),
        "Java 链接目标必须是树内相对路径"
    );
    let mut result = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
    for component in target.split('/') {
        match component {
            ".." => ensure!(result.pop(), "Java 链接不能越出运行时目录"),
            "." => (),
            other => {
                metadata::validate_id(other)?;
                result.push(other);
            }
        }
    }
    ensure!(
        !result.as_os_str().is_empty(),
        "Java 链接不能指向运行时根目录"
    );
    Ok(result)
}

fn plan_manifest(bytes: &[u8], platform: &str) -> Result<Plan> {
    let manifest: Manifest = serde_json::from_slice(bytes).context("Java 文件清单格式错误")?;
    ensure!(
        !manifest.files.is_empty() && manifest.files.len() <= 20_000,
        "Java 清单文件数量无效"
    );
    let mut plan = Plan {
        artifacts: vec![],
        directories: vec![],
        executables: vec![],
        links: vec![],
        java: PathBuf::new(),
    };
    let mut names = HashSet::new();
    let mut total = 0u64;
    let java_name = if platform.starts_with("windows-") {
        "bin/java.exe"
    } else if platform.starts_with("mac-os") {
        "jre.bundle/Contents/Home/bin/java"
    } else {
        "bin/java"
    };
    for (name, item) in &manifest.files {
        let relative = metadata::safe_relative(name)?;
        ensure!(
            !name.eq_ignore_ascii_case(".pcl-runtime.json") && names.insert(name.to_lowercase()),
            "Java 清单存在重复或保留路径"
        );
        // Never materialize a file underneath a manifest link/file, regardless
        // of entry ordering or case-insensitive target filesystem behavior.
        let mut parent = relative.parent();
        while let Some(path) = parent.filter(|p| !p.as_os_str().is_empty()) {
            let normalized = path.to_string_lossy().replace('\\', "/");
            if let Some((_, entry)) = manifest
                .files
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(&normalized))
            {
                ensure!(
                    matches!(entry, RuntimeFile::Directory),
                    "Java 清单将文件或链接当作父目录"
                );
            }
            parent = path.parent();
        }
        match item {
            RuntimeFile::Directory => plan.directories.push(relative),
            RuntimeFile::File {
                downloads,
                executable,
            } => {
                validate_download(&downloads.raw, false)?;
                total = total
                    .checked_add(downloads.raw.size)
                    .context("Java 文件大小溢出")?;
                ensure!(total <= 3 * 1024 * 1024 * 1024, "Java 下载总量超过 3 GiB");
                if *executable {
                    plan.executables.push(relative.clone());
                }
                if name == java_name {
                    plan.java = relative.clone();
                }
                plan.artifacts.push(Artifact {
                    relative_path: relative,
                    url: downloads.raw.url.clone(),
                    sha1: Some(downloads.raw.sha1.clone()),
                    size: Some(downloads.raw.size),
                    native: false,
                    excludes: vec![],
                });
            }
            RuntimeFile::Link { target } => {
                link_destination(&relative, target)?;
                plan.links.push((relative, target.clone()));
            }
        }
    }
    ensure!(
        !plan.java.as_os_str().is_empty(),
        "官方 Java 清单没有当前平台的可执行文件"
    );
    if !platform.starts_with("windows-") {
        ensure!(
            plan.executables.contains(&plan.java),
            "Java 主程序未标记可执行权限"
        );
    }
    Ok(plan)
}

fn ordinary_root(root: &Path) -> Result<PathBuf> {
    ensure!(root.is_absolute(), "Java 下载目录必须是绝对路径");
    fs::create_dir_all(root).context("创建 Java 下载目录失败")?;
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Java 下载目录不能是符号链接或文件"
    );
    Ok(root.canonicalize()?)
}

fn finish_tree(root: &Path, plan: &Plan, cancel: &AtomicBool) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in &plan.executables {
            install::cancelled(cancel)?;
            fs::set_permissions(root.join(path), fs::Permissions::from_mode(0o755))?;
        }
    }
    for (path, target) in &plan.links {
        install::cancelled(cancel)?;
        let destination = root.join(link_destination(path, target)?);
        let link = install::safe_target(root, path)?;
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            let _ = destination;
            std::os::unix::fs::symlink(target, &link)?;
        }
        #[cfg(windows)]
        {
            // The official Windows manifests use normal files. A file alias
            // can use a hard link without Windows developer-mode privileges.
            ensure!(destination.is_file(), "Windows Java 清单的目录链接不受支持");
            fs::hard_link(destination, &link)?;
        }
    }
    let canonical_root = root.canonicalize()?;
    for (path, _) in &plan.links {
        let resolved = root
            .join(path)
            .canonicalize()
            .context("Java 清单含失效或循环链接")?;
        ensure!(
            resolved.starts_with(&canonical_root),
            "Java 链接越出运行时目录"
        );
    }
    let java = root
        .join(&plan.java)
        .canonicalize()
        .context("下载的 Java 主程序不存在")?;
    ensure!(
        java.starts_with(&canonical_root),
        "Java 主程序越出运行时目录"
    );
    Ok(java)
}

pub fn download_runtime(
    root: &Path,
    target: &RuntimeDownload,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<java::JavaRuntime> {
    install::cancelled(cancel)?;
    let current = Platform::current();
    ensure!(
        target.platform == platform_key(&current)?,
        "只能安装当前系统原生架构的 Java"
    );
    // Revalidate the selection against the live publisher index before executing
    // anything. A caller cannot substitute an arbitrary signed-looking manifest.
    ensure!(
        list_runtimes(&current, cancel)?.contains(target),
        "Java 列表已变化，请刷新后重新选择"
    );
    let root = ordinary_root(root)?;
    let platform_root = install::safe_target(&root, Path::new(&target.platform))?;
    fs::create_dir_all(&platform_root)?;
    let folder = format!(
        "{}-{}-{}",
        target.component,
        target.version,
        &target.manifest.sha1[..12]
    );
    metadata::validate_id(&folder)?;
    let destination = install::safe_target(&platform_root, Path::new(&folder))?;
    ensure!(
        fs::symlink_metadata(&destination).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "此 Java 已存在或目标不可访问，未覆盖"
    );
    let client = install::http_client()?;
    progress(Progress {
        message: format!("读取 Java {} 文件清单", target.version),
        ..Default::default()
    });
    let bytes = install::request_bytes(
        &client,
        &target.manifest.url,
        Some(&target.manifest.sha1),
        Some(target.manifest.size),
        cancel,
    )?;
    let plan = plan_manifest(&bytes, &target.platform)?;
    let staged = tempfile::Builder::new()
        .prefix(".pcl-java-")
        .tempdir_in(&platform_root)?;
    for relative in &plan.directories {
        fs::create_dir_all(install::safe_target(staged.path(), relative)?)?;
    }
    let next = AtomicUsize::new(0);
    let failed = Mutex::new(None::<anyhow::Error>);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..4.min(plan.artifacts.len()) {
            let next = &next;
            let failed = &failed;
            let tx = tx.clone();
            let artifacts = &plan.artifacts;
            let client = &client;
            let staged = staged.path();
            scope.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) || failed.lock().unwrap().is_some() {
                    break;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(artifact) = artifacts.get(index) else {
                    break;
                };
                if let Err(error) = install::download_artifact(client, staged, artifact, cancel) {
                    failed.lock().unwrap().get_or_insert(error);
                    break;
                }
                if tx.send(artifact.relative_path.clone()).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for (index, path) in rx.into_iter().enumerate() {
            progress(Progress {
                message: format!("下载 Java：{}", path.display()),
                completed: index as u64 + 1,
                total: plan.artifacts.len() as u64,
                ..Default::default()
            });
        }
    });
    if let Some(error) = failed.into_inner().unwrap() {
        return Err(error).context("Java 下载未完成");
    }
    install::cancelled(cancel)?;
    let java = finish_tree(staged.path(), &plan, cancel)?;
    progress(Progress {
        message: "验证下载的 Java 版本和架构".into(),
        ..Default::default()
    });
    let runtime = java::inspect_java(&java)?;
    java::validate_for_version(&runtime, target.major, &current)?;
    install::cancelled(cancel)?;
    fs::write(
        staged.path().join(".pcl-runtime.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"platform":target.platform,"component":target.component,"version":target.version,"manifest":target.manifest,"java":plan.java}),
        )?,
    )?;
    ensure!(
        install::safe_target(&root, Path::new(&target.platform))? == platform_root,
        "Java 目录在下载期间发生变化"
    );
    install::cancelled(cancel)?;
    metadata::rename_directory_no_replace(staged.path(), &destination)?;
    let path = destination.join(&plan.java).canonicalize()?;
    ensure!(
        path.starts_with(destination.canonicalize()?),
        "Java 主程序不在已安装目录内"
    );
    progress(Progress {
        message: format!("Java {} 已下载并验证", target.version),
        completed: plan.artifacts.len() as u64,
        total: plan.artifacts.len() as u64,
        ..Default::default()
    });
    Ok(java::JavaRuntime {
        path,
        major: runtime.major,
        version: runtime.version,
        architecture: runtime.architecture,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(extra: serde_json::Value) -> Vec<u8> {
        let mut files = serde_json::json!({"jre.bundle/Contents/Home/bin/java":{"type":"file","executable":true,"downloads":{"raw":{"url":"https://piston-data.mojang.com/v1/objects/abc/java","sha1":"0000000000000000000000000000000000000000","size":10}}}});
        files
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::to_vec(&serde_json::json!({"files":files})).unwrap()
    }
    #[test]
    fn platform_mapping_and_legacy_version_parse() {
        let data = serde_json::json!({"windows-x64":{"jre-legacy":[{"version":{"name":"8u51-cacert462b08"},"manifest":{"url":"https://piston-meta.mojang.com/v1/manifest.json","sha1":"0000000000000000000000000000000000000000","size":100}}],"minecraft-java-exe":[]}});
        let list = parse_index(&serde_json::to_vec(&data).unwrap(), "windows-x64").unwrap();
        assert_eq!(list[0].major, 8);
        assert!(parse_index(&serde_json::to_vec(&data).unwrap(), "mac-os-arm64").is_err());
        assert_eq!(
            platform_key(&Platform {
                os: "osx".into(),
                arch: "aarch64".into(),
                version: String::new()
            })
            .unwrap(),
            "mac-os-arm64"
        );
    }
    #[test]
    fn manifest_rejects_traversal_link_parents_and_wrong_publishers() {
        for extra in [
            serde_json::json!({"../escape":{"type":"directory"}}),
            serde_json::json!({"jre.bundle/Contents":{"type":"link","target":"safe"}}),
            serde_json::json!({"escape":{"type":"link","target":"../../outside"}}),
            serde_json::json!({".pcl-runtime.json":{"type":"directory"}}),
        ] {
            assert!(plan_manifest(&manifest(extra), "mac-os-arm64").is_err());
        }
        let mut value: serde_json::Value =
            serde_json::from_slice(&manifest(serde_json::json!({}))).unwrap();
        value["files"]["jre.bundle/Contents/Home/bin/java"]["downloads"]["raw"]["url"] =
            "https://libraries.minecraft.net/arbitrary.jar".into();
        assert!(plan_manifest(&serde_json::to_vec(&value).unwrap(), "mac-os-arm64").is_err());
    }
    #[test]
    fn safe_relative_links_work_but_cycles_fail_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let plan=plan_manifest(&manifest(serde_json::json!({"legal/base":{"type":"directory"},"legal/other/LICENSE":{"type":"link","target":"../base/LICENSE"}})),"mac-os-arm64").unwrap();
        fs::create_dir_all(root.join("jre.bundle/Contents/Home/bin")).unwrap();
        fs::write(root.join(&plan.java), b"fixture").unwrap();
        fs::create_dir_all(root.join("legal/base")).unwrap();
        fs::write(root.join("legal/base/LICENSE"), b"license").unwrap();
        let java = finish_tree(root, &plan, &AtomicBool::new(false)).unwrap();
        assert!(java.starts_with(root.canonicalize().unwrap()));
        assert_eq!(
            fs::read(root.join("legal/other/LICENSE")).unwrap(),
            b"license"
        );
        let mut cyclic = plan;
        cyclic.links = vec![(PathBuf::from("cycle"), "cycle".into())];
        assert!(finish_tree(root, &cyclic, &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn commit_never_replaces_even_empty_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("stage");
        let target = dir.path().join("existing");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("new"), b"new").unwrap();
        fs::create_dir(&target).unwrap();
        assert!(metadata::rename_directory_no_replace(&source, &target).is_err());
        assert!(source.join("new").is_file());
        assert!(!target.join("new").exists());
        fs::write(target.join("user"), b"keep").unwrap();
        assert!(metadata::rename_directory_no_replace(&source, &target).is_err());
        assert_eq!(fs::read(target.join("user")).unwrap(), b"keep");
        let fresh = dir.path().join("fresh");
        metadata::rename_directory_no_replace(&source, &fresh).unwrap();
        assert_eq!(fs::read(fresh.join("new")).unwrap(), b"new");
    }
    #[test]
    fn cancellation_does_not_touch_existing_runtime() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("user"), b"keep").unwrap();
        let target = RuntimeDownload {
            platform: "mac-os-arm64".into(),
            component: "test".into(),
            version: "21".into(),
            major: 21,
            manifest: Download {
                url: String::new(),
                sha1: String::new(),
                size: 0,
            },
        };
        assert!(
            download_runtime(dir.path(), &target, &AtomicBool::new(true), |_| {})
                .unwrap_err()
                .is::<crate::model::OperationCancelled>()
        );
        assert_eq!(fs::read(dir.path().join("user")).unwrap(), b"keep");
    }
}
