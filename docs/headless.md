# 无头命令行

`pcl-cli` 是独立的控制台程序，只依赖 `pcl-core` 和命令行库，不依赖 eframe、winit、图形显示服务、字体或文件选择对话框。桌面程序带子命令时也调用同一入口；桌面程序无参数仍打开 GUI。对应 issue #2。

```sh
cargo build --locked --release -p pcl-cli
./target/release/pcl-cli --help
```

Windows 使用 `pcl-cli.exe`，可正常等待退出并重定向输出；macOS 包内位于 `PCL Rust.app/Contents/MacOS/pcl-cli`；Linux 包内位于 `pcl-cli`。只构建 CLI 无需安装 Linux 的 X11 / Wayland 编译依赖。

所有子命令默认无头，也接受显式 `--headless`。无参数或只有 `--headless` 时显示用法并返回 2，不打开窗口。

## 输入、输出与设置

- `--root` 指定绝对游戏目录；省略时读取现有桌面设置。
- `--config` 指定独立 JSON 设置文件；缺失文件使用默认值，不自动写回。推荐自动化任务显式传入，避免受到桌面偏好的影响。
- stdout 始终为最终 JSON；`--json` 将 stderr 的进度、日志、设备码和错误也转为逐行 JSON。无 `--json` 时 stderr 使用可读文本。
- 成功退出 0，操作失败 1，参数错误 2，Ctrl+C / SIGTERM 取消 130；游戏正常退出后输出结果，游戏非零退出码在 1–255 内透传，其余映射为 1。
- 下载沿用共享核心的来源设置、并发数、哈希校验、可恢复下载和禁止覆盖规则。`settings --apply` 可应用完整设置 JSON；`instance settings VERSION --apply FILE` 可应用实例设置。更改前均校验。
- Mod、资源、日志和启动使用同一个实例目录解析器，遵守共享／隔离目录设置。新游戏和加载器继承全局隔离策略；既有设置保留。
- 更新和移除会使用原有备份或系统废纸篓机制。CLI 不读取确认输入；`mod updates --apply`、`instance trash --apply` 是执行开关，省略时仅预览。

下列命令以 Bash 为例。先把 `ROOT` 换成独立绝对路径；PowerShell 使用 `$ROOT = 'D:\Minecraft'`。

```sh
ROOT="$PWD/headless-game"
pcl-cli --root "$ROOT" manifest
pcl-cli --root "$ROOT" install 1.21.1
pcl-cli --root "$ROOT" list
pcl-cli --root "$ROOT" repair 1.21.1
```

## 加载器和 Java

```sh
pcl-cli loaders fabric 1.21.1
pcl-cli --root "$ROOT" install-loader fabric 1.21.1 0.19.5
pcl-cli loaders forge 1.21.1
pcl-cli --root "$ROOT" install-loader forge 1.21.1 VERSION --java /absolute/path/to/java
pcl-cli java list
pcl-cli java inspect /absolute/path/to/java
pcl-cli java runtimes
pcl-cli java install java-runtime-delta --directory /absolute/path/to/runtimes
```

`loaders` / `install-loader` 支持 `fabric`、`quilt`、`forge`、`neoforge`、`liteloader`、`optifine`，版本必须来自相应发布方清单。Forge、NeoForge、OptiFine 安装需要显式 `--java`；`java install` 下载的是 Mojang 当前平台官方 Java。LiteLoader 可用 `--parent` 组合已有兼容版本；OptiFine 的 `--parent` 支持官方允许的 Forge 组合，或已经安装兼容 OptiFabric 的 Fabric 实例，继续遵守共享核心的兼容限制。

## Mod 与其他资源

```sh
pcl-cli search modmenu --minecraft 1.21.1 --loader fabric
pcl-cli resource project modmenu
pcl-cli resource versions modmenu --minecraft 1.21.1 --loader fabric
# VERSION_ID 取 versions 的 id；INSTANCE 取 list 的 id
pcl-cli resource download VERSION_ID /absolute/path/modmenu.jar
pcl-cli --root "$ROOT" resource install VERSION_ID INSTANCE --dry-run
pcl-cli --root "$ROOT" resource install VERSION_ID INSTANCE
pcl-cli --root "$ROOT" mods INSTANCE
pcl-cli --root "$ROOT" mod import INSTANCE /absolute/path/another.jar
pcl-cli --root "$ROOT" mod disable INSTANCE another.jar
pcl-cli --root "$ROOT" mod enable INSTANCE another.jar.disabled
pcl-cli --root "$ROOT" mod updates INSTANCE
pcl-cli --root "$ROOT" mod updates INSTANCE --apply --file another.jar
pcl-cli --root "$ROOT" mod remove INSTANCE another.jar
pcl-cli --root "$ROOT" mod restore INSTANCE /absolute/path/from/removal-result
```

`search`、`resource versions/download/install` 的 `--kind` 支持 `mod`、`modpack`、`resourcepack`、`shader`、`datapack`。安装 Mod 会从实例元数据推导游戏和加载器，再解析必需依赖；不能用参数伪装兼容性。资源包与光影包进入其对应目录，数据包必须指定 `--world 世界目录名`，不能越过实例的 `saves`。

