# Plain Craft Launcher (PCL) Rust 第三方重构版

**PCL Rust** is an independently developed, third-party Rust desktop launcher project for Windows and macOS, maintained by **mohui666**. It is under development and is not a complete replacement for the original PCL.

Original PCL author: **龙腾猫跃**. [Original PCL project](https://github.com/Meloong-Git/PCL) · [Support the original author](https://meloong.com/afd/a/LTCat).

This project is not an official release of PCL, Mojang, or Microsoft, and is not endorsed by them.

## Project scope

- A native Rust/egui desktop interface for Windows and macOS.
- Java Edition version management, downloads, Java runtime selection, and game launch management.
- Microsoft account sign-in and Minecraft entitlement/profile checks.
- Mod and modpack management, with current work focused on Modrinth resources and supported loaders.
- A Windows PCL-inspired interface, with functionality and visual migration still in progress.

## Microsoft and Minecraft authentication

The registered application display name is **PCL Rust**. Its public Application (Client) ID is `2c86a114-ddd8-468b-82ca-441923527e09`.

The launcher uses Microsoft's OAuth device authorization flow for personal Microsoft accounts, followed by Xbox Live authentication, XSTS authorization, and Minecraft Services authentication. The Microsoft login path checks Java Edition entitlements and the Minecraft player profile before accepting an account.

The launcher does not collect Microsoft account passwords. Microsoft refresh credentials are stored in the operating system credential store (macOS Keychain or Windows Credential Manager); the Minecraft access session is kept in memory. Users can remove saved launcher accounts.

The current implementation also includes a separate offline-profile mode. That mode does not obtain Microsoft or Minecraft Services tokens. It is disclosed here so that this project description does not imply that every launch passes through Microsoft authentication.

## Current status

As of 2026-10-04:

- The application registration supports personal Microsoft accounts and has public client flows enabled.
- A real device-code request succeeded. A subsequent login attempt reached Minecraft Services and returned HTTP 403 during `login_with_xbox`.
- Minecraft API access approval has not yet been received. Full authenticated login, ownership validation, and saved-account restoration have not been verified end to end.
- The local development build has passed 277 automated tests, with one ignored test. Selected macOS launcher interactions have been checked; this is not full migration, visual parity, Windows runtime, or game compatibility acceptance.

## Repository contents

This repository currently provides project information for development and the Minecraft AppID review process. It does **not** yet contain the launcher source code or downloadable application releases. Source and binary distribution remain pending review of the original project's custom distribution conditions and third-party assets.

No Microsoft login credentials, user account data, local game files, logs, or locally derived system fonts are published here.

## 中文说明

这是 mohui666 独立开发的 Rust 跨平台第三方重构项目，面向 macOS 与 Windows，仍在进行功能迁移及界面对照。原版 PCL 的作者是龙腾猫跃。本项目不是原版 PCL 或 Minecraft 官方产品。

当前仓库先提供项目介绍，供 Minecraft API 访问审核使用。正版登录已实际请求到 Minecraft 服务，但新应用收到 HTTP 403，审核尚未完成；不能将微软网页登录成功视为 Minecraft 正版登录已经完成。
