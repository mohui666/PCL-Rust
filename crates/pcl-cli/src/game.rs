use super::*;
use anyhow::{bail, ensure};
use pcl_core::{
    accounts, auth,
    java_selection::{self, JavaSelectionMode, JavaSelectionRequest, JavaSelectionResult},
    launch::{self, LaunchOptions, LaunchPlan},
    model::Session,
};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

#[cfg(all(test, unix))]
#[path = "game_tests.rs"]
mod tests;

#[derive(Debug)]
pub(super) struct GameExit(pub i32);
impl std::fmt::Display for GameExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "游戏退出码：{}", self.0)
    }
}
impl std::error::Error for GameExit {}

pub(super) fn plan(
    cx: &RuntimeContext,
    args: LaunchArgs,
    launching: bool,
) -> Result<(LaunchPlan, Session)> {
    ensure!(
        args.name.is_none(),
        "离线登录已禁用；请先 login，再用 --account 选择正版账号"
    );
    let id = args
        .account
        .as_deref()
        .context("离线登录已禁用；请先 login，再用 --account 选择正版账号")?;
    let session =
        accounts::restore_account(id, &cx.cancel, |s| cx.output.event("login", &s))?.session;
    plan_with_session(cx, args, launching, session)
}

fn plan_with_session(
    cx: &RuntimeContext,
    args: LaunchArgs,
    launching: bool,
    session: Session,
) -> Result<(LaunchPlan, Session)> {
    auth::require_microsoft_session(&session)?;
    let instance = config::load_instance_settings(&cx.root, &args.version)?;
    let mut priority = cx.settings.java_priority.clone();
    if let Some(path) = &cx.settings.java_path {
        if !priority.contains(path) {
            priority.insert(0, path.clone());
        }
    }
    let request = JavaSelectionRequest {
        root: cx.root.clone(),
        version_id: args.version.clone(),
        mode: if args.java.is_some() {
            JavaSelectionMode::Specific
        } else {
            java_selection::effective_mode(instance.java_mode, instance.java_path.as_deref())
        },
        version_range: instance.java_range,
        specified_path: args.java.or(instance.java_path),
        priority,
        excluded: cx.settings.java_excluded.clone(),
    };
    let runtime = match java_selection::select_java(&request, &[], &cx.platform, &cx.cancel)? {
        JavaSelectionResult::Selected {
            runtime, warnings, ..
        } => {
            for warning in warnings {
                cx.output.event("warning", &warning);
            }
            runtime
        }
        JavaSelectionResult::NeedsDownload {
            requirement,
            diagnostics,
        } => {
            bail!("没有兼容的 Java；请使用 java runtimes / java install，或显式 --java。建议组件：{}；{}",
                requirement.recommended_component.as_deref().unwrap_or("见 java runtimes"), diagnostics.join("；"))
        }
    };
    let prepared = if launching {
        Some(pcl_core::offline_skin::prepare(
            &cx.root,
            &args.version,
            &cx.settings,
            &session,
            &cx.cancel,
        )?)
    } else {
        None
    };
    let session = prepared
        .as_ref()
        .map_or(session, |skin| skin.session.clone());
    let mut plan = launch::build_plan_with_overrides(
        &LaunchOptions {
            root: cx.root.clone(),
            version_id: args.version,
            java: runtime.path,
            memory_mb: cx.settings.memory_mb,
            width: cx.settings.width,
            height: cx.settings.height,
        },
        &session,
        &cx.platform,
        &cx.settings,
        runtime.major,
        launch::LaunchOverrides {
            launcher_size: Some((
                cx.settings.launcher_window.width as u32,
                cx.settings.launcher_window.height as u32,
            )),
            server: args.server.as_deref(),
            memory_mb: args.memory,
        },
    )?;
    if let Some(prepared) = prepared {
        plan.behavior.warnings.extend(prepared.warnings);
        plan.behavior.offline_skin = Some(prepared.update);
    }
    Ok((plan, session))
}

