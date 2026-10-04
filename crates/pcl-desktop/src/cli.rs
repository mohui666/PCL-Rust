use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pcl_core::{
    auth, config, install, java,
    launch::{self, LaunchOptions},
    loaders, metadata,
    model::Platform,
    modpack, mods, packs,
};
use std::{path::PathBuf, sync::atomic::AtomicBool};

#[derive(Parser)]
#[command(
    name = "pcl-desktop",
    version,
    about = "PCL Rust 第三方重构版；无参数运行桌面窗口"
)]
struct Cli {
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// 显示操作系统、游戏目录、Java 和本地版本诊断
    Doctor,
    /// 列出本地版本（只读）
    List,
    /// 获取 Mojang 实时版本列表
    Manifest,
    /// 安装原版游戏并校验文件
    Install { version: String },
    /// 查询官方 Fabric/Quilt 加载器版本
    Loaders { kind: String, minecraft: String },
    /// 安装官方 Fabric/Quilt profile 与依赖
    InstallLoader {
        kind: String,
        minecraft: String,
        loader: String,
    },
    /// 只读列出某实例的本地 Mod
    Mods { version: String },
    /// 查看 Modrinth 整合包依赖和文件数量
    InspectPack { pack: PathBuf },
    /// 完整安装 mrpack 到不重名的新实例
    InstallPack {
        pack: PathBuf,
        instance: String,
        #[arg(long)]
        optional: bool,
    },
    /// 输出脱敏的启动命令；不启动游戏
    Plan {
        version: String,
        #[arg(long)]
        java: PathBuf,
        #[arg(long, default_value = "Player")]
        name: String,
        #[arg(long, default_value_t = 4096)]
        memory: u32,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let settings = config::load_settings(&config::settings_path())?;
    let root = cli.root.unwrap_or(settings.game_root);
    anyhow::ensure!(root.is_absolute(), "--root 必须是绝对路径");
    match cli.command {
        Action::Doctor => {
            let runtimes: Vec<_> = java::discover_java().iter().map(|j| serde_json::json!({ "path":j.path, "major":j.major, "architecture":j.architecture })).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"platform":Platform::current(), "root": root, "java":runtimes,"versions":metadata::list_installed(&root)?})
                )?
            );
        }
        Action::List => println!(
            "{}",
            serde_json::to_string_pretty(&metadata::list_installed(&root)?)?
        ),
        Action::Manifest => println!(
            "{}",
            serde_json::to_string_pretty(&install::fetch_manifest()?)?
        ),
        Action::Install { version } => {
            install::install_version(
                &root,
                &version,
                &Platform::current(),
                &AtomicBool::new(false),
                |p| eprintln!("{} / {}  {}", p.completed, p.total, p.message),
            )?;
            println!("已安装并校验 {version}");
        }
        Action::Loaders { kind, minecraft } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&loaders::list_loader_versions(
                    loader_kind(&kind)?,
                    &minecraft,
                    &AtomicBool::new(false)
                )?)?
            );
        }
        Action::InstallLoader {
            kind,
            minecraft,
            loader,
        } => {
            let id = loaders::install_loader(
                &root,
                loader_kind(&kind)?,
                &minecraft,
                &loader,
                &Platform::current(),
                &AtomicBool::new(false),
                |p| eprintln!("{} / {}  {}", p.completed, p.total, p.message),
            )?;
            println!("已安装并校验 {id}");
        }
        Action::Mods { version } => {
            metadata::validate_id(&version)?;
            let instance =
                metadata::confined_path(&root, &PathBuf::from("instances").join(version))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&mods::list_mods(&instance)?)?
            );
        }
        Action::InspectPack { pack } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&modpack::inspect_mrpack(&pack)?)?
            );
        }
        Action::InstallPack {
            pack,
            instance,
            optional,
        } => {
            let id = packs::install_pack(
                &root,
                &pack,
                &instance,
                optional,
                &Platform::current(),
                &AtomicBool::new(false),
                |p| eprintln!("{} / {}  {}", p.completed, p.total, p.message),
            )?;
            println!("已安装整合包 {id}");
        }
        Action::Plan {
            version,
            java,
            name,
            memory,
        } => {
            let runtime = java::inspect_java(&java).context("Java 检测失败")?;
            let platform = Platform::current();
            // --java is an explicit runtime choice, like the GUI's Specific mode.
            // Inspect the executable and native architecture without overriding
            // that choice with the automatic metadata version range.
            java::validate_architecture(&runtime, &platform)?;
            let options = LaunchOptions {
                root,
                version_id: version,
                java: runtime.path,
                memory_mb: memory,
                width: 1100,
                height: 700,
            };
            let plan = launch::build_plan_with_explicit_java(
                &options,
                &auth::offline_session(&name)?,
                &platform,
            )?;
            println!("{}", plan.redacted_command());
        }
    }
    Ok(())
}

fn loader_kind(value: &str) -> Result<loaders::LoaderKind> {
    match value.to_ascii_lowercase().as_str() {
        "fabric" => Ok(loaders::LoaderKind::Fabric),
        "quilt" => Ok(loaders::LoaderKind::Quilt),
        _ => anyhow::bail!("加载器必须是 fabric 或 quilt"),
    }
}
