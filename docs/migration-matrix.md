# PCL Rust 完整迁移对照表

记录更新：2026-10-05（Asia/Shanghai）。目标仍是迁移原版的完整功能与界面；本表用于暴露剩余工作，不能把当前可运行版本、已有导航入口或原版启动成功解释为完整迁移完成。

本轮以主工作区当前源码为准，非登录功能与共享控件的实际范围见 [UI 缺口清单](ui-gap-audit.md)。已新增 OptiFine、继承文件补全/收据约束重建、多格式整合包、CurseForge/中文索引/Mod 更新、下载策略、启动补丁/窗口与离线皮肤、其他设置、音乐/主页/帮助和崩溃分析；下文逐行保留未覆盖范围。本轮总测试数和双平台产物以最终验证记录为准；此表只更新功能范围，不从数量推定完整迁移。Mac 真实鼠标已验证账号下拉定位、名字与下载筛选输入修复；这不增加登录授权验收。只读网络 probe 通过不等于安装或游戏通过，CurseForge 本机无可用 Key、未在线验收。

此前登录反馈304批通过/0失败/1忽略（核心211、桌面93），工作区全部目标严格Clippy通过，138份冻结输入保持，见 `test-output/login-feedback-checks.json`。已接分类提示、有限重新认证、原账号绑定、披风确认/中文名称与消息框/提示队列；Mac签名release与Windows交叉构建通过；Mac原生生产渲染器以模拟数据验证普通/警告/设备码/输入框与三色提示，新生产应用启动/设置页中文通过，原13号主题与设置文件hash不变。旋转入场360ms暂缓指针提交，键盘可用。详见 `test-output/login-feedback-{macos-validation,windows-validation,ui-validation}.json`；不从模拟数据推断真实授权、披风操作或全页像素一致。

此前277测试批补上关闭Minecraft浮动按钮与资源加载提示；联合检查277通过/0失败/1忽略（核心209、桌面68），工作区全部目标严格Clippy、Mac签名release和Windows交叉构建通过，136份冻结输入保持。Mac实际从设置页关闭游戏并确认进程退出/入口恢复；真实资源查询捕获镐子卡片到结果。单进程关闭和局部加载状态不等于完整启动面板、完整Loader调度或全页像素等价。

