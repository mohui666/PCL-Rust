# PCL 官方界面资源来源

本目录仅保存官方仓库的原始资源/源码和来源记录。目标基准为 Windows 官方 PCL2 默认蓝色界面；这些资源已取回并验证，尚不代表 Rust 界面已完成像素验证。

- 仓库：https://github.com/Meloong-Git/PCL
- 固定提交：`0e0d12fdce6a2804916fb2be60e41144da637c18`
- 获取日期：2026-10-04（Asia/Shanghai）
- 上游文件前缀：`Plain Craft Launcher 2/`
- 初次 17 个文件的下载方式：GitHub 官方 Git Data blob API，正常 TLS 证书校验。raw.githubusercontent.com 在本机发生 TLS 握手超时，因此改用官方 API 的 base64 原始 blob；未关闭 TLS 校验。
- 后续新增的 `Images/Blocks/{Anvil,NeoForge,Fabric,Grass,CommandBlock,CobbleStone,GoldBlock}.png` 从本项目 `test-output/upstream` 的上述固定提交本地 checkout 原样复制。本次逐一读取固定提交的 Git tree/blob，并确认 blob、本地 checkout 与资源目录中的字节完全一致；本次未通过 blob API 下载这些图。manifest 的 `acquisition` 字段记录实际获取方式，`source` 保留固定提交的原始来源链接。
- 每个原始文件按 Git 的 `SHA1("blob " + byte_length + NUL + bytes)` 算法与固定提交目录树核对。原文件未重编码、裁剪、重画、缩放或修改。
- 逐文件来源 URL、实际获取方式、长度、Git blob SHA-1 和 SHA-256 见 [SOURCE-MANIFEST.json](SOURCE-MANIFEST.json)；原有条目的 `download_api` 保留初次 API 获取记录。本 README 和 manifest 是本项目生成的来源记录。
- 上游许可原文保留在项目根目录 [UPSTREAM-LICENCE](../../../../UPSTREAM-LICENCE)。

## 字体

上游 `Application.xaml:125,132,247` 的字体栈是 `Resources/#PCL English, Microsoft YaHei UI`，默认 TextBlock 字号为 13 DIP。原版西文字体为 [Resources/Font.ttf](Resources/Font.ttf)，可原样内嵌，不应以系统 sans 替代。

实读 TTF name/head/maxp 表：family=`PCL English`，style=`Regular`，PostScript name=`PCLEnglish`，unitsPerEm=1000，glyph 数=154。字体文件 20,188 字节。它是原版西文资源，不能解决中文字体缺失。

本机按文件名检查了用户文档目录、用户与系统字体目录，以及常见 Microsoft Office 应用字体资源路径，未发现 `msyh*.ttc/ttf/otf`、`*YaHei*.ttc/ttf/otf` 或 `*微软雅黑*.ttc/ttf/otf`。本机没有可用的 Wine、CrossOver、Whisky 或 Office 字体副本；未下载第三方字体站文件。

**用户已接受 macOS 使用苹方（PingFang）呈现中文，允许其字形、字宽和栅格化与 Windows Microsoft YaHei UI 存在差异。** 因此本机缺少微软雅黑不再作为 macOS 交付阻断，也不把苹方截图当作 Windows 同字体像素验证的证据。Windows 仍应优先使用用户系统已安装的 Microsoft YaHei UI（`Windows/Fonts/msyh.ttc`），同一 TTC 的 UI face 需按字体族正确选择；其余布局、配色和交互仍按原版基准核对。

## Steve 皮肤与头像

[Images/Skins/Steve.png](Images/Skins/Steve.png) 是上游 64×64 原始皮肤图。本目录保留完整 PNG，没有产出修改后的头像图片。具体皮肤由上游 `PageLaunchLeft.SkinLegacy` 的设置与用户名规则选择；本文件只是 Steve 资源本身。

头像构造依据 `Pages/PageLaunch/MySkin.xaml.vb:99–130` 和 `MySkin.xaml`：

- 以 64×64 原皮肤为基准，脸层取 `(x=8,y=8,w=8,h=8)`，在 64×64 控件中央显示为 48×48。
- 帽子/头发层取 `(40,8,8,8)`，通过原版透明度判断后，在同一控件中央显示为 56×56。
- 使用 NearestNeighbor；控件使用布局取整，图像像素对齐。
- 阴影使用 `ColorObject2`，BlurRadius=10、ShadowDepth=0，默认 Opacity=0.2，悬浮目标 Opacity=0.8。

