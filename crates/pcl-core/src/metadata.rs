use crate::model::{Artifact, InstalledVersion, Platform};
use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.ends_with(['.', ' '])
        || id
            .chars()
            .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
    {
        bail!("不安全的版本或资源标识：{id:?}");
    }
    let stem = id.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        bail!("标识不能使用 Windows 保留设备名：{id}");
    }
    Ok(())
}

/// Metadata paths always use forward slashes, regardless of the current host.
pub fn safe_relative(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') {
        bail!("不安全的相对路径：{path:?}");
    }
    let mut output = PathBuf::new();
    for part in path.split('/') {
        validate_id(part).with_context(|| format!("不安全的相对路径：{path:?}"))?;
        output.push(part);
    }
    Ok(output)
}

/// Reject existing symlink ancestors that escape the selected game root.
pub fn confined_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    let canonical_root = root
        .canonicalize()
        .context("Minecraft 根目录不存在或无法读取")?;
    let candidate = root.join(relative);
    let mut ancestor = candidate.as_path();
    loop {
        if fs::symlink_metadata(ancestor).is_ok() {
            let actual = ancestor
                .canonicalize()
                .with_context(|| format!("路径不可访问：{}", ancestor.display()))?;
            if !actual.starts_with(&canonical_root) {
                bail!(
                    "路径通过符号链接离开 Minecraft 根目录：{}",
                    candidate.display()
                );
            }
            break;
        }
        ancestor = ancestor.parent().context("无法确定路径所属目录")?;
    }
    Ok(candidate)
}

pub fn resolve_version(root: &Path, id: &str) -> Result<Value> {
    resolve_inner(root, id, &mut HashSet::new(), 0)
}