随后自有Client ID已由UI保存并核对JSON，Entra支持个人账号及公共客户端流程；真实设备码成功，Minecraft `login_with_xbox` 返回HTTP403。此前Microsoft/Xbox/XSTS完成属于顺序控制流推断，所有权/profile及真实凭据恢复仍未验。AppID访问审核已获用户确认后提交，网页仅给出收件回执，尚未获批。[公开仓库](https://github.com/mohui666/PCL-Rust)先发布6份整理文档，随后按用户要求在 `85aa0c0acd4392c28cbc6624263be38abfdc0df9` 公开源码、资源来源与构建/CI配置，共146文件、47个Rust文件。未发布发行包、本机字库或原始日志；源码公开不增加或改写上述安装包/账号验收结论。

历史269测试包已加入15个本地主题、全局调色与参数持久化、原版语义提示色和微软设备流阶段反馈；269项测试、严格Clippy及双平台构建通过，134份冻结输入保持。Windows/Wine与真实微软授权未验。旧PCL配置迁移以及“更多”的百宝箱/反馈/投票入口按用户要求移除，整合包导入、帮助与关于保留。各批仅支持自身记录范围，具体产物与边界见 [validation.md](validation.md)。

216包的Java/导出/帮助证据、170及更早102/94/87/70测试与游戏日志保留历史归属，不移用于新构建。历史Forge仅合成离线验证；真实微软账号、Windows实机、两端联机及全页成对像素验收均缺。旧PCL配置迁移、百宝箱、反馈和投票入口按用户明确要求排除，不再列为当前成果或必须补齐的缺口。

## 基准与统计方法

上游基准为官方 [Meloong-Git/PCL](https://github.com/Meloong-Git/PCL/tree/0e0d12fdce6a2804916fb2be60e41144da637c18)，固定提交 `0e0d12fdce6a2804916fb2be60e41144da637c18`。本次直接读取 `test-output/upstream/` 中该提交的 XAML、事件代码、模块与配置，不根据截图中的导航文字推测功能。下文上游相对路径以 `Plain Craft Launcher 2/` 为根，另注明 `PCLCS/` 的除外。

| 范围 | 数量 | 统计边界 |
| --- | ---: | --- |
| `Pages/**/*.xaml` | 43 | 39 个非 `My*` 页面/容器文件与 4 个复用子控件；不是 43 个独立顶层页面 |
| `Controls/**/*.xaml` | 17 | 另有纯 VB 自定义控件，不能只按 XAML 计算控件总量 |
| `Controls/**/*.vb` | 34 | 含 XAML 后台、Behaviors 与纯代码控件 |
| `Modules/**/*.vb` | 25 | 只统计此目录，不含 Pages 后台、窗口代码及外部子模块 |
| `PCLCS/**/*.cs` | 6 | Java、配置、启动辅助、资源映射等 |
| `Pages/PageSetup/Settings.vb` 的 `New Setting(...)` | 191 | 包含缓存、提示已读状态、账号缓存及实例项，不等于 191 个独立用户功能 |
| `PCLCS/Configs.cs` 配置声明 | 5 | 3 个 `ConfigEntry` 与 2 个实例 `DynamicConfigEntry` |

源仓库还通过 `.gitmodules` 引用 [MeloongCore](https://github.com/Meloong-Git/MeloongCore)。本次另核实gitlink `811438aa3c7c0437c7e60e85b516cc593dcf6cb1` 中配置提供器、路径及编码实现，用于核实旧配置来源；该迁移功能已按用户要求撤回，不能据上述数量声称已经审计或迁移了整个子模块。`FormMain.xaml/.vb`、`Application.xaml/.vb`、资源与图标也在页面目录之外。

状态含义：**局部实现**表示可定位 Rust 代码，但行为范围或 UI 仍有缺口；**开发中**表示代码正在接入，尚无完成证据；**未迁移**表示尚无对应实现；**上游公开代码缺失**表示相关私有实现或服务配置不在此提交中，需要合法取得接口信息或独立实现，不能用空桩冒充；**用户排除**表示仍保留原版清单供对照，但不纳入本次迁移待办。没有任何一行仅凭编译通过就被认定为全量等价。

## 页面逐项对照

下面完整列出43个XAML文件，每个仅出现一次。最右列同时是逐页待办清单：列出未实现功能与还缺的原版UI/状态证据，不用整体“局部实现”掩盖它们。Rust 页面位于 `crates/pcl-desktop/src/app.rs` 及其 `shell_ui`、`version_ui`、`download_ui`、`install_ui`、`resource_ui`、`appearance_ui`、`task_ui`、`account_ui`、`java_ui`、`setup_launch_ui`、`instance_setup_ui`、`pack_export_ui`、`more_ui`、`setup_system_ui`、`home_ui`、`xaml_ui`、`music`、`mod_update_ui`、`crash_ui` 子模块；同一 Rust 方法对应多个原版页面时，表示尚未还原原版页面边界。

### 启动与账号：10 个文件

| 上游文件（`Pages/` 下） | 原版行为 | Rust 对应与当前状态 | 未覆盖/验收边界 |
| --- | --- | --- | --- |
| `PageLaunch/PageLaunchLeft.xaml` | 账号切换、启动、版本选择/设置、阶段进度、下载速度与取消 | `shell_ui` 账号选择、启动与版本入口；启动前刷新有效会话；局部实现 | 设备流/恢复/失败/过期多状态已接；真实账号完整流程、原版提示与动画未等价验收 |
| `PageLaunch/PageLaunchRight.xaml` | 默认空白、自定义主页、快照提示、调试启动日志 | `home_ui/xaml_ui` 默认空白、本地/联网/固定预设与缓存、常见布局/图片/静态样式/模板及受限事件；局部实现 | 新闻预设只读获取/解析通过，不代表全部预设或 GUI；完整 WPF Binding/自动 Triggers/CLR 未实现，回声洞原句库不可得；内嵌调试日志/全部快照提示未等价 |
| `PageLaunch/PageLoginLegacy.xaml` | 可编辑历史玩家名、离线皮肤、名称提示 | 离线名字/历史、MD5 UUID；`offline_skin` 默认/Steve/Alex/正版名称/本地 PNG，Run 时模型 UUID 与资源包/选项更新；局部实现 | 不改变认证身份；正版名称方式限 1.20 前，1.6 前无自定义资源包、旧版 Alex 提示；新皮肤未实机游戏验收，账号框鼠标输入修复不等于皮肤验收 |
| `PageLaunch/PageLoginMs.xaml` | 微软登录、正版购买与官网入口 | `auth/accounts/account_ui` 设备流、真实阶段回调、安全保存/Ready后成功；登录/启动入口分类提示及恢复按钮，刷新凭据失效最多一次设备认证，网络/安全/403不自动重登；购买/官网；自有公共Client ID已保存、真实设备码成功；局部实现 | Minecraft登录HTTP403，未到权益/profile；前段Microsoft/Xbox/XSTS完成为源码顺序推断。审核已提交待审批，不认定唯一失败原因或完整正版登录通过 |
| `PageLaunch/PageLoginMsSkin.xaml` | 已登录身份、皮肤/披风、信息修改、切换账号 | 多账号列表/选择/安全移除/重新登录、恢复/闲时刷新/启动前有效性检查、在线皮肤、上传/重置与401有限恢复；披风暂选→确定/取消、29个中文名称及专用提示；恢复后核对原账号，已提交变更不自动重放；局部实现 | Keychain 仅合成凭据往返验证；真实账号、皮肤/披风变更、Windows Credential Manager 实机及全部状态像素未验 |
| `PageLaunch/PageLoginAuth.xaml` | Authlib Injector/LittleSkin 邮箱密码、记住密码、注册 | 未迁移 | 无第三方 Yggdrasil 登录和 authlib-injector 注入 |
| `PageLaunch/PageLoginAuthSkin.xaml` | 已登录第三方身份、更换角色、切换账号 | 未迁移 | 角色列表、多角色选择、第三方皮肤未迁移 |
| `PageLaunch/PageLoginNide.xaml` | 统一通行证账号密码、注册与记住密码 | 未迁移 | Nide 登录、服务器 ID、令牌生命周期未迁移 |
| `PageLaunch/PageLoginNideSkin.xaml` | 统一通行证身份、改密、切换账号 | 未迁移 | 无相应已登录页面及服务交互 |
| `PageLaunch/MyMsgLogin.xaml` | 设备登录提示控件、重开网页、复制代码、取消 | `account_ui/modal_ui` 设备码弹层、复制/重开网页/取消、Enter不触发网页与Esc取消；真实设备OAuth进入Xbox阶段才提示网页成功并关闭设备码窗，请求代次隔离；局部实现 | 阶段/陈旧事件/有限重入/账号绑定/弹窗键盘与动画有fixture；模拟数据的原生设备框/键盘已验，完整真实授权、成功后恢复、各服务错误分支与成对像素仍未验收 |

`PageLaunch/MySkin.xaml` 是下表单列的第 4 个复用子控件之一，因此本节的 10 行加后面的 MySkin 才是 `PageLaunch/` 的全部 11 个 XAML 文件。

### 版本选择与实例：9 个文件

| 上游文件（`Pages/` 下） | 原版行为 | Rust 对应与当前状态 | 未覆盖/验收边界 |
| --- | --- | --- | --- |
| `PageSelectLeft.xaml` | 多个 Minecraft 文件夹、创建/添加/移除/删除/重命名/打开/切换 | `shell_ui::folder_sidebar` 已有目录列表、添加已有目录、持久化与切换；局部实现 | 新多目录完整GUI流程待验；创建/移除/删除/重命名、各目录独立状态及旧数据迁移未覆盖 |
| `PageSelectRight.xaml` | 分类版本列表、收藏、隐藏版本、选择及右键管理 | `version_ui` 分类折叠、预设图标、描述、收藏/隐藏/F11与手动分类；局部实现 | 收藏持久化有中间 Mac GUI 证据；新全部交互态、右键管理及成对像素仍待验 |
| `PageInstance/PageInstanceLeft.xaml` | 概览、设置、初始化、Mod 管理、导出 | 216包概览/设置/Mod/导出导航已接；局部实现 | 初始化/重置全套行为仍缺；导出页可操作不等于所有包格式已迁移，导航/滚动/返回全状态需验 |
| `PageInstance/PageInstanceOverall.xaml` | 图标/分类/描述/改名/收藏、目录快捷入口、启动脚本、补全、删除 | 描述/12预设图标/分类/隐藏/收藏、目录、重命名/回收站/脚本；`install_repair` 继承文件补全及收据约束 Forge/NeoForge 生成物隔离重建；局部实现 | 缺父 JSON、无哈希/收据生成物明确拒绝；原配置与自有额外文件保留；任意图标、实际处理器重建、Windows/GUI删除及完整状态未验 |
| `PageInstance/PageInstanceSetup.xaml` | 实例隔离、Java 范围/强制选择、RAM、登录服务器、GC/参数/预命令、禁用更新或补丁 | 隔离/Java四模式/RAM/GC/信息/参数/服务器/登录限制、预命令与等待；实例标题/最大化、内存回收、JLW/LUA禁用已接；局部实现 | 本次PID窗口控制与有限标记，不冒充全原版变量；真实游戏/Windows及全部组合未验；实例禁更新开关、第三方认证另缺，内存只回收启动器自身 |
| `PageInstance/PageInstanceMod.xaml` | 搜索、解析、导入、批量启停/删除/更新、远端信息、冲突反馈 | `mods/version_ui` 扫描/导入/启停/搜索/批量选择；`mod_updates/mod_update_ui` 官方哈希识别、显式更新及依赖隔离准备、禁用保留、原件备份/回滚；局部实现 | 新更新仅夹具，未线上 Mod 更新/游戏验收；未识别文件单列；完整远端标签/冲突诊断及批量删除仍缺 |
| `PageInstance/PageInstanceModDisabled.xaml` | 不支持 Mod 的版本提示、转下载或版本选择 | `version_ui` 按已读取真实加载器、LiteLoader 或手动 Mod 分类显示不可用页/下载入口；局部实现 | 不在元数据未读完时冒充原版；真实加载器识别不等于支持其安装；新页面 GUI/像素待验 |
| `PageInstance/PageInstanceExport.xaml` | 配置式导出整合包、文件精细选择、隐私排除、协议、资源下载引用、含启动器包 | 逐项文件/资源/世界选择及排除；mrpack/MMC/MCBBS/原版HMCL；版本目录Java、当前平台PCL双ZIP；配置与无覆盖提交；局部实现 | 当前Mac派生PingFang阻止附带该程序；OptiFine/LiteLoader导出、HMCL加载器和所有高级来源策略缺；新格式只夹具，历史269 mrpack GUI证据不代表新格式/Windows |
| `PageInstance/MyLocalModItem.xaml` | 本地 Mod 项：名称/译名/描述/标签/版本/启用/选择/更新状态 | JAR名称/文件名/版本/加载器/错误与启停/选择；更新对话框另显示已识别项目/新旧版本/来源；局部实现 | 复用行全部译名/远端标签/状态图标仍非完整原版；更新流程不等于每行像素复现 |

### 下载与社区资源：10 个文件

| 上游文件（`Pages/` 下） | 原版行为 | Rust 对应与当前状态 | 未覆盖/验收边界 |
| --- | --- | --- | --- |
| `PageDownload/PageDownloadLeft.xaml` | 原版、Mod、整合包、数据包、资源包、光影包分类与刷新 | 原版/Mod/在线整合包/数据包/资源包/光影分类，本地 mrpack/ZIP 导入入口；局部实现 | 新全部格式与双源安装未做 GUI 验收；分类导航不等于每条安装链路通过 |
| `PageDownload/PageDownloadInstall.xaml` | 原版版本选择及 Forge/NeoForge/Fabric/API/OptiFine/OptiFabric/LiteLoader 组合安装 | 原版/自定义实例名、Fabric/Quilt、现代与受限历史Forge/NeoForge；OptiFine官方列表、独立安装和精确Forge组合；共用加载/重试/typed取消；局部实现 | OptiFine仅官方列表真实只读/生成物夹具，厂商安装器未跑；LiteLoader/OptiFabric/API推荐及其它OptiFine组合缺；历史universal/client、额外旧native等仍拒绝，完整恢复/像素未验 |
| `PageDownload/Resource/PageResource.xaml` | 共用搜索、源/版本/加载器/类型过滤、翻页、安装已有包与失败反馈 | Modrinth/CurseForge五类搜索、分类、分页、详情、版本；请求代次/typed取消；冻结MC百科译名/唯一中文名查询；局部实现 | 真实Modrinth中文查询通过；CF无Key未在线验，API拒分发明确失败；完整排序、多源持久缓存、全部GUI与像素未验 |
| `PageDownload/Resource/PageDownloadMod.xaml` | Mod 类型层级与搜索筛选 | 双源资源适配、必需依赖计划/精确版本冲突/复用/事务回滚；Mod更新另走显式检查与备份流程；局部实现 | 此前ModMenu+2依赖隔离实装不覆盖当前CF/更新；新组合/全部标签/游戏与GUI仍需验 |
| `PageDownload/Resource/PageDownloadPack.xaml` | 整合包类型、搜索、详情、安装 | 在线mrpack/CF ZIP及本地mrpack/MMC-Prism/基础HMCL/MCBBS/CF manifest/双层mrpack；单个Fabric/Quilt/Forge/NeoForge依赖；局部实现 | CF无Key；HMCL pack.json、MCBBS OptiFine/外部文件及无manifest普通游戏ZIP拒绝；新格式回放不等于其它启动器/游戏实机互通 |
| `PageDownload/Resource/PageDownloadDataPack.xaml` | 数据包分类、版本与下载 | Modrinth/CurseForge数据包适配，显式选择实例内有level.dat世界；无覆盖写入/回滚；局部实现 | 历史真实Modrinth下载仅写合成世界；CF无Key，未由Minecraft识别/启用或验全类别筛选 |
| `PageDownload/Resource/PageDownloadResourcePack.xaml` | 资源包风格/特性/分辨率筛选与下载 | 双源资源包分类/详情/版本与哈希下载到resourcepacks；中文索引与百科入口；局部实现 | NeoFullbright历史实装不覆盖CF；GUI安装/游戏启用、全部筛选等价仍未验 |
| `PageDownload/Resource/PageDownloadShader.xaml` | 光影风格、性能、Iris/OptiFine/原版筛选 | 双源光影分类/版本/ZIP；core shader走resourcepacks，常规shaderpacks；局部实现 | 历史MakeUp UltraFast实装/复用不覆盖CF或当前游戏；不自动安装Iris/OptiFine，完整兼容未验 |
| `PageDownload/Resource/PageDownloadCompDetail.xaml` | 项目详情、官网/MC 百科、名称复制、版本文件列表与下载 | 公共标题返回、简介/官网/复制、版本分组/文件/依赖计划；平台slug映射中文名与MC百科；局部实现 | 固定索引不是实时完整库；统一持久缓存、双源全部GUI状态及像素仍未验 |
| `PageDownload/Resource/MyResourceItem.xaml` | 社区项目复用条目、标签和元数据 | 双源统一ProjectHit元数据、来源与冻结索引中文标题；条目UI已接；局部实现 | 不是所有中文别名或远端标签；新CF条目未在线/GUI验收，不等于64DIP全部状态像素一致 |

### 设置：5 个文件

| 上游文件（`Pages/` 下） | 原版行为 | Rust 对应与当前状态 | 未覆盖/验收边界 |
| --- | --- | --- | --- |
| `PageSetup/PageSetupLeft.xaml` | 启动、联机、个性化、其他，按页初始化 | 启动/个性化/其他导航已接；旧配置迁移保持用户排除；局部实现 | 联机、各页完整重置/独立滚动状态与原版所有设置仍未迁移 |
| `PageSetup/PageSetupLaunch.xaml` | 默认隔离、标题/自定义信息、启动器可见性、进程优先级、窗口、RAM、离线皮肤、Java列表与高级参数 | 四卡布局/Java/RAM/隔离/窗口/参数；可见性/优先级/预命令；标题/最大化、仅本进程内存回收、离线五皮肤、JLW/LUA及Windows临时GPU偏好；局部实现 | Mac AX需用户已有授权；平台/游戏可拒绝，明确提示；固定补丁条件与禁用开关生效，真实游戏/Windows/完整变量和全部像素未验 |
| `PageSetup/PageSetupLink.xaml` | P2P/延迟策略、自定义节点、节点状态与贡献入口 | 未迁移 | 需要 EasyTier 进程、独立配置和跨机连通验证 |
| `PageSetup/PageSetupUI.xaml` | 主题、透明度、背景图/模糊、音乐、标题栏、主页源/预设、功能隐藏 | 15主题/自定义参数、背景透明度/模糊/13布局/GIF帧循环、窗口透明度/Logo；原生本地音乐及联动，本地/联网/预设主页；局部实现 | 1–13主题及模糊核非官方私有精确实现；Logo非原生Splash；音乐Mac静音WAV实跑、Windows仅编译，主页仅新闻只读通过；功能隐藏/部分标题栏和全状态仍缺 |
| `PageSetup/PageSetupSystem.xaml` | 源优先级、并发/限速、社区文件命名、更新/公告、缓存、遥测、导入导出、语言、调试 | `setup_system_ui/system/network`源优先级、资产线程/接入流限速、CF安全存储、游戏更新提示/首次中文、本项目GitHub更新检查/校验下载与缓存；局部实现 | 只缓存更新包、不搬游戏/Java、不自动安装；原版私有公告/遥测无请求；社区命名/完整调试/语言/重置及原键兼容仍缺，旧配置迁移不恢复 |

### 联机、更多、任务：8 个文件

| 上游文件（`Pages/` 下） | 原版行为 | Rust 对应与当前状态 | 未覆盖/验收边界 |
| --- | --- | --- | --- |
| `PageLink/PageLinkMain.xaml` | EasyTier 下载、创建/加入房间、邀请码、延迟/人数/端口、退出与错误恢复 | 未迁移；当前无入口 | 模块下载和节点选择依赖缺失的在线配置，macOS 网络权限亦需实际处理 |
| `PageOther/PageOtherLeft.xaml` | 帮助、关于、百宝箱、反馈、投票 | 当前仅保留帮助/关于；百宝箱、反馈、投票三个入口及其状态/说明弹窗按用户要求删除 | 三个已删除入口不再列为迁移缺口；保留页面的导航/返回及完整几何状态仍需验 |
| `PageOther/PageOtherAbout.xaml` | 作者/协作者/服务鸣谢、赞助、版本、更新、赞助者列表 | 上游作者/官网/赞助与固定鸣谢，Rust作者mohui666及第三方身份；本项目GitHub更新入口；局部实现 | 固定快照非实时赞助等级；本项目下载需公开可校验包，不代表原版更新服务；全部链接/头像/滚动状态待验 |
| `PageOther/PageOtherHelp.xaml` | 本地/缓存帮助目录、分类、搜索、加载与刷新 | 固定40条帮助分类/搜索/刷新，加显式事件打开的本地及远程JSON/XAML帮助；局部实现 | 原版统一在线目录版本/全部缓存和用户扩展发现规则未等价；远程读取不执行自动动作，全部失败/滚动/像素未验 |
| `PageOther/PageOtherHelpDetail.xaml` | 动态帮助详情、XAML 内容、事件入口 | 共用惰性XAML子集、图片/静态资源/模板/变量、页面导航与显式事件；受影响文件/程序/下载/设置/变量需确认；局部实现 | 无完整WPF Binding/自动Triggers/CLR；设置白名单，未知动作报错；远程模板与全部图文布局未逐页实机核验 |
| `PageOther/PageOtherTest.xaml` | 百宝箱容器 | 用户排除；入口和占位页已删除 | 原版公开代码仍缺此实现；保留本行用于43文件清单完整性，不纳入当前迁移待办 |
| `PageSpeedLeft.xaml` | 全局任务总进度、下载速度、剩余文件/线程 | 单任务真实网络字节/速度/待校验文件及active/已知并发上限；200 DIP侧栏四组，未知上限或无遥测显示— | 仅原版管线有线程遥测，不能把其它生产者猜成8；已知单计划仍阶段等权，非原版权重/字节比。全局聚合/逐任务控制和完整新GUI流程待验；下载设置的资产并发配置另已接入 |
| `PageSpeedRight.xaml` | 任务树、分任务状态/进度、取消 | 阶段/取消及六阶段自动原版四组；JobId绑定root/目标/取消状态，旧事件不清新锁，浏览失效不吞安装终态；269包逐项导出完成后按钮恢复 | JobId/旧root/取消与真实错误有fixture；该次GUI导出释放锁不等于所有安装路线终态。原版组合任务树/并行队列/持久历史、取消后重试仍待迁移或验收 |

### 皮肤复用控件：1 个文件

| 上游文件（`Pages/` 下） | 原版行为 | Rust 对应与当前状态 | 未覆盖/验收边界 |
| --- | --- | --- | --- |
| `PageLaunch/MySkin.xaml` | 64 DIP 容器、48 DIP 脸/56 DIP 帽层、皮肤加载、缓存与交互 | 官方纹理受限下载与64DIP容器/脸帽裁切；offline_skin提供五模式资源包/模型和头像读取接口；局部实现 | 真实在线账号不纳本轮；新离线头像全部状态/游戏显示未验，版本限制详见UI缺口表 |

合计：10 + 9 + 10 + 5 + 8 + 1 = 43。复用 `My*` 子控件为 `MyMsgLogin`、`MySkin`、`MyResourceItem`、`MyLocalModItem`。

## 核心模块逐项对照

| 上游文件（`Modules/` 下） | Rust 对应与当前状态 | 主要剩余范围 |
| --- | --- | --- |
| `Base/ModAnimation.vb` | 下拉/消息框/提示/加载与GIF各自接入部分动画；未迁统一动画系统 | 完整时间线/缓动组合、页面进退场和全部颜色/位置状态仍需逐项核对 |
| `Base/ModBase.vb` | `metadata/model/config` 与标准库局部替代 | 替换标记、平台工具及广泛公共方法必须按调用链核查；不能按文件名整体划为完成 |
| `Base/ModLoader.vb` | 后台线程/channel/typed取消、显式阶段计划和单任务页；局部实现 | LoaderTask/Combo依赖图、原版权重、多任务并行、重载缓存与持久历史仍缺；局部阶段进度不等于完整调度器 |
| `Base/ModNet.vb` | `install/network/transfer`HTTPS/哈希/原子落盘、已知官方路径BMCLAPI回退、资产worker线程配置与接入流全局限速/遥测；局部实现 | 分片/断点续传、所有网络生产者统一策略/遥测及全局多任务统计未覆盖；身份接口不走镜像 |
| `Base/ModValidate.vb` | 路径/ID/配置/登录输入各自校验；局部实现 | 原版所有控件校验器与错误反馈行为 |
| `Base/MyBitmap.vb` | `ui_style` 纹理/原版资产，`auth/account_ui` 在线皮肤受限下载与分层裁切；局部实现 | 完整图像缓存、缩放模式、主题处理和真实账号头像未验 |
| `Base/PclLogger.vb` | 内存日志/已知凭据脱敏；`crash`本地日志收集与ZIP导出；局部实现 | 不是全原版持久日志/级别系统；未知私人信息仍需分享前检查 |
| `Minecraft/ModCrash.vb` | `crash/crash_ui`实际PID/cwd/启动时间隔离、异常退出延后收集、25类线索/建议、手工导入与无覆盖ZIP导出；局部实现 | 未覆盖全部原规则；合成日志不是实际游戏崩溃，分析线索不当成确定根因，无自动上传 |
| `Minecraft/ModDownload.vb` | 原版/Fabric/Quilt/Forge/NeoForge/Java、受限历史Forge、OptiFine独立与精确Forge组合；继承补全及收据绑定生成物重建；局部实现 | 历史其他分支、LiteLoader/OptiFabric/API推荐、无收据修复缺；本轮OptiFine安装器/生成物重跑仅夹具，官方列表只读不等于安装 |
| `Minecraft/ModJava.vb` | java/java_selection/java_download：完整版本与范围、四种模式、原版约束、有界发现、优先/排除、官方平台下载；局部实现 | 21.0.7/21.0.12.1实际只读探测/选择通过；全部历史版本/发行版/原版缓存与Windows运行仍未验，手选/范围不是无条件exact-major |
| `Minecraft/ModLaunch.vb` | `launch/process/native_window/launch_patches/offline_skin`会话/配置优先级、预命令/窗口/进服/可见性、PID窗口控制、条件补丁、离线资源包与本进程内存回收；局部实现 | 只用本机预命令；真实游戏/Windows、全部历史版本/事件/变量仍需验；第三方认证不在本轮范围，GPU强杀启动器可能遗留临时偏好 |
| `Minecraft/ModMinecraft.vb` | 多目录/识别、收藏隐藏分类、隔离、重命名/回收站；局部实现 | 每目录完整管理/状态、全部版本识别与GUI仍缺；旧170包别名往返仅证明记录语义，不能泛化任意用户数据。旧PCL配置迁移不在当前用户要求范围 |
| `Minecraft/ModModpack.vb` | mrpack/MMC-Prism/基础HMCL/MCBBS/CF及双层mrpack导入，Forge/Neo依赖；多格式导出/精细选择、Java/当前平台PCL双ZIP；局部实现 | 未知格式/无manifest游戏ZIP、HMCL pack.json和加载器导出、MCBBS额外addon/下载、OptiFine/LiteLoader导出缺；Mac派生字体禁止随包，未新格式实机互通 |
| `Minecraft/ModWatcher.vb` | 仅本次Child/PID日志/就绪/退出、关闭按钮与可见性；PID窗口标题/最大化和异常退出本地崩溃分析；局部实现 | 单受管游戏；操作系统/游戏可拒绝窗口控制；本批真实游戏/Windows全生命周期未验，日志标记不是视觉就绪证明 |
| `ModDevelop.vb` | 未迁移 | 开发辅助与对应调试入口；不把内部工具数量计为用户功能数量 |
| `ModEvent.vb` | `home_ui/xaml_ui/more_ui`受限布局/静态资源及显式网页/复制/导航/启动/包入口等事件，写入/文件/程序/下载需确认；局部实现 | 无完整WPF/自动事件/CLR；白名单设置与有限变量，服务器参数型启动事件拒绝；不从下载内容自动执行 |
| `ModMain.vb` | `app/main/ui_style/hint_ui/modal_ui` 部分窗口/资源行为；三色提示最多20条、重复合并/刷新、进出与抖动动画；消息框显式警告/首按钮/键盘/焦点与进出动画，关闭快照不重复执行动作 | 渲染及输入fixture不等于全部消息框实机/像素验收；窗口生命周期、拖放/快捷键、异常路径及全部主界面状态仍缺 |
| `ModMusic.vb` | 本地列表/音量/随机循环/切曲/暂停/自动播放/游戏联动，Mac AVAudioPlayer与Windows MCI，清空留备份；局部实现 | Mac临时静音WAV已实跑；Windows未执行、系统codec和完整GUI未验，无联网音乐服务 |
| `ModSecret.vb` | 默认蓝色/HSL2/渐变几何由公开算法重算；15主题本地可用，1–13私有参数采用Rust近似 | 官方预设数值、虹彩周期/解锁规则未公开；不冒充官方等级/身份。微软/CF服务资格、原版私有公告/识别码仍有缺失；自有下载与更新已独立实现，见下表 |
| `Resource/LocalResourceFile.vb` | 本地JAR解析/启停，MR SHA512和CF精确指纹+官方SHA1确认、未知哈希不推断项目；局部实现 | 完整原版资源分类/缓存/全部状态未等价；CF无Key未在线验 |
| `Resource/LocalResourceLoaders.vb` | 本地扫描/批量启停与Mod显式更新；新依赖隔离准备/禁用保留/原件备份回滚/取消；局部实现 | 完整远端持久缓存、全部更新兼容组合、Mod删除与GUI恢复未验或未覆盖 |
| `Resource/ResourceProject.vb` | MR/CF五类统一模型/详情/图标，冻结MC百科中文名和slug关联；局部实现 | CF无Key；实时完整译名/统一持久缓存及全部像素状态未验 |
| `Resource/ResourceSearcher.vb` | 双源query/分类/版本/loader/分页；冻结索引唯一中文名称转换；局部实现 | 完整可选排序/模糊中文关联/热度排名未迁移；CF无Key、全部GUI交互态未验 |
| `Resource/ResourceVersion.vb` | 双源文件hash/大小、required依赖/循环/冲突/复用/事务提交；CF遵守API许可与SHA1；Mod更新独立显式备份路线 | 不自动替换普通安装中的不兼容文件；跨类别依赖及文件名推断拒绝；CF在线和全部更新/恢复GUI未验 |
| `ThirdParty/DragHelper.vb` | 未迁移 | 原版可拖动排序/列表拖放；原生选文件不等于拖放覆盖 |

| `PCLCS/` 文件 | Rust 对应与当前状态 | 主要剩余范围 |
| --- | --- | --- |
| `Configs.cs` | config使用自有JSON模型；未迁移原存储实现 | 旧PCL配置迁移按用户要求撤回；不会复制原版缓存版本/秘密provider或读取旧registry |
| `Constants.cs` | `model.rs` 等局部类型 | 所有来源/资源/加载器/登录等枚举语义需逐调用点对应 |
| `Java.cs` | java/java_selection：完整版本、排序/优先、排除、递归版本目录发现与真实探测；局部实现 | Windows实机、所有历史/架构/探测错误恢复仍需验证；当前不迁移旧Java序列化缓存 |
| `Launch/LaunchUtils.cs` | `launch_patches/native_window`受校验内置JLW/LUA和Windows临时Java GPU偏好；局部实现 | JLW仅Win Java6–18非GBK且无自定义agent；LUA仅LWJGL3.4.1+Java25；真实Windows补丁/恢复未验 |
| `Resource/Resource.cs` | `curseforge/resources`提供方ID、类别/加载器映射、搜索/详情/版本/依赖/下载；局部实现 | 需要合法用户Key；禁止分发/缺URL报错不拼CDN；无本机CF在线验收 |
| `Resource/WikiEntry.cs` | `wiki`内嵌固定上游索引，MC百科ID/平台slug/中文名、唯一全名查询转换；局部实现 | 未迁实时更新/热度尾行排名/完整模糊搜索，冻结索引可能过时 |

## 控件与视觉复现

17 个 XAML 控件为 `MyButton`、`MyCheckBox`、`MyExtraButton`、`MyExtraTextButton`、`MyHint`、`MyIconButton`、`MyIconTextButton`、`MyListItem`、`MyLoading`、`MyMsg/MyMsgInput`、`MyMsg/MyMsgSelect`、`MyMsg/MyMsgText`、`MyRadioBox`、`MyRadioButton`、`MySearchBox`、`MySlider`、`MySliderDot`。

另外 17 个只有 VB 的文件为 `Behaviors/ClipboardInterceptor`、`Behaviors/Interfaces`、`Behaviors/LazyLoadBehavior`、`MyCard`、`MyComboBox`、`MyComboBoxItem`、`MyDropShadow`、`MyImage`、`MyMenuItem`、`MyPageLeft`、`MyPageRight`、`MyResizer`、`MyScrollBar`、`MyScrollViewer`、`MyTextBox`、`MyTextButton`、`MyVirtualizingElement`。本项列举的是文件，`Interfaces` 不是可见控件。

Rust 已参照部分原版控件的几何与默认配色绘制按钮、导航、卡片、输入框、滑块、图标及静态皮肤。本机新增真实PingFangSC-Semibold轮廓（weight 600），与Regular（400）均通过当时源码字符的ab_glyph轮廓检查；这不等于卡片GUI正确切换字体族或Windows字体已验收。原版控件的键盘焦点、禁用/错误/选中/悬停/按下状态、弹窗、滚动虚拟化、拖拽及动画还需逐项对应。复用 egui 的同名控件不等于迁移了原版控件。

304源码批新增 `modal_ui` 原版消息框选项及入场/退出渲染，警告标题/按钮和遮罩独立指定；设备码弹层保留复制/网页按钮、不由Enter自动开网页。`hint_ui` 以Info/Success/Error语义展示最多20条提示，重复合并刷新，忙碌期间仍可保留网页成功与后续失败两条真实事件。披风选择使用单选草稿与确定/取消，未确认不提交；29个原版披风中文别名已接。本批Mac原生生产渲染器的模拟状态已检查普通/警告/设备码/输入框与三色提示；360ms旋转入场内暂缓指针提交，键盘可用。未做完整进出动画轨迹或成对像素验收，更不是实际授权或披风提交。

277批新增 `MyExtraButton` 任务/关闭游戏统一浮动堆叠：圆直径40 DIP、中心距50、右下15，主题Color3/4/8及原关闭图标；稳定ID防止显隐后焦点串用。`loading_ui` 依据 `MyLoading` 与两个资源页分别实现镐子/碎屑/错误叉、16 DIP全文、400ms防闪/最短展示、列表自然卡片与详情剩余区域居中、错误控件点击重试原请求；没有真实分数时不显示百分比。原版全部入场/恢复/队列动画及成对像素检查仍不在本批完成结论内。

固定参考图、来源限制、原版像素参数与运行时色值见 [reference/sources.md](reference/sources.md)。当前主页确认了 48 DIP 顶栏、300 DIP 左栏、底部 260×54/125×35 按钮、空白主区与默认渐变的方向。17:10 截图发现的名字框下拉箭头与主窗圆角已在17:30的 `ui-home-corrected.jpg` 中可见；导航内边距/各图标缩放、左栏阴影、按钮 idle/hover 和输入框状态仍需同基准逐状态测差，不能把可见改进当全页等价。

`test-output/ui-home.png` 实际编码为 JPEG（1728×1032，2× 对应 864×516），内嵌名为 `Display` 的 ICC profile。Windows 来源图使用不同色彩标记。JPEG 的量化与跨 ICC 显示使当前图不适合无损逐像素验收；其原始蓝色像素接近源码默认值，不能仅凭查看器里偏青就断言 Rust RGB 算错。`ui-settings.png` 当时实际截到多人页，不能作为设置页验收。当前没有同版本、同尺寸、同状态、同色彩空间的全页面成对截图；不宣称全页像素一致。macOS 字体使用用户允许的苹方替代，字体栅格本身不作为 Windows 微软雅黑逐像素相等的依据。

历史 `ui-mods.jpg` 显示空Mod列表，`ui-mrpack-preview.jpg` 显示无额外Mod的测试包预览；`modrinth-gui-ferritecore-installed.jpg` 已用 `view_image` 确认真实FerriteCore条目、中文与启用状态。图片为JPEG，不能作为无损逐像素对照；启停操作和引擎运行有另行记录。用户随后提供Windows PCL 2.13.1.1、125%缩放的六张原图，已用于静态几何校准，包括版本选择、概览与Mod管理对应布局；Mac仍在实机复查。新多目录、背景和批量操作未完成全流程GUI验证，亦尚未完成新参考的成对像素验收。

早期Mac包的 `modrinth-layout-left-aligned.jpg`、`install-components-left-aligned.jpg`、`install-forge-list-expanded.jpg` 已复看：搜索卡片、组件标题左对齐，中文与加粗标题可见，Forge实际列表有52.1.16。主任务另实测选择与清除，但未点击安装。这批仍是JPEG可见性/操作证据，不能写成无损像素验收，也不自动覆盖后续Windows参考几何修正。

### 跨页UI收敛清单

上述43行最右列保留各页具体缺口，以下是会同时影响多页、必须单独复核的共同项：

- 版本/目录/下载/资源/帮助页：原版卡片与行的展开/折叠、空态、错误/加载、滚动位置、返回层级和键盘焦点；目前只核过部分静态几何与局部交互。
- 账号页：304源码已接分类消息/恢复动作、有限重新认证与原账号绑定、皮肤401续办及披风确认；仍需未登录/设备码/pending/拒绝/过期/恢复失败/多账号切换/皮肤上传/披风的完整真实账号与成对像素验收，不能由合成事件跳过。
- 安装和任务页：原版组合兼容推荐、各子任务权重/层级、取消重试、完成/失败返回；当前四组展示仍是单任务受限映射，没有完整调度器。
- 导出与关于页：216包的居中/遮挡等问题已修，257历史包已观察高级卡/按钮分离和作者鸣谢可见；仍须复核新包全部状态，不能据可见性宣布像素通过。
- 所有页面：按钮idle/hover/press/disabled、输入校验/选择、弹窗、滚动条/拖拽/动画需相同状态参考；Mac苹方替代是已接受的字体差异，Windows微软雅黑及DPI仍要实机验证。

## 设置与数据兼容

Rust 全局 `Settings` 已覆盖游戏目录/目录列表、背景、15主题及色调/饱和度/亮度/渐变、Java、手动/自动内存、窗口策略/尺寸、启动器可见性/进程优先级/全局预命令及等待、背景透明度/模糊/布局、窗口透明度/启动图标、默认隔离、GC、自定义信息/JVM/游戏参数、离线名字/历史/皮肤、游戏标题/最大化/本进程内存回收/补丁禁用/Windows GPU偏好、音乐/主页、下载策略、system更新与缓存、版本选择与 Microsoft Client ID。实例 `versions/<id>/PCL-Rust/instance.json` 支持隔离/Java/内存/窗口/GC覆盖、服务器/登录限制、实例预命令及等待、标题/内存回收/补丁禁用、描述/收藏/隐藏/图标/分类，保存时保留自有未知键。全局和实例预命令分别执行，实例不会覆盖掉全局命令；加载的下载元数据不作为命令来源。自有模型不是原版191键的全兼容模型；旧PCL配置迁移核心、桌面入口与独占依赖已按用户要求整体撤回，不读取注册表/原版账号配置，不把迁移历史检查列为当前功能。下面完整保留191键用于界定原版行为范围。

账号目录另存非秘密资料与选择状态；Microsoft 刷新凭据只交 macOS Keychain / Windows Credential Manager，Minecraft access session 只在内存。恢复与启动前检查会刷新，失败不使用旧会话或退离线。没有复制原版私有凭据。合成 Keychain 往返不等于真实账号授权/续期成功。

| 原版设置前缀 | 数量 | 当前对应与缺口 |
| --- | ---: | --- |
| `Ui` | 41 | 15主题、自定义参数、全局调色、本地背景/彩色开关已接自有持久配置；预设1–13是Rust近似，非官方私有参数还原；模糊/背景布局/透明度/图标开关已接；音乐与受限本地/联网主页已接；功能隐藏/完整标题栏及原键兼容未迁移 |
| `Cache` | 35 | 部分自有下载缓存；原版账号、资源、版本及 Java 缓存不兼容 |
| `Launch` | 27 | 名字/Java/动态RAM、默认隔离、默认/全屏/自定义/跟随窗口尺寸、信息/JVM/游戏参数/GC已有自有配置；可见性/优先级/预命令及等待已接；标题/最大化、离线皮肤与条件JLW/LUA已接；全历史版本实机未验；不做旧键导入 |
| `Version` | 26 | 实例模型及覆盖启动已实现一部分；Java四模式已有；实例预命令/等待已接；实例补丁禁用已接；第三方认证/实例禁更新开关仍缺；使用自有配置，不做旧键导入 |
| `System` | 16 | 自有system模型有游戏更新基线提示、首次中文、本项目GitHub更新/校验下载与缓存；原版公告/遥测/调试及全部计数状态未迁移 |
| `Hint` | 14 | 未迁移原版提示已读/条件状态 |
| `Tool` | 14 | network配置分开文件/清单源、资产线程和接入流限速；双源资源/冻结中文索引；社区命名、完整调试/缓存/排序与原键兼容仍缺 |
| `Login` | 9 | 离线与微软多账号/安全凭据/有限恢复、原账号绑定、皮肤披风API及确认/分类提示局部实现，完整真实账号未验；第三方/Nide/角色未迁移 |
| `Link` | 4 | 联机配置未迁移 |
| `Window` | 2 | 原版启动器窗口尺寸持久化尚未等价；旧配置导入已排除 |
| `Identify`、`April`、`Potatoes` | 各 1 | 设备识别、活动与赞助相关，公开代码有缺失 |

实例布局已提供隔离开关和新装默认策略：隔离时优先保留已存在 `instances/<id>/`，否则用 `versions/<id>/`；关闭隔离时用根目录。切换不会搬移/复制存档，整合包保持独立。选择现有 `.minecraft` 不等于导入原版设置、皮肤或账号，原数据兼容仍需逐项验证。

## 外部服务与公开源码缺失

以下是具体依赖，不是缩减完整迁移目标的理由。能独立实现的部分仍应推进；需要服务身份或真实账号的部分保留清晰边界，不借用、提取或伪造官方私有凭据。

| 功能 | 上游证据 | 当前障碍与合理完成路径 |
| --- | --- | --- |
| 微软正版登录 | `ModSecret.vb` 第10行从 `PCL_MS_CLIENT_ID` 取值 | 自有公共客户端ID已保存，真实设备码成功但Minecraft `login_with_xbox` HTTP403，不能确定唯一原因。已按用户确认提交AppID审核、待审批；权益/profile、真实凭据保存后重启恢复和续期仍需验收，注册/提交不代表API资格已批准 |
| CurseForge | 同文件第 12、35 行为 `PCL_CURSEFORGE_API_KEY` 与 `x-api-key` | 双源适配与Key安全存储已实现；使用合法自有Key，API不许分发/无URL则明确拒绝，不拼CDN地址。本机无可用Key，只有夹具/静态防护，不能写线上CF通过 |
| 官方更新/联网公告配置 | 同文件 `Update*`、`DownloadLatestPCL` 为空；`ServerLoader` 只记日志 | 已独立接本项目GitHub Releases稳定版、匹配平台且SHA256/大小完整的包下载；没有release/匹配包会明确显示。只下载不替换应用；原版私有公告/遥测服务未接，不冒充官方更新 |
| EasyTier 房间/节点 | `PageLinkMain.xaml.vb` 使用 `ServerConfig("Link")` 的版本、下载 URL、发现/中继/强制节点与黑名单；公开节点 API 部分可见 | 公共 EasyTier 不代表官方服务配置已提供；可独立接入官方公开二进制与自有/公开节点，并做两端实机连通、退出清理、错误恢复测试。当前没有联机实现 |
| 赞助/主题解锁/识别码 | `CurrentRank=None`；`InputPotatoCode`/`GeneratePotatoCode` 空；`GetIdentify` 固定零；`ThemeUnlock=False`；预设参数分支未公开 | 用户要求15主题本地直接可用，Rust提供近似预设/独立虹彩，不调用官方解锁或伪造等级、识别码及身份；原作者/赞助/第三方与许可标识保留，官方私有参数无法据此声称精确 |
| 匿名上报 | `ClsBaseUrl` 为空 | 不得假定存在可用的官方写入端点；若独立实现，需相应服务与用户可见设置 |
| 百宝箱/反馈/投票入口 | 百宝箱公开XAML/后台无完整实现；原更多栏列有三个入口 | 用户已明确删除三入口；本地入口/关联状态及说明弹窗已移除，不再作为服务障碍或迁移待办 |
| 自定义主页/帮助 | `PageLaunchRight` 与 `ModEvent` 公开动态 XAML/事件协议 | 本地/联网文档、图片、静态样式/模板、有限替换/事件已接；有外部后果的操作需用户点击再确认。完整WPF/自动Triggers/CLR、私有回声洞句库及所有预设仍不等价 |

许可记录见 [upstream.md](upstream.md)。上游不是 MIT；公开源码可读不等于官方品牌、私有服务、字体或其他资产可不受限制重新分发。

公开仓库首提交 `adfc9cc033b4054396a425489486843906395042` 提供独立介绍，`b4e82a61bf51686f50036ce1218cd7ac5a8a4f96` 更新审核已提交；历史 `4c8bcfbc75a3ee409f4a61742969e7f9bf523546` 整理发布6份Markdown且远端文件哈希核对一致。历史 `85aa0c0acd4392c28cbc6624263be38abfdc0df9` 按用户要求公开源码与文档，本地副本为 `test-output/github-project-page`，原开发目录仍非Git仓库。公开副本只作cargo fmt规范化、本机路径脱敏及CI取消打包/上传，未改变产品功能；其304测试、严格Clippy与release编译通过，见 `test-output/source-publication-validation.json`。公开CI独立验收，未由本地结果推定通过；发行包、本机字体、原日志与账号/游戏数据未发布，自定义许可和资产的二进制分发条件仍需核对。

## 验证证据与尚未完成的验收

| 证据 | 可以支持的结论 | 不能支持的结论 |
| --- | --- | --- |
| `test-output/nonlogin-parity/account-dropdown-anchor-validation.json` | 本轮Mac真实鼠标打开并选择两处账号菜单、名字输入及下载名称/分类/连续版本输入，修复定位/命中问题 | 没有登录API请求；不是全部输入控件、像素或Windows验收，后续新构建仍须复核 |
| `test-output/nonlogin-parity/network-live.log` | 只读官方/镜像原版清单、Fabric及OptiFine列表、Modrinth中文搜索均返回实际数据 | 无JAR安装/游戏执行，不覆盖CF；本机CF没有Key，不能声称线上通过 |
| `test-output/personalization-home-crash/` | 合成XAML/主页/崩溃/生命周期夹具；Mac临时静音WAV真实播放/暂停/音量；固定新闻主页只读获取解析 | 不是所有音频/联网预设/GUI、真实游戏崩溃或Windows运行；未点击远程动作，不自动上传日志 |
| `test-output/pack-formats-tests.log`、`generated-repair-tests.log`、`optifine-tests.log` | 常见整合包隔离回放/无覆盖/取消/敏感排除，继承与收据绑定生成物发布、OptiFine官方行和生成profile校验 | 合成数据不证明厂商OptiFine安装器/修复处理器实际运行；未知格式/缺收据仍拒绝，CF与新格式跨启动器实机未验 |
| `test-output/microsoft-app-setup-validation.json`、用户403截图及 `minecraft-app-review-submitted.jpg` | Entra个人账号/公共客户端保存、本机Client ID持久化、真实设备码成功；完成尝试停在Minecraft HTTP403；用户确认后审核已提交，网页回执已查看 | 前面Microsoft/Xbox/XSTS成功为源码顺序推断；403唯一原因未知，未到所有权/profile及凭据恢复，回执不等于批准 |
| 公开 `mohui666/PCL-Rust` 的历史文档提交4c8bcfb与 `test-output/docs-publication-validation.json` | 当时6份整理文档的远端main/文件blob与本地提交一致 | 这是源码公开前的文档发布记录，保留原时点 |
| 源码提交 `85aa0c0acd4392c28cbc6624263be38abfdc0df9`、`test-output/source-publication-validation.json` | 用户要求的源码+文档146文件（47个Rust文件）已公开，远端HEAD/所有blob核对一致；副本格式、304测试/1忽略、严格Clippy与release编译通过；产品功能未变 | 原目录仍非Git；无发行包/本机字库/原日志/账号游戏数据。公开CI不打包上传，其运行结果须另核；不把138输入的旧双端安装包验收移用于公开副本 |
| `test-output/login-feedback-checks.json` / `login-feedback-tests.log` / `login-feedback-clippy.log` | 历史304通过、0失败、1忽略（核心211/桌面93），工作区全部目标严格Clippy通过；138冻结输入保持；分类/恢复/原账号绑定、披风草稿确认、提示及消息框输入有fixture | 合成凭据/事件/本地响应不是真实授权、线上皮肤变更或像素验收 |
| `test-output/login-feedback-macos-validation.json` / `login-feedback-windows-validation.json` | Mac签名主程序16191744字节、Windows18544640字节，尺寸/hash已独立复算，见validation；138冻结输入保持，Windows PE32+ x86-64 GUI/29DLL | Windows/Wine未运行，构建与签名不证明真实账号或像素等价 |
| `test-output/login-feedback-ui-validation.json` 与6张JPEG | Mac原生生产渲染器的模拟普通/警告/设备码/输入框及三色提示；新生产应用启动/设置中文可见，13号主题与设置完整hash保持；截图尺寸/hash已核对 | 未发账号网络请求或操作凭据；未真实登录、皮肤/披风变更。360ms旋转入场暂缓指针提交；完整动画轨迹、Windows及成对像素未验 |
| `test-output/shutdown-loading-tests.log` / `shutdown-loading-clippy.log` / `shutdown-loading-checks.json` | 历史277通过、0失败、1忽略（核心209/桌面68），工作区全部目标严格Clippy通过；136冻结输入未变 | 不代表多进程、完整Loader/动画、Windows运行或全页像素验收 |
| `test-output/shutdown-loading-macos-validation.json` / `shutdown-loading-windows-validation.json` | Mac签名主程序16109568字节、Windows18481152字节，大小/哈希独立复算与记录一致；冻结输入保持，Windows PE32+ AMD64 GUI/29系统DLL | Windows/Wine未执行，静态构建/签名不证明授权、游戏或像素等价 |
| `test-output/shutdown-loading-gui-validation.json` 与6张所列JPEG | 277包Mac首页/设置页电源按钮可见，设置页关闭PID21840后确认进程消失/入口恢复；真实Mod/Fabric API详情/资源包列表成功，镐子卡片frame1→结果frame2；设置键未变 | 没有mock或人为延迟；全动画轨迹、真实网络失败重试、游戏视觉/世界、微软授权、Windows和像素等价未验。截图已逐张复看/核hash，空白防闪帧不作加载证明 |
| `test-output/shutdown-process-tests.log` 与浮动按钮/加载fixture | 5项进程测试确认仅kill自有Child、自然退出文案、日志管道不阻塞；真实egui输入确认浮动堆叠/显隐后Space不串操作，错误控件重试与400ms展示检查 | 临时进程及合成UI状态不是Minecraft、线上请求成功/失败、Windows或像素实机证明 |
| `test-output/themes-author-tests.log` / `themes-author-clippy.log` | 历史269通过、0失败、1忽略（核心209/桌面60），工作区严格Clippy通过；134冻结输入未变 | 不代表后续构建、原版全部功能、真实账号、Windows运行或全页像素验收 |
| `test-output/themes-author-macos-validation.json` / `themes-author-windows-validation.json` | Mac签名主程序16109664字节、Windows18501632字节，哈希已独立复算，详见validation；Windows PE32+ AMD64 GUI/29 DLL | Windows/Wine未执行，Mac静态签名/构建不能替代实际GUI、Java、授权或游戏验收 |
| `test-output/themes-author-gui-validation.json` 与7张同前缀JPEG | 269包Mac15主题逐项选择保存、自定义重启恢复、作者/两项更多栏/内存布局/缺Client ID提示关闭；离线启动进程及从UI关闭 | 滑条实机输入未验，CUA坐标失败且AX未改值；没有微软授权/游戏视觉或世界/Windows运行/像素等价。用户后选13号主题已保留，其余全局设置语义未变 |
| `test-output/themes-author-export-items-validation.json` / `themes-author-export-items-macos.jpg` | 269包Mac真实逐项导出2512字节mrpack；同名资源目录区分、非空目录、光影及其附属设置选入，敏感备份样本排除；8override与来源一致，14合成源文件未改；导出按钮恢复 | 本次EmbedAll无外部哈希上传，包CRC/大小/SHA256/所选目录已独立复核；没有回导/游戏或Windows运行，不证明远端引用与全部格式 |
| `test-output/ui-fidelity-forge-tests.log` / `ui-fidelity-forge-clippy.log` | 历史237通过、0失败、1忽略（核心199/桌面38），严格Clippy通过；配置迁移已撤，整合包导入保留 | 不代表当前构建、完整原版功能覆盖率或全部GUI/真实账号验收 |
| `test-output/ui-fidelity-forge-macos-validation.json` / `ui-fidelity-forge-windows-validation.json` | Mac签名主程序16043280字节、Windows18363904字节，hash已独立复算；131输入保持，Windows PE32+ AMD64 GUI/29DLL | Windows/Wine未执行；不由静态包推断字体、Java、安全存储、游戏或像素一致 |
| `test-output/ui-fidelity-forge-no-legacy-import-macos.jpg` | 历史237包设置侧栏完整可见，“其他”禁用，旧配置入口已撤 | 不证明当前包、所有设置/任务或原整合包安装重新验收 |
| `test-output/migration-ui-import-*-validation.json` | 历史257包及被撤功能的证据保留 | 配置迁移已由用户排除，不计当前功能；该包hash与测试数不代表当前构建 |
| `test-output/java-export-help-tests.log` / `java-export-help-clippy.log` | 历史216通过、0失败、1忽略（核心186/桌面30），严格Clippy通过 | 不覆盖后续task/历史Forge/GUI修正，不是当前源码验收或功能覆盖率 |
| `test-output/java-export-help-macos-validation.json` / `java-export-help-windows-validation.json` | Mac签名包15877440字节、Windows18193920字节；hash见validation；Windows126输入构建前后相同、29唯一系统DLL | Mac字体脚本在216测试后修过非BMP；Windows/Wine未运行，公开发布/真实微软/像素等价均false |
| `test-output/pack-export-gui-validation.json` / `launch-script-gui-validation.json` | Mac GUI导出隔离mrpack四个override/排除2项，来源不变且未上送hash；离线.command采用实例Java，权限0700 | 脚本未执行、包未回导游戏；全内嵌一次成功不证明托管引用、完整格式或WindowsGUI |
| `test-output/java-selection-validation/` | 本机两Java实际probe、Auto/Range及CLI显式路径优先语义；无设置改动 | 没运行Minecraft；四策略所有GUI状态/Windows尚待验 |
| `test-output/legacy-forge-validation.json`、`middle-forge-validation.json`、`task-display-source-tests.log` | 历史Forge两分支新增7+6项合成fixture，Forge总22通过；任务展示9fixture | 不代表真实旧installer/游戏或四组任务页完整GUI通过；旧配置迁移证据仅保留历史，不计当前功能 |
| `test-output/accounts-settings-resources-macos-validation.json`、`accounts-settings-resources-windows-validation.json` | 历史170包Mac14771200字节、Windows16962048字节；Windows29系统DLL、94输入hash构建前后一致 | Windows/Wine未执行；Mac只验证该时点已列流程，不是新源码/包或全量像素验收 |
| `test-output/accounts-settings-resources-tests.log` / `accounts-settings-resources-clippy.log` | 历史170通过、0失败、1忽略：核心150、桌面20；严格Clippy通过 | 测试数不等于原版功能覆盖率，不代替GUI、真实授权、Windows或像素验收 |
| `test-output/account-storage-validation/result.json` / `keychain-result.json` | 合成刷新凭据经macOS Keychain写入/读取一致后移除；auth9/accounts8 fixture通过 | 未使用真实凭据、未进行Microsoft授权；Windows只有交叉检查 |
| `test-output/java-runtime-live/result.json`、`java-runtime-gui-validation.json` | Mac ARM64官方Java21.0.7隔离实装145文件/104202936字节校验；后续同一集成包GUI列表→下载→完成，默认runtime145文件复验/探测通过、原JDK不变 | 未启动游戏或Windows运行；Java下载取消/错误与全部选择分支未GUI验收 |
| `test-output/community-resources-live/` | 新四类下载双哈希、Quilt在线包17个Mod/计划；ModMenu+2必需依赖真实安装及第二轮0写入复用；ZIP计划下载/复用 | 未启动Quilt/新依赖组合或验证四类GUI；数据包目标是合成世界，光影未启用 |
| `instances::tests` 11项、`deletion::tests` 9项与 `test-output/deletion-native-validation/result.json` | 重命名/回滚/引用及冲突保护；仅临时测试目录真实移入macOS废纸篓，共享fixture未变 | 没有删除用户版本；不是GUI删除/Windows回收站实测；重命名另有旧包测试别名往返，部分失败仍保留准确报告 |
| `test-output/install-1.21.1.log` | 曾真实从官方源安装并校验 1.21.1 及资源 | 此日志早于现代 native classpath 修复；不能证明修复后安装路径、其他版本/所有加载器或 Windows 安装 |
| `test-output/launch-plan.txt` | 本次保存过启动参数诊断 | 参数存在不等于游戏成功、认证成功、世界可玩或所有平台兼容 |
| `test-output/minecraft-1.21.1-engine.log`，原位置为本机 `instances/1.21.1/logs/latest.log`，17:04/17:05 | 修复现代native后经Rust UI启动，Render thread、LWJGL、OpenAL、声音引擎及纹理图集实际加载；审计已读取 | 无主菜单/世界截图，无世界创建/保存/重进证据；不是Windows实测，后续Java修复/包运行另有记录 |
| `test-output/ui-home-corrected.jpg`、`ui-mods.jpg`、`ui-mrpack-preview.jpg` | 已实际打开主页、空 Mod 管理页及包预览，中文可见 | JPEG/ICC/参考版本状态差异下不能宣称无损逐像素一致，更不能代表未截页面或之后新构建 |
| `test-output/unit-tests.txt`，17:40:16历史快照 | 70 通过、0 失败：核心68、桌面2；核心另1项 live 默认忽略；当时 fmt/Clippy 通过 | fixture 不替代真实账号、两端联机或游戏运行；此结果对应保存时的源码，不自动覆盖新增模块和后续改动 |
| `test-output/workspace-tests-final.log`、`workspace-clippy-final.log`，早期快照 | 当时87通过、0失败、1忽略：核心84、桌面3；Clippy及当时fmt check通过 | 文件名中的final不是后续源码完成标记；不自动覆盖Windows参考修正/新功能，也不能替代真实账号、世界运行或Windows实机 |
| `test-output/windows-reference-ui-tests.log`、`windows-reference-ui-clippy.log`，历史快照 | 当时94通过、0失败、1忽略：核心88、桌面6；Clippy通过 | 覆盖对应源码fixture及静态检查，不证明任务页或新增多目录/背景/批量操作的完整GUI流程与像素等价 |
| `test-output/task-ui-workspace-tests.log`、`task-ui-clippy.log`（历史） | 当时102通过、0失败、1忽略：核心92、桌面10；Clippy通过；包含阶段重置、未知统计、真实字节采样及取消分类fixture | 不替代真实GUI所有终态、多任务调度、Windows执行或像素等价 |
| `test-output/task-ui-macos-build.log`、`task-ui-live-download-macos.jpg`、`task-ui-cancelled-macos.jpg` 与主任务操作记录 | Mac构建通过；26.3下载阶段/速度/剩余变化、返回重进、×取消约0.7秒回组件页；旧1.21.1快照29文件哈希保持不变 | 26.3.json尚未提交，不能记录26.3安装完成/游戏启动；JPEG不是无损像素证明，别名实例完成与隔离失败另列 |
| `test-output/task-ui-completed-instance.json`、`task-ui-completed-return-macos.jpg`、`task-ui-failed-macos.jpg` | 别名PCL-Rust-任务完成检查继承1.21.1，实例存在且29原文件未变；完成后返回列表。隔离failure-root内libraries常规文件触发Not a directory，失败页实际可见；配置已恢复原game_root | 不是26.3成功、游戏启动、正常根目录损坏或完整故障恢复验收；只覆盖本次登记成功和隔离故障，不能泛化所有终态 |
| 历史任务页Mac包与 `test-output/windows-task-ui-validation.json` | 当时Mac主程序13772896字节、Windows15781376字节，当前SHA256已独立复算，详见validation；Windows为PE32+ x86-64 GUI、26系统DLL且无额外MinGW运行库 | Windows仅交叉构建静态检查，Windows/Wine均未执行；旧引擎日志不能绑定到当前Mac包，真实账号登录仍未验收 |
| `loaders.rs` 10个普通测试、另1项此前独立执行的Fabric live，以及实际安装产物 | 官方profile/Maven/继承、Quilt双mapping、取消、既有用户profile只读复用/冲突保护；Fabric 0.19.5 + 1.21.1的8个JAR大小与SHA1复验通过，后续测试包实际加载Fabric | 本条旧fixture/Fabric证据不覆盖Quilt；本轮Quilt隔离安装另列，仍未启动游戏；不覆盖Forge处理器或全部Mod组合 |
| `test-output/forge-live-validation.json`、`forge-live-launch-plans.log` | Forge52.1.16/NeoForge21.1.255+MC1.21.1隔离真实安装，3/6个client processors、61/77文件复核；40/63参数plan无未展开占位符 | 无游戏启动、GUI安装或Windows验收；后续GUI版本选择另有记录；NeoForge生成物只有本地哈希，不能称官方输出SHA1校验；不覆盖旧格式/全部版本/组合安装 |
| `resources.rs` 6个定向测试、`test-output/resources-live-smoke/result.json` / `result.txt` | 真实过滤分页/项目/版本与空过滤/图标；LazyDFU双哈希实装；改名文件仍识别重复项目；缺required依赖的ModMenu拒绝且0写入 | 本组是核心隔离测试，未启动LazyDFU游戏；不是自动递归依赖解决/更新，不覆盖CurseForge或中文关联 |
| `test-output/modrinth-gui-ferritecore-installed.jpg`、`minecraft-fabric-ferritecore-engine.log` 及实际实例JAR | Mac GUI搜索/详情/安装FerriteCore7.0.3、禁用/启用SHA1不变；123450字节及双哈希复验匹配官方；18:27实际加载Fabric0.19.5、FerriteCore、LWJGL/OpenAL/atlas | 没有游戏主菜单/世界操作，GUI取消/错误恢复和其他Mod组合未验收；JPEG不证明像素等价；引擎记录对应当时运行包，后续布局/修复重建需区分 |
| `test-output/fonts/weight-validation.json` / `weight-validation.txt` | 两种字面736glyph、当时582源码字符覆盖，581非空白轮廓逐字可渲染；Regular400/Semibold600的真实alpha不同 | 字体仅本机；不证明后续文案、GUI选中字重或Windows微软雅黑像素一致 |
| `mods.rs` 4个fixture + `test-output/mrpack-live-smoke/result.json` | 真实LazyDFU下载14,531字节、官方SHA1/SHA512一致、overrides正确、Fabric元数据读取和启停往返字节不变 | 这是核心隔离导入/管理实测，不能当GUI实际Mod行操作或Minecraft含Mod运行验收 |
| `test-output/gui-smoke.mrpack`、实际JSON/配置文件与 `minecraft-fabric-mrpack-engine.log` | GUI安装无Mod测试包成功，继承Fabric 0.19.5，配置属于自己的实例；17:50/17:51实际加载Fabric、LWJGL、OpenAL和atlas | 此包没有下载实际Mod，未验收主菜单/世界；不能与LazyDFU核心下载测试合并成含Mod包游戏通过 |
| `test-output/java-probe-diagnosis/` 与原生JDK目录选择实操 | 选择同一完整JDK后0.45秒检查通过，随后包引擎启动成功；同一构建退出重启、无需重选后0.43秒再过；无延长超时/系统权限变更 | 原dyld/TCC采样不足以单独证明完整根因；未来重新构建/重签名后的访问行为需另验 |
| 早期Mac包、`test-output/macos-release-build-final.log` 与当时搜索/组件截图 | 当时arm64 release构建、签名验证、重开中文/界面通过；历史主程序SHA256见验证记录 | `dist/PCL Rust.app`会随重建变化；旧包hash/截图/引擎日志不自动对应本轮集成包，当前hash单独记录 |
| 早期Windows包、`test-output/windows-release-validation.json` 与当时PE/导入表记录 | 当时release交叉构建完成，PE32+ x86-64 GUI；15441920字节与历史SHA256已复算，26个系统DLL导入，无额外MinGW运行库 | 此旧记录不代表本轮集成EXE。Windows/Wine均未执行，字体、Java/路径/UAC/网络行为未验收 |

完成当前非登录批次仍须：逐行补齐上述确切缺口；在重新构建/重签名后重验启动路径；对新OptiFine/修复处理器/含实际Mod包格式进行实际安装与启动；补足窗口/皮肤/更新/音乐/主页关键状态及Windows实机，建立同状态参考图。完整项目目标另有真实认证与两端联机，不能在本轮默认纳入已验收。旧PCL配置迁移、百宝箱、反馈和投票入口已由用户排除。不可用一个主页截图、单个1.21.1启动、EXE交叉编译或测试数量代替这些结果。

Fabric本次实际安装的profile在 `~/Library/Application Support/pcl-rust/game/versions/fabric-loader-0.19.5-1.21.1/fabric-loader-0.19.5-1.21.1.json`。文档审计另行只读复验：`inheritsFrom=1.21.1`，8个 `libraries[].downloads.artifact` 对应文件大小与SHA1全部符合profile。后续GUI测试包继承它，已实际加载Fabric与游戏引擎，证据见上表。

文档自身已脚本检查：43 个上游 XAML 路径在页面表中各出现一次、191 个键全部列出、上述文件数量与当前固定源码相符、所有 Markdown 表格列数一致。

## 可复查的设置键清单

以下清单直接取自固定上游 `Pages/PageSetup/Settings.vb`，用于防止遗漏。缓存与内部标志仍保留其原名，不将其误报成 UI 选项。Java 新配置另列于末尾。


### April（1）

`AprilYear`。

### Cache（35）

`CacheDrops`, `CacheConfig`, `CacheExportConfig`, `CacheSavedPageUrl`, `CacheSavedPageVersion`, `CacheMsOAuthRefresh`, `CacheMsAccess`, `CacheMsProfileJson`, `CacheMsUuid`, `CacheMsName`, `CacheMsV2Migrated`, `CacheMsV2OAuthRefresh`, `CacheMsV2Access`, `CacheMsV2ProfileJson`, `CacheMsV2Uuid`, `CacheMsV2Name`, `CacheMsV2Expires`, `CacheNideAccess`, `CacheNideClient`, `CacheNideUuid`, `CacheNideName`, `CacheNideUsername`, `CacheNidePass`, `CacheNideServer`, `CacheAuthAccess`, `CacheAuthClient`, `CacheAuthUuid`, `CacheAuthName`, `CacheAuthUsername`, `CacheAuthPass`, `CacheAuthServerServer`, `CacheAuthServerName`, `CacheAuthServerRegister`, `CacheDownloadFolder`, `CacheJavaListVersion`。

### Hint（14）

`HintDownloadThread`, `HintNotice`, `HintDownload`, `HintHide`, `HintHandInstall`, `HintBuy`, `HintClearRubbish`, `HintUpdateMod`, `HintCustomCommand`, `HintCustomWarn`, `HintMoreAdvancedSetup`, `HintIndieSetup`, `HintExportConfig`, `HintSnapshot`。

### Identify（1）

`Identify`。

### Launch（27）

`LaunchSkinID`, `LaunchSkinType`, `LaunchSkinSlim`, `LaunchFolderSelect`, `LaunchFolders`, `LaunchArgumentTitle`, `LaunchArgumentInfo`, `LaunchArgumentJavaSelect`, `LaunchArgumentJavaAll`, `LaunchArgumentIndie`, `LaunchArgumentIndieV2`, `LaunchArgumentVisible`, `LaunchArgumentPriority`, `LaunchArgumentWindowWidth`, `LaunchArgumentWindowHeight`, `LaunchArgumentWindowType`, `LaunchArgumentRam`, `LaunchAdvanceJvm`, `LaunchAdvanceGame`, `LaunchAdvanceRun`, `LaunchAdvanceRunWait`, `LaunchAdvanceDisableJLW`, `LaunchAdvanceDisableLUA`, `LaunchAdvanceGraphicCard`, `LaunchAdvanceGC`, `LaunchRamType`, `LaunchRamCustom`。

### Link（4）

`LinkLastAutoJoinInviteCode`, `LinkLatencyMode`, `LinkCustomPeer`, `LinkEasyTierVersion`。

### Login（9）

`LoginRemember`, `LoginLegacyName`, `LoginMsJson`, `LoginNideEmail`, `LoginNidePass`, `LoginAuthEmail`, `LoginAuthPass`, `LoginType`, `LoginPageType`。

### Potatoes（1）

`Potatoes`。

### System（16）

`SystemEulaVersion`, `SystemCount`, `SystemLaunchCount`, `SystemLastVersionReg`, `SystemHighestSavedBetaVersionReg`, `SystemHighestBetaVersionReg`, `SystemHighestAlphaVersionReg`, `SystemHelpVersion`, `SystemDebugMode`, `SystemDebugAnim`, `SystemDebugDelay`, `SystemDebugSkipCopy`, `SystemSystemCache`, `SystemSystemUpdate`, `SystemSystemActivity`, `SystemSystemTelemetry`。

### Tool（14）

`ToolHelpChinese`, `ToolDownloadThread`, `ToolDownloadSpeed`, `ToolDownloadSource`, `ToolDownloadVersion`, `ToolDownloadTranslate`, `ToolDownloadTranslateV2`, `ToolDownloadCert`, `ToolDownloadMod`, `ToolModLocalNameStyle`, `ToolUpdateRelease`, `ToolUpdateSnapshot`, `ToolUpdateReleaseLast`, `ToolUpdateSnapshotLast`。

### Ui（41）

`UiLauncherTransparent`, `UiLauncherHue`, `UiLauncherSat`, `UiLauncherDelta`, `UiLauncherLight`, `UiLauncherTheme`, `UiLauncherThemeHide`, `UiLauncherThemeHide2`, `UiLauncherLogo`, `UiLauncherEmail`, `UiBackgroundColorful`, `UiBackgroundOpacity`, `UiBackgroundBlur`, `UiBackgroundSuit`, `UiCustomType`, `UiCustomPreset`, `UiCustomNet`, `UiLogoType`, `UiLogoText`, `UiLogoLeft`, `UiMusicVolume`, `UiMusicStop`, `UiMusicStart`, `UiMusicRandom`, `UiMusicAuto`, `UiHiddenPageDownload`, `UiHiddenPageLink`, `UiHiddenPageSetup`, `UiHiddenPageOther`, `UiHiddenFunctionSelect`, `UiHiddenFunctionModUpdate`, `UiHiddenFunctionHidden`, `UiHiddenSetupLaunch`, `UiHiddenSetupUi`, `UiHiddenSetupLink`, `UiHiddenSetupSystem`, `UiHiddenOtherHelp`, `UiHiddenOtherFeedback`, `UiHiddenOtherVote`, `UiHiddenOtherAbout`, `UiHiddenOtherTest`。

### Version（26）

`VersionAdvanceJvm`, `VersionAdvanceGame`, `VersionAdvanceAssets`, `VersionAdvanceAssetsV2`, `VersionAdvanceRun`, `VersionAdvanceRunWait`, `VersionAdvanceDisableJLW`, `VersionAdvanceDisableLUA`, `VersionAdvanceDisableModUpdate`, `VersionAdvanceGC`, `VersionRamType`, `VersionRamCustom`, `VersionRamOptimize`, `VersionArgumentTitle`, `VersionArgumentInfo`, `VersionArgumentIndie`, `VersionArgumentIndieV2`, `VersionArgumentJavaSelect`, `VersionArgumentJavaV2`, `VersionArgumentJavaRange`, `VersionServerEnter`, `VersionServerLogin`, `VersionServerNide`, `VersionServerAuthRegister`, `VersionServerAuthName`, `VersionServerAuthServer`。

### Window（2）

`WindowHeight`, `WindowWidth`。

### PCLCS Java 配置（5）

`JavaConfigVersion`、`JavaList`、`JavaRemovedList`、`InstanceMigratedJava`（声明名 `JavaMigrated`）、`InstanceForcedJava`（声明名 `JavaForced`）。
