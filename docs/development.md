# 源码开发与本地构建

[返回项目首页](../README.md) · [验证记录](validation.md)

本文对应 **2026-10-05 发布的构建源码**。仓库提供 `crates/`、Cargo 工作区与锁文件、`scripts/`、必要桌面资产及 CI；可克隆后按下面步骤在本机构建。应用发行包、游戏数据、原始验证记录和本机派生字体不随源码提供。

## 工作区结构

| 仓库或本地生成路径 | 职责 |
| --- | --- |
| `crates/pcl-core/` | 配置、版本与启动计划、下载与校验、Java、账号、加载器、资源与整合包处理 |
| `crates/pcl-desktop/` | egui 原生窗口、页面、消息框、提示、任务进度与交互 |
| `scripts/` | 本机字体处理、macOS 与 Windows 打包入口 |
| `docs/` | 公开迁移矩阵、验证摘要、登录与来源说明 |
| `crates/pcl-desktop/assets/` | 构建所需资产及来源记录 |
| `.github/workflows/build.yml` | 格式、Clippy、测试及 release 构建验证；不打包或上传发行产物 |
| `test-output/` | 本地生成目录，含字体和验证产物；不提交 |
| `dist/` | 手动打包生成的应用目录；不随源码提交或由 CI 上传 |

工作区使用 Rust 2021 edition，声明最低 Rust 版本为 1.88。上一批开发验证使用 Rust 1.99；这不代表已经在最低版本上完成测试。依赖由 `Cargo.lock` 锁定。

## 构建与检查

先克隆仓库，所有命令在仓库根目录执行。需要 Rust 工具链及平台编译/链接环境：macOS 需要 Xcode Command Line Tools；Windows 需要与所选 Rust target 匹配的工具链（使用 MSVC target 时包含 C++ 构建工具和 Windows SDK）。首次获取依赖需要网络；依赖缓存齐全时可加 `--offline`。

```sh
git clone https://github.com/mohui666/PCL-Rust.git
cd PCL-Rust
```

macOS 首次启动前先完成下方的本机字体生成。随后可运行和检查：

```sh
cargo run --locked -p pcl-desktop
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

自动化测试默认不执行显式忽略的联网安装测试。CI 在 macOS/Windows runner 执行格式、严格 Clippy、测试及 `cargo build --workspace --release --locked`，不调用本地打包脚本，也不上传 `dist/`。CI 配置不代表某次运行已经成功；本次检出的检查结果见[验证记录](validation.md)。真实账号、远端安装及游戏运行应单独记录。

### macOS

```sh
bash scripts/package-macos.sh
```

脚本在本机生成字体、构建 release、创建 `dist/PCL Rust.app` 并进行本机签名检查。当前本机 ad-hoc 签名不是 Developer ID 公证或公开发行证明。

本机系统苹方使用当前渲染库无法直接处理的轮廓格式。已有脚本通过 CoreText 导出本机字形并生成可渲染字体，需 `python3`、`xcrun swift` 与系统 CoreText：

```sh
python3 scripts/build-local-pingfang-sfnt.py --style all
```

新增中文文案后需重新生成字库；macOS 打包入口会执行这一步。生成的 Regular/Semibold 字体和轮廓仅用于本机运行，不在本仓库分发。其他系统的字体可用性与许可需分别确认。

### Windows

在仓库根目录的 Windows PowerShell 中：

```powershell
./scripts/package-windows.ps1
```

上一批开发产物验证来自 macOS 上对 `x86_64-pc-windows-gnu` 的交叉构建和 PE 静态检查。Windows 原生打包、启动、系统凭据管理与游戏运行仍需实机验证，不能据交叉构建成功推定通过。

## 配置与数据

| 数据 | 约定 |
| --- | --- |
| macOS 配置与游戏目录 | 默认位于 `~/Library/Application Support/pcl-rust/` 下 |
| Windows 全局设置 | `%APPDATA%` 下的 `pcl-rust/settings.json` |
| Windows 默认游戏目录 | `%LOCALAPPDATA%` 下的 `pcl-rust/game` |
| 版本实例设置 | `versions/<id>/PCL-Rust/instance.json` |
| Microsoft 刷新凭据 | macOS Keychain / Windows Credential Manager |
| Minecraft 访问会话 | 内存；详情见[登录说明](login.md) |

实例隔离启用时保留已存在的 `instances/<id>/`，否则使用 `versions/<id>/`；关闭隔离时使用游戏根目录。切换隔离不自动搬移存档。已有 `.minecraft` 可以作为游戏目录，但这不等于导入旧 PCL 设置或账号。

启动参数以参数数组传给进程，显示用的脱敏命令不能直接当作 shell 命令执行。测试应使用隔离目录，保留已有游戏、配置与账号数据。

## 发布与验证

文档、源码检查、GUI 交互、真实服务、游戏运行及平台兼容分别记录。上一批本地开发产物指纹和本次公开源码检出的检查不能混为一项；详情见[验证记录](validation.md)。本次发布源码和构建脚本，不提供应用发行包。保留上游自定义许可与资产来源声明；本地构建成功不代表所有第三方材料可以无限制再分发。
