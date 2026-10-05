//! Inert metadata, following LocalResourceFile's local fallbacks. Declarations
//! are evidence, never code to execute or a reason to edit a user's mod files.
use super::{read_entry, LocalMod};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Seek},
};
use zip::ZipArchive;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModMetadata {
    pub description: String,
    pub authors: Vec<String>,
    pub homepage: Option<String>,
    pub environment: Option<String>,
    pub provides: BTreeMap<String, String>,
    pub dependencies: Vec<ModDependency>,
    pub diagnostics: Vec<ModDiagnostic>,
    pub bundled: bool,
    pub inspected: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyKind {
    Required,
    Optional,
    Incompatible,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModDependency {
    pub id: String,
    /// Fabric predicates and Forge Maven intervals are retained verbatim.
    pub requirement: String,
    pub kind: DependencyKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModDiagnostic {
    pub code: String,
    pub message: String,
    /// False means an uncertain declaration/compatibility warning, not a broken file.
    pub error: bool,
}
fn short(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(4096)
        .collect()
}
fn homepage(text: Option<&str>) -> Option<String> {
    text.filter(|s| s.len() <= 2048)
        .and_then(|s| reqwest::Url::parse(s).ok())
        .filter(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
        })
        .map(|u| u.to_string())
}
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![short(s)],
        Value::Array(a) => a
            .iter()
            .take(128)
            .filter_map(|v| v.as_str().or_else(|| v["name"].as_str()))
            .map(short)
            .collect(),
        Value::Object(m) => m.keys().take(128).map(|s| short(s)).collect(),
        _ => vec![],
    }
}
fn dependency_map(out: &mut ModMetadata, value: &Value, kind: DependencyKind) {
    if let Some(map) = value.as_object() {
        for (id, v) in map.iter().take(1024) {
            let alternatives = strings(v);
            if !alternatives.is_empty() {
                out.dependencies.push(ModDependency {
                    id: short(id),
                    requirement: alternatives.join(" || "),
                    kind,
                });
            }
        }
    }
}
pub(super) fn read_metadata<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    item: &mut LocalMod,
) -> Result<()> {
    let mut info = ModMetadata {
        inspected: true,
        ..Default::default()
    };
    if let Some(text) = read_entry(archive, "quilt.mod.json")? {
        let v: Value = serde_json::from_str(&text).context("Quilt 元数据无效")?;
        let q = &v["quilt_loader"];
        let id = q["id"].as_str().context("Quilt 元数据缺少 id")?;
        item.loader = "quilt".into();
        item.mod_ids = vec![id.into()];
        item.name = q["metadata"]["name"].as_str().unwrap_or(id).into();
        item.version = q["version"].as_str().map(str::to_owned);
        info.description = short(q["metadata"]["description"].as_str().unwrap_or_default());
        info.authors = strings(&q["metadata"]["contributors"]);
        info.homepage = homepage(q["metadata"]["contact"]["homepage"].as_str());
        info.environment = v["minecraft"]["environment"].as_str().map(str::to_owned);
        info.bundled = q["jars"].as_array().is_some_and(|a| !a.is_empty());
        for (key, kind) in [
            ("depends", DependencyKind::Required),
            ("breaks", DependencyKind::Incompatible),
        ] {
            if let Some(deps) = q[key].as_array() {
                for d in deps.iter().take(1024) {
                    if let Some(id) = d.as_str().or_else(|| d["id"].as_str()) {
                        let req = strings(&d["versions"]);
                        info.dependencies.push(ModDependency {
                            id: short(id),
                            requirement: if req.is_empty() {
                                "*".into()
                            } else {
                                req.join(" || ")
                            },
                            kind: if d["optional"] == true {
                                DependencyKind::Optional
                            } else {
                                kind
                            },
                        });
                    } else {
                        info.bundled = true;
                    } // Composite Quilt predicates cannot be asserted missing.
                }
            }
        }
        if let Some(provides) = q["provides"].as_array() {
            for p in provides.iter().take(1024) {
                if let Some(id) = p["id"].as_str() {
                    info.provides.insert(
                        short(id),
                        p["version"]
                            .as_str()
                            .or(item.version.as_deref())
                            .unwrap_or("*")
                            .into(),
                    );
                }
            }
        }
    } else if let Some(text) = read_entry(archive, "fabric.mod.json")? {
        let v: Value = serde_json::from_str(&text)?;
        info.description = short(v["description"].as_str().unwrap_or_default());
        info.authors = strings(&v["authors"]);
        info.homepage = homepage(v["contact"]["homepage"].as_str());
        info.environment = v["environment"].as_str().map(str::to_owned);
        info.bundled = v["jars"].as_array().is_some_and(|a| !a.is_empty());
        for (key, kind) in [
            ("depends", DependencyKind::Required),
            ("recommends", DependencyKind::Optional),
            ("suggests", DependencyKind::Optional),
            ("breaks", DependencyKind::Incompatible),
            ("conflicts", DependencyKind::Incompatible),
        ] {
            dependency_map(&mut info, &v[key], kind);
        }
        for id in strings(&v["provides"]) {
            info.provides
                .insert(id, item.version.clone().unwrap_or("*".into()));
        }
    } else if item.loader == "forge" || item.loader == "neoforge" {
        let entry = if item.loader == "forge" {
            "META-INF/mods.toml"
        } else {
            "META-INF/neoforge.mods.toml"
        };
        let v: toml::Value = toml::from_str(&read_entry(archive, entry)?.unwrap_or_default())?;
        if let Some(mods) = v.get("mods").and_then(toml::Value::as_array) {
            for (i, m) in mods.iter().enumerate() {
                if let Some(id) = m.get("modId").and_then(toml::Value::as_str) {
                    info.provides.insert(
                        id.into(),
                        m.get("version")
                            .and_then(toml::Value::as_str)
                            .filter(|s| !s.contains("${"))
                            .or(item.version.as_deref())
                            .unwrap_or("*")
                            .into(),
                    );
                }
                if i == 0 {
                    info.description = short(
                        m.get("description")
                            .and_then(toml::Value::as_str)
                            .unwrap_or_default(),
                    );
                    info.authors = m
                        .get("authors")
                        .and_then(toml::Value::as_str)
                        .map(|s| vec![short(s)])
                        .unwrap_or_default();
                    info.homepage = homepage(m.get("displayURL").and_then(toml::Value::as_str));
                }
            }
        }
        if let Some(deps) = v.get("dependencies").and_then(toml::Value::as_table) {
            for entries in deps.values() {
                if let Some(list) = entries.as_array() {
                    for d in list.iter().take(1024) {
                        if d.get("side").and_then(toml::Value::as_str) == Some("SERVER") {
                            continue;
                        }
                        if let Some(id) = d.get("modId").and_then(toml::Value::as_str) {
                            let kind = match d.get("type").and_then(toml::Value::as_str) {
                                Some("incompatible") => DependencyKind::Incompatible,
                                Some("optional" | "discouraged") => DependencyKind::Optional,
                                _ if d.get("mandatory").and_then(toml::Value::as_bool)
                                    == Some(false) =>
                                {
                                    DependencyKind::Optional
                                }
                                _ => DependencyKind::Required,
                            };
                            info.dependencies.push(ModDependency {
                                id: short(id),
                                requirement: d
                                    .get("versionRange")
                                    .and_then(toml::Value::as_str)
                                    .unwrap_or("*")
                                    .into(),
                                kind,
                            });
                        }
                    }
                }
            }
        }
        info.bundled = read_entry(archive, "META-INF/jarjar/metadata.json")?.is_some();
    } else {
        let old = read_entry(archive, "mcmod.info")?.or(read_entry(archive, "litemod.json")?);
        if let Some(text) = old {
            let v: Value = serde_json::from_str(&text).context("旧 Mod 元数据无效")?;
            let entries = v
                .as_array()
                .or_else(|| v["modList"].as_array())
                .cloned()
                .unwrap_or_else(|| vec![v]);
            for (index, m) in entries.iter().take(1024).enumerate() {
                if let Some(id) = m["modid"].as_str().or_else(|| m["name"].as_str()) {
                    item.mod_ids.push(short(id));
                    info.provides
                        .insert(short(id), m["version"].as_str().unwrap_or("*").into());
                }
                if index == 0 {
                    item.name = m["name"].as_str().unwrap_or(&item.name).into();
                    item.version = m["version"].as_str().map(str::to_owned);
                    info.description = short(m["description"].as_str().unwrap_or_default());
                    info.authors = strings(&m["authorList"]);
                    if info.authors.is_empty() {
                        info.authors = strings(&m["author"]);
                    }
                    info.homepage = homepage(m["url"].as_str());
                }
                for required in strings(&m["requiredMods"]) {
                    let (id, req) = required.split_once('@').unwrap_or((&required, "*"));
                    info.dependencies.push(ModDependency {
                        id: short(id),
                        requirement: short(req),
                        kind: DependencyKind::Required,
                    });
                }
            }
            if item.loader == "unknown" {
                item.loader = "forge".into();
            }
        }
    }
    for id in &item.mod_ids {
        info.provides
            .entry(id.clone())
            .or_insert_with(|| item.version.clone().unwrap_or("*".into()));
    }
    item.mod_ids.sort();
    item.mod_ids.dedup();
    item.metadata = info;
    Ok(())
}

