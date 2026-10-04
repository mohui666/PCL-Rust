# Plain Craft Launcher (PCL) Rust 第三方重构版

面向 **macOS 与 Windows** 的 Rust 原生桌面启动器重构，由 **mohui666** 开发。目标是完整迁移功能，并以 **Windows 官方 PCL 2.13.1.1 默认界面**为外观基准；macOS 使用苹方。项目仍在开发，尚未达到完整功能或像素等价。

原版作者：**龙腾猫跃** · [官方 PCL](https://github.com/Meloong-Git/PCL) · [支持原作者](https://meloong.com/afd/a/LTCat)

本项目是第三方开发项目，不是 PCL、Mojang 或 Microsoft 的官方发布，也不代表其背书。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [迁移矩阵](docs/migration-matrix.md) | 43 个上游页面/复用控件文件逐项对照、设置范围与剩余功能 |
| [验证记录](docs/validation.md) | 最新测试、两端构建哈希、Mac 实测与尚未验证的范围 |
| [登录说明](docs/login.md) | 微软设备码流程、凭据存储、恢复语义和当前 HTTP 403 状态 |
| [开发说明](docs/development.md) | 本地工作区结构、构建与验证命令、中文字体处理 |
| [参考来源](docs/upstream.md) | Windows 外观基准、固定源码提交、署名与发布范围 |

## 当前进展

更新日期：**2026-10-05（Asia/Shanghai）**。以下是本地开发结果，公开仓库目前提供整理后的项目文档。

- 最新工作区测试 **304 项通过、0 失败、1 忽略**；全部目标严格 Clippy 通过。忽略项为需要实际安装的联网 Fabric 集成测试。
- macOS 签名 release 包和 Windows x86-64 交叉构建完成。Mac 已进行限定界面检查；Windows/Wine 尚未执行。
- 最近补入登录分类提示、三色提示队列、消息框颜色/焦点/动画、会话失效后的限次恢复、原账号操作绑定，以及披风选择后的确定/取消和 29 个中文名称。
- Mac 消息框和提示使用生产渲染器、模拟登录数据完成原生检查；另已重新打开实际启动器核对中文与设置保留。这些检查不代表真实登录或披风修改成功。
- Minecraft AppID 审核申请已提交并收到回执。上次真实登录在 Minecraft `login_with_xbox` 返回 HTTP 403，尚未确认审核通过，也未完成正版登录端到端验证。

已接入的功能包括版本解析与原版下载、Java 选择和下载、部分加载器安装、本地 Mod 管理、Modrinth 资源查询与下载、部分 mrpack 导入导出、多账号设备登录及游戏进程管理。各项实现和验收程度不同，详见[逐项矩阵](docs/migration-matrix.md)。

仍有第三方认证、部分加载器与整合包格式、多源资源服务、完整任务调度、若干设置及全状态 UI 等缺口。完整像素对照、Windows 实机和多项实际游戏流程仍待验收，不提供没有统一验收依据的完成百分比。

## 仓库与发布范围

当前远端只发布项目文档，**没有启动器源码或可下载的应用发行包**。开发说明中的命令用于已有完整本地工作区，直接克隆此文档仓库不能构建启动器。

源码、二进制与资源的公开分发条件仍待核对。原始测试日志、审核表内容、个人账号资料、本地游戏文件和系统字体派生文件不包含在文档仓库中。当前文档保留上游作者和第三方身份说明；更多内容见[参考来源](docs/upstream.md)。

## Project information for Minecraft AppID review

**PCL Rust** is a third-party Rust desktop launcher project for Windows and macOS, maintained by **mohui666**. It is under development and is not an official PCL, Mojang, or Microsoft product.

The registered application display name is **PCL Rust**. Its public Application (Client) ID is `2c86a114-ddd8-468b-82ca-441923527e09`.

The Microsoft sign-in implementation uses device authorization for personal Microsoft accounts, followed by Xbox Live, XSTS, and Minecraft Services authentication, entitlement and player profile checks. It does not collect Microsoft account passwords. Refresh credentials use the operating system credential store; Minecraft access sessions stay in memory. A separate offline-profile mode does not obtain Microsoft or Minecraft Services tokens.

The AppID review request has been submitted and its receipt confirmed. Approval and end-to-end authenticated login have not been verified. The previous real attempt returned HTTP 403 from Minecraft Services. This repository currently contains documentation only; source code and application releases have not been published.
