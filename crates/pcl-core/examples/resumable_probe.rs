//! Explicit live transfer check in a temporary directory; no launcher settings or games are changed.
use anyhow::{Context, Result};
use pcl_core::{
    install,
    resumable::{self, Checksum, TransferEvent},
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
fn main() -> Result<()> {
    let manifest = install::fetch_manifest()?;
    let version = manifest["versions"]
        .as_array()
        .context("versions")?
        .iter()
        .find(|v| v["id"] == "1.21.1")
        .context("1.21.1")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(40))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let raw = client
        .get(version["url"].as_str().context("version url")?)
        .send()?
        .error_for_status()?
        .bytes()?;
    use sha1::Digest;
    anyhow::ensure!(
        format!("{:x}", sha1::Sha1::digest(&raw))
            == version["sha1"].as_str().context("version hash")?,
        "version metadata checksum"
    );
    let metadata: serde_json::Value = serde_json::from_slice(&raw)?;
    let library = metadata["libraries"]
        .as_array()
        .context("libraries")?
        .iter()
        .find(|l| {
            l["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("com.google.code.gson:gson:"))
        })
        .context("gson")?;
    let artifact = &library["downloads"]["artifact"];
    let url = artifact["url"].as_str().context("artifact URL")?;
    anyhow::ensure!(
        url.starts_with("https://libraries.minecraft.net/"),
        "official artifact only"
    );
    let checksum = Checksum {
        size: artifact["size"].as_u64().context("size")?,
        sha1: Some(artifact["sha1"].as_str().context("sha1")?.into()),
        sha256: None,
        sha512: None,
    };
    let requests = Mutex::new(Vec::new());
    let transferred = AtomicUsize::new(0);
    let fetch = |range: Option<(u64, u64)>| -> Result<_> {
        let mut request = client.get(url).header("Accept-Encoding", "identity");
        if let Some((a, b)) = range {
            request = request.header("Range", format!("bytes={a}-{b}"));
        }
        let response = request.send()?.error_for_status()?;
        requests
            .lock()
            .unwrap()
            .push(serde_json::json!({"range":range,"status":response.status().as_u16()}));
        Ok(response)
    };
    let temporary = tempfile::tempdir()?;
    let target = temporary.path().join("gson.jar");
    let cache = temporary.path().join("cache");
    let cancel = AtomicBool::new(false);
    let result = resumable::download(&target, &cache, &checksum, &cancel, fetch, |event| {
        if let TransferEvent::Bytes(n) = event {
            transferred.fetch_add(n, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed)
        }
    });
    anyhow::ensure!(
        result
            .unwrap_err()
            .is::<pcl_core::model::OperationCancelled>(),
        "cancelled"
    );
    anyhow::ensure!(!target.exists(), "unverified target must remain absent");
    resumable::download(
        &target,
        &cache,
        &checksum,
        &AtomicBool::new(false),
        fetch,
        |event| {
            if let TransferEvent::Bytes(n) = event {
                transferred.fetch_add(n, Ordering::Relaxed);
            }
        },
    )?;
    let output = std::fs::read(target)?;
    anyhow::ensure!(
        Some(format!("{:x}", sha1::Sha1::digest(&output))) == checksum.sha1,
        "download hash"
    );
    println!(
        "{}",
        serde_json::json!({"artifact":library["name"],"expected_size":checksum.size,"network_bytes":transferred.load(Ordering::Relaxed),"cancel_preserved_target":true,"requests":requests.into_inner().unwrap(),"sha1_verified":true,"temporary_only":true})
    );
    Ok(())
}
