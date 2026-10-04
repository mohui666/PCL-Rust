# 本地开发说明

[返回项目首页](../README.md) · [验证记录](validation.md)

本文对应 2026-10-05 的完整本地开发工作区。**当前公开仓库只含文档，下列源码、脚本和产物路径尚未随仓库发布。** 因此不能直接在此文档仓库执行 Cargo 或打包命令。

## 工作区结构

| 本地相对路径 | 职责 |
| --- | --- |
| `crates/pcl-core/` | 配置、版本与启动计划、下载与校验、Java、账号、加载器、资源与整合包处理 |
| `crates/pcl-desktop/` | egui 原生窗口、页面、消息框、提示、任务进度与交互 |
| `scripts/` | 本机字体处理、macOS 与 Windows 打包入口 |
| `docs/` | 本地完整迁移记录与参考资料；本仓库提供面向公开读者的整理版本 |
| `test-output/` | 本地测试日志、证据、临时实例及验证产物 |
| `dist/` | 本地生成的应用包 |

工作区使用 Rust 2021 edition，声明最低 Rust 版本为 1.88。最新验证使用 Rust 1.99；这不代表已经在最低版本上完成测试。依赖由 `Cargo.lock` 锁定。

## 构建与检查

以下命令均在**完整源码工作区根目录**执行。首次获取依赖需要网络；依赖缓存齐全时可加 `--offline`。

```sh
cargo run --locked -p pcl-desktop
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

自动化测试默认不执行显式忽略的联网安装测试。修改功能后运行对应测试与必要构建，真实账号、远端资源安装及游戏运行应单独记录，不能用单元测试替代。

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

在具备完整源码及构建环境的 Windows PowerShell 中：

```powershell
./scripts/package-windows.ps1
```

当前产物验证来自 macOS 上对 `x86_64-pc-windows-gnu` 的交叉构建和 PE 静态检查。Windows 原生打包、启动、系统凭据管理与游戏运行仍需实机验证，不能据交叉构建成功推定通过。

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

文档、代码构建、GUI 交互、真实服务、游戏运行和 Windows/macOS 兼容分别记录。最新版产物指纹与尚未完成的检查见[验证记录](validation.md)。源码和应用公开发布需在确认分发条件后另行进行。
