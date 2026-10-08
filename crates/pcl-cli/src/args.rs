use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "pcl-cli",
    version,
    about = "PCL Rust 无头命令行；结果为 JSON，进度写入 stderr"
)]
pub(crate) struct Cli {
    /// Minecraft 根目录（绝对路径）
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
    /// 独立设置文件；不指定时读取桌面端设置
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// 显式无头入口（所有子命令本来就不创建启动器窗口）
    #[arg(long, global = true)]
    pub headless: bool,
    /// stderr 也使用 JSON Lines；stdout 始终只有最终 JSON
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Action,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Action {
    /// 当前平台、目录、Java 和已安装版本
    Doctor,
    /// 本地游戏版本
    List,
    /// Mojang 官方游戏版本清单
    Manifest,
    /// 安装并校验原版游戏
    Install { version: String },
    /// 修复游戏、资源、支持库和原生库
    Repair {
        version: String,
        #[arg(long)]
        java: Option<PathBuf>,
    },
    /// 查询六类加载器版本
    Loaders { kind: Loader, minecraft: String },
    /// 安装加载器及原版依赖
    InstallLoader {
        kind: Loader,
        minecraft: String,
        loader: String,
        /// Forge/NeoForge/OptiFine 安装器使用的 Java
        #[arg(long)]
        java: Option<PathBuf>,
        /// LiteLoader / OptiFine 的已安装父版本（合法组合）
        #[arg(long)]
        parent: Option<String>,
    },
    /// 本地 Mod 列表（兼容旧 CLI）
    Mods { version: String },
    /// Mod 导入、启停、移除、恢复和更新
    Mod {
        #[command(subcommand)]
        command: ModAction,
    },
    /// 搜索 Modrinth / CurseForge 资源
    Search {
        query: String,
        #[arg(long, default_value = "mod")]
        kind: Kind,
        #[arg(long, default_value = "modrinth")]
        provider: Provider,
        #[arg(long)]
        minecraft: Option<String>,
        #[arg(long)]
        loader: Option<String>,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// 项目、版本、资源下载及带依赖安装
    Resource {
        #[command(subcommand)]
        command: ResourceAction,
    },
    /// Java 检测及 Mojang 官方运行时下载
    Java {
        #[command(subcommand)]
        command: JavaAction,
    },
    /// 检查 mrpack / CurseForge / MMC / HMCL / MCBBS / 游戏 ZIP
    InspectPack { pack: PathBuf },
    /// 将本地整合包安装到新实例
    InstallPack {
        pack: PathBuf,
        instance: String,
        #[arg(long)]
        optional: bool,
        #[arg(long)]
        java: Option<PathBuf>,
    },
    /// 导出整合包；可用 JSON 选项选择世界、资源和附带 Java
    ExportPack {
        version: String,
        output: PathBuf,
        #[arg(long)]
        options: Option<PathBuf>,
        #[arg(long, default_value = "mrpack")]
        format: PackFormat,
        /// 附带指定启动器，不默认附带当前命令行程序
        #[arg(long)]
        launcher: Option<PathBuf>,
    },
    /// 输出脱敏启动计划，不启动游戏
    Plan(LaunchArgs),
    /// 导出脱敏启动脚本；扩展名使用 .command 或 .bat
    ExportScript {
        #[command(flatten)]
        launch: LaunchArgs,
        output: PathBuf,
    },
    /// 启动游戏并等待退出；无启动器窗口，游戏本身仍需图形环境
    Launch(LaunchArgs),
    /// 列出已保存的账户（不含凭据）
    Accounts,
    /// 设备码登录；终端输出地址和验证码，不自动打开浏览器
    Login {
        #[arg(long)]
        client_id: Option<String>,
    },
    /// 删除指定账户在本机保存的凭据
    Logout { account: String },
    /// 版本元数据、实例设置与重命名
    Instance {
        #[command(subcommand)]
        command: InstanceAction,
    },
    /// 脱敏查看设置，或从 JSON 文件校验并应用设置
    Settings {
        #[arg(long)]
        apply: Option<PathBuf>,
    },
    /// 分析当前实例或指定文件的日志
    Logs {
        #[arg(long, required_unless_present = "file", conflicts_with = "file")]
        instance: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        export: Option<PathBuf>,
    },
}

#[derive(Args, Debug)]
pub(crate) struct LaunchArgs {
    pub version: String,
    #[arg(long)]
    pub java: Option<PathBuf>,
    /// 已停用；离线名称不能用于登录或启动
    #[arg(long, conflicts_with = "account", hide = true)]
    pub name: Option<String>,
    /// 已保存的微软账户 ID；启动、预览和导出必须指定
    #[arg(long)]
    pub account: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(256..=262144))]
    pub memory: Option<u32>,
    #[arg(long)]
    pub server: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ModAction {
    List {
        instance: String,
    },
    Import {
        instance: String,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    Enable {
        instance: String,
        file: String,
    },
    Disable {
        instance: String,
        file: String,
    },
    /// 可恢复移除，输出备份路径
    Remove {
        instance: String,
        #[arg(required = true)]
        files: Vec<String>,
    },
    Restore {
        instance: String,
        backup: PathBuf,
    },
    /// 默认仅查询，--apply 才执行；--file 可重复以选择更新项
    Updates {
        instance: String,
        #[arg(long)]
        apply: bool,
        #[arg(long = "file", requires = "apply")]
        files: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ResourceAction {
    Project {
        project: String,
    },
    Versions {
        project: String,
        #[arg(long, default_value = "mod")]
        kind: Kind,
        #[arg(long, default_value = "")]
        minecraft: String,
        #[arg(long, default_value = "")]
        loader: String,
    },
    /// 下载单个资源，不安装；CurseForge 版本用 cf:项目ID:文件ID
    Download {
        version: String,
        output: PathBuf,
        #[arg(long, default_value = "mod")]
        kind: Kind,
    },
    /// 安装到实例，自动解析必需依赖并检查游戏/加载器兼容性
    Install {
        version: String,
        instance: String,
        #[arg(long, default_value = "mod")]
        kind: Kind,
        /// 数据包所在的世界目录名
        #[arg(long)]
        world: Option<String>,
        /// 只输出依赖计划；整合包不支持此选项
        #[arg(long)]
        dry_run: bool,
        /// 整合包安装时包含可选文件
        #[arg(long)]
        optional: bool,
        #[arg(long)]
        java: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum JavaAction {
    List,
    Inspect {
        path: PathBuf,
    },
    Runtimes,
    /// 按 runtimes 输出的 component 名称安装
    Install {
        component: String,
        #[arg(long)]
        directory: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum InstanceAction {
    Show {
        version: String,
    },
    Settings {
        version: String,
        #[arg(long)]
        apply: Option<PathBuf>,
    },
    Rename {
        version: String,
        new_name: String,
    },
    /// 默认预览；--apply 移入系统废纸篓/回收站，保留共享资源
    Trash {
        version: String,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub(crate) enum Loader {
    Fabric,
    Quilt,
    Forge,
    Neoforge,
    Liteloader,
    Optifine,
}
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub(crate) enum Kind {
    Mod,
    Modpack,
    #[value(name = "resourcepack", alias = "resource-pack")]
    ResourcePack,
    Shader,
    #[value(name = "datapack", alias = "data-pack")]
    DataPack,
}
impl From<Kind> for pcl_core::resources::ResourceKind {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Mod => Self::Mod,
            Kind::Modpack => Self::Modpack,
            Kind::ResourcePack => Self::ResourcePack,
            Kind::Shader => Self::Shader,
            Kind::DataPack => Self::DataPack,
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Provider {
    Modrinth,
    Curseforge,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum PackFormat {
    Mrpack,
    Multimc,
    Hmcl,
    Mcbbs,
}
impl From<PackFormat> for pcl_core::pack_export::PackFormat {
    fn from(value: PackFormat) -> Self {
        match value {
            PackFormat::Mrpack => Self::Mrpack,
            PackFormat::Multimc => Self::MultiMc,
            PackFormat::Hmcl => Self::Hmcl,
            PackFormat::Mcbbs => Self::Mcbbs,
        }
    }
}
