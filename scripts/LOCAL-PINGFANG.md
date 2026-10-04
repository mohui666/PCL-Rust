# 本机苹方字形转换

macOS 27 的系统苹方位于：

```text
/System/Library/PrivateFrameworks/FontServices.framework/Resources/Reserved/PingFangUI.ttc
```

本机该字体使用 Apple `hvgl` 轮廓；直接交给当前 egui/ab_glyph 时有 cmap、没有可用轮廓，导致中文字透明。此流程通过 CoreText 分别读取 `PingFangSC-Regular` 与 `PingFangSC-Semibold` 的真实字形路径，生成 ab_glyph 可以解析的普通 sfnt。系统原字体不修改，不从第三方站点下载字体。

已验证且不需额外 Python 包的生成命令（在项目根目录执行）：

```sh
python3 scripts/build-local-pingfang-sfnt.py
```

默认仍只生成 Regular，兼容已有调用。单独生成 Semibold，或同时生成两种字重：

```sh
python3 scripts/build-local-pingfang-sfnt.py --style semibold
python3 scripts/build-local-pingfang-sfnt.py --style all
```

需要本机 `xcrun swift` 与 CoreText。脚本每次自动从全部 `crates/**/*.rs` 收集字符（排除控制字符），补 ASCII、CJK/全角标点，通过 `export-pingfang-outlines.swift` 导出路径及 advance，再构建普通 TrueType `glyf` sfnt。少数苹方不含的符号（例如源码中的 `▣`）由 CoreText 选择本机后备字体，实际字体名称和系统路径逐字记录在 `.source.json` 的 `fallback_glyphs`；汉字使用指定的苹方字面。如果源码字符仍有缺失，生成会明确失败，避免静默交付透明字。

输出沿用约定的 `.otf` 文件名；内部 sfnt scaler type 是 `0x00010000`，不是 CFF。曲线以每段 24 个小线段近似，坐标取整至 1/1000 em；这用于本机可见文字恢复，不能作为与 Windows 微软雅黑像素一致的证据。

本机生成物全部在现有 `.gitignore` 已忽略的目录内：

- `test-output/fonts/PingFang-Regular.otf`：供 egui 使用。
- `test-output/fonts/PingFang-Regular.source.json`：系统源路径、字形数、格式与 SHA-256。
- `test-output/fonts/PingFang-Regular.characters.txt`：实际字符集合。
- `test-output/fonts/PingFang-Regular.outlines.json`：CoreText 原始轮廓数据。
- `test-output/fonts/PingFang-Semibold.otf`：供 egui 显式粗体字体族使用；同名 `.source.json`、`.characters.txt`、`.outlines.json` 记录其来源及覆盖范围。

**这些系统字体派生文件仅供本机运行和验证，不提交、不打包公开分发。** 可共享的是脚本；其他机器应使用自己已安装字体在本机构建。新的中文文案加入源代码后需重新执行生成命令。

`scripts/package-macos.sh` 在本机构建前调用 `--style all`，随后将两份字库分别复制到本机应用的 `Contents/Resources/PingFang-Regular.otf` 与 `PingFang-Semibold.otf`。本次脚本修正没有执行应用打包、签名或重启。

字重写入 `OS/2.usWeightClass`（表内偏移 4）：Regular 为 400，Semibold 为 600。按照 [OpenType OS/2 规范](https://learn.microsoft.com/en-us/typography/opentype/spec/os2#usweightclass)，700 才是 Bold。Semibold 的 `fsSelection` 与 `head.macStyle` 粗体位保持 0；若以后导出真正 Bold 字面，则写入 700，并同时设置 `fsSelection` 位 5 与 `head.macStyle` 位 0。字体名及 typographic subfamily 同时标记 `Semibold`。**改变这些数值不会改变轮廓粗细**；实际加粗来自独立 CoreText 字面，egui 必须显式选择该字体数据，不能仅依靠 `RichText::strong()` 的颜色变化。

本次 bundled Python 没有 fontTools，正常 TLS 下载官方 PyPI 资源超时，因此实际交付只保留上面的无依赖流程；无需安装额外 Python 包。

2026-10-04 实际验证：CoreText 初次导出 619 个字形；项目自身的 ab_glyph 依赖成功加载生成物，并对“启动下载设置游戏我的世界”逐字检查 glyph ID 非零、outline 非空及 raster alpha 总和大于 5。对应验证程序与可执行文件位于 `test-output/fonts/verify-ab-glyph.rs` 和 `test-output/fonts/verify-ab-glyph`。此验证证明字形可渲染；应用窗口的最终观感由主任务另行检查。

2026-10-04 Semibold 扩展验证：两种字面均导出 736 个字形，覆盖当时全部 582 个源码字符；ab_glyph 对其中 581 个非空白字符逐个验证 glyph ID 非零、outline 非空和 raster alpha 大于 0。24 px 的“启动下载设置游戏我的世界”样本 alpha 总量分别为 Regular 1086.51、Semibold 1619.67，证明使用了更粗的真实轮廓。sfnt 表解析确认字重分别为 400、600，且均含 `glyf`。证据位于 `test-output/fonts/weight-validation.json` 与 `weight-validation.txt`；后续新增文案应重新生成，计数不代表未来源码快照。
