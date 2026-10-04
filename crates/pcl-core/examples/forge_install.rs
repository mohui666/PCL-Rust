//! Explicit live smoke test: cargo run -p pcl-core --example forge_install -- <isolated-root> <java> <forge|neoforge> <mc> <loader>
use anyhow::{Context, Result};
use pcl_core::{
    forge::{install_forge, ForgeKind},
    model::Platform,
};
use std::{path::Path, sync::atomic::AtomicBool};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        anyhow::bail!("usage: forge_install <isolated-root> <java> <forge|neoforge> <mc> <loader>");
    }
    let kind = match args[3].as_str() {
        "forge" => ForgeKind::Forge,
        "neoforge" => ForgeKind::NeoForge,
        _ => anyhow::bail!("unknown loader"),
    };
    let id = install_forge(
        Path::new(&args[1]),
        kind,
        &args[4],
        &args[5],
        Path::new(&args[2]),
        &Platform::current(),
        &AtomicBool::new(false),
        |p| eprintln!("{}/{} {}", p.completed, p.total, p.message),
    )
    .context("live official processor install")?;
    println!("Installed and verified: {id}");
    Ok(())
}