CurseForge 搜索使用 `--provider curseforge`，项目写作 `cf:项目ID`，文件写作 `cf:项目ID:文件ID`。API Key 沿用系统安全存储或 `PCL_CURSEFORGE_API_KEY` 环境变量；没有 Key 或作者禁止 API 下载时明确报错，不绕过限制。

## 整合包

```sh
pcl-cli inspect-pack /absolute/path/pack.mrpack
pcl-cli --root "$ROOT" install-pack /absolute/path/pack.mrpack MyPack
pcl-cli --root "$ROOT" resource install VERSION_ID MyPack --kind modpack
pcl-cli --root "$ROOT" export-pack MyPack /absolute/path/export.mrpack
pcl-cli --root "$ROOT" export-pack MyPack /absolute/path/export.zip --format multimc
pcl-cli --root "$ROOT" export-pack MyPack /absolute/path/export.mrpack --options /absolute/path/export-options.json
```

导入支持共享核心已经支持的 mrpack、CurseForge、MultiMC / Prism、HMCL、MCBBS、游戏 ZIP；始终创建不重名的新实例。可加 `--optional`，Forge / NeoForge 整合包可加 `--java`。导出支持 `mrpack`、`multimc`、`hmcl`、`mcbbs`。JSON 选项遵循 `PackExportOptions`，例如：

```json
{"name":"MyPack","version":"1.0.0","resource_mode":"EmbedAll","selection":{"mods":true,"mod_configs":true}}
```

`EmbedAll` 将选中的本地文件嵌入包；默认 `PreferHosted` 根据哈希查找下载来源。选项中的 `selection.worlds` 明确列出要附带的世界，`include_java` 控制 Java。`--launcher /absolute/path/to/launcher` 可附带指定启动器；此时输出须为 ZIP。使用 `--options` 时格式取该 JSON 的 `format`，默认 `Mrpack`。不会自动附带账户、令牌或执行包内启动命令。

## 登录、启动和维护

```sh
pcl-cli accounts
pcl-cli login --client-id YOUR_APPROVED_CLIENT_ID
pcl-cli --root "$ROOT" plan INSTANCE --java /absolute/path/to/java --account ACCOUNT_ID
pcl-cli --root "$ROOT" launch INSTANCE --account ACCOUNT_ID --memory 4096
pcl-cli --root "$ROOT" launch INSTANCE --account ACCOUNT_ID
pcl-cli --root "$ROOT" export-script INSTANCE /absolute/path/launch.command --java /absolute/path/to/java --account ACCOUNT_ID
pcl-cli --root "$ROOT" instance show INSTANCE
pcl-cli --root "$ROOT" instance rename INSTANCE NewName
pcl-cli --root "$ROOT" instance trash INSTANCE
pcl-cli --root "$ROOT" logs --instance INSTANCE
pcl-cli logs --file /absolute/path/latest.log --export /absolute/path/report.zip
```

设备码及验证地址写入 stderr；可以在另一台机器的浏览器完成授权，CLI 不自动打开浏览器。账户仍须通过微软、Xbox 和 Minecraft 的授权／所有权检查；刷新凭据只存系统安全存储。Linux 安全凭据库仍沿用核心的现有限制，未新增明文保存回退；真实微软服务访问受 AppID 审批状态影响。

离线登录已禁用。`launch`、`plan` 和 `export-script` 必须传入 `--account ACCOUNT_ID`，账号由 `login` 完成正版验证后保存；失败不会退回离线身份。旧 `--name` 参数或仅有本地玩家名称会明确报错。旧实例若设为 `offline`，须将其登录方式改为正版登录；修改实例 JSON 不能放开共享核心的账号校验。下载、Java、加载器、Mod 和整合包管理仍可在未登录时执行。

`--java` 是明确选择；省略时沿用实例 Java 策略并自动选择已安装的兼容运行时，缺失时提示 `java install`，不会暗中下载。`--memory` 仅覆盖本次启动，不改写实例设置；`--server` 同样仅对本次生效。

`plan` 只生成脱敏命令，不启动游戏；`export-script` 写入脱敏的 `.command` 或 `.bat`，已有文件不会覆盖，导出的诊断脚本不含正版凭据。`launch` 启动 Java、转发脱敏日志并等待退出；取消只终止本次管理的进程。它保留 JVM／游戏参数、隔离目录、登录限制、启动前命令与首次语言设置。命令行使用保存的启动器尺寸解释“跟随窗口”设置；游戏窗口标题、最大化、系统 GPU 偏好和进程优先级由桌面／系统管理，有相关设置时输出提示。

**无头指启动器无窗口。Minecraft 客户端自身仍需要可用的图形环境；此功能不是 Minecraft 服务端模式。** `instance trash --apply` 使用系统废纸篓／回收站；系统不支持时明确失败，不改为永久删除。
