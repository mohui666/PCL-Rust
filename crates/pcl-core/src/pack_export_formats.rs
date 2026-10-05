use super::*;
use serde_json::json;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum PackFormat {
    #[default]
    Mrpack,
    MultiMc,
    Hmcl,
    Mcbbs,
}
impl PackFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mrpack => "Modrinth (.mrpack)",
            Self::MultiMc => "MultiMC / Prism (.zip)",
            Self::Hmcl => "HMCL (.zip)",
            Self::Mcbbs => "MCBBS (.zip)",
        }
    }
    pub fn extension(self) -> &'static str {
        if self == Self::Mrpack {
            "mrpack"
        } else {
            "zip"
        }
    }
    pub(super) fn payload_prefix(self) -> &'static str {
        match self {
            Self::Mrpack | Self::Mcbbs => "overrides/",
            Self::MultiMc => ".minecraft/",
            Self::Hmcl => "minecraft/",
        }
    }
}

pub(super) fn manifests(
    index: &PackIndex<'_>,
    format: PackFormat,
) -> Result<Vec<(String, Vec<u8>)>> {
    let minecraft = &index.dependencies["minecraft"];
    let single = |name: &str, value: serde_json::Value| -> Result<_> {
        Ok(vec![(name.into(), serde_json::to_vec_pretty(&value)?)])
    };
    match format {
        PackFormat::Mrpack => single("modrinth.index.json", serde_json::to_value(index)?),
        PackFormat::MultiMc => {
            let mut components = Vec::new();
            for (key, version) in &index.dependencies {
                let uid = match key.as_str() {
                    "minecraft" => "net.minecraft",
                    "forge" => "net.minecraftforge",
                    "neoforge" => "net.neoforged",
                    "fabric-loader" => "net.fabricmc.fabric-loader",
                    "quilt-loader" => "org.quiltmc.quilt-loader",
                    _ => bail!("MMC 导出不支持依赖：{key}"),
                };
                components.push(json!({"uid":uid,"version":version,"important":key=="minecraft"}));
            }
            let mut files = single(
                "mmc-pack.json",
                json!({"formatVersion":1,"components":components}),
            )?;
            // Export only descriptive fields; no commands, absolute Java paths or accounts.
            files.push((
                "instance.cfg".into(),
                format!("InstanceType=OneSix\nname={}\n", index.name).into_bytes(),
            ));
            Ok(files)
        }
        PackFormat::Hmcl => {
            ensure!(
                index.dependencies.len() == 1,
                "HMCL 导出目前仅支持原版；加载器版本请选 MMC、MCBBS 或 mrpack"
            );
            single(
                "modpack.json",
                json!({"name":index.name,"version":index.version_id,"description":index.summary,"gameVersion":minecraft,"formatVersion":1}),
            )
        }
        PackFormat::Mcbbs => {
            let mut addons = Vec::new();
            for (key, version) in &index.dependencies {
                let id = match key.as_str() {
                    "minecraft" => "game",
                    "forge" => "forge",
                    "neoforge" => "neoforge",
                    "fabric-loader" => "fabric",
                    "quilt-loader" => "quilt",
                    _ => bail!("MCBBS 导出不支持依赖：{key}"),
                };
                addons.push(json!({"id":id,"version":version}));
            }
            single(
                "mcbbs.packmeta",
                json!({"manifestType":"minecraftModpack","manifestVersion":2,"name":index.name,"version":index.version_id,"description":index.summary,"addons":addons,"files":[]}),
            )
        }
    }
}
