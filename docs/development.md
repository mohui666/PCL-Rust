# 源码开发与本地构建

[项目首页](../README.md) / [文档索引](README.md)

适用范围：公开 Rust 工作区。先克隆并检查依赖，再按对应平台运行或打包；当前支持范围见 [当前范围](remaining-migration.md)。

## 工作区结构

| 仓库或本地生成路径 | 职责 |
| --- | --- |
| `crates/pcl-core/` | 配置、版本与启动计划、下载与校验、Java、账号、加载器、资源与整合包处理 |
| `crates/pcl-desktop/` | egui 原生窗口、页面、消息框、提示、任务进度与交互 |
| `scripts/` | 字体处理与 macOS、Windows、Linux 打包入口 |
| `docs/` | 公开迁移矩阵、验证摘要、登录与来源说明 |
| `crates/pcl-desktop/assets/` | 构建所需资产及来源记录 |
| `.github/workflows/build.yml` | Mac/Windows 格式、Clippy、测试与 release 检查；Linux 构建、静态字体与包产物 |
| `test-output/` | 本地生成目录，含字体和验证产物；不提交 |
| `dist/` | 打包生成的应用目录，不提交到源码；预览包下载见 [v0.1.0](https://github.com/mohui666/PCL-Rust/releases/tag/v0.1.0) |

工作区使用 Rust 2021 edition，声明最低 Rust 版本为 1.88。当前记录中的开发验证使用 Rust 1.99；这不代表已经在最低版本上完成测试。依赖由 `Cargo.lock` 锁定。

## 构建与检查

所有命令在仓库根目录执行。首次获取依赖需要网络；缓存齐全时可加 `--offline`。

| 平台 | 构建前准备 |
| --- | --- |
| 通用 | Rust 工具链；依赖版本由 Cargo.lock 锁定。 |
| macOS | Python 3、Xcode Command Line Tools（xcrun swift）、系统苹方；首次运行前生成字库。 |
| Windows | 与所选 Rust target 匹配的编译/链接工具；MSVC target 需要 C++ 构建工具和 Windows SDK。 |
| Linux x86_64 | Ubuntu 22.04、系统图形开发库、Python 3 与 fontTools；命令见下节。 |

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

默认测试跳过标记为 ignored 的用例。CI 的 Mac/Windows job 执行格式、严格 Clippy、测试及 release 构建；Linux job 在 Ubuntu 22.04 生成程序、中文字库与压缩包，并上传构建产物。运行结果见 [验证记录](validation.md)。

### macOS

```sh
bash scripts/package-macos.sh
```

默认模式从本机生成苹方字库，构建 release、创建 `dist/PCL Rust.app` 并检查 ad-hoc 签名；此签名不是 Developer ID 公证。

公开 macOS 包改用可分发的 Noto Sans SC，并附 OFL 许可。构建公开包时先准备 fontTools：

```sh
python3 -m venv test-output/linux-fonts-venv
test-output/linux-fonts-venv/bin/pip install fonttools==4.60.1
PCL_MACOS_FONT_MODE=noto PYTHON_BIN="$PWD/test-output/linux-fonts-venv/bin/python" bash scripts/package-macos.sh
```

也可将 `PYTHON_BIN` 指向已经安装该版本 fontTools 的解释器。

本机系统苹方使用当前渲染库无法直接处理的轮廓格式。已有脚本通过 CoreText 导出本机字形并生成可渲染字体，需 `python3`、`xcrun swift` 与系统 CoreText：

```sh
python3 scripts/build-local-pingfang-sfnt.py --style all
```

默认苹方模式下，新增中文文案后需重新生成字库，macOS 打包入口会执行这一步；Noto 模式使用静态 Regular/Semibold 字体。字库放在应用 Resources 内，不提交到源码仓库。整合包附带 Mac 启动器时完整保留所选 .app 的字体、资源和签名文件。

### Windows

在仓库根目录的 Windows PowerShell 中：

```powershell
./scripts/package-windows.ps1
```

本地 Windows 开发产物验证来自 macOS 上对 `x86_64-pc-windows-gnu` 的交叉构建和 PE 静态检查。Windows 原生打包、启动、系统凭据管理与游戏运行仍需实机验证，不能据交叉构建成功推定通过。

### Linux x86_64

构建脚本面向 Ubuntu 22.04 x86_64。安装依赖并准备独立 Python 环境：

```sh
sudo apt-get install build-essential pkg-config libx11-dev libxkbcommon-dev libwayland-dev libegl1-mesa-dev python3-venv
python3 -m venv test-output/linux-fonts-venv
test-output/linux-fonts-venv/bin/pip install fonttools==4.60.1
PYTHON_BIN="$PWD/test-output/linux-fonts-venv/bin/python" bash scripts/package-linux.sh
```

输出为 `dist/PCL-Rust-Linux-x86_64/` 和同名 `.tar.gz`。脚本从固定来源校验并生成 Noto Sans SC Regular/Semibold 静态 TrueType 字体，随包保存 OFL 许可与来源记录；检查 ELF、动态依赖、CLI 启动和中文字形。

在 Linux 桌面会话运行 `./PCL-Rust`，并将 `resources/` 保留在程序旁。运行需要 X11 或 Wayland、OpenGL/EGL 和桌面的 xdg-desktop-portal 文件对话框服务。Linux GUI、文件选择和游戏运行尚未验证。

## 导出时附带三端启动器

导出页默认勾选“PCL Rust 启动器（Windows、macOS、Linux）”。取消勾选后按所选格式导出普通整合包；勾选时导出外层 ZIP，内含 `modpack.mrpack` 和三端程序。

可以从 [v0.1.0 下载页](https://github.com/mohui666/PCL-Rust/releases/tag/v0.1.0) 获取 `launcher-bundle.zip` 三端目录包。启动器会寻找应用旁的 `launcher-bundle`，也可点击“选择启动器目录”。目录结构：

```text
launcher-bundle/
├── windows/
│   └── PCL-Rust.exe
├── macos/
│   └── PCL-Rust.app/
└── linux/
    ├── PCL-Rust
    └── resources/
        ├── NotoSansSC-Regular.ttf
        ├── NotoSansSC-Semibold.ttf
        ├── OFL.txt
        └── FONT-SOURCES.json
```

Windows 与 Linux 的运行依赖、资源和许可文件放在各自目录内。Mac 使用完整 `.app`，保留字体、资源和签名字节；Linux 与 Mac 主程序在 ZIP 中保留可执行权限。缺少任一平台或程序格式不符时明确报错，不用本机程序代替。

导出不包含账户和全局设置，不覆盖已有目标文件。接收方打开对应平台的启动器后手动导入 `modpack.mrpack`；Java 与游戏不会自动运行。

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

[v0.1.0 开发预览版下载](https://github.com/mohui666/PCL-Rust/releases/tag/v0.1.0)：Windows x86_64、macOS arm64、Linux x86_64（glibc 2.35+）。构建、签名、CI 与实机结果见 [验证记录](validation.md)，许可和资产来源见 [来源说明](upstream.md)。
