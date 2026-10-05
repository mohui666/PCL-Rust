# macOS 中文字库

当前渲染库不能直接读取新版苹方的 `hvgl` 轮廓。脚本通过 CoreText 读取本机字形，生成可渲染的 TrueType 字库；不修改系统字体。

## 生成

需要 Python 3、Xcode Command Line Tools 和系统苹方，在项目根目录执行：

```sh
python3 scripts/build-local-pingfang-sfnt.py --style all
```

默认仅生成 Regular；`--style semibold` 仅生成 Semibold。`scripts/package-macos.sh` 会自动生成两种字重并放入应用的 `Contents/Resources/`。

## 输出

生成物位于 `test-output/fonts/`，不提交到源码仓库：

| 文件 | 内容 |
| --- | --- |
| `PingFang-Regular.otf` / `PingFang-Semibold.otf` | 界面使用的字库，内部为 TrueType `glyf` 格式 |
| `*.source.json` | 字体来源、后备字形、覆盖范围和哈希 |
| `*.characters.txt` | 字符集合 |
| `*.outlines.json` | CoreText 字形轮廓 |

字符来自 Rust 文案、随附帮助和冻结的 MC 百科索引。新增中文文案后重新生成；缺失必需字形时脚本会报错。外部项目名中的表情不保证完整覆盖。

Regular 使用字重 400，Semibold 使用独立字面的 600。两份字体均已通过 `ab_glyph` 字形与栅格检查。完整记录见[验证摘要](../docs/validation.md)。

导出整合包时保留所选 Mac 应用的完整字体和签名资源。系统字体的权利仍归原权利人。