## 顶栏标题和导航图标

顶栏的 PCL 标题不是 `Images/Heads/Logo.png` 或 `Images/Heads/PCL2.png`。默认标题是 [FormMain.xaml:117](FormMain.xaml) 的 `ShapeTitleLogo` 矢量：

```text
M26,29 v-25 h6 a7,7 180 0 1 0,14 h-6 M62.5,6.5 a10,11.5 180 1 0 0,18 M71,2.5 v24.5 h13.5
```

该路径使用 White stroke、StrokeThickness=2.2、Width=39、Stretch=fill、Margin=`19,15.5,0,15.5`。`LabTitleLogo` 与 `ImageTitleLogo` 默认隐藏。Heads 目录的 PNG 和 `Images/icon.ico` 仅保留作其他用途的上游参考，不能直接替代这个标题。

导航的原始 `Logo` 路径完整保存在 `FormMain.xaml`，无需手绘或使用 Unicode 图标近似：

| 元素 | 文字 | 行 | LogoScale |
|---|---|---:|---:|
| BtnTitleSelect0 | 启动 | 121–122 | 0.9 |
| BtnTitleSelect1 | 下载 | 123–124 | 0.9 |
| BtnTitleSelect2 | 联机 | 125–126 | 1.05 |
| BtnTitleSelect3 | 设置 | 127–128 | 1.1 |
| BtnTitleSelect4 | 更多 | 129–130 | 0.93 |
| BtnTitleClose | 关闭 | 107–108 | 0.72 |
| BtnTitleMin | 最小化 | 109–110 | 0.72 |

`Controls/MyRadioButton.xaml` 定义顶栏导航控件：高27、圆角13.5；图标最大16×16，左margin12；文字margin `8,0,12,0`，默认字号13。普通导航文字/图标白色；选中态背景白色，文字/图标 `ColorBrush3`。父容器导航项左右margin5，整体在48高顶栏中央。

## MyButton 的颜色与状态

`Controls/MyButton.xaml` 的前层圆角3、边框1，默认背景 `ColorBrushHalfWhite`（`#55FFFFFF`），文字13 DIP且颜色绑定边框。启动按钮在 `PageLaunchLeft.xaml` 使用 `ColorType=Highlight`，并非整块实心蓝色按钮。

`Controls/MyButton.xaml.vb` 定义：普通按钮边框/文字=`ColorBrush1`；Highlight=`ColorBrush2`；悬浮二者变为`ColorBrush3`且背景变为`ColorBrush7`；禁用=`ColorBrushGray4`；移出恢复半透明白。颜色进入100ms，移出200ms；按压前80ms缩放至0.955，再700ms缩小0.01，松开300ms恢复。

`Application.xaml:22–65` 的初始蓝色资源表：

| 资源 | 原始值 |
|---|---|
| ColorBrush1 | #343d4a |
| ColorBrush2 | #0b5bcb |
| ColorBrush3 | #1370f3 |
| ColorBrush4 | #4890f5 |
| ColorBrush5 | #96c0f9 |
| ColorBrush6 | #d5e6fd |
| ColorBrush7 | #e0eafd |
| ColorBrush8 | #eaf2fe |
| ColorBrushBg0 | #96c0f9 |
| ColorBrushBg1 | #bee0eafd |
| ColorBrushGray3 | #8c8c8c |
| ColorBrushGray4 | #a6a6a6 |
| ColorBrushHalfWhite | #55ffffff |
| ColorBrushBackgroundTransparentSidebar | #f1ffffff |

这些是 XAML 初始资源值；实际主题初始化可能覆盖动态资源，需与主题逻辑、Windows 默认界面截图一起核对，不能仅凭这张表宣称最终像素颜色一致。

## 原始文件清单

