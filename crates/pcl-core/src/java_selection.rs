//! Java selection follows the fixed upstream ModJava.vb and PCLCS/Java.cs rules.
//! Selection performs no download and never changes saved user choices.
use crate::{
    java::{self, JavaRuntime, JavaVersion},
    metadata,
    model::Platform,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JavaSelectionMode {
    #[default]
    Automatic,
    VersionRange,
    VersionFolder,
    Specific,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaRange {
    pub lower: Option<JavaVersion>,
    pub upper: Option<JavaVersion>,
    pub lower_inclusive: bool,
    pub upper_inclusive: bool,
}
impl JavaRange {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        ensure!(
            !text.is_empty() && text.len() <= 100,
            "Java 版本区间不能为空且不能超过 100 字节"
        );
        let mut chars = text.chars();
        let left = chars.next().unwrap();
        let right = chars.next_back().context("Java 版本区间格式不完整")?;
        ensure!(
            matches!(left, '[' | '(') && matches!(right, ']' | ')'),
            "Java 版本区间应写为 [17.0.1,25.0) 等格式"
        );
        let content = chars.as_str();
        let (lower, upper) = content.split_once(',').context("Java 版本区间缺少逗号")?;
        ensure!(!upper.contains(','), "Java 版本区间只能包含一个逗号");
        let parse = |value: &str| -> Result<Option<JavaVersion>> {
            if value.trim().is_empty() {
                return Ok(None);
            }
            let version = JavaVersion::parse(value)?;
            ensure!(
                version.major > 1
                    || (version.minor == 0 && version.patch == 0 && version.build == 0),
                "范围请使用 8、17 等主版本，不要使用 1.x 格式"
            );
            Ok(Some(version))
        };
        let lower = parse(lower)?;
        let upper = parse(upper)?;
        ensure!(upper.is_none_or(|v| v.major > 4), "Java 范围要求的版本过低");
        ensure!(
            !(right == ']' && upper.is_some_and(|v| v.minor == 0 && v.patch == 0)),
            "右侧闭区间含义不明确；排除该主版本请用圆括号，允许整个主版本请用下一主版本的圆括号"
        );
        let range = Self {
            lower,
            upper,
            lower_inclusive: left == '[',
            upper_inclusive: right == ']',
        };
        ensure!(!range.is_empty(), "范围下限比上限高或区间为空");
        ensure!(
            range
                .intersection(&Self {
                    lower: Some(JavaVersion::new(5, 0, 0, 0)),
                    upper: Some(JavaVersion::new(99, 0, 0, 0)),
                    lower_inclusive: false,
                    upper_inclusive: true
                })
                .is_some(),
            "该范围无法匹配常见的 Java 版本"
        );
        Ok(range)
    }
    pub fn contains(&self, version: JavaVersion) -> bool {
        self.lower
            .is_none_or(|v| version > v || (self.lower_inclusive && version == v))
            && self
                .upper
                .is_none_or(|v| version < v || (self.upper_inclusive && version == v))
    }
    pub fn all() -> Self {
        Self {
            lower: None,
            upper: None,
            lower_inclusive: false,
            upper_inclusive: false,
        }
    }
    fn at_least(version: JavaVersion) -> Self {
        Self {
            lower: Some(version),
            lower_inclusive: true,
            ..Self::all()
        }
    }
    fn below(version: JavaVersion) -> Self {
        Self {
            upper: Some(version),
            ..Self::all()
        }
    }
    fn at_most(version: JavaVersion) -> Self {
        Self {
            upper: Some(version),
            upper_inclusive: true,
            ..Self::all()
        }
    }
    fn is_empty(&self) -> bool {
        matches!((self.lower,self.upper),(Some(a),Some(b)) if a>b||(a==b&&(!self.lower_inclusive||!self.upper_inclusive)))
    }
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let (lower, lower_inclusive) = match (self.lower, other.lower) {
            (Some(a), Some(b)) if a == b => {
                (Some(a), self.lower_inclusive && other.lower_inclusive)
            }
            (Some(a), Some(b)) if a > b => (Some(a), self.lower_inclusive),
            (Some(_), Some(b)) => (Some(b), other.lower_inclusive),
            (None, b) => (b, other.lower_inclusive),
            (a, None) => (a, self.lower_inclusive),
        };
        let (upper, upper_inclusive) = match (self.upper, other.upper) {
            (Some(a), Some(b)) if a == b => {
                (Some(a), self.upper_inclusive && other.upper_inclusive)
            }
            (Some(a), Some(b)) if a < b => (Some(a), self.upper_inclusive),
            (Some(_), Some(b)) => (Some(b), other.upper_inclusive),
            (None, b) => (b, other.upper_inclusive),
            (a, None) => (a, self.upper_inclusive),
        };
        let result = Self {
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
        };
        (!result.is_empty()).then_some(result)
    }
}
impl std::fmt::Display for JavaRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{},{}{}",
            if self.lower_inclusive { "[" } else { "(" },
            self.lower.map(|v| v.to_string()).unwrap_or_default(),
            self.upper.map(|v| v.to_string()).unwrap_or_default(),
            if self.upper_inclusive { "]" } else { ")" }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaRequirement {
    pub range: JavaRange,
    pub recommended_component: Option<String>,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct JavaSelectionRequest {
    pub root: PathBuf,
    pub version_id: String,
    pub mode: JavaSelectionMode,
    pub version_range: String,
    pub specified_path: Option<PathBuf>,
    pub priority: Vec<PathBuf>,
    pub excluded: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum JavaSelectionResult {
    Selected {
        runtime: JavaRuntime,
        source: JavaSelectionMode,
        requirement: Option<JavaRequirement>,
        warnings: Vec<String>,
    },
    NeedsDownload {
        requirement: JavaRequirement,
        diagnostics: Vec<String>,
    },
}

pub fn effective_mode(
    mode: Option<JavaSelectionMode>,
    legacy_path: Option<&Path>,
) -> JavaSelectionMode {
    mode.unwrap_or(if legacy_path.is_some() {
        JavaSelectionMode::Specific
    } else {
        JavaSelectionMode::Automatic
    })
}

pub fn resolve_requirement(root: &Path, id: &str) -> Result<JavaRequirement> {
    requirement_from_metadata(&metadata::resolve_version(root, id)?)
}

fn numeric_version(text: &str) -> Option<(u32, u32, u32)> {
    let values: Vec<_> = text.split('.').collect();
    if !(2..=3).contains(&values.len()) {
        return None;
    }
    let numbers: Option<Vec<u32>> = values.into_iter().map(|p| p.parse().ok()).collect();
    let numbers = numbers?;
    Some((numbers[0], numbers[1], *numbers.get(2).unwrap_or(&0)))
}
fn loader_version(text: &str) -> Option<JavaVersion> {
    JavaVersion::parse(text.split('-').next()?).ok()
}

/// All inferred facts come from the resolved vanilla id/releaseTime/javaVersion
/// and exact Maven coordinates. Unknown loader constraints are not invented.
pub fn requirement_from_metadata(value: &Value) -> Result<JavaRequirement> {
    ensure!(value.is_object(), "版本元数据不是对象");
    let game_arguments = value.pointer("/arguments/game").and_then(Value::as_array);
    let game_property = |name: &str| {
        game_arguments.and_then(|args| {
            args.windows(2).find_map(|pair| {
                (pair[0].as_str() == Some(name))
                    .then(|| pair[1].as_str())
                    .flatten()
            })
        })
    };
    let vanilla = ["_pcl_jar_id", "inheritsFrom", "id"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .find_map(numeric_version)
        .or_else(|| game_property("--fml.mcVersion").and_then(numeric_version));
    let date = value
        .get("releaseTime")
        .and_then(Value::as_str)
        .and_then(|s| s.get(..10))
        .filter(|s| s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-');
    let mut requirement = JavaRequirement {
        range: JavaRange::all(),
        recommended_component: None,
        reasons: Vec::new(),
    };
    let v = |major| JavaVersion::new(major, 0, 0, 0);
    let mut add = |range: JavaRange, reason: &str| -> Result<()> {
        requirement.range = requirement.range.intersection(&range).with_context(|| {
            format!(
                "Java 要求冲突：{} 与 {range}（{reason}）；请在版本设置检查或指定范围",
                requirement.range
            )
        })?;
        requirement.reasons.push(reason.into());
        Ok(())
    };
    let known = vanilla.is_some_and(|(major, _, _)| major == 1);
    if vanilla.is_some_and(|version| version.0 == 1 && version >= (1, 20, 5))
        || (!known && date.is_some_and(|d| d >= "2024-04-02"))
    {
        add(
            JavaRange::at_least(v(21)),
            "Minecraft 1.20.5 / 24w14a 或更新版本至少 Java 21",
        )?;
    } else if vanilla.is_some_and(|(major, minor, _)| major == 1 && minor >= 18)
        || (!known && date.is_some_and(|d| d >= "2021-11-16"))
    {
        add(
            JavaRange::at_least(v(17)),
            "Minecraft 1.18 pre2 或更新版本至少 Java 17",
        )?;
    } else if vanilla.is_some_and(|(major, minor, _)| major == 1 && minor >= 17)
        || (!known && date.is_some_and(|d| d >= "2021-05-11"))
    {
        add(
            JavaRange::at_least(v(16)),
            "Minecraft 1.17 / 21w19a 或更新版本至少 Java 16",
        )?;
    } else if date.is_some_and(|d| d >= "2017-01-01")
        || vanilla.is_some_and(|(major, minor, _)| major == 1 && minor >= 12)
    {
        add(
            JavaRange::at_least(v(8)),
            "Minecraft 1.12 或更新版本至少 Java 8",
        )?;
    } else if date.is_some_and(|d| ("2001-01-01"..="2013-05-01").contains(&d)) {
        add(
            JavaRange::below(v(9)),
            "2013-05-01 及更早历史版本最高 Java 8",
        )?;
    }
    let major = value
        .pointer("/javaVersion/majorVersion")
        .or_else(|| value.get("java_version"))
        .map(|value| {
            value
                .as_u64()
                .context("版本元数据中的 Java 主版本必须是正整数")
        })
        .transpose()?;
    if let Some(major) = major {
        ensure!(
            (5..100).contains(&major),
            "元数据 Java 主版本不受支持：{major}"
        );
        if major >= 22 || (vanilla.is_none() && date.is_none()) {
            add(
                JavaRange::at_least(v(major as u32)),
                "版本元数据声明的 Java 最低要求",
            )?;
        }
    }
    let libraries: Vec<_> = value
        .get("libraries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .collect();
    let coordinate = |group: &str, artifact: &str| {
        libraries.iter().find_map(|name| {
            let p: Vec<_> = name.split(':').collect();
            (p.len() >= 3
                && p[0].eq_ignore_ascii_case(group)
                && p[1].eq_ignore_ascii_case(artifact))
            .then(|| p[2])
        })
    };
    let optifine = coordinate("optifine", "OptiFine").is_some();
    if let Some((1, minor, _)) = vanilla {
        if optifine {
            if minor < 7 || minor == 12 {
                add(JavaRange::below(v(9)), "此旧版 OptiFine 最高 Java 8")?;
            } else if (8..12).contains(&minor) {
                add(
                    JavaRange::at_least(v(8)),
                    "Minecraft 1.8–1.11 的 OptiFine 需要 Java 8",
                )?;
                add(JavaRange::below(v(9)), "此 OptiFine 不能使用 Java 9+")?;
            }
        }
    }
    if let Some(forge) =
        coordinate("net.minecraftforge", "forge").or_else(|| game_property("--fml.forgeVersion"))
    {
        let version = forge
            .rsplit_once('-')
            .map(|(_, version)| version)
            .unwrap_or(forge);
        let forge = loader_version(version);
        let minor = vanilla.filter(|v| v.0 == 1).map(|v| v.1);
        if vanilla.is_some_and(|mc| ((1, 6, 1)..=(1, 7, 2)).contains(&mc)) {
            add(JavaRange::at_least(v(7)), "Forge 1.6.1–1.7.2 需要 Java 7")?;
            add(JavaRange::below(v(8)), "此旧版 Forge 不兼容 Java 8+")?;
        } else if minor.is_none_or(|minor| minor <= 12) {
            add(JavaRange::below(v(9)), "Forge 1.12 及更早版本最高 Java 8")?;
        } else if minor.is_some_and(|minor| minor <= 14) {
            add(JavaRange::at_least(v(8)), "Forge 1.13–1.14 至少 Java 8")?;
            add(JavaRange::below(v(11)), "Forge 1.13–1.14 最高 Java 10")?;
        } else if minor == Some(15) {
            add(JavaRange::at_least(v(8)), "Forge 1.15 至少 Java 8")?;
            add(JavaRange::below(v(16)), "Forge 1.15 最高 Java 15")?;
        } else if forge.is_some_and(|f| {
            (JavaVersion::new(34, 0, 0, 0)..=JavaVersion::new(36, 2, 25, 0)).contains(&f)
        }) {
            add(
                JavaRange::at_most(JavaVersion::new(8, 0, 320, 0)),
                "Forge 34.0.0–36.2.25 最高 Java 8u320",
            )?;
        } else if forge.is_some_and(|f| f >= JavaVersion::new(36, 2, 26, 0) && f < v(37)) {
            add(JavaRange::below(v(24)), "Forge 36.2.26–36.x 最高 Java 23")?;
        } else if forge.is_some_and(|f| (v(37)..=JavaVersion::new(37, 0, 79, 0)).contains(&f)) {
            add(JavaRange::below(v(17)), "Forge 37.0.0–37.0.79 最高 Java 16")?;
        } else if minor == Some(18) && optifine {
            add(
                JavaRange::below(v(19)),
                "Forge + OptiFine 1.18 最高 Java 18",
            )?;
        } else if forge.is_some_and(|f| {
            (JavaVersion::new(45, 0, 21, 0)..=JavaVersion::new(45, 0, 65, 0)).contains(&f)
        }) {
            add(
                JavaRange::below(v(20)),
                "Forge 45.0.21–45.0.65 最高 Java 19",
            )?;
        } else if forge.is_some_and(|f| {
            (JavaVersion::new(45, 0, 66, 0)..=JavaVersion::new(47, 4, 8, 0)).contains(&f)
        }) {
            add(JavaRange::below(v(22)), "Forge 45.0.66–47.4.8 最高 Java 21")?;
        }
    }
    if let Some(neo) = coordinate("net.neoforged", "neoforge")
        .or_else(|| coordinate("net.neoforged", "forge"))
        .or_else(|| game_property("--fml.neoForgeVersion"))
    {
        if vanilla == Some((1, 20, 1))
            || (!neo.contains("25w14craftmine")
                && loader_version(neo).is_some_and(|f| f <= JavaVersion::new(20, 2, 62, 0)))
        {
            add(JavaRange::below(v(22)), "早期 NeoForge 最高 Java 21")?;
        }
    }
    if let Some(fabric) = coordinate("net.fabricmc", "fabric-loader") {
        if let Some((1, minor, _)) = vanilla {
            if (15..=16).contains(&minor) {
                add(JavaRange::at_least(v(8)), "Fabric 1.15–1.16 至少 Java 8")?;
            } else if minor >= 18 {
                add(JavaRange::at_least(v(17)), "Fabric 1.18+ 至少 Java 17")?;
            }
            if loader_version(fabric).is_some_and(|f| f < JavaVersion::new(0, 17, 0, 0)) {
                add(
                    JavaRange::below(v(25)),
                    "Fabric Loader 0.16.x 及更早版本不兼容 Java 25",
                )?;
            }
        }
    }
    if coordinate("com.mumfrey", "liteloader").is_some() && known {
        add(JavaRange::below(v(9)), "LiteLoader 最高 Java 8")?;
    }
    if major.is_some_and(|major| major >= 22) {
        requirement.recommended_component = value
            .pointer("/javaVersion/component")
            .or_else(|| value.get("java_component"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
    }
    Ok(requirement)
}

fn path_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

/// Refresh semantics: new discoveries first, then surviving saved entries in
/// their user-defined order. Removing an entry never deletes Java from disk.
pub fn merge_java_priority(
    discovered: &[JavaRuntime],
    previous: &[PathBuf],
    excluded: &[PathBuf],
) -> Vec<JavaRuntime> {
    let previous: Vec<_> = previous.iter().map(|p| path_key(p)).collect();
    let excluded: HashSet<_> = excluded.iter().map(|p| path_key(p)).collect();
    let mut fresh: Vec<_> = discovered
        .iter()
        .filter(|r| {
            !previous.contains(&path_key(&r.path)) && !excluded.contains(&path_key(&r.path))
        })
        .cloned()
        .collect();
    java::sort_java_candidates(&mut fresh);
    for path in previous {
        if excluded.contains(&path) {
            continue;
        }
        if let Some(runtime) = discovered.iter().find(|r| path_key(&r.path) == path) {
            fresh.push(runtime.clone());
        }
    }
    let mut seen = HashSet::new();
    fresh.retain(|r| seen.insert(path_key(&r.path)));
    fresh
}

pub fn select_java(
    request: &JavaSelectionRequest,
    candidates: &[JavaRuntime],
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<JavaSelectionResult> {
    select_with(
        request,
        candidates,
        platform,
        cancel,
        java::inspect_java_with_cancel,
        java::discover_java_with_cancel,
    )
}

fn select_with(
    request: &JavaSelectionRequest,
    candidates: &[JavaRuntime],
    platform: &Platform,
    cancel: &AtomicBool,
    mut inspect: impl FnMut(&Path, &AtomicBool) -> Result<JavaRuntime>,
    mut discover: impl FnMut(&AtomicBool) -> Result<java::JavaDiscovery>,
) -> Result<JavaSelectionResult> {
    crate::install::cancelled(cancel)?;
    metadata::validate_id(&request.version_id)?;
    if request.mode == JavaSelectionMode::Specific {
        let path = request
            .specified_path
            .as_ref()
            .context("尚未指定 Java 路径")?;
        ensure!(path.is_absolute(), "指定 Java 必须使用绝对路径");
        let runtime = inspect(path, cancel)?;
        java::validate_architecture(&runtime, platform)?;
        return Ok(JavaSelectionResult::Selected {
            runtime,
            source: request.mode,
            requirement: None,
            warnings: vec![],
        });
    }
    if request.mode == JavaSelectionMode::VersionFolder {
        let directory = metadata::confined_path(
            &request.root,
            &Path::new("versions").join(&request.version_id),
        )?;
        let found = java::discover_java_in(&directory, cancel)?;
        for runtime in found.runtimes {
            if java::validate_architecture(&runtime, platform).is_ok() {
                return Ok(JavaSelectionResult::Selected {
                    runtime,
                    source: request.mode,
                    requirement: None,
                    warnings: found.diagnostics,
                });
            }
        }
        bail!(
            "版本文件夹中没有可运行且架构匹配的 Java：{}",
            directory.display()
        );
    }
    let requirement = if request.mode == JavaSelectionMode::VersionRange {
        JavaRequirement {
            range: JavaRange::parse(&request.version_range)?,
            recommended_component: None,
            reasons: vec!["使用用户明确指定的 Java 版本范围".into()],
        }
    } else {
        resolve_requirement(&request.root, &request.version_id)?
    };
    let excluded: HashSet<_> = request.excluded.iter().map(|p| path_key(p)).collect();
    let mut ordered = request.priority.clone();
    let mut defaults = candidates.to_vec();
    java::sort_java_candidates(&mut defaults);
    ordered.extend(defaults.into_iter().map(|r| r.path));
    let mut diagnostics = Vec::new();
    let mut checked = HashSet::new();
    let mut failed = HashSet::new();
    let mut recovered = HashMap::new();
    for round in 0..2 {
        if round == 1 {
            let report = discover(cancel)?;
            diagnostics.extend(report.diagnostics);
            for runtime in &report.runtimes {
                let key = path_key(&runtime.path);
                if failed.contains(&key) {
                    checked.remove(&key);
                    // Discovery already probed this executable successfully.
                    // Reuse that result instead of spawning a third probe.
                    recovered.insert(key, runtime.clone());
                }
            }
            ordered = merge_java_priority(&report.runtimes, &request.priority, &request.excluded)
                .into_iter()
                .map(|r| r.path)
                .collect();
        }
        for path in &ordered {
            crate::install::cancelled(cancel)?;
            let key = path_key(path);
            if excluded.contains(&key) || !checked.insert(key.clone()) {
                continue;
            }
            match recovered
                .remove(&key)
                .map(Ok)
                .unwrap_or_else(|| inspect(path, cancel))
            {
                Ok(runtime) => {
                    if let Err(error) = java::validate_architecture(&runtime, platform) {
                        diagnostics.push(error.to_string());
                        continue;
                    }
                    ensure!(
                        runtime.version.major == runtime.major && runtime.major > 0,
                        "Java 完整版本与主版本不一致，请重新检查"
                    );
                    if requirement.range.contains(runtime.version) {
                        return Ok(JavaSelectionResult::Selected {
                            runtime,
                            source: request.mode,
                            requirement: Some(requirement),
                            warnings: diagnostics,
                        });
                    }
                    diagnostics.push(format!(
                        "Java {} 不在范围 {}：{}",
                        runtime.version,
                        requirement.range,
                        path.display()
                    ));
                }
                Err(error) => {
                    if error
                        .chain()
                        .any(|cause| cause.is::<crate::model::OperationCancelled>())
                    {
                        return Err(error);
                    }
                    failed.insert(key);
                    diagnostics.push(format!("{error:#}"));
                }
            }
        }
    }
    Ok(JavaSelectionResult::NeedsDownload {
        requirement,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn v(text: &str) -> JavaVersion {
        JavaVersion::parse(text).unwrap()
    }
    fn runtime(path: &str, version: &str) -> JavaRuntime {
        let version = v(version);
        JavaRuntime {
            path: path.into(),
            major: version.major,
            version,
            architecture: "aarch64".into(),
        }
    }
    fn platform() -> Platform {
        Platform {
            os: "osx".into(),
            arch: "aarch64".into(),
            version: "15".into(),
        }
    }
    fn request(root: &Path, mode: JavaSelectionMode) -> JavaSelectionRequest {
        JavaSelectionRequest {
            root: root.into(),
            version_id: "test".into(),
            mode,
            version_range: "[17.0.1,25.0)".into(),
            specified_path: None,
            priority: vec![],
            excluded: vec![],
        }
    }
    fn profile(root: &Path, value: Value) {
        std::fs::create_dir_all(root.join("versions/test")).unwrap();
        std::fs::write(root.join("versions/test/test.json"), value.to_string()).unwrap();
    }
    fn no_discovery(_: &AtomicBool) -> Result<java::JavaDiscovery> {
        panic!("must not search after successful/explicit selection")
    }
    #[test]
    fn official_range_examples_include_correct_patch_and_open_boundaries() {
        for (range, yes, no) in [
            (
                "[17.0.1, 25.0)",
                vec!["17.0.1", "21.0.7", "24.9"],
                vec!["17.0.0", "25"],
            ),
            ("(, 18.0)", vec!["8.0.442", "17.0.9"], vec!["18", "21"]),
            (
                "[8.0.81, 8.0.141]",
                vec!["8.0.81", "8.0.141"],
                vec!["8.0.80", "8.0.142", "17"],
            ),
            ("[21.0, )", vec!["21", "25"], vec!["17.0.15"]),
        ] {
            let range = JavaRange::parse(range).unwrap();
            for version in yes {
                assert!(range.contains(v(version)), "{range} {version}");
            }
            for version in no {
                assert!(!range.contains(v(version)), "{range} {version}");
            }
        }
        for invalid in [
            "",
            "[21,17)",
            "(21,21)",
            "[1.8,9)",
            "[8,17]",
            "[8,4)",
            "[100,101)",
            "[8,,17)",
            "17",
        ] {
            assert!(JavaRange::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn vanilla_dates_metadata_and_loader_constraints_match_known_upstream_rules() {
        for (id, libraries, yes, no) in [
            ("1.20.1", vec![], "21", "16"),
            ("1.20.5", vec![], "25", "17"),
            (
                "1.21.1",
                vec!["net.fabricmc:fabric-loader:0.16.14"],
                "24",
                "25",
            ),
            (
                "1.21.1",
                vec!["net.fabricmc:fabric-loader:0.19.5"],
                "25",
                "17",
            ),
            (
                "1.16.5",
                vec!["net.minecraftforge:forge:1.16.5-36.2.25"],
                "8.0.320",
                "8.0.321",
            ),
            (
                "1.16.5",
                vec!["net.minecraftforge:forge:1.16.5-36.2.26"],
                "23",
                "24",
            ),
            (
                "1.17.1",
                vec!["net.minecraftforge:forge:1.17.1-37.0.79"],
                "16",
                "17",
            ),
            (
                "1.19.4",
                vec!["net.minecraftforge:forge:1.19.4-45.0.65"],
                "19",
                "20",
            ),
            (
                "1.20.1",
                vec!["net.minecraftforge:forge:1.20.1-47.4.8"],
                "21",
                "22",
            ),
            (
                "1.20.2",
                vec!["net.neoforged:neoforge:20.2.62-beta"],
                "21",
                "22",
            ),
            (
                "1.7.2",
                vec!["net.minecraftforge:forge:1.7.2-10.12.2.1147"],
                "7",
                "8",
            ),
            (
                "1.8.9",
                vec!["optifine:OptiFine:1.8.9_HD_U_M5"],
                "8.0.442",
                "17",
            ),
            ("1.12.2", vec!["com.mumfrey:liteloader:1.12.2"], "8", "17"),
        ] {
            let value = json!({"id":id,"libraries":libraries.into_iter().map(|name|json!({"name":name})).collect::<Vec<_>>()});
            let requirement = requirement_from_metadata(&value).unwrap();
            assert!(
                requirement.range.contains(v(yes)),
                "{id} {} should allow {yes}",
                requirement.range
            );
            assert!(
                !requirement.range.contains(v(no)),
                "{id} {} should reject {no}",
                requirement.range
            );
        }
        let snapshot =
            requirement_from_metadata(&json!({"id":"24w14a","releaseTime":"2024-04-03T00:00:00Z"}))
                .unwrap();
        assert!(snapshot.range.contains(v("21")));
        assert!(!snapshot.range.contains(v("17")));
        let modern=requirement_from_metadata(&json!({"id":"26.3","javaVersion":{"majorVersion":25,"component":"java-runtime-epsilon"}})).unwrap();
        assert_eq!(
            modern.recommended_component.as_deref(),
            Some("java-runtime-epsilon")
        );
        assert!(modern.range.contains(v("25")));
        assert!(!modern.range.contains(v("21")));
        let neo=requirement_from_metadata(&json!({"id":"custom-instance","arguments":{"game":["--fml.mcVersion","1.20.2","--fml.neoForgeVersion","20.2.62-beta"]}})).unwrap();
        assert!(neo.range.contains(v("21")));
        assert!(!neo.range.contains(v("22")));
    }

    #[test]
    fn incompatible_inferred_constraints_are_errors_not_silently_replaced() {
        let value = json!({"id":"1.21.1","javaVersion":{"majorVersion":25},"libraries":[{"name":"net.fabricmc:fabric-loader:0.16.14"}]});
        assert!(requirement_from_metadata(&value)
            .unwrap_err()
            .to_string()
            .contains("冲突"));
    }

    #[test]
    fn range_overrides_metadata_and_refreshes_cached_runtime_version() {
        let root = tempfile::tempdir().unwrap();
        profile(
            root.path(),
            json!({"id":"test","javaVersion":{"majorVersion":25}}),
        );
        let request = request(root.path(), JavaSelectionMode::VersionRange);
        let cached = runtime("/fixture/java", "25");
        let selected = select_with(
            &request,
            &[cached],
            &platform(),
            &AtomicBool::new(false),
            |path, _| Ok(runtime(path.to_str().unwrap(), "21.0.12.1")),
            no_discovery,
        )
        .unwrap();
        assert!(
            matches!(selected,JavaSelectionResult::Selected{runtime:JavaRuntime{version,..},source:JavaSelectionMode::VersionRange,..} if version==v("21.0.12.1"))
        );
    }

    #[test]
    fn global_priority_and_exclusions_choose_first_real_compatible_candidate() {
        let root = tempfile::tempdir().unwrap();
        let mut request = request(root.path(), JavaSelectionMode::VersionRange);
        let old = runtime("/fixture/java8", "8.0.442");
        let chosen = runtime("/fixture/java17", "17.0.16");
        let omitted = runtime("/fixture/java21", "21.0.7");
        let all = [old, chosen.clone(), omitted];
        request.priority = vec![
            all[2].path.clone(),
            all[0].path.clone(),
            all[1].path.clone(),
        ];
        request.excluded = vec![all[2].path.clone()];
        let mut checked = Vec::new();
        let selected = select_with(
            &request,
            &all,
            &platform(),
            &AtomicBool::new(false),
            |path, _| {
                checked.push(path.to_path_buf());
                Ok(all.iter().find(|r| r.path == path).unwrap().clone())
            },
            no_discovery,
        )
        .unwrap();
        assert_eq!(checked, vec![all[0].path.clone(), all[1].path.clone()]);
        assert!(
            matches!(selected,JavaSelectionResult::Selected{runtime,..} if runtime.path==chosen.path)
        );
    }

    #[test]
    fn successful_refresh_recovers_failed_path_without_an_extra_probe() {
        let root = tempfile::tempdir().unwrap();
        let mut request = request(root.path(), JavaSelectionMode::VersionRange);
        let java = runtime("/fixture/recovered-java", "21.0.7");
        request.priority = vec![java.path.clone()];
        let mut inspections = 0;
        let mut discoveries = 0;
        let result = select_with(
            &request,
            std::slice::from_ref(&java),
            &platform(),
            &AtomicBool::new(false),
            |_, _| {
                inspections += 1;
                bail!("temporary first-probe failure")
            },
            |_| {
                discoveries += 1;
                Ok(java::JavaDiscovery {
                    runtimes: vec![java.clone()],
                    diagnostics: vec![],
                })
            },
        )
        .unwrap();
        assert!(
            matches!(result, JavaSelectionResult::Selected {runtime, ..} if runtime.path == java.path)
        );
        assert_eq!(inspections, 1, "reuse the successful discovery probe");
        assert_eq!(discoveries, 1, "do not add discovery rounds");
    }

    #[test]
    fn missing_compatible_java_returns_download_information_but_bad_metadata_does_not() {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path(), JavaSelectionMode::VersionRange);
        let missing = select_with(
            &request,
            &[],
            &platform(),
            &AtomicBool::new(false),
            |_, _| panic!("no candidate"),
            |_| Ok(java::JavaDiscovery::default()),
        )
        .unwrap();
        assert!(matches!(missing, JavaSelectionResult::NeedsDownload { .. }));
        let request = JavaSelectionRequest {
            mode: JavaSelectionMode::Automatic,
            ..request
        };
        assert!(select_with(
            &request,
            &[],
            &platform(),
            &AtomicBool::new(false),
            |_, _| panic!("bad metadata"),
            no_discovery
        )
        .is_err());
        for invalid in [json!("21"), json!(-1), json!(0), json!(null)] {
            profile(
                root.path(),
                json!({"id":"test","javaVersion":{"majorVersion":invalid}}),
            );
            assert!(select_with(
                &request,
                &[],
                &platform(),
                &AtomicBool::new(false),
                |_, _| panic!("invalid metadata must fail before Java inspection"),
                no_discovery
            )
            .is_err());
        }
    }

    #[test]
    fn specific_path_never_falls_back_or_obeys_automatic_range_but_still_checks_architecture() {
        let root = tempfile::tempdir().unwrap();
        let mut request = request(root.path(), JavaSelectionMode::Specific);
        request.specified_path = Some(root.path().join("manual"));
        request.priority = vec!["/fixture/other".into()];
        let result = select_with(
            &request,
            &[],
            &platform(),
            &AtomicBool::new(false),
            |_, _| Ok(runtime("/fixture/manual", "8.0.81")),
            no_discovery,
        )
        .unwrap();
        assert!(matches!(
            result,
            JavaSelectionResult::Selected {
                requirement: None,
                runtime: JavaRuntime { major: 8, .. },
                ..
            }
        ));
        assert!(select_with(
            &request,
            &[],
            &platform(),
            &AtomicBool::new(false),
            |_, _| bail!("not runnable"),
            no_discovery
        )
        .is_err());
        assert!(select_with(
            &request,
            &[],
            &platform(),
            &AtomicBool::new(false),
            |_, _| {
                let mut r = runtime("/fixture/manual", "21");
                r.architecture = "x86_64".into();
                Ok(r)
            },
            no_discovery
        )
        .is_err());
    }

    #[test]
    fn refresh_preserves_existing_user_order_and_puts_new_discoveries_first() {
        let discovered = vec![
            runtime("/fixture/old17", "17"),
            runtime("/fixture/new25", "25"),
            runtime("/fixture/old21", "21"),
            runtime("/fixture/excluded", "21"),
        ];
        let merged = merge_java_priority(
            &discovered,
            &[discovered[0].path.clone(), discovered[2].path.clone()],
            &[discovered[3].path.clone()],
        );
        assert_eq!(
            merged.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
            vec![
                discovered[1].path.clone(),
                discovered[0].path.clone(),
                discovered[2].path.clone()
            ]
        );
    }

    #[test]
    fn cancellation_does_not_return_needs_download_or_run_more_candidates() {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path(), JavaSelectionMode::VersionRange);
        let error = select_with(
            &request,
            &[runtime("/fixture/java", "21")],
            &platform(),
            &AtomicBool::new(false),
            |_, _| Err(crate::model::OperationCancelled.into()),
            no_discovery,
        )
        .unwrap_err();
        assert!(error
            .chain()
            .any(|e| e.is::<crate::model::OperationCancelled>()));
        assert_eq!(
            effective_mode(None, Some(Path::new("/java"))),
            JavaSelectionMode::Specific
        );
        assert_eq!(
            effective_mode(Some(JavaSelectionMode::Automatic), Some(Path::new("/java"))),
            JavaSelectionMode::Automatic
        );
    }
}
