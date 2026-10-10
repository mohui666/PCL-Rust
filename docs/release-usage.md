# PCL Rust v0.1.1 使用说明

从 [GitHub Release](https://github.com/mohui666/PCL-Rust/releases/tag/v0.1.1) 下载与你的系统对应的包，解压后运行：

| 系统 | 下载文件 | 启动方式 |
| --- | --- | --- |
| Windows x86_64 | `PCL-Rust-v0.1.1-windows-x86_64.zip` | 打开 `PCL-Rust.exe` |
| macOS Apple Silicon | `PCL-Rust-v0.1.1-macos-arm64.zip` | 将 `PCL Rust.app` 移到“应用程序”，打开应用 |
| Linux x86_64 | `PCL-Rust-v0.1.1-linux-x86_64.tar.gz` | 在桌面会话中运行 `./PCL-Rust`，保留同目录的 `resources` |

程序已配置 PCL Rust 自有 Microsoft Client ID 和 CurseForge 应用 Key，无需用户申请或填写。正版账号仍须由用户登录自己的微软账号；安装包不包含任何玩家账号或登录令牌。离线登录可直接填写名称使用。

macOS 包采用本地签名，未经过 Apple 公证。如系统提示无法验证开发者，在“系统设置 → 隐私与安全性”确认来源后允许打开。公开 macOS / Linux 包使用 Noto Sans SC 字体，并附带 OFL 许可。

Linux 以 Ubuntu 22.04（glibc 2.35+）为基准，需要 X11 或 Wayland 桌面、OpenGL/EGL、libxkbcommon，以及桌面文件对话框服务。Linux 微软凭据存储尚未实现，目前使用离线登录；Linux GUI 和实际游戏运行仍需更多实机验证。Windows / macOS 支持系统凭据存储。

各包都附带 `pcl-cli`（Windows 为 `pcl-cli.exe`）。运行 `pcl-cli --help` 查看无头用法；完整说明见 [命令行文档](https://github.com/mohui666/PCL-Rust/blob/main/docs/headless.md)。

如需在导出整合包时附带三端启动器，另下载 `PCL-Rust-v0.1.1-launcher-bundle.zip`，解压后在导出页选择其中的 `launcher-bundle` 目录。它不包含 Minecraft 游戏文件、Java、用户账号或设置。

本版恢复离线登录，修复官方 HTTP 皮肤地址的安全 HTTPS 转换、账户姓名对齐、皮肤菜单布局、披风选项圆圈裁切，以及任务历史标题居中问题。保留微软正版权益验证和实例登录类型限制。

第三方 Rust 重构版，原 PCL 作者为龙腾猫跃，Rust 版作者为 mohui666。见随包 `UPSTREAM-LICENCE` 及 `licenses` 中的来源与许可。
