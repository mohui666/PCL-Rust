//! Recognize declarative launcher metadata. Never reuse arbitrary launch arguments,
//! repository URLs or executable hooks supplied by an imported pack.
use crate::metadata::validate_id;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn components(profile: &Value, minecraft: &str) -> Result<BTreeMap<String, String>> {
    validate_id(minecraft)?;
    let mut result = BTreeMap::<String, String>::new();
    for library in profile["libraries"].as_array().into_iter().flatten() {
        let Some(name) = library["name"].as_str() else {
            continue;
        };
        let parts = name.split(':').collect::<Vec<_>>();
        if parts.len() < 3 {
            continue;
        }
        let (key, version) = match (parts[0], parts[1]) {
            ("net.fabricmc", "fabric-loader") => ("fabric-loader", parts[2]),
            ("org.quiltmc", "quilt-loader") => ("quilt-loader", parts[2]),
            ("net.minecraftforge", "forge") => (
                "forge",
                parts[2]
                    .strip_prefix(&format!("{minecraft}-"))
                    .context("Forge 与包声明的 Minecraft 版本不符")?,
            ),
            ("net.neoforged", "neoforge") => ("neoforge", parts[2]),
            ("net.neoforged", "forge") => (
                "neoforge",
                parts[2]
                    .strip_prefix(&format!("{minecraft}-"))
                    .context("NeoForge 与 Minecraft 不符")?,
            ),
            ("com.mumfrey", "liteloader") => ("liteloader", parts[2]),
            ("optifine", "OptiFine") => (
                "optifine",
                parts[2]
                    .strip_prefix(&format!("{minecraft}_"))
                    .context("OptiFine 与 Minecraft 不符")?,
            ),
            _ => continue,
        };
        validate_id(version)?;
        if let Some(previous) = result.insert(key.into(), version.into()) {
            ensure!(previous == version, "整合包重复声明冲突的 {key} 版本");
        }
    }
    // Modern Forge profiles may contain only the bootstrap classpath.
    let arguments = profile.pointer("/arguments/game").and_then(Value::as_array);
    let legacy_neoforge = result.contains_key("neoforge")
        || arguments.is_some_and(|values| {
            values.windows(2).any(|pair| {
                pair[0].as_str() == Some("--fml.forgeGroup")
                    && pair[1].as_str() == Some("net.neoforged")
            })
        });
    for (flag, key) in [
        (
            "--fml.forgeVersion",
            if legacy_neoforge { "neoforge" } else { "forge" },
        ),
        ("--fml.neoForgeVersion", "neoforge"),
    ] {
        for pair in arguments
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .windows(2)
        {
            if pair[0].as_str() == Some(flag) {
                let version = pair[1].as_str().context("加载器版本参数无效")?;
                validate_id(version)?;
                if let Some(previous) = result.insert(key.into(), version.into()) {
                    ensure!(previous == version, "加载器声明冲突");
                }
            }
        }
    }
    let primary = result
        .keys()
        .filter(|key| {
            matches!(
                key.as_str(),
                "fabric-loader" | "quilt-loader" | "forge" | "neoforge"
            )
        })
        .count();
    ensure!(primary <= 1, "整合包含有冲突的主加载器");
    if result.is_empty() {
        if let Some(main) = profile["mainClass"].as_str() {
            ensure!(
                matches!(
                    main,
                    "net.minecraft.client.main.Main" | "net.minecraft.client.Minecraft"
                ),
                "无法识别整合包的自定义启动类 {main}"
            );
        }
    }
    Ok(result)
}

pub(crate) fn hmcl_profile(dependencies: &BTreeMap<String, String>, id: &str) -> Result<Value> {
    let minecraft = dependencies
        .get("minecraft")
        .context("缺少 Minecraft 版本")?;
    let mut libraries = Vec::new();
    let mut main = "net.minecraft.client.main.Main";
    for (key, version) in dependencies {
        let name = match key.as_str() {
            "minecraft" => continue,
            "fabric-loader" => format!("net.fabricmc:fabric-loader:{version}"),
            "quilt-loader" => format!("org.quiltmc:quilt-loader:{version}"),
            "forge" => format!("net.minecraftforge:forge:{minecraft}-{version}"),
            "neoforge" => format!("net.neoforged:neoforge:{version}"),
            "optifine" => format!("optifine:OptiFine:{minecraft}_{version}"),
            "liteloader" => format!("com.mumfrey:liteloader:{version}"),
            _ => bail!("HMCL 导出不支持依赖 {key}"),
        };
        main = "net.minecraft.launchwrapper.Launch";
        libraries.push(json!({"name":name}));
    }
    // HMCL uses pack.json for component recognition and downloads canonical
    // profiles itself. It does not launch this stripped descriptor directly.
    Ok(
        json!({"id":id,"jar":minecraft,"inheritsFrom":minecraft,"mainClass":main,"libraries":libraries}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_neoforge_uses_its_publisher_group_despite_forge_argument_name() {
        let mut profile = json!({
            "mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
            "libraries":[{"name":"net.neoforged:forge:1.20.1-47.1.106:universal"}],
            "arguments":{"game":["--fml.forgeVersion","47.1.106","--fml.mcVersion","1.20.1","--fml.forgeGroup","net.neoforged"]}
        });
        let expected = BTreeMap::from([("neoforge".into(), "47.1.106".into())]);
        assert_eq!(components(&profile, "1.20.1").unwrap(), expected);
        profile["libraries"] = json!([]);
        assert_eq!(components(&profile, "1.20.1").unwrap(), expected);
        profile["libraries"] = json!([{"name":"net.minecraftforge:forge:1.20.1-47.1.106"}]);
        assert!(components(&profile, "1.20.1").is_err());
    }
}
