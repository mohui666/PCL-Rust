# 固定上游帮助与鸣谢资料

来源：PCL 2.13.1.1 对应本地源码 `0e0d12fdce6a2804916fb2be60e41144da637c18`，原作者龙腾猫跃及帮助各条目署名作者。适用许可保留于项目根目录 `UPSTREAM-LICENCE`；不将这些资料声明为本项目原创。

`build_catalog.py` 从固定 `Resources/Help.zip` 的 70 个文件生成 40 个帮助项目，抽取 `PageOtherAbout.xaml` 的 10 项鸣谢、382 个赞助者名称及上游版权声明，并复制 13 个原版头像、保留 6 个侧栏矢量。`SOURCE.json` 记录原始压缩包、每个条目、头像和生成目录的 SHA-256；生成结果为 UTF-8 中文。

这是一份离线文本快照，不是在线帮助服务或完整图文复刻。远程图片仅提供用户点击的 HTTP(S) 链接，不自动下载。XAML 只在生成时按 XML 解析显示属性，不执行；运行时只允许已存在目录项的内部跳转与 HTTP(S) 外链。文件执行、下载文件、启动游戏、清理等自定义事件均不可执行。原文针对 Windows 原版，不能作为 Rust 当前功能已实现的证据。

重建（从仓库根目录）：

```sh
python3 crates/pcl-desktop/assets/help/build_catalog.py
```