| 本地相对路径 | 字节数 | 固定提交原始来源 |
|---|---:|---|
| `Application.xaml` | 38370 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Application.xaml) |
| `FormMain.xaml` | 21354 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/FormMain.xaml) |
| `Controls/MyButton.xaml` | 1156 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Controls/MyButton.xaml) |
| `Controls/MyButton.xaml.vb` | 8312 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Controls/MyButton.xaml.vb) |
| `Controls/MyRadioButton.xaml` | 835 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Controls/MyRadioButton.xaml) |
| `Controls/MyRadioButton.xaml.vb` | 12272 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Controls/MyRadioButton.xaml.vb) |
| `Pages/PageLaunch/MySkin.xaml` | 3700 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Pages/PageLaunch/MySkin.xaml) |
| `Pages/PageLaunch/MySkin.xaml.vb` | 16422 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Pages/PageLaunch/MySkin.xaml.vb) |
| `Pages/PageLaunch/PageLoginLegacy.xaml` | 946 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Pages/PageLaunch/PageLoginLegacy.xaml) |
| `Pages/PageLaunch/PageLoginLegacy.xaml.vb` | 3954 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Pages/PageLaunch/PageLoginLegacy.xaml.vb) |
| `Pages/PageLaunch/PageLaunchLeft.xaml` | 14548 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Pages/PageLaunch/PageLaunchLeft.xaml) |
| `Resources/Font.ttf` | 20188 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Resources/Font.ttf) |
| `Images/Skins/Steve.png` | 958 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Skins/Steve.png) |
| `Images/Heads/Logo.png` | 2758 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Heads/Logo.png) |
| `Images/Heads/PCL2.png` | 2723 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Heads/PCL2.png) |
| `Images/icon.ico` | 199464 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/icon.ico) |
| `Modules/ModMain.vb` | 53413 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Modules/ModMain.vb) |
| `Images/Blocks/Anvil.png` | 880 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/Anvil.png) |
| `Images/Blocks/NeoForge.png` | 1412 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/NeoForge.png) |
| `Images/Blocks/Fabric.png` | 275 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/Fabric.png) |
| `Images/Blocks/Grass.png` | 1467 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/Grass.png) |
| `Images/Blocks/CommandBlock.png` | 2785 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/CommandBlock.png) |
| `Images/Blocks/CobbleStone.png` | 991 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/CobbleStone.png) |
| `Images/Blocks/GoldBlock.png` | 1079 | [GitHub](https://raw.githubusercontent.com/Meloong-Git/PCL/0e0d12fdce6a2804916fb2be60e41144da637c18/Plain%20Craft%20Launcher%202/Images/Blocks/GoldBlock.png) |

派生资源：`../icon.png` 与 `../icon.icns` 由本目录 `Images/icon.ico` 转码；`../icons/game.svg`、`mod.svg`、`pack.svg` 来自同一固定提交 `Pages/PageDownload/PageDownloadLeft.xaml` 的原始 Logo 路径，不使用生成图替代原图标。


## 新增侧栏路径派生资源

下列六个SVG由本地固定提交的XAML `Logo` 属性经XML解析后包装为white-fill SVG（viewBox=0 0 1200 1200）。已逐项确认SVG的`path.d`与上游属性完全相等，没有重新描绘路径。它们是派生SVG，不冒充Git仓库中的原始SVG；manifest单独的`derived_files`记录输出SHA256、源文件Git blob/摘要、元素名与变换说明。

| 派生文件 | 上游XAML | 元素 |
|---|---|---|
| `../icons/appearance.svg` | `Pages/PageSetup/PageSetupLeft.xaml` | `ItemUI` |
| `../icons/overview.svg` | `Pages/PageInstance/PageInstanceLeft.xaml` | `ItemOverall` |
| `../icons/wrench.svg` | `Pages/PageInstance/PageInstanceLeft.xaml` | `ItemSetup` |
| `../icons/datapack.svg` | `Pages/PageDownload/PageDownloadLeft.xaml` | `ItemDataPack` |
| `../icons/resourcepack.svg` | `Pages/PageDownload/PageDownloadLeft.xaml` | `ItemResourcePack` |
| `../icons/shader.svg` | `Pages/PageDownload/PageDownloadLeft.xaml` | `ItemShader` |

`CommandBlock.png`也是固定提交的未修改原PNG，用于快照版本图标；其实际像素资源不能与上述路径派生混为同一种获取方式。

`CobbleStone.png` 与 `GoldBlock.png` 同样从本地固定checkout原样复制，并与Git tree/blob逐字节验证，分别用于相应历史版本/特殊版本图标；不是生成或重绘资源。

`Images/Skins/Alex.png` 从同一固定提交原样复制，用于离线 Alex 与默认 UUID 模型的真实头像预览。已与固定 Git blob `984d92b72053fac384174c36c7232037b00646ce` 逐字节核对；未重画、裁剪或重编码原图。运行时仅按原皮肤 UV 区域绘制头像。
