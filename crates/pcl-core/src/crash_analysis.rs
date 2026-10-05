//! Structured counterparts of ModCrash.AnalyzeCrit1/2/3, AnalyzeStackKeyword
//! and AnalyzeModName. Stack correlation stays explicitly tentative.
use super::{CrashReport, Finding, LogEvidence};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("fixed crash expression")
}
fn excerpt(value: &str) -> String {
    value.chars().take(1000).collect()
}
fn finding(code: &str, title: &str, explanation: &str, evidence: Vec<String>) -> Option<Finding> {
    if evidence.is_empty() {
        return None;
    }
    Some(Finding {
        code: code.into(),
        title: title.into(),
        explanation: explanation.into(),
        evidence: evidence.into_iter().take(8).map(|e| excerpt(&e)).collect(),
    })
}
fn block(lines: &[&str], start: usize, max: usize) -> Vec<String> {
    lines
        .iter()
        .skip(start)
        .take(max)
        .take_while(|line| {
            !line.trim_start().starts_with("at ")
                && !line.starts_with("-- System Details")
                && !line.starts_with("[")
        })
        .filter(|line| !line.trim().is_empty())
        .map(|line| excerpt(line.trim()))
        .collect()
}
fn mod_names(report: &CrashReport) -> BTreeMap<String, BTreeSet<String>> {
    let debug = regex(r"(?i)valid mod file (.+?\.jar) with \{([^}]+)\}");
    let fabric = regex(r"^\s{2,}([a-z][a-z0-9_-]+): (.+?)\s+\S+\s*$");
    let legacy = regex(r"(?i)^\s*[A-Z]+\s+([a-z][a-z0-9_-]*)\{[^}]*\}.*\(([^()]+\.jar)\)");
    let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in &report.files {
        let mut fabric_list = false;
        for line in file.text.lines() {
            if line.contains("Fabric Mods:") || line.contains("Quilt Mods:") {
                fabric_list = true;
                continue;
            }
            if fabric_list && !line.starts_with(char::is_whitespace) {
                fabric_list = false;
            }
            if let Some(c) = debug.captures(line) {
                for id in c[2].split(',').map(str::trim) {
                    names
                        .entry(id.replace('_', "").to_ascii_lowercase())
                        .or_default()
                        .insert(excerpt(&c[1]));
                }
            }
            if fabric_list {
                if let Some(c) = fabric.captures(line) {
                    if !c[1].starts_with("fabric-")
                        && !matches!(
                            &c[1],
                            "minecraft" | "java" | "fabricloader" | "quilt_loader"
                        )
                    {
                        names
                            .entry(c[1].replace('_', "").to_ascii_lowercase())
                            .or_default()
                            .insert(excerpt(&c[2]));
                    }
                }
            }
            // Forge's crash table: state | mod id | version | filename | signature.
            if line.contains('|') && line.to_ascii_lowercase().contains(".jar") {
                let fields: Vec<_> = line.split('|').map(str::trim).collect();
                if let Some((index, jar)) = fields
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.to_ascii_lowercase().ends_with(".jar"))
                {
                    // Legacy Forge puts filename after id/version; modern Forge
                    // puts filename first, then display name and id.
                    let id = if index <= 1 {
                        fields.get(index + 2)
                    } else {
                        fields.get(index - 2)
                    };
                    if let Some(id) = id.filter(|id| {
                        !id.is_empty()
                            && id
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                            && !matches!(**id, "minecraft" | "forge" | "FML" | "mcp")
                    }) {
                        names
                            .entry(id.replace('_', "").to_ascii_lowercase())
                            .or_default()
                            .insert(excerpt(jar));
                    }
                }
            }
            if let Some(c) = legacy.captures(line) {
                names
                    .entry(c[1].replace('_', "").to_ascii_lowercase())
                    .or_default()
                    .insert(excerpt(&c[2]));
            }
        }
    }
    names
}
fn matched_names(keyword: &str, names: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    let normalized = keyword.replace('_', "").to_ascii_lowercase();
    if let Some(exact) = names.get(&normalized) {
        return exact.clone();
    }
    if normalized.len() < 4 {
        return BTreeSet::new();
    }
    names
        .values()
        .flat_map(|values| values.iter())
        .filter(|value| {
            value
                .replace('_', "")
                .to_ascii_lowercase()
                .contains(&normalized)
        })
        .cloned()
        .collect()
}
fn known_name(id: &str, names: &BTreeMap<String, BTreeSet<String>>) -> String {
    let files: BTreeSet<_> = id
        .split(['(', ')'])
        .map(str::trim)
        .flat_map(|part| matched_names(part, names))
        .collect();
    if files.is_empty() {
        id.into()
    } else {
        format!(
            "{id}（{}）",
            files.into_iter().collect::<Vec<_>>().join("、")
        )
    }
}
fn is_crash_report(text: &str) -> bool {
    text.contains("---- Minecraft Crash Report ----")
        || text.contains("A detailed walkthrough of the error")
}
fn exception_stack(file: &LogEvidence) -> String {
    if is_crash_report(&file.text) {
        return file
            .text
            .split("System Details")
            .next()
            .unwrap_or("")
            .into();
    }
    if file
        .name
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.starts_with("hs_err_pid"))
    {
        return file
            .text
            .split_once("T H R E A D")
            .map(|(_, tail)| tail.split("Registers:").next().unwrap_or(tail).to_owned())
            .unwrap_or_default();
    }
    let mut inside = false;
    let mut text = String::new();
    for line in file.text.lines() {
        let starts_error =
            line.contains("/FATAL]") || line.contains("Unreported exception thrown!");
        if starts_error {
            inside = true;
        } else if line.starts_with('[') {
            inside = false;
        }
        if inside {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}
fn is_mixin_failure(line: &str) -> bool {
    [
        "Mixin prepare failed",
        "Mixin apply failed",
        "MixinApplyError",
        "MixinTransformerError",
        "InvalidMixinException",
        "mixin.injection.throwables.",
        "Critical injection failure",
        ".json] FAILED during",
    ]
    .iter()
    .any(|p| line.contains(p))
}
fn stack_candidates(text: &str) -> Vec<(String, String)> {
    const IGNORE: &[&str] = &[
        "java.",
        "sun.",
        "javax.",
        "jdk.",
        "oolloo.",
        "org.lwjgl.",
        "com.sun.",
        "net.minecraftforge.",
        "net.neoforged.",
        "paulscode.sound.",
        "com.mojang.",
        "net.minecraft.",
        "cpw.mods.",
        "com.google.",
        "org.apache.",
        "org.spongepowered.",
        "net.fabricmc.",
        "org.quiltmc.",
        "com.mumfrey.",
        "com.electronwill.nightconfig.",
        "it.unimi.dsi.",
    ];
    const WORDS:&str="com org net asm fml mod jar sun lib map gui dev nio api dsi top mcp core init mods main file game load read done util tile item base fake oshi impl data pool task forge setup block model mixin event unimi netty world lwjgl fakes fabric gitlab common server config mixins compat loader launch script entity assist client plugin modapi mojang shader events github recipe render packet preinit preload machine reflect channel general handler content systems modules service scripts network fastutil optifine internal platform override fabricmc neoforge external injection listeners scheduler minecraft universal multipart neoforged microsoft transformer transformers minecraftforge blockentity spongepowered electronwill concurrent";
    let ignored: BTreeSet<_> = WORDS.split_whitespace().collect();
    let frame =
        regex(r"^\s*(?:at\s+|[jJ]\s+(?:\d+\s+(?:[cC]\d\s+)?)?)([A-Za-z_$][A-Za-z0-9_.$/@+-]+)\(");
    let injected = regex(r"\.\w+\$\w+\$(\w+(?:\$\w+)*)\$\w+\(");
    let mut candidates = BTreeMap::new();
    for line in text
        .split("-- System Details")
        .next()
        .unwrap_or(text)
        .lines()
        .take(10000)
    {
        let Some(c) = frame.captures(line) else {
            continue;
        };
        let class = c[1].split('/').next_back().unwrap_or(&c[1]);
        let injected_words = injected.captures(line).map(|c| c[1].replace('$', "."));
        // Mixin injected method names can identify a mod even when the owner
        // is a vanilla class, which is otherwise deliberately excluded.
        let words = if let Some(words) = injected_words {
            words
        } else if IGNORE.iter().any(|prefix| class.starts_with(prefix)) {
            continue;
        } else {
            class.to_owned()
        };
        for word in words.split('.').take(4) {
            let lower = word.to_ascii_lowercase();
            if word.len() > 2
                && !word.starts_with("func_")
                && !ignored.contains(lower.as_str())
                && !word.contains('$')
            {
                candidates.entry(lower).or_insert_with(|| excerpt(line));
            }
        }
    }
    if candidates.len() > 10 {
        vec![]
    } else {
        candidates.into_iter().collect()
    }
}
pub(super) fn enrich(report: &mut CrashReport) {
    let names = mod_names(report);
    let mut extra = Vec::new();
    let explicit = regex(
        r"(?:Caught exception from |due to errors, provided by '|Failed to create mod instance\. ModI[dD]:? )([^'\r\n,]+)",
    );
    let mixin = regex(r"(?:from mod |for mod )([a-zA-Z0-9_-]+)");
    let mixin_config = regex(r"([A-Za-z0-9_.-]+\.json)(?::[A-Za-z0-9_.$]+)?");
    for file in &report.files {
        let lines: Vec<_> = file.text.lines().collect();
        let mut exact = Vec::new();
        let mut mixin_ids = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if let Some(c) = explicit.captures(line) {
                let id = c[1].split(" for ").next().unwrap_or(&c[1]).trim();
                exact.push(format!(
                    "{}：{}；{}",
                    file.name,
                    known_name(id, &names),
                    excerpt(line)
                ));
            }
            if is_mixin_failure(line) {
                if let Some(c) = mixin.captures(line) {
                    mixin_ids.push(format!(
                        "{}:{}：{}；{}",
                        file.name,
                        i + 1,
                        known_name(&c[1], &names),
                        excerpt(line)
                    ));
                } else if let Some(config) = mixin_config.captures(line) {
                    extra.extend(finding("mixin-config", "Mixin 错误指向了配置文件",
                        "日志点名了此 Mixin 配置。配置文件名可能与 Mod ID 不同，请结合加载清单确认所属组件。",
                        vec![format!("{}:{}：{}；{}", file.name, i + 1, &config[1], excerpt(line))]));
                }
            }
            if [
                "Missing or unsupported mandatory dependencies:",
                "Found duplicate mods",
                "DuplicateModsFoundException",
                "ModResolutionException: Duplicate",
            ]
            .iter()
            .any(|p| line.contains(p))
            {
                let code = if line.contains("dependencies:") {
                    "mod-dependencies"
                } else {
                    "duplicate-mods"
                };
                let evidence = std::iter::once(excerpt(line))
                    .chain(block(&lines, i + 1, 16))
                    .map(|text| format!("{}：{text}", file.name))
                    .collect();
                extra.extend(finding(code, "加载器报告的 Mod 与依赖明细",
                    "以下保留加载器给出的 Mod ID、文件名和要求范围；按完整版本要求核对，不自动移除文件。", evidence));
            }
            if let Some((_, details)) = line.split_once("Multiple entries with same key: ") {
                let key = details.split('=').next().unwrap_or(details).trim();
                extra.extend(finding("duplicate-entry", "注册条目使用了重复键",
                    "日志指出条目冲突。此键可能属于被覆盖方或重复注册项，需检查双方 Mod，不能仅按键名删除组件。",
                    vec![format!("{}:{}：{}；{}", file.name, i + 1, known_name(key, &names), excerpt(line))]));
            }
            if line.contains(
                "com.electronwill.nightconfig.core.io.ParsingException: Not enough data available",
            ) && !file.text.contains("Failed loading config file ")
            {
                extra.extend(finding("nightconfig-truncated", "NightConfig 读取到不完整配置数据",
                    "该特征与原版识别的 NightConfig 问题相符，也可能是配置文件被截断。先备份日志指向的配置并检查完整性，不自动删除世界或整个配置目录。",
                    vec![format!("{}:{}：{}",file.name,i+1,excerpt(line))]));
            }
            if line.contains("has mods that were not found")
                && line.to_ascii_lowercase().contains("optifine")
            {
                extra.extend(finding("optifine-forge", "OptiFine 未被当前 Forge 正确识别",
                    "加载器明确报告 OptiFine 文件中的组件无法识别；核对该发布版与 Forge 的兼容范围。",
                    vec![format!("{}:{}：{}",file.name,i+1,excerpt(line))]));
            }
            if line.contains("A potential solution has been determined")
                || line.contains("确定了一种可能的解决方法")
            {
                let suggestion: Vec<_> = lines
                    .iter()
                    .skip(i + 1)
                    .take(32)
                    .take_while(|s| s.trim().is_empty() || s.starts_with(char::is_whitespace))
                    .filter(|s| s.trim_start().starts_with('-'))
                    .map(|s| format!("{}：{}", file.name, excerpt(s.trim())))
                    .collect();
                extra.extend(finding("fabric-solution","Fabric 提供了依赖调整建议","以下是加载器给出的建议，并未自动安装、删除或更改文件。先核对游戏版本和完整依赖链，再决定调整。",suggestion));
            }
            if line.trim().starts_with("-- MOD ") {
                let section: Vec<_> = lines
                    .iter()
                    .skip(i)
                    .take(24)
                    .take_while(|s| *s == line || !s.starts_with("-- "))
                    .collect();
                if section.iter().any(|s| s.contains("Failure message:")) {
                    let mut continuation = false;
                    let evidence = section
                        .iter()
                        .filter(|s| {
                            let text = s.trim();
                            if text.starts_with("Failure message:")
                                || text.starts_with("Exception message:")
                            {
                                continuation = true;
                                return true;
                            }
                            if text.starts_with("Mod ")
                                || text.starts_with("Stacktrace:")
                                || text.starts_with("at ")
                            {
                                continuation = false;
                            }
                            text.contains("MOD ")
                                || text.starts_with("Mod File:")
                                || (continuation && !text.is_empty())
                        })
                        .map(|s| format!("{}：{}", file.name, excerpt(s.trim())))
                        .collect();
                    extra.extend(finding("forge-mod-error","Forge 报告了组件加载错误","错误区块指出了组件和失败信息；被点名的组件可能是依赖冲突的受影响方，并不一定是唯一根因。",evidence));
                }
            }
            if line.contains("the game will display an error screen and halt.") {
                extra.extend(finding(
                    "forge-loader-error",
                    "Forge 停止加载并报告错误",
                    "按下列加载器错误检查组件版本与前置依赖；不应只根据退出码重装游戏。",
                    block(&lines, i + 1, 12)
                        .into_iter()
                        .map(|s| format!("{}：{s}", file.name))
                        .collect(),
                ));
            }
            if line.contains("Suspected Mod") && !line.contains("None") {
                let suspects = lines
                    .iter()
                    .skip(i)
                    .take(12)
                    .take_while(|s| {
                        !s.contains("Stacktrace:") && !s.starts_with("-- System Details")
                    })
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| format!("{}：{}", file.name, excerpt(s)))
                    .collect();
                extra.extend(finding("suspected-mods","崩溃报告列出了疑似 Mod","这是报告的候选列表，需要结合首个异常、依赖及近期改动复核，不能直接认定其中全部 Mod 有错。",suspects));
            }
        }
        extra.extend(finding("mod-reported-error","加载器点名了发生错误的 Mod","保留具体 Mod/文件名以便检查。明确的加载失败不证明该 Mod 独自造成崩溃；同时检查它的依赖与兼容版本。",exact));
        extra.extend(finding(
            "mixin-owner",
            "Mixin 错误包含来源 Mod",
            "日志明确提供了 Mixin 的来源；请核对其目标版本及冲突组件。",
            mixin_ids,
        ));
        let is_crash = is_crash_report(&file.text);
        if is_crash {
            for (marker, code, title) in [
                (
                    "-- Entity being ticked --",
                    "ticking-entity",
                    "处理特定实体时发生异常",
                ),
                (
                    "-- Block entity being ticked --",
                    "ticking-block",
                    "处理特定方块实体时发生异常",
                ),
                (
                    "-- Block being ticked --",
                    "ticking-block",
                    "处理特定方块时发生异常",
                ),
            ] {
                if let Some(start) = lines.iter().position(|s| s.trim() == marker) {
                    let evidence = lines
                        .iter()
                        .skip(start + 1)
                        .take(40)
                        .take_while(|s| !s.starts_with("-- "))
                        .filter(|s| {
                            [
                                "Entity Type:",
                                "Entity Name:",
                                "Entity ID:",
                                "Entity's Exact location:",
                                "Entity's Block location:",
                                "Dimension:",
                                "Level dimension:",
                                "Block:",
                                "Block location:",
                                "Name:",
                            ]
                            .iter()
                            .any(|key| s.trim().starts_with(key))
                        })
                        .map(|s| format!("{}：{}", file.name, excerpt(s.trim())))
                        .collect();
                    extra.extend(finding(code,title,"报告记录了相关对象及坐标。先备份存档，再检查对应 Mod/世界数据；对象出现在异常现场不等于可直接删除它。",evidence));
                }
            }
            // Older reports omit the section heading but retain these paired fields.
            for (marker, field, code, title) in [
                (
                    "Block location: World:",
                    "Block:",
                    "ticking-block",
                    "异常报告包含特定方块与坐标",
                ),
                (
                    "Entity's Exact location:",
                    "Entity Type:",
                    "ticking-entity",
                    "异常报告包含特定实体与坐标",
                ),
            ] {
                if file.text.contains(marker) && file.text.contains(field) {
                    let evidence = lines
                        .iter()
                        .filter(|s| s.trim().starts_with(marker) || s.trim().starts_with(field))
                        .map(|s| format!("{}：{}", file.name, excerpt(s.trim())))
                        .collect();
                    extra.extend(finding(
                        code,
                        title,
                        "这是崩溃现场的对象信息，需结合异常栈判断；未自动修改存档。",
                        evidence,
                    ));
                }
            }
        }
        for (code,title,explain,patterns,also) in [
            ("jdk-cast","旧组件假定了不同的 Java 类加载器","这是具体类加载器类型转换失败；选择组件支持的 Java 或更新组件，并不意味着所有 JDK 都不能运行游戏。",&["java.lang.ClassCastException: java.base/jdk","java.lang.ClassCastException: class jdk."][..],None),
            ("optifine-world","OptiFine 与区块加载接口不兼容","该组合缺少区块加载方法；核对 OptiFine 与 Forge 的兼容版本。",&["net.minecraft.world.server.ChunkManager$ProxyTicketManager.shouldForceTicks(J)Z"][..],Some("OptiFine")),
            ("java-11-required","组件要求 Java 11 或对应兼容环境","日志明确引用了 Java 11 字节码或兼容级别。仍应核对游戏和加载器的完整 Java 范围，避免只改主版本。",&["class file version 55.0","The requested compatibility level JAVA_11","no such method: sun.misc.Unsafe.defineAnonymousClass"][..],None),
            ("forge-incomplete","Forge 安装文件不完整","加载目标或支持库不存在，请校验该版本的官方安装文件。",&["Invalid paths argument, contained no existing paths"][..],Some("fmlcore")),
            ("optifine-forge","OptiFine/渲染组件接口不兼容","日志报告当前组件调用的渲染接口不存在，请核对匹配版本。",&["net.minecraft.client.renderer.block.model.BakedQuad.<init>","net.minecraft.server.level.DistanceManager","net.minecraft.network.chat.FormattedText net.minecraft.client.gui.Font.ellipsize"][..],Some("NoSuchMethodError")),
        ]{
            if also.is_none_or(|p|file.text.contains(p)){let evidence=lines.iter().filter(|s|patterns.iter().any(|p|s.contains(p))).take(4).map(|s|format!("{}：{}",file.name,excerpt(s))).collect();extra.extend(finding(code,title,explain,evidence));}
        }
    }
    // Entity/block coordinates describe the scene; they do not suppress
    // the more useful stack candidate attribution.
    let strong =
        |finding: &Finding| !matches!(finding.code.as_str(), "ticking-entity" | "ticking-block");
    if !report.findings.iter().any(strong) && !extra.iter().any(strong) {
        for file in &report.files {
            let stack = exception_stack(file);
            let candidates = stack_candidates(&stack);
            let mut recognized = Vec::new();
            let mut unknown = Vec::new();
            for (keyword, line) in candidates {
                let mods = matched_names(&keyword, &names);
                if !mods.is_empty() {
                    recognized.push(format!(
                        "{}：候选 {}；{}",
                        file.name,
                        mods.iter().cloned().collect::<Vec<_>>().join("、"),
                        line
                    ));
                } else {
                    unknown.push(format!("{}：包关键词 {keyword}；{line}", file.name));
                }
            }
            if !recognized.is_empty() {
                extra.extend(finding(
                    "stack-mod-candidates",
                    "异常堆栈与已加载 Mod 记录存在关联",
                    "这是包名与日志 Mod 清单的对应线索，属于候选归因，不能单凭栈中出现就判定元凶。",
                    recognized,
                ));
            } else {
                extra.extend(finding("stack-keywords","异常堆栈包含第三方包线索","未能把这些包关键词匹配到日志中的 Mod 清单；请结合完整异常检查，不按猜测删除文件。",unknown));
            }
        }
    }
    if report.findings.is_empty() && extra.is_empty() {
        let short = report
            .files
            .iter()
            .filter(|file| !file.text.trim().is_empty())
            .collect::<Vec<_>>();
        if short.len() == 1
            && short[0].text.len() < 100
            && !short[0].text.contains("INFO]")
            && !short[0].text.contains("at net.")
            && !is_crash_report(&short[0].text)
        {
            report.warnings.push(format!(
                "{} 的程序输出过短，缺少可确认的异常链；请保留完整日志后继续分析。",
                short[0].name
            ));
        }
    }
    for mut value in extra {
        if let Some(previous) = report.findings.iter_mut().find(|p| p.code == value.code) {
            previous.evidence.append(&mut value.evidence);
            previous.evidence.sort();
            previous.evidence.dedup();
            previous.evidence.truncate(8);
        } else {
            report.findings.push(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(files: &[(&str, &str)]) -> CrashReport {
        let mut report = CrashReport {
            files: files
                .iter()
                .map(|(name, text)| super::super::LogEvidence {
                    name: (*name).into(),
                    text: super::super::redact(text, &[]),
                    truncated: false,
                })
                .collect(),
            ..Default::default()
        };
        super::super::analyze(&mut report);
        report
    }
    #[test]
    fn entity_block_coordinates_are_context_not_deletion_advice() {
        let r=report(&[("crash.txt","---- Minecraft Crash Report ----\n-- Entity being ticked --\n\tEntity Type: example:bird (Bird)\n\tEntity's Exact location: 12.4, 64.0, -3.5\n-- Block entity being ticked --\n\tBlock: Block{example:machine}\n\tBlock location: World: (1,64,2)\n")]);
        for code in ["ticking-entity", "ticking-block"] {
            let f = r.findings.iter().find(|f| f.code == code).unwrap();
            assert!(f.evidence.len() >= 2);
            assert!(f.explanation.contains("存档"));
        }
        let normal = report(&[(
            "latest.log",
            "INFO Entity's Exact location: 12,64,3\nEntity Type: x",
        )]);
        assert!(normal.findings.is_empty());
    }
    #[test]
    fn fabric_solutions_and_forge_failure_include_actual_components() {
        let r=report(&[("latest.log","Incompatible mods found!\nA potential solution has been determined:\n\t - Replace mod 'A' with version 2.\n\t - Install dependency B.\n\tat net.fabricmc.loader.Run.go(Run.java:1)\n"),("crash.txt","---- Minecraft Crash Report ----\n-- MOD demo --\nMod File: mods/demo.jar\nFailure message: demo requires lib 2\nException message: Missing\n-- System Details --\n")]);
        let f = r
            .findings
            .iter()
            .find(|f| f.code == "fabric-solution")
            .unwrap();
        assert_eq!(f.evidence.len(), 2);
        assert!(f.evidence[0].contains("Replace"));
        let f = r
            .findings
            .iter()
            .find(|f| f.code == "forge-mod-error")
            .unwrap();
        assert!(f.evidence.iter().any(|e| e.contains("demo.jar")));
    }
    #[test]
    fn stack_maps_to_fabric_and_debug_records_but_not_platform_frames() {
        let r=report(&[("crash.txt","---- Minecraft Crash Report ----\njava.lang.RuntimeException\n\tat com.examplemod.machine.Tick.run(Tick.java:4)\n\tat net.minecraft.World.run(World.java:1)\n-- System Details --\nFabric Mods:\n\t\texamplemod: Example Mod 1.0\n"),("debug.log","Found valid mod file examplemod-1.jar with {examplemod} mods - versions {1.0}")]);
        let f = r
            .findings
            .iter()
            .find(|f| f.code == "stack-mod-candidates")
            .unwrap();
        assert!(f.evidence[0].contains("examplemod-1.jar"));
        assert!(f.explanation.contains("候选"));
        assert!(!f.evidence.iter().any(|e| e.contains("minecraft.World")));
        assert!(report(&[(
            "latest.log",
            "[INFO] Diagnostics\n\tat com.examplemod.Test.go(Test.java:1)"
        )])
        .findings
        .is_empty());
    }
    #[test]
    fn explicit_mixin_and_suspects_preserve_uncertainty_and_redaction() {
        let r=report(&[("latest.log","Mixin apply failed demo.mixin.json from mod demo\nCaught exception from demo --accessToken secretvalue\n"),("crash.txt","---- Minecraft Crash Report ----\nSuspected Mods:\n\tDemo (demo)\nStacktrace:\n")]);
        assert!(r.findings.iter().any(|f| f.code == "mixin-owner"));
        assert!(r.findings.iter().any(|f| f.code == "suspected-mods"));
        assert!(!format!("{r:?}").contains("secretvalue"));
        assert!(!report(&[(
            "crash.txt",
            "---- Minecraft Crash Report ----\nSuspected Mods: None\nStacktrace:"
        )])
        .findings
        .iter()
        .any(|f| f.code == "suspected-mods"));
    }

    #[test]
    fn successful_mixin_and_nonfatal_stacks_are_not_accused() {
        let report = report(&[("latest.log", "[12:00:00] [main/INFO]: Mixin loaded successfully from mod innocent\n\tat com.innocent.Loader.run(Loader.java:1)\n[12:00:01] [main/FATAL]: Fatal exception\n\tat net.minecraft.Main.run(Main.java:1)\n[12:00:02] [main/INFO]: Shutdown diagnostics\n\tat com.unrelated.Cleanup.run(Cleanup.java:2)")]);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }

    #[test]
    fn forge_table_variants_module_frames_and_vm_frames_map_to_real_filenames() {
        for (name,text,filename) in [
            ("modern.txt", "---- Minecraft Crash Report ----\n\tat TRANSFORMER/example@2.0/org.example.machine.Tick.run(Tick.java:2)\n-- System Details --\nMod List:\n\tExampleMachines-2.jar | Example Machines | example | 2.0 | DONE | None", "ExampleMachines-2.jar"),
            ("old.txt", "---- Minecraft Crash Report ----\n\tat org.oldmachines.Tick.run(Tick.java:2)\n-- System Details --\n\tUCHIJAAAA oldmachines{1.0} [Old Machines] (OldMachines-1.jar)", "OldMachines-1.jar"),
            ("logs/hs_err_pid10.log", "# fatal JVM error\nT H R E A D\nJ 125 c2 org.jvmexample.engine.Tick.run()V (23 bytes) @ 0x1234\nRegisters:\nFound valid mod file JvmExample.jar with {jvmexample} mods - versions {1}", "JvmExample.jar"),
        ] {
            let r = report(&[(name,text)]);
            let f = r.findings.iter().find(|f|f.code=="stack-mod-candidates").unwrap_or_else(||panic!("{name}: {:?}",r.findings));
            assert!(f.evidence.iter().any(|line|line.contains(filename)));
            assert!(f.explanation.contains("候选"));
        }
    }

    #[test]
    fn injected_mixin_stack_and_entity_coordinates_can_both_be_reported() {
        let r = report(&[("crash.txt","---- Minecraft Crash Report ----\n\tat net.minecraft.World.handler$abc000$examplemod$onTick(World.java:3)\n-- Entity being ticked --\n\tEntity Type: examplemod:bird\n\tEntity's Exact location: 1.0, 64.0, 2.0\n-- System Details --\nFabric Mods:\n\t\texamplemod: Example Mod 1.0")]);
        assert!(r.findings.iter().any(|f| f.code == "ticking-entity"));
        assert!(r.findings.iter().any(|f| f.code == "stack-mod-candidates"
            && f.evidence.iter().any(|e| e.contains("Example Mod"))));
    }

    #[test]
    fn precise_findings_in_later_logs_suppress_heuristic_stack_candidates() {
        let r = report(&[
            (
                "crash.txt",
                "---- Minecraft Crash Report ----\n\tat org.examplemod.Tick.run(Tick.java:1)",
            ),
            ("latest.log", "java.lang.OutOfMemoryError: Java heap space"),
        ]);
        assert!(r.findings.iter().any(|f| f.code == "out-of-memory"));
        assert!(!r.findings.iter().any(|f| f.code.starts_with("stack-")));
    }

    #[test]
    fn multiline_failure_dependency_details_and_mixin_config_are_preserved() {
        let r=report(&[("crash.txt","---- Minecraft Crash Report ----\n-- MOD example --\nMod File: Example.jar\nFailure message:\n\tExample requires helper >= 3.0\n\tCurrently helper 2.0 is installed\nMod Version: 1.0\n-- System Details --"),
            ("latest.log","Missing or unsupported mandatory dependencies:\n\tMod ID: helper, Requested by: example, Expected range: [3.0,), Actual version: 2.0\n\tat net.minecraftforge.Loader.run(Loader.java:1)\nMixin prepare failed example.mixins.json:WorldMixin -> net.minecraft.World")]);
        let error = r
            .findings
            .iter()
            .find(|f| f.code == "forge-mod-error")
            .unwrap();
        assert!(error
            .evidence
            .iter()
            .any(|e| e.contains("Currently helper 2.0")));
        let dependency = r
            .findings
            .iter()
            .find(|f| f.code == "mod-dependencies")
            .unwrap();
        assert!(dependency
            .evidence
            .iter()
            .any(|e| e.contains("Expected range: [3.0,)")));
        assert!(r.findings.iter().any(|f| f.code == "mixin-config"
            && f.evidence.iter().any(|e| e.contains("example.mixins.json"))));
    }

    #[test]
    fn original_specific_signatures_keep_evidence_and_do_not_modify_data() {
        for (line, code) in [
            (
                "java.lang.ClassNotFoundException: java.lang.invoke.LambdaMetafactory",
                "java-too-new",
            ),
            (
                "com.electronwill.nightconfig.core.io.ParsingException: Not enough data available",
                "nightconfig-truncated",
            ),
            (
                "The Mod File libraries/optifine/OptiFine/test.jar has mods that were not found",
                "optifine-forge",
            ),
            (
                "Multiple entries with same key: example:machine=one and example:machine=two",
                "duplicate-entry",
            ),
            ("MixinTransformerError: failed", "mixin"),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("fixture.log");
            std::fs::write(&path, line).unwrap();
            let r = super::super::import(&path, &[]).unwrap();
            assert!(
                r.findings.iter().any(|f| f.code == code),
                "{line}: {:?}",
                r.findings
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), line);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
        let unknown = report(&[("launcher-output.log", "exit -1")]);
        assert!(unknown.findings.is_empty());
        assert!(unknown.warnings.iter().any(|w| w.contains("程序输出过短")));
    }
    #[test]
    fn noisy_stack_is_not_an_unbounded_list_of_accused_mods() {
        let text = format!(
            "---- Minecraft Crash Report ----\n{}",
            (0..20)
                .map(|i| format!("\tat custompackage{i}.Something.run(File.java:1)\n"))
                .collect::<String>()
        );
        assert!(!report(&[("crash.txt", &text)])
            .findings
            .iter()
            .any(|f| f.code.starts_with("stack-")));
    }
}
