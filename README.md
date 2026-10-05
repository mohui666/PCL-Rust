# Plain Craft Launcher (PCL) Rust 第三方重构版

面向 **macOS 与 Windows** 的 Rust 原生桌面启动器重构，由 **mohui666** 开发。目标是完整迁移功能，并以 **Windows 官方 PCL 2.13.1.1 默认界面**为外观基准；macOS 使用苹方。项目仍在开发，尚未达到完整功能或像素等价。

原版作者：**龙腾猫跃** · [官方 PCL](https://github.com/Meloong-Git/PCL) · [支持原作者](https://meloong.com/afd/a/LTCat)

本项目是第三方开发项目，不是 PCL、Mojang 或 Microsoft 的官方发布，也不代表其背书。

## 本轮进展

已逐一复查 31 个界面/容器文件，继续修复长列表、文字/按钮重叠、悬停操作消失、实例设置空位与错位等同类问题，见[复查清单](docs/ui-consistency-audit.md)。本机 **563 项测试通过**；构建和各平台证据见[验证记录](docs/validation.md)。另修复实例设置页尾及侧栏初始化入口，明确列出[仍未完整迁移的 9 类能力](docs/remaining-migration.md)。不宣称全页面像素等价。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [迁移矩阵](docs/migration-matrix.md) | 43 个上游页面/复用控件文件逐项对照、设置范围与剩余功能 |
| [明确剩余项](docs/remaining-migration.md) | 9 类未完整迁移能力、截图漏项与验证欠账分别列明 |
| [UI 缺口清单](docs/ui-gap-audit.md) | 下载、设置和全部灰色控件的静态核对与剩余范围 |
| [验证记录](docs/validation.md) | 最新测试、两端构建哈希、Mac 实测与尚未验证的范围 |
| [登录说明](docs/login.md) | 微软设备码流程、凭据存储、恢复语义和当前 HTTP 403 状态 |
| [开发说明](docs/development.md) | 源码结构、构建与验证命令、中文字体处理 |
| [参考来源](docs/upstream.md) | Windows 外观基准、固定源码提交、署名与发布范围 |

## 当前进展

本次源码发布日期：**2026-10-05（Asia/Shanghai）**。仓库包含 Rust 源码、Cargo 配置与锁文件、构建脚本、必要资产、来源记录、文档和 CI 验证工作流。本次源码检出检查与此前开发批次的产物、实机证据分别记录。

- 本批界面复查源码在 macOS arm64 通过 **563 项测试（0 失败、3 忽略）**与全部目标严格 Clippy。历史音乐、联网主页和界面操作按原构建单独记录，详见[验证记录](docs/validation.md)。测试数不代表迁移完成率。
- 输入与菜单修复包已在 Mac 用真实鼠标/键盘检查：离线姓名可编辑，账号弹层与字段等宽对齐，下载查询与分类可输入、选择。新一批完整功能包的运行记录另列，Windows 本地交叉构建不能替代实机验证。
- 本批接入下载来源与限速、CurseForge 适配、MC 百科名称关联、OptiFine、继承版本补全、常见整合包格式、Mod 更新、离线皮肤、音乐/GIF/主页、崩溃分析与系统设置。各项支持范围不同，详见 [UI 缺口清单](docs/ui-gap-audit.md)；CurseForge 仍需开发者 API Key。
- 此前已补入登录分类提示、三色提示队列、消息框颜色/焦点/动画、会话失效后的限次恢复、原账号操作绑定，以及披风选择后的确定/取消和 29 个中文名称。
- Mac 消息框和提示使用生产渲染器、模拟登录数据完成原生检查；另已重新打开实际启动器核对中文与设置保留。这些检查不代表真实登录或披风修改成功。
- Minecraft AppID 审核申请已提交并收到回执。上次真实登录在 Minecraft `login_with_xbox` 返回 HTTP 403，尚未确认审核通过，也未完成正版登录端到端验证。

现有实现包含原版与部分加载器下载、Java 管理、Modrinth/CurseForge 资源入口、整合包导入导出、多账号设备登录和游戏进程管理。CurseForge 在线服务、大型整合包互通、部分加载器组合、EasyTier 联机、全部 WPF 主页语义与多任务调度仍有未实现或未验证部分；登录不属于本轮补齐范围。详见[逐项矩阵](docs/migration-matrix.md)。

完整像素对照、Windows 实机和多项实际游戏流程仍待验收，不提供没有统一验收依据的完成百分比。

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

The AppID review request has been submitted and its receipt confirmed. Approval and end-to-end authenticated login have not been verified. The previous real attempt returned HTTP 403 from Minecraft Services. The source release dated 2026-10-05 includes Rust code, build scripts, required assets and validation CI. Downloadable application releases and locally derived system fonts are not published. The current source checkout was separately checked on macOS arm64: 563 tests passed, with three ignored, alongside formatting and strict Clippy. The development tree also produced a signed macOS application and a cross-compiled Windows executable, with limited native macOS UI checks. These results do not establish remote CI, Windows execution, complete pixel equivalence or authenticated login acceptance.
