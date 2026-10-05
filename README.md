# Plain Craft Launcher (PCL) Rust 第三方重构版

面向 **macOS 与 Windows** 的 Rust 原生桌面启动器重构，由 **mohui666** 开发。目标是完整迁移功能，并以 **Windows 官方 PCL 2.13.1.1 默认界面**为外观基准；macOS 使用苹方。项目仍在开发，尚未达到完整功能或像素等价。

原版作者：**龙腾猫跃** · [官方 PCL](https://github.com/Meloong-Git/PCL) · [支持原作者](https://meloong.com/afd/a/LTCat)

本项目是第三方开发项目，不是 PCL、Mojang 或 Microsoft 的官方发布，也不代表其背书。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [迁移矩阵](docs/migration-matrix.md) | 43 个上游页面/复用控件文件逐项对照、设置范围与剩余功能 |
| [UI 缺口清单](docs/ui-gap-audit.md) | 下载、设置和全部灰色控件的静态核对与剩余范围 |
| [验证记录](docs/validation.md) | 最新测试、两端构建哈希、Mac 实测与尚未验证的范围 |
| [登录说明](docs/login.md) | 微软设备码流程、凭据存储、恢复语义和当前 HTTP 403 状态 |
| [开发说明](docs/development.md) | 源码结构、构建与验证命令、中文字体处理 |
| [参考来源](docs/upstream.md) | Windows 外观基准、固定源码提交、署名与发布范围 |

## 当前进展

本次源码发布日期：**2026-10-05（Asia/Shanghai）**。仓库包含 Rust 源码、Cargo 配置与锁文件、构建脚本、必要资产、来源记录、文档和 CI 验证工作流。本次源码检出检查与此前开发批次的产物、实机证据分别记录。

- 本次公开源码检出在 **macOS arm64 / Rust 1.99** 通过格式检查、**342 项测试（0 失败、1 忽略）**与全部目标严格 Clippy；当前实现也通过 Windows 目标全部目标严格 Clippy。测试数量不代表功能覆盖率，详情见[验证记录](docs/validation.md)。
- 当前开发版已生成 macOS 签名 release 包和 Windows x86-64 交叉构建，重新打开 Mac 应用检查下拉菜单及真实版本查询。Windows/Wine 尚未执行，应用发行包未上传。
- 本批重做普通/可编辑下拉菜单、版本加载/错误/取消状态，补齐可见性、进程优先级、全局/实例启动前命令、跟随窗口尺寸、窗口透明度和背景参数。全部灰色控件的静态核对及余项见 [UI 缺口清单](docs/ui-gap-audit.md)。
- 此前已补入登录分类提示、三色提示队列、消息框颜色/焦点/动画、会话失效后的限次恢复、原账号操作绑定，以及披风选择后的确定/取消和 29 个中文名称。
- Mac 消息框和提示使用生产渲染器、模拟登录数据完成原生检查；另已重新打开实际启动器核对中文与设置保留。这些检查不代表真实登录或披风修改成功。
- Minecraft AppID 审核申请已提交并收到回执。上次真实登录在 Minecraft `login_with_xbox` 返回 HTTP 403，尚未确认审核通过，也未完成正版登录端到端验证。

已接入的功能包括版本解析与原版下载、Java 选择和下载、部分加载器安装、本地 Mod 管理、Modrinth 资源查询与下载、部分 mrpack 导入导出、多账号设备登录及游戏进程管理。各项实现和验收程度不同，详见[逐项矩阵](docs/migration-matrix.md)。

仍有第三方认证、部分加载器与整合包格式、多源资源服务、完整任务调度、若干设置及全状态 UI 等缺口。完整像素对照、Windows 实机和多项实际游戏流程仍待验收，不提供没有统一验收依据的完成百分比。

## 从源码运行

需要 Rust 工具链和对应平台的编译/链接环境。项目声明 Rust 1.88 起，上一批实际验证使用 Rust 1.99，最低版本尚未单独验收。

```sh
git clone https://github.com/mohui666/PCL-Rust.git
cd PCL-Rust
```

macOS 还需要 Python 3、Xcode Command Line Tools（`xcrun swift`）和系统苹方；首次运行及新增中文文案后，先在本机生成字库：

```sh
python3 scripts/build-local-pingfang-sfnt.py --style all
cargo run --locked -p pcl-desktop
```

Windows 在对应 Rust 构建环境中执行 `cargo run --locked -p pcl-desktop`。本地打包分别使用 `bash scripts/package-macos.sh` 或 PowerShell 中的 `./scripts/package-windows.ps1`；完整要求与检查命令见[开发说明](docs/development.md)。

## 仓库与发布范围

本次提供可供本地构建的源码与脚本，**尚未上传可下载的应用发行包**。CI 进行格式、Clippy、测试和 release 构建检查，不打包或上传 `dist/`；工作流存在不等于远端运行已经通过。

上游自定义许可、作者署名和资产来源记录随源码保留，不改称 MIT/Apache。第三方材料的分发条件尚未全部核验，源码公开不代表其获得无限制的再分发授权。原始测试日志、审核表内容、个人账号资料、本地游戏文件及系统字体派生文件不随仓库发布；macOS 字体由使用者在本机生成。更多内容见[参考来源](docs/upstream.md)。

## Project information for Minecraft AppID review

**PCL Rust** is a third-party Rust desktop launcher project for Windows and macOS, maintained by **mohui666**. It is under development and is not an official PCL, Mojang, or Microsoft product.

The registered application display name is **PCL Rust**. Its public Application (Client) ID is `2c86a114-ddd8-468b-82ca-441923527e09`.

The Microsoft sign-in implementation uses device authorization for personal Microsoft accounts, followed by Xbox Live, XSTS, and Minecraft Services authentication, entitlement and player profile checks. It does not collect Microsoft account passwords. Refresh credentials use the operating system credential store; Minecraft access sessions stay in memory. A separate offline-profile mode does not obtain Microsoft or Minecraft Services tokens.

The AppID review request has been submitted and its receipt confirmed. Approval and end-to-end authenticated login have not been verified. The previous real attempt returned HTTP 403 from Minecraft Services. The source release dated 2026-10-05 includes Rust code, build scripts, required assets and validation CI. Downloadable application releases and locally derived system fonts are not published. The current source checkout was separately checked on macOS arm64: 342 tests passed, with one ignored, alongside formatting and strict Clippy. The development tree also produced a signed macOS application and a cross-compiled Windows executable, with limited native macOS UI checks. These results do not establish remote CI, Windows execution, complete pixel equivalence or authenticated login acceptance.
