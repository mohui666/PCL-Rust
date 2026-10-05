# 源码开发与本地构建

[项目首页](../README.md) / [文档索引](README.md)

适用范围：公开 Rust 工作区。先克隆并检查依赖，再按对应平台运行或打包；当前支持范围见 [当前范围](remaining-migration.md)。

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

工作区使用 Rust 2021 edition，声明最低 Rust 版本为 1.88。当前记录中的开发验证使用 Rust 1.99；这不代表已经在最低版本上完成测试。依赖由 `Cargo.lock` 锁定。

## 构建与检查

所有命令在仓库根目录执行。首次获取依赖需要网络；缓存齐全时可加 `--offline`。

| 平台 | 构建前准备 |
| --- | --- |
| 通用 | Rust 工具链；依赖版本由 Cargo.lock 锁定。 |
| macOS | Python 3、Xcode Command Line Tools（xcrun swift）、系统苹方；首次运行前生成字库。 |
| Windows | 与所选 Rust target 匹配的编译/链接工具；MSVC target 需要 C++ 构建工具和 Windows SDK。 |

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

默认测试跳过标记为 ignored 的用例。CI 在 macOS/Windows runner 执行格式、严格 Clippy、测试及 release 构建，不打包或上传 dist；运行结果见 [验证记录](validation.md)。

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

本地 Windows 开发产物验证来自 macOS 上对 `x86_64-pc-windows-gnu` 的交叉构建和 PE 静态检查。Windows 原生打包、启动、系统凭据管理与游戏运行仍需实机验证，不能据交叉构建成功推定通过。

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

构建、签名、CI 与实机结果见 [验证记录](validation.md)。仓库目前不提供应用发行包；许可和资产分发范围见 [来源说明](upstream.md)。
