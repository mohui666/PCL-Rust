//! Export a prepared launch plan without executing Java or persisting credentials.
//!
//! Upstream: PageInstanceOverall.xaml.vb BtnManageScript_Click and
//! ModLaunch.vb McLaunchCustom (fixed PCL 2.13.1.1). Unlike its partial token mask,
//! the export consumes LaunchPlan's complete credential sanitization.
use crate::{install, launch::LaunchPlan};
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptFormat {
    /// A POSIX shell script suitable for macOS Terminal's .command files.
    MacCommand,
    /// A .bat bootstrap with a fixed Windows PowerShell 5+ process wrapper.
    WindowsBatch,
}
impl ScriptFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::MacCommand => "command",
            Self::WindowsBatch => "bat",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptExport {
    pub path: PathBuf,
    pub format: ScriptFormat,
    /// False means credentials were removed. This is a diagnostic script;
    /// authenticated launch must still use the launcher's login flow.
    pub runnable_without_credentials: bool,
    pub bytes: u64,
}

/// Write a new script atomically, never replace an existing file, and never run it.
/// The parent directory must already exist. Java/arguments come from a plan the
/// caller has already prepared with the selected version's effective settings.
pub fn export_launch_script(
    path: &Path,
    plan: &LaunchPlan,
    format: ScriptFormat,
) -> Result<ScriptExport> {
    export_launch_script_with_cancel(path, plan, format, &AtomicBool::new(false))
}

pub fn export_launch_script_with_cancel(
    path: &Path,
    plan: &LaunchPlan,
    format: ScriptFormat,
    cancel: &AtomicBool,
) -> Result<ScriptExport> {
    install::cancelled(cancel)?;
    ensure!(path.is_absolute(), "启动脚本保存位置必须是绝对路径");
    let name = path.file_name().context("启动脚本保存位置缺少文件名")?;
    ensure!(
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(format.extension())),
        "启动脚本扩展名应为 .{}",
        format.extension()
    );
    let parent = path
        .parent()
        .context("启动脚本保存位置缺少父目录")?
        .canonicalize()
        .context("启动脚本保存目录不存在或不可访问")?;
    ensure!(parent.is_dir(), "启动脚本保存位置的父路径不是目录");
    let destination = parent.join(name);
    require_new_file(&destination)?;
    let (contents, runnable) = render(plan, format)?;
    install::cancelled(cancel)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".pcl-launch-script-")
        .tempfile_in(&parent)
        .context("无法创建启动脚本临时文件")?;
    staged
        .write_all(contents.as_bytes())
        .context("无法写入启动脚本")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))
            .context("无法设置启动脚本权限")?;
    }
    staged.as_file().sync_all().context("无法保存启动脚本")?;
    install::cancelled(cancel)?;
    // Atomic no-clobber also covers a file/symlink created after our early check.
    staged.persist_noclobber(&destination).map_err(|error| {
        // Do not expose the prepared command, arguments or tempfile contents.
        anyhow::anyhow!("启动脚本未提交（目标可能已存在）：{}", error.error)
    })?;
    Ok(ScriptExport {
        path: destination,
        format,
        runnable_without_credentials: runnable,
        bytes: contents.len() as u64,
    })
}

fn require_new_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("无法检查启动脚本保存位置"),
        Ok(_) => bail!("启动脚本保存位置已存在，未覆盖"),
    }
}