fn resolve_inner(
    root: &Path,
    id: &str,
    visiting: &mut HashSet<String>,
    depth: usize,
) -> Result<Value> {
    validate_id(id)?;
    if depth > 64 || !visiting.insert(id.to_owned()) {
        bail!("版本继承存在循环或超过 64 层：{id}");
    }
    let relative = safe_relative(&format!("versions/{id}/{id}.json"))?;
    let path = confined_path(root, &relative)?;
    let text = fs::read_to_string(&path)
        .with_context(|| format!("无法读取版本元数据：{}", path.display()))?;
    let child: Value =
        serde_json::from_str(&text).with_context(|| format!("版本 JSON 无效：{id}"))?;
    let child = child.as_object().context("版本元数据必须是 JSON 对象")?;
    let mut output = if let Some(parent) = child.get("inheritsFrom") {
        let parent = parent.as_str().context("inheritsFrom 必须是字符串")?;
        resolve_inner(root, parent, visiting, depth + 1)?
            .as_object()
            .context("父版本元数据无效")?
            .clone()
    } else {
        let mut initial = Map::new();
        initial.insert("_pcl_jar_id".into(), Value::String(id.to_owned()));
        initial
    };
    for (key, value) in child {
        match key.as_str() {
            "libraries" => {
                let children = value.as_array().context("libraries 必须是数组")?;
                let parents = output
                    .get("libraries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut seen = HashSet::new();
                let mut libraries = Vec::new();
                // Child libraries must precede parent libraries on the classpath.
                for library in children.iter().chain(parents.iter()) {
                    let name = library
                        .get("name")
                        .and_then(Value::as_str)
                        .context("支持库缺少 name")?;
                    let coordinate = MavenCoordinate::parse(name)?;
                    let key = format!(
                        "{}:{}:{}:{}",
                        coordinate.group,
                        coordinate.artifact,
                        coordinate.classifier.unwrap_or(""),
                        coordinate.extension
                    );
                    if seen.insert(key) {
                        libraries.push(library.clone());
                    }
                }
                output.insert(key.clone(), Value::Array(libraries));
            }
            "arguments" => {
                let child_args = value.as_object().context("arguments 必须是对象")?;
                let mut combined = output
                    .get("arguments")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (kind, arguments) in child_args {
                    let additions = arguments
                        .as_array()
                        .context("arguments 中的参数组必须是数组")?;
                    let mut inherited = combined
                        .get(kind)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    inherited.extend(additions.iter().cloned());
                    combined.insert(kind.clone(), Value::Array(inherited));
                }
                output.insert(key.clone(), Value::Object(combined));
            }
            "_pcl_jar_id" => {} // Internal state must never be supplied by external JSON.
            _ => {
                output.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(jar) = child.get("jar") {
        let jar = jar.as_str().context("jar 必须是字符串")?;
        validate_id(jar)?;
        output.insert("_pcl_jar_id".into(), Value::String(jar.to_owned()));
    } else if child
        .get("downloads")
        .and_then(|d| d.get("client"))
        .is_some()
    {
        output.insert("_pcl_jar_id".into(), Value::String(id.to_owned()));
    }
    output.insert("id".into(), Value::String(id.to_owned()));
    visiting.remove(id);
    Ok(Value::Object(output))
}

pub fn list_installed(root: &Path) -> Result<Vec<InstalledVersion>> {
    let directory = root.join("versions");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let directory = confined_path(root, Path::new("versions"))?;
    let mut versions = Vec::new();
    for entry in fs::read_dir(directory).context("无法列出已安装版本")? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        match resolve_version(root, &id) {
            Ok(value) => versions.push(InstalledVersion {
                id,
                kind: value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("custom")
                    .to_owned(),
                required_java: value
                    .pointer("/javaVersion/majorVersion")
                    .and_then(Value::as_u64)
                    .unwrap_or(8) as u32,
                error: if value.get("mainClass").and_then(Value::as_str).is_none() {
                    Some("版本缺少 mainClass".into())
                } else {
                    None
                },
            }),
            Err(error) => versions.push(InstalledVersion {
                id,
                kind: "invalid".into(),
                required_java: 8,
                error: Some(format!("{error:#}")),
            }),
        }
    }
    versions.sort_by_key(|version| version.id.to_lowercase());
    Ok(versions)
}

pub fn rules_allow(
    rules: &Value,
    platform: &Platform,
    features: &HashMap<String, bool>,
) -> Result<bool> {
    if rules.is_null() {
        return Ok(true);
    }
    let rules = rules.as_array().context("rules 必须是数组")?;
    if rules.is_empty() {
        return Ok(true);
    }
    let mut allowed = false;
    for rule in rules {
        let action = rule
            .get("action")
            .and_then(Value::as_str)
            .context("rule 缺少 action")?;
        if !matches!(action, "allow" | "disallow") {
            bail!("未知 rule action：{action}");
        }
        let mut matches = true;
        if let Some(os) = rule.get("os") {
            let os = os.as_object().context("rule.os 必须是对象")?;
            if let Some(name) = os.get("name") {
                matches &= name.as_str().context("os.name 必须是字符串")? == platform.os;
            }
            if let Some(arch) = os.get("arch") {
                let pattern = arch.as_str().context("os.arch 必须是字符串")?;
                let architecture = match platform.arch.as_str() {
                    "x86_64" => "amd64",
                    "aarch64" => "aarch64",
                    other => other,
                };
                let regex =
                    Regex::new(&format!("^(?:{pattern})$")).context("os.arch 正则表达式无效")?;
                matches &= regex.is_match(architecture) || regex.is_match(&platform.arch);
            }
            if let Some(version) = os.get("version") {
                let regex = Regex::new(version.as_str().context("os.version 必须是字符串")?)
                    .context("os.version 正则表达式无效")?;
                if matches && platform.version.is_empty() {
                    bail!("无法读取当前系统版本，不能安全判断 os.version 启动规则");
                }
                matches &= regex.is_match(&platform.version);
            }
        }
        if let Some(requirements) = rule.get("features") {
            for (name, expected) in requirements
                .as_object()
                .context("rule.features 必须是对象")?
            {
                matches &= features.get(name).copied().unwrap_or(false)
                    == expected.as_bool().context("feature 必须是布尔值")?;
            }
        }
        if matches {
            allowed = action == "allow";
        }
    }
    Ok(allowed)
}

struct MavenCoordinate<'a> {
    group: &'a str,
    artifact: &'a str,
    version: &'a str,
    classifier: Option<&'a str>,
    extension: &'a str,
}

impl<'a> MavenCoordinate<'a> {
    fn parse(name: &'a str) -> Result<Self> {
        let (coordinate, extension) = name.split_once('@').unwrap_or((name, "jar"));
        let parts: Vec<_> = coordinate.split(':').collect();
        if !(3..=4).contains(&parts.len()) {
            bail!("无效 Maven 坐标：{name}");
        }
        for component in &parts {
            validate_id(component)?;
        }
        validate_id(extension)?;
        for part in parts[0].split('.') {
            validate_id(part)?;
        }
        Ok(Self {
            group: parts[0],
            artifact: parts[1],
            version: parts[2],
            classifier: parts.get(3).copied(),
            extension,
        })
    }

    fn path(&self, classifier: Option<&str>) -> Result<String> {
        if let Some(value) = classifier {
            validate_id(value)?;
        }
        let suffix = classifier.map(|v| format!("-{v}")).unwrap_or_default();
        Ok(format!(
            "{}/{}/{}/{}-{}{}.{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.artifact,
            self.version,
            suffix,
            self.extension
        ))
    }
}

pub fn library_artifacts(version: &Value, platform: &Platform) -> Result<Vec<Artifact>> {
    let Some(libraries) = version.get("libraries") else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    let mut paths = HashSet::new();
    for library in libraries.as_array().context("libraries 必须是数组")? {
        if !rules_allow(&library["rules"], platform, &HashMap::new())? {
            continue;
        }
        let name = library
            .get("name")
            .and_then(Value::as_str)
            .context("支持库缺少 name")?;
        let coordinate = MavenCoordinate::parse(name)?;
        let excludes: Vec<String> = library
            .pointer("/extract/exclude")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| vec!["META-INF/".into()]);
        let mut add =
            |download: Option<&Value>, classifier: Option<&str>, native: bool| -> Result<()> {
                let fallback = coordinate.path(classifier)?;
                let relative = download
                    .and_then(|d| d.get("path"))
                    .and_then(Value::as_str)
                    .unwrap_or(&fallback);
                let relative_path = safe_relative(&format!("libraries/{relative}"))?;
                let url = if let Some(url) =
                    download.and_then(|d| d.get("url")).and_then(Value::as_str)
                {
                    url.to_owned()
                } else {
                    format!(
                        "{}/{}",
                        library
                            .get("url")
                            .and_then(Value::as_str)
                            .unwrap_or("https://libraries.minecraft.net")
                            .trim_end_matches('/'),
                        relative
                    )
                };
                if paths.insert(relative_path.clone()) {
                    output.push(Artifact {
                        relative_path,
                        url,
                        sha1: download
                            .and_then(|d| d.get("sha1"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        size: download.and_then(|d| d.get("size")).and_then(Value::as_u64),
                        native,
                        excludes: excludes.clone(),
                    });
                }
                Ok(())
            };
        if let Some(download) = library.pointer("/downloads/artifact") {
            // Modern LWJGL native jars contain resources such as
            // macos/arm64/org/lwjgl/glfw/libglfw.dylib and belong on the classpath.
            // Only the legacy `natives` map requests extraction to java.library.path.
            add(Some(download), coordinate.classifier, false)?;
        } else if library.get("downloads").is_none() {
            add(None, coordinate.classifier, false)?;
        }
        if let Some(native) = library.get("natives").and_then(|n| n.get(&platform.os)) {
            let classifier = native
                .as_str()
                .context("native classifier 必须是字符串")?
                .replace(
                    "${arch}",
                    if matches!(platform.arch.as_str(), "x86" | "i386" | "i686") {
                        "32"
                    } else {
                        "64"
                    },
                );
            let download = library
                .get("downloads")
                .and_then(|d| d.get("classifiers"))
                .and_then(|d| d.get(&classifier));
            if library.get("downloads").is_some() && download.is_none() {
                bail!("支持库 {name} 缺少 native classifier：{classifier}");
            }
            add(download, Some(&classifier), true)?;
        }
    }
    Ok(output)
}

/// Atomic directory rename that never replaces an existing destination.
pub(crate) fn rename_directory_no_replace(source: &Path, target: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        unsafe extern "C" {
            fn renamex_np(
                from: *const std::ffi::c_char,
                to: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        let from = CString::new(source.as_os_str().as_bytes())?;
        let to = CString::new(target.as_os_str().as_bytes())?;
        // macOS SDK sys/stdio.h: RENAME_EXCL = 0x00000004.
        if unsafe { renamex_np(from.as_ptr(), to.as_ptr(), 4) } != 0 {
            return Err(std::io::Error::last_os_error()).context("目录已存在或无法提交，未覆盖");
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileW(from: *const u16, to: *const u16) -> i32;
        }
        let mut from: Vec<u16> = source.as_os_str().encode_wide().collect();
        let mut to: Vec<u16> = target.as_os_str().encode_wide().collect();
        anyhow::ensure!(!from.contains(&0) && !to.contains(&0), "路径含空字符");
        from.push(0);
        to.push(0);
        if unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error()).context("目录已存在或无法提交，未覆盖");
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        unsafe extern "C" {
            fn renameat2(
                olddirfd: i32,
                oldpath: *const std::ffi::c_char,
                newdirfd: i32,
                newpath: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        let from = CString::new(source.as_os_str().as_bytes())?;
        let to = CString::new(target.as_os_str().as_bytes())?;
        // Linux rename(2): AT_FDCWD = -100, RENAME_NOREPLACE = 1.
        // A missing syscall/filesystem feature must fail, never fall back to
        // rename() (which could replace a destination created concurrently).
        if unsafe { renameat2(-100, from.as_ptr(), -100, to.as_ptr(), 1) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("目标已存在或无法原子提交，未覆盖");
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (source, target);
        bail!("此平台尚未实现 目录的原子安全提交");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    fn exclusive_rename_handles_files_and_directories_without_clobbering() {
        let directory = tempfile::tempdir().unwrap();
        for is_directory in [false, true] {
            let source = directory.path().join(if is_directory {
                "directory-source"
            } else {
                "file-source"
            });
            let target = directory.path().join(if is_directory {
                "directory-target"
            } else {
                "file-target"
            });
            if is_directory {
                fs::create_dir(&source).unwrap();
                fs::create_dir(&target).unwrap();
                fs::write(source.join("original"), b"source").unwrap();
            } else {
                fs::write(&source, b"source").unwrap();
                fs::write(&target, b"target").unwrap();
            }
            assert!(rename_directory_no_replace(&source, &target).is_err());
            assert!(source.exists() && target.exists());
            if is_directory {
                assert!(!target.join("original").exists());
                fs::remove_dir(&target).unwrap();
            } else {
                assert_eq!(fs::read(&source).unwrap(), b"source");
                assert_eq!(fs::read(&target).unwrap(), b"target");
                fs::remove_file(&target).unwrap();
            }
            rename_directory_no_replace(&source, &target).unwrap();
            assert!(!source.exists());
            assert_eq!(
                fs::read(if is_directory {
                    target.join("original")
                } else {
                    target
                })
                .unwrap(),
                b"source"
            );
        }
    }

    fn write_version(root: &Path, id: &str, json: Value) {
        let directory = root.join("versions").join(id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(format!("{id}.json")), json.to_string()).unwrap();
    }

    #[test]
    fn inheritance_preserves_child_library_precedence_and_parent_arguments() {
        let root = tempfile::tempdir().unwrap();
        write_version(
            root.path(),
            "base",
            json!({"mainClass":"Base", "libraries":[{"name":"example:lib:1"},{"name":"example:other:1"}], "arguments":{"game":["--base"]}}),
        );
        write_version(
            root.path(),
            "modded",
            json!({"inheritsFrom":"base", "mainClass":"Modded", "libraries":[{"name":"example:lib:2"}], "arguments":{"game":["--mod"]}}),
        );
        let result = resolve_version(root.path(), "modded").unwrap();
        assert_eq!(result["mainClass"], "Modded");
        assert_eq!(result["_pcl_jar_id"], "base");
        assert_eq!(result["libraries"][0]["name"], "example:lib:2");
        assert_eq!(result["libraries"].as_array().unwrap().len(), 2);
        assert_eq!(result["arguments"]["game"], json!(["--base", "--mod"]));
    }

    #[test]
    fn rejects_cycles_and_cross_platform_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        write_version(root.path(), "a", json!({"inheritsFrom":"b"}));
        write_version(root.path(), "b", json!({"inheritsFrom":"a"}));
        assert!(resolve_version(root.path(), "a")
            .unwrap_err()
            .to_string()
            .contains("循环"));
        for path in [
            "../evil",
            "/absolute",
            "C:/evil",
            "a\\..\\evil",
            "a/../evil",
            "a//b",
            "a/CON",
            "a/b.",
        ] {
            assert!(safe_relative(path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn ordered_rules_match_architecture_and_features() {
        let platform = Platform {
            os: "osx".into(),
            arch: "aarch64".into(),
            version: "14.0".into(),
        };
        let rules = json!([{"action":"allow"}, {"action":"disallow","os":{"name":"osx"}}, {"action":"allow","os":{"arch":"aarch64"},"features":{"demo":true}}]);
        assert!(!rules_allow(&rules, &platform, &HashMap::new()).unwrap());
        assert!(rules_allow(&rules, &platform, &HashMap::from([("demo".into(), true)])).unwrap());
        assert!(rules_allow(
            &json!([{"action":"allow","os":{"arch":"["}}]),
            &platform,
            &HashMap::new()
        )
        .is_err());
    }

    #[test]
    fn resolves_legacy_and_modern_native_downloads() {
        let platform = Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            version: String::new(),
        };
        let version = json!({"libraries":[
            {"name":"org.example:core:1.0"},
            {"name":"org.example:native:2", "natives":{"windows":"natives-${arch}"}, "downloads":{"classifiers":{"natives-64":{"path":"org/example/native/2/native-2-natives-64.jar","url":"https://example.test/native.jar"}}}},
            {"name":"org.example:modern:3:natives-windows", "downloads":{"artifact":{"url":"https://example.test/modern.jar"}}}
        ]});
        let artifacts = library_artifacts(&version, &platform).unwrap();
        assert_eq!(artifacts.len(), 3);
        assert_eq!(
            artifacts[0].relative_path,
            Path::new("libraries/org/example/core/1.0/core-1.0.jar")
        );
        assert!(!artifacts[0].native);
        assert!(artifacts[1].native);
        assert!(!artifacts[2].native);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_version_symlinks_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("versions")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("versions/evil")).unwrap();
        assert!(resolve_version(root.path(), "evil")
            .unwrap_err()
            .to_string()
            .contains("符号链接"));
    }

    #[test]
    fn missing_os_version_fails_only_for_relevant_os_rules() {
        let platform = Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            version: String::new(),
        };
        let matching = serde_json::json!([{ "action": "allow", "os": { "name": "windows", "version": "^10\\." } }]);
        assert!(rules_allow(&matching, &platform, &HashMap::new())
            .unwrap_err()
            .to_string()
            .contains("无法读取当前系统版本"));
        let unrelated = serde_json::json!([{ "action": "allow", "os": { "name": "osx", "version": "^15\\." } }]);
        assert!(!rules_allow(&unrelated, &platform, &HashMap::new()).unwrap());
        let detected = Platform {
            version: "10.0.26100".into(),
            ..platform
        };
        assert!(rules_allow(&matching, &detected, &HashMap::new()).unwrap());
    }
}