fn numbers(value: &str) -> Option<Vec<u64>> {
    let v = value.strip_prefix('v').unwrap_or(value);
    if v.is_empty() || !v.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    v.split('.').map(|x| x.parse().ok()).collect()
}
fn compare(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    (0..left.len().max(right.len()))
        .map(|i| {
            (
                left.get(i).copied().unwrap_or(0),
                right.get(i).copied().unwrap_or(0),
            )
        })
        .find_map(|(a, b)| (a != b).then(|| a.cmp(&b)))
        .unwrap_or(std::cmp::Ordering::Equal)
}
/// None preserves uncertainty for loader-specific/non-numeric predicates; it is never a conflict.
fn satisfies(version: &str, requirement: &str) -> Option<bool> {
    let req = requirement.trim();
    if req.is_empty() || req == "*" {
        return Some(true);
    }
    if req.contains("||") {
        let parts: Vec<_> = req.split("||").map(|p| satisfies(version, p)).collect();
        return if parts.contains(&Some(true)) {
            Some(true)
        } else if parts.contains(&None) {
            None
        } else {
            Some(false)
        };
    }
    if version == req {
        return Some(true);
    }
    let actual = numbers(version)?;
    if (req.starts_with('[') || req.starts_with('(')) && (req.ends_with(']') || req.ends_with(')'))
    {
        let inner = &req[1..req.len() - 1];
        if let Some((low, high)) = inner.split_once(',') {
            let lower = if low.trim().is_empty() {
                true
            } else {
                let c = compare(&actual, &numbers(low.trim())?);
                if req.starts_with('[') {
                    !c.is_lt()
                } else {
                    c.is_gt()
                }
            };
            let upper = if high.trim().is_empty() {
                true
            } else {
                let c = compare(&actual, &numbers(high.trim())?);
                if req.ends_with(']') {
                    !c.is_gt()
                } else {
                    c.is_lt()
                }
            };
            return Some(lower && upper);
        }
        return Some(compare(&actual, &numbers(inner)?) == std::cmp::Ordering::Equal);
    }
    let mut result = true;
    for part in req.split_whitespace() {
        let (op, raw) = [">=", "<=", ">", "<", "=", "~", "^"]
            .into_iter()
            .find_map(|op| part.strip_prefix(op).map(|v| (op, v)))
            .unwrap_or(("=", part));
        if raw.split('.').any(|s| matches!(s, "*" | "x" | "X")) {
            if op != "=" {
                return None;
            }
            let prefix: Vec<u64> = raw
                .split('.')
                .take_while(|s| !matches!(*s, "*" | "x" | "X"))
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()
                .ok()?;
            result &= prefix
                .iter()
                .enumerate()
                .all(|(i, p)| actual.get(i).copied().unwrap_or(0) == *p);
            continue;
        }
        let wanted = numbers(raw)?;
        let c = compare(&actual, &wanted);
        result &= match op {
            ">=" => !c.is_lt(),
            "<=" => !c.is_gt(),
            ">" => c.is_gt(),
            "<" => c.is_lt(),
            "=" => c.is_eq(),
            "~" | "^" => {
                let mut upper = wanted.clone();
                let index = if op == "~" {
                    if upper.len() > 1 {
                        1
                    } else {
                        0
                    }
                } else {
                    upper
                        .iter()
                        .position(|n| *n != 0)
                        .unwrap_or(upper.len() - 1)
                };
                upper[index] = upper[index].checked_add(1)?;
                for n in &mut upper[index + 1..] {
                    *n = 0;
                }
                !c.is_lt() && compare(&actual, &upper).is_lt()
            }
            _ => return None,
        };
    }
    Some(result)
}
pub(super) fn diagnose(mods: &mut [LocalMod]) {
    let mut installed: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    for (i, m) in mods
        .iter()
        .enumerate()
        .filter(|(_, m)| m.enabled && m.error.is_none())
    {
        for (id, v) in &m.metadata.provides {
            installed
                .entry(id.clone())
                .or_default()
                .push((i, v.clone()));
        }
    }
    for (i, m) in mods.iter_mut().enumerate() {
        m.metadata.diagnostics.clear();
        if !m.enabled || m.error.is_some() {
            continue;
        }
        if m.metadata.environment.as_deref() == Some("server") {
            m.metadata.diagnostics.push(ModDiagnostic {
                code: "server-only".into(),
                message: "元数据声明此 Mod 仅用于服务端，不会作为客户端 Mod 加载".into(),
                error: false,
            });
        }
        for id in m.metadata.provides.keys() {
            if installed[id].iter().any(|(other, _)| *other != i) {
                m.metadata.diagnostics.push(ModDiagnostic {
                    code: "duplicate".into(),
                    message: format!("重复的 Mod ID：{id}（多个启用文件声明同一 ID）"),
                    error: true,
                });
            }
        }
        for d in &m.metadata.dependencies {
            if d.kind == DependencyKind::Optional
                || matches!(
                    d.id.as_str(),
                    "minecraft"
                        | "java"
                        | "fabricloader"
                        | "quilt_loader"
                        | "forge"
                        | "neoforge"
                        | "fml"
                        | "liteloader"
                )
            {
                continue;
            }
            let candidates = installed.get(&d.id);
            let satisfied = candidates.and_then(|v| {
                let matches: Vec<_> = v
                    .iter()
                    .map(|(_, v)| satisfies(v, &d.requirement))
                    .collect();
                if matches.contains(&Some(true)) {
                    Some(true)
                } else if matches.contains(&None) {
                    None
                } else {
                    Some(false)
                }
            });
            let diagnostic = match (d.kind, candidates, satisfied) {
                (DependencyKind::Required, None, _) => Some((
                    "missing",
                    format!(
                        "{}前置 {} {}",
                        if m.metadata.bundled {
                            "未能从顶层文件确认"
                        } else {
                            "缺少"
                        },
                        d.id,
                        d.requirement
                    ),
                    !m.metadata.bundled,
                )),
                (DependencyKind::Required, Some(_), Some(false)) => Some((
                    "version",
                    format!("前置版本不匹配：{} 需要 {}", d.id, d.requirement),
                    true,
                )),
                (DependencyKind::Incompatible, Some(_), Some(true)) => Some((
                    "incompatible",
                    format!("声明不兼容：{} {}", d.id, d.requirement),
                    true,
                )),
                (_, Some(_), None) => Some((
                    "unverified",
                    format!("需由加载器核对版本条件：{} {}", d.id, d.requirement),
                    false,
                )),
                _ => None,
            };
            if let Some((code, message, error)) = diagnostic {
                m.metadata.diagnostics.push(ModDiagnostic {
                    code: code.into(),
                    message,
                    error,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Write};
    fn jar(root: &std::path::Path, name: &str, entry: &str, text: &str) {
        let mut w = zip::ZipWriter::new(File::create(root.join("mods").join(name)).unwrap());
        w.start_file(entry, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(text.as_bytes()).unwrap();
        w.finish().unwrap();
    }
    #[test]
    fn local_descriptions_quilt_legacy_and_declared_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("mods")).unwrap();
        jar(
            temp.path(),
            "a.jar",
            "fabric.mod.json",
            r#"{"id":"a","version":"1.0","description":"Real description","authors":[{"name":"Author"}],"depends":{"b":">=2.0","optional":"*"},"breaks":{"c":"*"}}"#,
        );
        jar(
            temp.path(),
            "b.jar",
            "quilt.mod.json",
            r#"{"quilt_loader":{"id":"b","version":"1.5","metadata":{"name":"Quilt B","description":"Quilt description"}}}"#,
        );
        jar(
            temp.path(),
            "c.jar.disabled",
            "mcmod.info",
            r#"[{"modid":"c","name":"Legacy C","version":"1.0","description":"Legacy description"}]"#,
        );
        let mods = super::super::list_mods(temp.path()).unwrap();
        assert_eq!(mods[0].metadata.authors, ["Author"]);
        assert_eq!(mods[1].loader, "quilt");
        assert_eq!(mods[2].metadata.description, "Legacy description");
        assert!(mods[0]
            .metadata
            .diagnostics
            .iter()
            .any(|d| d.code == "version"));
        assert!(mods[0]
            .metadata
            .diagnostics
            .iter()
            .any(|d| d.code == "missing"));
        assert!(!mods[0]
            .metadata
            .diagnostics
            .iter()
            .any(|d| d.code == "incompatible"));
        assert!(mods[2].metadata.diagnostics.is_empty());
    }
    #[test]
    fn aliases_duplicate_and_bundled_uncertainty_are_not_guessed() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("mods")).unwrap();
        jar(
            temp.path(),
            "one.jar",
            "fabric.mod.json",
            r#"{"id":"one","version":"1.0","provides":["alias"],"jars":[{"file":"nested.jar"}],"depends":{"nested":"*"}}"#,
        );
        jar(
            temp.path(),
            "two.jar",
            "fabric.mod.json",
            r#"{"id":"two","version":"1.0","provides":["alias"],"depends":{"alias":"1.x"}}"#,
        );
        let mods = super::super::list_mods(temp.path()).unwrap();
        assert!(mods
            .iter()
            .all(|m| m.metadata.diagnostics.iter().any(|d| d.code == "duplicate")));
        assert!(mods[0]
            .metadata
            .diagnostics
            .iter()
            .any(|d| d.code == "missing" && !d.error));
        assert!(!mods[1]
            .metadata
            .diagnostics
            .iter()
            .any(|d| d.code == "missing"));
    }
    #[test]
    fn supported_ranges_and_unknown_versions() {
        for (v, r, yes) in [
            ("1.2.3", ">=1.0 <2", true),
            ("2.0", "[1,2)", false),
            ("1.9", "[1,)", true),
            ("0.2.8", "^0.2.3", true),
            ("0.3", "^0.2.3", false),
            ("1.3", "~1.2", false),
            ("1.2.8", "1.2.x", true),
            ("2", "1 || 2", true),
        ] {
            assert_eq!(satisfies(v, r), Some(yes), "{v} {r}");
        }
        assert_eq!(satisfies("release-custom", ">=1"), None);
    }
    #[test]
    fn forge_client_dependencies_are_checked_without_executing_or_modifying_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("mods")).unwrap();
        let metadata="[[mods]]\nmodId='sample'\nversion='1.0'\ndisplayName='Sample'\ndescription='A real TOML description'\nauthors='A, B'\n[[dependencies.sample]]\nmodId='server_lib'\nmandatory=true\nversionRange='[1,)'\nside='SERVER'\n[[dependencies.sample]]\nmodId='optional_lib'\nmandatory=false\nversionRange='[1,)'\n[[dependencies.sample]]\nmodId='required_lib'\nmandatory=true\nversionRange='[2,3)'\n";
        jar(temp.path(), "sample.jar", "META-INF/mods.toml", metadata);
        let path = temp.path().join("mods/sample.jar");
        let before = std::fs::read(&path).unwrap();
        let files = super::super::list_mods(temp.path()).unwrap();
        assert_eq!(files[0].metadata.description, "A real TOML description");
        assert_eq!(files[0].metadata.diagnostics.len(), 1);
        assert!(files[0].metadata.diagnostics[0]
            .message
            .contains("required_lib"));
        assert_eq!(before, std::fs::read(path).unwrap());
    }
}
