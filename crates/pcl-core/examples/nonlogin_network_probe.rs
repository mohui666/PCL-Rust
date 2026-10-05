//! Explicit, read-only live checks. No settings, games or credentials are modified.
use pcl_core::{install, loaders, network, resources};
use std::sync::atomic::AtomicBool;
fn main() -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    let mut failed = false;
    for source in [
        network::SourcePreference::OfficialOnly,
        network::SourcePreference::MirrorFirst,
    ] {
        network::configure(&network::DownloadOptions {
            version_source: source,
            ..Default::default()
        })?;
        let result=install::fetch_manifest_with_cancel(&cancel).map(|v|serde_json::json!({"versions":v["versions"].as_array().map(Vec::len),"release":v["latest"]["release"]}));
        report(&format!("manifest-{source:?}"), result, &mut failed);
    }
    report(
        "fabric-list",
        loaders::list_loader_versions(loaders::LoaderKind::Fabric, "1.21.1", &cancel)
            .map(|v| serde_json::json!({"count":v.len()})),
        &mut failed,
    );
    report(
        "optifine-official-list",
        loaders::optifine::list_versions("1.21.1", &cancel)
            .map(|v| serde_json::json!({"count":v.len()})),
        &mut failed,
    );
    let request = resources::SearchOptions {
        sort: Default::default(),
        provider: resources::ResourceProvider::Modrinth,
        query: "钠".into(),
        minecraft: Some("1.21.1".into()),
        loader: Some("fabric".into()),
        offset: 0,
        limit: 5,
    };
    report("modrinth-chinese-search",resources::search_mods(&request,&cancel).map(|v|serde_json::json!({"count":v.hits.len(),"projects":v.hits.iter().map(|h|&h.slug).collect::<Vec<_>>()})),&mut failed);
    anyhow::ensure!(!failed, "至少一项实时探测失败；见逐项记录");
    Ok(())
}
fn report(label: &str, result: anyhow::Result<serde_json::Value>, failed: &mut bool) {
    let value = match result {
        Ok(v) => serde_json::json!({"check":label,"ok":true,"result":v}),
        Err(e) => {
            *failed = true;
            serde_json::json!({"check":label,"ok":false,"error":format!("{e:#}")})
        }
    };
    println!("{value}");
}