#[derive(Serialize)]
struct ScriptData<'a> {
    java: &'a str,
    cwd: &'a str,
    arguments: &'a [String],
    authenticated_credentials_removed: bool,
}
fn render(plan: &LaunchPlan, format: ScriptFormat) -> Result<(String, bool)> {
    // Only the plan itself knows every credential that may be embedded in its
    // custom arguments. Never ask the caller to pair an unrelated Session with it.
    let (arguments, removed) = plan.sanitized_arguments();
    let java = plan.java.to_str().context("Java 路径不是有效 Unicode")?;
    let cwd = plan.cwd.to_str().context("游戏目录不是有效 Unicode")?;
    ensure!(
        plan.java.is_absolute() && plan.cwd.is_absolute(),
        "启动计划必须使用绝对路径"
    );
    ensure!(
        !java.contains('\0')
            && !cwd.contains('\0')
            && arguments.iter().all(|arg| !arg.contains('\0')),
        "启动计划含不能传递给操作系统的 NUL 字符"
    );
    let text = match format {
        ScriptFormat::MacCommand => render_posix(java, cwd, &arguments, removed),
        ScriptFormat::WindowsBatch => render_windows(java, cwd, &arguments, removed)?,
    };
    Ok((text, !removed))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
fn render_posix(java: &str, cwd: &str, arguments: &[String], removed: bool) -> String {
    let mut text = String::from(
        "#!/bin/sh\n# PCL Rust exported launch script. Generated only; export did not start Java.\nset +x\n",
    );
    if removed {
        text.push_str("# DIAGNOSTIC: authentication credentials were replaced with F.\n# Authenticated login is unavailable in this script; launch through PCL Rust.\nprintf '%s\\n' 'Authentication credentials are redacted; use the launcher to sign in.' >&2\n");
    } else {
        text.push_str("# Offline launch: no authenticated credentials are stored here.\n");
    }
    text.push_str("cd ");
    text.push_str(&shell_quote(cwd));
    text.push_str(" || exit $?\nexec ");
    text.push_str(&shell_quote(java));
    for argument in arguments {
        text.push_str(" \\\n  ");
        text.push_str(&shell_quote(argument));
    }
    text.push('\n');
    text
}

// Microsoft C runtime quoting, independent of cmd.exe and PowerShell evaluation.
// https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments
fn quote_windows_argument(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
        } else {
            result.extend(std::iter::repeat_n(
                '\\',
                if character == '"' {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            result.push(character);
            slashes = 0;
        }
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}
fn render_windows(java: &str, cwd: &str, arguments: &[String], removed: bool) -> Result<String> {
    let commandline = arguments
        .iter()
        .map(|arg| quote_windows_argument(arg))
        .collect::<Vec<_>>()
        .join(" ");
    // .NET Framework ProcessStartInfo.Arguments has a 32,699-character bound.
    // This exporter cannot silently pretend an oversized Windows command works.
    let argument_units = commandline.encode_utf16().count();
    let executable_units = quote_windows_argument(java).encode_utf16().count();
    ensure!(
        argument_units < 32_699 && executable_units + 1 + argument_units < 32_767,
        "Windows 启动参数超过系统命令行长度限制，无法导出此脚本"
    );
    let payload = serde_json::to_vec(&ScriptData {
        java,
        cwd,
        arguments,
        authenticated_credentials_removed: removed,
    })
    .context("无法编码脱敏启动计划")?;
    let payload = base64(&payload);
    let mut text = String::from(
        "@echo off\r\nsetlocal DisableDelayedExpansion\r\nrem PCL Rust exported launch script. Export does not run Java.\r\n",
    );
    if removed {
        text.push_str("rem DIAGNOSTIC: authentication credentials are replaced with F.\r\nrem This script cannot log in; use PCL Rust for authenticated launch.\r\n");
    }
    // No user-controlled text is interpolated into the cmd/PowerShell bootstrap.
    // The full path expansion stays inside SET's quotes; delayed expansion is off.
    text.push_str(concat!(
        "set \"PCL_RUST_SCRIPT_SELF=%~f0\"\r\n",
        "\"%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -Command \"$source=[IO.File]::ReadAllText($env:PCL_RUST_SCRIPT_SELF);$marker='# PCL_RUST_POWERSHELL_BODY';$offset=$source.LastIndexOf($marker);if($offset -lt 0){exit 2};& ([scriptblock]::Create($source.Substring($offset+$marker.Length)))\"\r\n",
        "set \"PCL_RUST_LAUNCH_STATUS=%errorlevel%\"\r\n",
        "echo Game process exited.\r\n",
        "pause\r\n",
        "exit /b %PCL_RUST_LAUNCH_STATUS%\r\n",
        "# PCL_RUST_POWERSHELL_BODY\r\n",
        "$ErrorActionPreference='Stop'\r\n",
        "Set-PSDebug -Off\r\n",
        "$ProgressPreference='SilentlyContinue'\r\n",
    ));
    text.push_str("$data=ConvertFrom-Json -InputObject ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('");
    text.push_str(&payload);
    text.push_str("')))\r\n");
    // The payload is inert JSON. Quoting happens on each argument before passing
    // a command line to ProcessStartInfo; neither shell interprets user values.
    text.push_str(WINDOWS_PROCESS_WRAPPER);
    Ok(text)
}
const WINDOWS_PROCESS_WRAPPER: &str = concat!(
    "function Quote-PclArgument([AllowEmptyString()][string]$value) {\r\n",
    "  $builder=New-Object Text.StringBuilder\r\n",
    "  [void]$builder.Append('\"');$slashes=0\r\n",
    "  foreach($character in $value.ToCharArray()) {\r\n",
    "    if($character -eq '\\') {$slashes++;continue}\r\n",
    "    if($character -eq '\"') {[void]$builder.Append(('\\' * ($slashes*2+1)))} else {[void]$builder.Append(('\\' * $slashes))}\r\n",
    "    [void]$builder.Append($character);$slashes=0\r\n",
    "  }\r\n",
    "  [void]$builder.Append(('\\' * ($slashes*2)));[void]$builder.Append('\"')\r\n",
    "  return $builder.ToString()\r\n",
    "}\r\n",
    "try {\r\n",
    "  if($data.authenticated_credentials_removed) {[Console]::Error.WriteLine('Authentication credentials are redacted; use the launcher to sign in.')}\r\n",
    "  $arguments=@($data.arguments | ForEach-Object {Quote-PclArgument ([string]$_)})\r\n",
    "  $start=New-Object Diagnostics.ProcessStartInfo\r\n",
    "  $start.FileName=[string]$data.java\r\n",
    "  $start.WorkingDirectory=[string]$data.cwd\r\n",
    "  $start.Arguments=[string]::Join(' ',[string[]]$arguments)\r\n",
    "  $start.UseShellExecute=$false\r\n",
    "  $process=[Diagnostics.Process]::Start($start)\r\n",
    "  if($null -eq $process){throw 'No process returned'}\r\n",
    "  $process.WaitForExit();$code=$process.ExitCode;$process.Dispose();exit $code\r\n",
    "} catch {[Console]::Error.WriteLine('Launch failed. Check the Java path and game files with the launcher.');exit 1}\r\n",
);
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        result.push(ALPHABET[(first >> 2) as usize] as char);
        result.push(ALPHABET[(((first & 3) << 4) | (second >> 4)) as usize] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[(((second & 15) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(third & 63) as usize] as char
        } else {
            '='
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        launch::{self, LaunchOptions},
        model::{Platform, Session},
    };
    use serde_json::json;

    fn fixture(token: &str, legacy: bool) -> (tempfile::TempDir, LaunchPlan) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("versions/test")).unwrap();
        let version = if legacy {
            json!({"mainClass":"Main", "minecraftArguments":"--username ${auth_player_name} --session ${auth_session}"})
        } else {
            json!({
                "mainClass":"Main",
                "arguments":{
                    "jvm":["-cp","${classpath}","-Dembedded=${auth_access_token}"],
                    "game":["--username","${auth_player_name}","--accessToken","${auth_access_token}"]
                }
            })
        };
        fs::write(root.join("versions/test/test.json"), version.to_string()).unwrap();
        fs::write(
            root.join("versions/test/test.jar"),
            b"fixture jar, never executed",
        )
        .unwrap();
        fs::write(root.join("java"), b"fixture Java, never executed").unwrap();
        let options = LaunchOptions {
            root: root.into(),
            version_id: "test".into(),
            java: root.join("java"),
            memory_mb: 2048,
            width: 854,
            height: 480,
        };
        let session = Session {
            username: "Player".into(),
            uuid: "0123456789abcdef0123456789abcdef".into(),
            access_token: token.into(),
            user_type: if token == "0" { "legacy" } else { "msa" }.into(),
        };
        let platform = Platform {
            os: "osx".into(),
            arch: "aarch64".into(),
            version: "14.0".into(),
        };
        let plan = launch::build_plan(&options, &session, &platform).unwrap();
        (directory, plan)
    }
    // A separate streaming decoder checks what the generated Windows wrapper
    // will actually read, not just whether a token is absent from encoded text.
    fn decode_payload(script: &str) -> serde_json::Value {
        let encoded = script
            .split("FromBase64String('")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut accumulator = 0u32;
        let mut bits = 0;
        let mut bytes = Vec::new();
        for byte in encoded.bytes().take_while(|byte| *byte != b'=') {
            accumulator = (accumulator << 6)
                | alphabet.iter().position(|entry| *entry == byte).unwrap() as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                bytes.push((accumulator >> bits) as u8);
                accumulator &= (1 << bits) - 1;
            }
        }
        serde_json::from_slice(&bytes).unwrap()
    }
    #[test]
    fn credentials_are_removed_from_both_plain_and_encoded_scripts_in_every_argument_form() {
        let secret = "PRIVATE_FIXTURE_ACCESS_TOKEN_NEVER_REAL";
        let (root, mut plan) = fixture(secret, false);
        plan.args.extend([
            format!("--accessToken={secret}"),
            "--clientToken".into(),
            "SECOND_UNKNOWN_TOKEN".into(),
            format!("-Drepeated={secret}/{secret}"),
            "--session=ANOTHER_SECRET_SESSION".into(),
        ]);
        let originals = plan.args.clone();
        for format in [ScriptFormat::MacCommand, ScriptFormat::WindowsBatch] {
            let path = root.path().join(format!("launch.{}", format.extension()));
            let result = export_launch_script(&path, &plan, format).unwrap();
            assert!(!result.runnable_without_credentials);
            let script = fs::read_to_string(&path).unwrap();
            assert!(script.contains("DIAGNOSTIC"));
            for secret in [secret, "SECOND_UNKNOWN_TOKEN", "ANOTHER_SECRET_SESSION"] {
                assert!(!script.contains(secret));
            }
            if format == ScriptFormat::WindowsBatch {
                let payload = decode_payload(&script);
                let raw = payload.to_string();
                for secret in [secret, "SECOND_UNKNOWN_TOKEN", "ANOTHER_SECRET_SESSION"] {
                    assert!(!raw.contains(secret));
                }
                assert_eq!(payload["authenticated_credentials_removed"], true);
                assert!(payload["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value == "-Drepeated=F/F"));
            }
            assert!(!format!("{result:?}").contains(secret));
        }
        assert_eq!(
            plan.args, originals,
            "export must not mutate the runnable in-memory plan"
        );
    }
    #[test]
    fn offline_modern_and_legacy_session_values_remain_runnable() {
        for legacy in [false, true] {
            let (root, plan) = fixture("0", legacy);
            let result = export_launch_script(
                &root.path().join("offline.command"),
                &plan,
                ScriptFormat::MacCommand,
            )
            .unwrap();
            assert!(result.runnable_without_credentials);
            let script = fs::read_to_string(result.path).unwrap();
            assert!(!script.contains("DIAGNOSTIC"));
            if legacy {
                assert!(script.contains("token:0:0123456789abcdef0123456789abcdef"));
            }
        }
    }
    #[test]
    fn quotes_special_characters_as_data_and_keeps_windows_bootstrap_constant() {
        let (_root, mut plan) = fixture("0", false);
        let special = [
            "",
            "with spaces",
            "中文路径",
            "quote'\"tail",
            "$(touch MUST_NOT_EXIST)",
            "`touch MUST_NOT_EXIST`",
            "%PATH% !var! & echo bad | x > y < z ^",
            "trailing\\",
            "first\nsecond",
        ];
        plan.args = special.iter().map(|value| (*value).into()).collect();
        let (posix, offline) = render(&plan, ScriptFormat::MacCommand).unwrap();
        assert!(offline);
        assert!(posix.contains("'quote'\"'\"'\"tail'"));
        assert!(posix.contains("'$(touch MUST_NOT_EXIST)'"));
        assert!(posix.contains("'first\nsecond'"));
        let (windows, _) = render(&plan, ScriptFormat::WindowsBatch).unwrap();
        assert_eq!(decode_payload(&windows)["arguments"], json!(special));
        let bootstrap = windows
            .split("# PCL_RUST_POWERSHELL_BODY\r\n")
            .next()
            .unwrap();
        for value in special.iter().filter(|value| !value.is_empty()) {
            assert!(!bootstrap.contains(value));
        }
        assert!(bootstrap.contains("setlocal DisableDelayedExpansion"));
        assert!(!windows.contains("-ExecutionPolicy"));
        assert!(windows.contains("$start.UseShellExecute=$false"));
    }
    #[test]
    fn windows_crt_quoting_preserves_quotes_and_trailing_backslashes() {
        for (input, expected) in [
            ("", r#""""#),
            ("space value", r#""space value""#),
            (r#"ab"c"#, r#""ab\"c""#),
            (r"C:\folder\", r#""C:\folder\\""#),
            (r#"a\\\"b"#, r#""a\\\\\\\"b""#),
        ] {
            assert_eq!(quote_windows_argument(input), expected);
        }
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
    #[test]
    fn existing_target_cancelled_export_and_invalid_arguments_leave_no_partial_file() {
        let (root, mut plan) = fixture("0", false);
        let path = root.path().join("existing.command");
        fs::write(&path, b"user original").unwrap();
        assert!(export_launch_script(&path, &plan, ScriptFormat::MacCommand).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"user original");
        let fresh = root.path().join("cancel.command");
        let error = export_launch_script_with_cancel(
            &fresh,
            &plan,
            ScriptFormat::MacCommand,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(error
            .chain()
            .any(|cause| cause.is::<crate::model::OperationCancelled>()));
        assert!(!fresh.exists());
        plan.args.push("DO_NOT_ECHO_THIS\0private".into());
        let error = export_launch_script(&fresh, &plan, ScriptFormat::MacCommand).unwrap_err();
        assert!(!format!("{error:#}").contains("DO_NOT_ECHO_THIS"));
        assert!(!fresh.exists());
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pcl-launch-script-")
        }));
    }
    #[test]
    fn oversized_windows_command_fails_before_writing() {
        let (root, mut plan) = fixture("0", false);
        plan.args = vec!["x".repeat(32699)];
        let path = root.path().join("too-long.bat");
        assert!(export_launch_script(&path, &plan, ScriptFormat::WindowsBatch).is_err());
        assert!(!path.exists());
        // CreateProcess's complete command also includes the executable name;
        // a long Java path must not sneak past the Arguments-only bound.
        plan.args = vec!["x".repeat(32_500)];
        plan.java = root.path().join("j".repeat(300));
        assert!(export_launch_script(&path, &plan, ScriptFormat::WindowsBatch).is_err());
        assert!(!path.exists());
    }
    #[cfg(unix)]
    #[test]
    fn unix_export_is_private_executable_rejects_symlink_and_only_parses_shell_syntax() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (root, mut plan) = fixture("0", false);
        let untouched = root.path().join("notes");
        fs::write(&untouched, b"keep").unwrap();
        let link = root.path().join("linked.command");
        symlink(&untouched, &link).unwrap();
        assert!(export_launch_script(&link, &plan, ScriptFormat::MacCommand).is_err());
        assert_eq!(fs::read(&untouched).unwrap(), b"keep");
        plan.args
            .extend(["$(false)".into(), "quote'\"".into(), "x\ny".into()]);
        let output = root.path().join("syntax-only.command");
        export_launch_script(&output, &plan, ScriptFormat::MacCommand).unwrap();
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // -n reads/parses the synthetic file. It executes no script commands;
        // fixture Java is inert text and is never started.
        let status = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