pub(super) fn run(cx: &RuntimeContext, plan: LaunchPlan, session: Session) -> Result<Value> {
    auth::require_microsoft_session(&session)?;
    cx.check_cancel()?;
    std::fs::create_dir_all(&plan.cwd)?;
    for warning in &plan.behavior.warnings {
        cx.output.event("warning", warning);
    }
    if let Some(update) = &plan.behavior.offline_skin {
        pcl_core::offline_skin::apply(&plan.cwd, update, &cx.cancel)?;
    }
    if plan.behavior.auto_chinese {
        pcl_core::offline_skin::set_initial_language(
            &plan.cwd,
            &plan.behavior.language_code,
            &cx.cancel,
        )?;
    }
    // Window decoration, launcher memory and GPU preferences belong to the GUI frontend.
    if !plan.behavior.window.title.is_empty()
        || plan.behavior.window.maximize
        || plan.behavior.high_performance_gpu
    {
        cx.output.event(
            "warning",
            "无头启动保留游戏参数；窗口标题、最大化和系统 GPU 偏好需由桌面环境管理。",
        );
    }
    if plan.behavior.priority != config::ProcessPriority::Normal {
        cx.output
            .event("warning", "无头游戏进程使用系统默认优先级。");
    }
    let mut preceding = Vec::new();
    for entry in &plan.behavior.commands {
        cx.check_cancel()?;
        let text = plan.behavior.shell_text(&entry.text, cfg!(windows));
        let mut command = shell(text);
        command
            .current_dir(&plan.behavior.command_cwd)
            .envs(plan.behavior.environment())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cx.output
            .event("pre_launch", &format!("执行{}启动前命令", entry.label));
        match command.spawn() {
            Ok(child) => {
                let mut child = OwnedChild(child, true);
                if entry.wait {
                    let status = wait(cx, &mut child)?;
                    if !status.success() {
                        cx.output.event(
                            "warning",
                            &format!("{}启动前命令退出异常（{status}），继续启动", entry.label),
                        );
                    }
                } else {
                    preceding.push(child);
                }
            }
            Err(error) => cx.output.event(
                "warning",
                &format!("{}启动前命令未能启动：{error}", entry.label),
            ),
        }
    }
    cx.check_cancel()?;
    let mut command = Command::new(&plan.java);
    command
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_process_group(&mut command);
    let mut child = OwnedChild(command.spawn().context("启动 Java 进程失败")?, true);
    let pid = child.0.id();
    cx.output
        .value(json!({"type":"game_started","pid":pid,"cwd":plan.cwd}));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    for source in [
        child
            .0
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .0
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let secret = session.access_token.clone();
        let output = cx.output;
        let done = done_tx.clone();
        thread::spawn(move || {
            if let Err(error) = stream_logs(source, &secret, output) {
                output.event("warning", &format!("读取游戏日志失败：{error}"));
            }
            let _ = done.send(());
        });
    }
    drop(done_tx);
    let status = wait(cx, &mut child)?;
    // Bound pipe draining: descendants may inherit the game's output handles.
    for _ in 0..2 {
        let _ = done_rx.recv_timeout(Duration::from_millis(500));
    }
    // Successfully launched no-wait commands retain their usual independent lifetime.
    for child in &mut preceding {
        child.1 = false;
    }
    drop(preceding);
    ensure!(
        status.success(),
        GameExit(status.code().filter(|c| *c > 0 && *c <= 255).unwrap_or(1))
    );
    Ok(json!({"pid":pid,"exit_code":0,"cwd":plan.cwd}))
}

fn shell(text: String) -> Command {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/D", "/S", "/V:ON", "/C"])
            .raw_arg(format!("\"{text}\""));
        cmd
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", &text]);
        cmd
    };
    own_process_group(&mut command);
    command
}

fn own_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}

struct OwnedChild(Child, bool);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.1 || matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // Only the group created above; never the terminal's group or another game.
            unsafe {
                kill(-(self.0.id() as i32), 9);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &self.0.id().to_string(), "/T", "/F"])
                .creation_flags(0x08000000)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait(cx: &RuntimeContext, child: &mut OwnedChild) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.0.try_wait()? {
            return Ok(status);
        }
        cx.check_cancel()?;
        thread::sleep(Duration::from_millis(50));
    }
}

fn stream_logs(source: impl std::io::Read, token: &str, output: Output) -> Result<()> {
    let mut reader = BufReader::new(source);
    let mut line = Vec::new();
    let mut overflow = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let count = end.map_or(buffer.len(), |n| n + 1);
        if !overflow {
            if line.len() + count <= 16 * 1024 {
                line.extend_from_slice(&buffer[..count]);
            } else {
                line.clear();
                overflow = true;
            }
        }
        reader.consume(count);
        if end.is_some() {
            log_line(&line, overflow, token, output);
            line.clear();
            overflow = false;
        }
    }
    if overflow || !line.is_empty() {
        log_line(&line, overflow, token, output);
    }
    Ok(())
}
fn log_line(line: &[u8], overflow: bool, token: &str, output: Output) {
    let text = if overflow {
        "[超长游戏日志已省略]".into()
    } else {
        pcl_core::crash::redact(String::from_utf8_lossy(line).trim_end(), &[token.into()])
    };
    output.event("game_log", &text);
}
