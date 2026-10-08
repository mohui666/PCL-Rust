use crate::app::Event;
use pcl_core::{
    config::{LauncherVisibility, ProcessPriority},
    launch::{GameWindowOptions, LaunchBehavior, LaunchPlan},
};
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    time::Duration,
};

#[allow(dead_code)] // Compatibility entry; the desktop passes a distinct launch cancellation flag.
pub fn run_game(
    plan: LaunchPlan,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    run_game_with_cancel(plan, token, tx, stop, Arc::new(AtomicBool::new(false)))
}

pub fn run_game_with_cancel(
    plan: LaunchPlan,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    run_game_inner(plan, token, tx, stop, cancel, None)
}

pub fn run_game_with_launch_progress(
    plan: LaunchPlan,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    request: u64,
) -> anyhow::Result<()> {
    run_game_inner(plan, token, tx, stop, cancel, Some(request))
}

fn run_game_inner(
    plan: LaunchPlan,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    request: Option<u64>,
) -> anyhow::Result<()> {
    use crate::app::launch_ui::{LaunchEvent, Stage};
    let stage = |stage| {
        if let Some(request) = request {
            let _ = tx.send(Event::Launch(LaunchEvent::Stage { request, stage }));
        }
    };
    check_cancel(&cancel)?;
    stage(Stage::PreRun);
    std::fs::create_dir_all(&plan.cwd)?;
    for warning in &plan.behavior.warnings {
        let _ = tx.send(Event::LaunchWarning(warning.clone()));
    }
    if plan.behavior.memory_optimize {
        match crate::native_window::reclaim_launcher_memory() {
            Ok(bytes) => {
                let message = bytes.map_or_else(
                    || "已请求回收启动器自身工作集；未清理其他进程内存。".into(),
                    |n| format!("已回收启动器可释放内存 {n} 字节；未清理其他进程内存。"),
                );
                let _ = tx.send(Event::Log(message));
            }
            Err(error) => {
                let _ = tx.send(Event::LaunchWarning(format!(
                    "回收启动器内存失败：{error:#}；继续启动。"
                )));
            }
        }
    }
    if let Some(update) = &plan.behavior.offline_skin {
        if let Err(error) = pcl_core::offline_skin::apply(&plan.cwd, update, &cancel) {
            if error.is::<pcl_core::model::OperationCancelled>() {
                return Err(error);
            }
            let _ = tx.send(Event::LaunchWarning(format!(
                "离线皮肤资源包设置失败：{error:#}；继续启动。"
            )));
        }
    }
    if plan.behavior.auto_chinese {
        if let Err(error) = pcl_core::offline_skin::set_initial_language(
            &plan.cwd,
            &plan.behavior.language_code,
            &cancel,
        ) {
            if error.is::<pcl_core::model::OperationCancelled>() {
                return Err(error);
            }
            let _ = tx.send(Event::LaunchWarning(format!(
                "首次设置游戏中文失败：{error:#}；继续启动。"
            )));
        }
    }
    stage(Stage::Commands);
    let mut preceding = run_pre_launch(&plan.behavior, &tx, &cancel)?;
    if cancel.load(Ordering::Relaxed) {
        stop_commands(&mut preceding, &tx);
    }
    check_cancel(&cancel)?;
    let mut command = Command::new(&plan.java);
    command
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut gpu = if plan.behavior.high_performance_gpu {
        match crate::native_window::GpuPreference::request(&plan.java) {
            Ok(guard) => Some(guard),
            Err(error) => {
                let _ = tx.send(Event::LaunchWarning(format!(
                    "设置本次 Java 的高性能 GPU 偏好失败：{error:#}；继续启动。"
                )));
                None
            }
        }
    } else {
        None
    };
    let launch_started = std::time::SystemTime::now();
    stage(Stage::Spawn);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            restore_gpu(&mut gpu, &tx);
            reap_commands(preceding);
            return Err(error.into());
        }
    };
    if cancel.load(Ordering::Relaxed) {
        let _ = child.kill();
        let _ = child.wait();
        stop_commands(&mut preceding, &tx);
        restore_gpu(&mut gpu, &tx);
        check_cancel(&cancel)?;
    }
    reap_commands(preceding);
    if let Err(error) = apply_priority(&child, plan.behavior.priority) {
        let _ = tx.send(Event::LaunchWarning(format!(
            "设置游戏进程优先级失败：{error}；继续使用系统允许的优先级。"
        )));
    }
    let _ = tx.send(Event::GameContext {
        pid: child.id(),
        game_dir: plan.cwd.clone(),
        started: launch_started,
        secret: token.clone(),
    });
    run_child_with_visibility(
        child,
        token,
        tx,
        stop,
        plan.behavior.visibility,
        plan.behavior.window,
        gpu,
    )
}

fn check_cancel(cancel: &AtomicBool) -> anyhow::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow::Error::new(pcl_core::model::OperationCancelled).context("启动已取消"));
    }
    Ok(())
}

fn command_process(text: String) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd.exe");
        command
            .args(["/D", "/S", "/V:ON", "/C"])
            .raw_arg(format!("\"{text}\""));
        command.creation_flags(0x08000000);
        command
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command.process_group(0).args(["-c", &text]);
        command
    }
}

fn terminate_pre_command(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // Each pre-command was born in its own group; its unreaped leader holds
        // the PID. Never target the launcher's or an unrelated process group.
        if unsafe { kill(-(child.id() as i32), 9) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(3) {
                return Err(error);
            } // ESRCH: already exited.
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let system = std::env::var_os("SystemRoot")
            .ok_or_else(|| std::io::Error::other("Windows 系统目录不可用"))?;
        let status = Command::new(Path::new(&system).join("System32/taskkill.exe"))
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(
                "命令树终止未确认，后台子命令可能仍在运行",
            ));
        }
    }
    child.wait()?;
    Ok(())
}
fn stop_commands(children: &mut Vec<Child>, tx: &Sender<Event>) {
    for mut child in children.drain(..) {
        if let Err(error) = terminate_pre_command(&mut child) {
            let _ = tx.send(Event::LaunchWarning(format!(
                "启动已取消，但停止启动前命令未确认：{error}"
            )));
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}
fn reap_commands(children: Vec<Child>) {
    for mut child in children {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
fn run_pre_launch(
    behavior: &LaunchBehavior,
    tx: &Sender<Event>,
    cancel: &AtomicBool,
) -> anyhow::Result<Vec<Child>> {
    let mut detached = Vec::new();
    for entry in &behavior.commands {
        if cancel.load(Ordering::Relaxed) {
            stop_commands(&mut detached, tx);
            check_cancel(cancel)?;
        }
        let mut command = command_process(behavior.shell_text(&entry.text, cfg!(windows)));
        command
            .current_dir(&behavior.command_cwd)
            .envs(behavior.environment())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = tx.send(Event::Log(format!(
            "正在执行{}启动前命令{}",
            entry.label,
            if entry.wait {
                "（等待完成）"
            } else {
                "（不等待）"
            }
        )));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = tx.send(Event::LaunchWarning(format!(
                    "{}启动前命令无法启动：{error}；继续启动游戏。",
                    entry.label
                )));
                continue;
            }
        };
        if !entry.wait {
            detached.push(child);
            continue;
        }
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        let _ = tx.send(Event::LaunchWarning(format!(
                            "{}启动前命令退出异常（{status}）；继续启动游戏。",
                            entry.label
                        )));
                    }
                    break;
                }
                Err(error) => {
                    stop_commands(&mut vec![child], tx);
                    let _ = tx.send(Event::LaunchWarning(format!(
                        "无法等待{}启动前命令：{error}；继续启动游戏。",
                        entry.label
                    )));
                    break;
                }
                Ok(None) => {}
            }
            if cancel.load(Ordering::Relaxed) {
                detached.push(child);
                stop_commands(&mut detached, tx);
                return Err(
                    anyhow::Error::new(pcl_core::model::OperationCancelled).context("启动已取消")
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(detached)
}

fn apply_priority(child: &Child, priority: ProcessPriority) -> std::io::Result<()> {
    if priority == ProcessPriority::Normal {
        return Ok(());
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn setpriority(which: i32, who: u32, priority: i32) -> i32;
        }
        // No elevation: negative nice may be rejected; the UI reports that fact.
        let nice = if priority == ProcessPriority::High {
            -5
        } else {
            5
        };
        if unsafe { setpriority(0, child.id(), nice) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetPriorityClass(process: *mut std::ffi::c_void, class: u32) -> i32;
        }
        let class = if priority == ProcessPriority::High {
            0x8000
        } else {
            0x4000
        };
        if unsafe { SetPriorityClass(child.as_raw_handle(), class) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child;
        Err(std::io::Error::other("此系统不支持进程优先级设置"))
    }
}

#[cfg(all(test, unix))]
fn run_child(
    child: Child,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    run_child_with_visibility(
        child,
        token,
        tx,
        stop,
        LauncherVisibility::Keep,
        GameWindowOptions::default(),
        None,
    )
}

fn run_child_with_visibility(
    mut child: Child,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    visibility: LauncherVisibility,
    window: GameWindowOptions,
    mut gpu: Option<crate::native_window::GpuPreference>,
) -> anyhow::Result<()> {
    let pid = child.id();
    let _ = tx.send(Event::GameStarted(pid));
    let ready = ReadySignal {
        pid,
        visibility,
        sent: Arc::new(AtomicBool::new(false)),
        defer: true,
    };
    let ready_flag = ready.sent.clone();
    let mut window_control = GameWindowControl::new(pid, window);
    if let Some(stream) = child.stdout.take() {
        let _ = read_game_output(stream, tx.clone(), token.clone(), Some(ready.clone()));
    }
    if let Some(stream) = child.stderr.take() {
        let _ = read_game_output(stream, tx.clone(), token, Some(ready));
    }
    let mut killed = false;
    let mut ready_published = false;
    let status = loop {
        // Observe a natural exit before handling a possibly simultaneous click.
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if !killed && stop.load(Ordering::Relaxed) {
            // Terminate only the Java process created by this launch operation.
            match child.kill() {
                Ok(()) => killed = true,
                Err(error) => {
                    if let Some(status) = child.try_wait()? {
                        break status;
                    }
                    // Keep monitoring a child that is still alive. Returning here
                    // would strand game_pid and prevent the user from trying again.
                    stop.store(false, Ordering::Relaxed);
                    let _ = tx.send(Event::GameStopFailed {
                        pid,
                        message: format!("关闭 Minecraft 失败：{error}。进程仍在运行，可以重试。"),
                    });
                }
            }
        }
        if !killed && !stop.load(Ordering::Relaxed) && ready_flag.load(Ordering::Acquire) {
            window_control.tick(
                std::time::Instant::now(),
                &tx,
                crate::native_window::game_window,
            );
            if !ready_published
                && (visibility != LauncherVisibility::CloseOnLaunch || window_control.maximize_done)
            {
                restore_gpu(&mut gpu, &tx);
                let _ = tx.send(Event::GameReady { pid, visibility });
                ready_published = true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // A descendant can inherit stdout/stderr and keep their pipes open after
    // this child exits. Readers finish independently at EOF; they may drain
    // remaining redacted logs, but must not hold the game's UI state open.
    restore_gpu(&mut gpu, &tx);
    let message = if status.success() {
        "游戏已正常退出".into()
    } else if killed {
        "已关闭运行中的 Minecraft！".into()
    } else {
        format!("游戏已退出（{status}），请查看运行日志")
    };
    let _ = tx.send(Event::GameFinished {
        pid,
        success: status.success(),
        stopped: killed,
        message,
    });
    Ok(())
}

#[derive(Clone)]
struct ReadySignal {
    pid: u32,
    visibility: LauncherVisibility,
    sent: Arc<AtomicBool>,
    defer: bool,
}
fn game_ready_line(line: &str) -> bool {
    !line.contains("[CHAT]")
        && ((line.contains("Created") && line.contains("textures") && line.contains("-atlas"))
            || line.contains("Found animation info"))
}

#[cfg(test)]
fn read_output(
    stream: impl std::io::Read + Send + 'static,
    tx: Sender<Event>,
    token: String,
) -> std::thread::JoinHandle<()> {
    read_game_output(stream, tx, token, None)
}
fn read_game_output(
    stream: impl std::io::Read + Send + 'static,
    tx: Sender<Event>,
    token: String,
    ready: Option<ReadySignal>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            line.clear();
            // Buffer a whole bounded line before redaction. If a line exceeds the
            // limit, drain and discard it instead of exposing token fragments.
            let count = match std::io::Read::take(&mut reader, 16 * 1024 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(n) => n,
                Err(_) => break,
            };
            if count == 0 {
                break;
            }
            if line.len() > 16 * 1024 {
                let mut ended = line.last() == Some(&b'\n');
                while !ended {
                    let (consumed, newline) = match reader.fill_buf() {
                        Ok([]) | Err(_) => break,
                        Ok(bytes) => match bytes.iter().position(|b| *b == b'\n') {
                            Some(index) => (index + 1, true),
                            None => (bytes.len(), false),
                        },
                    };
                    reader.consume(consumed);
                    ended = newline;
                }
                if tx
                    .send(game_log_event(ready.as_ref(), "[超长日志行已省略]".into()))
                    .is_err()
                {
                    break;
                }
                continue;
            }
            let line = String::from_utf8_lossy(&line);
            if let Some(signal) = ready.as_ref().filter(|_| game_ready_line(&line)) {
                if !signal.sent.swap(true, Ordering::Release) && !signal.defer {
                    let _ = tx.send(Event::GameReady {
                        pid: signal.pid,
                        visibility: signal.visibility,
                    });
                }
            }
            let secrets = if token.is_empty() || token == "0" {
                &[][..]
            } else {
                std::slice::from_ref(&token)
            };
            let safe = pcl_core::crash::redact(&line, secrets);
            if tx
                .send(game_log_event(ready.as_ref(), safe.trim_end().to_owned()))
                .is_err()
            {
                break;
            }
        }
    })
}

fn game_log_event(ready: Option<&ReadySignal>, line: String) -> Event {
    match ready {
        Some(signal) => Event::GameLog {
            pid: signal.pid,
            line,
        },
        None => Event::Log(line),
    }
}

pub fn open_folder(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(windows)]
    let mut command = Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = Command::new("xdg-open");
    command.arg(path).spawn().map(|_| ())
}

fn restore_gpu(gpu: &mut Option<crate::native_window::GpuPreference>, tx: &Sender<Event>) {
    if let Some(guard) = gpu.as_mut() {
        match guard.restore() {
            Ok(()) => *gpu = None,
            Err(error) => {
                // Retain the original value for another attempt at real exit.
                let _ = tx.send(Event::LaunchWarning(format!(
                    "还原所选 Java 的 GPU 偏好失败：{error:#}"
                )));
            }
        }
    }
}

struct GameWindowControl {
    pid: u32,
    options: GameWindowOptions,
    ready_at: Option<std::time::Instant>,
    next: Option<std::time::Instant>,
    title_done: bool,
    maximize_done: bool,
}
impl GameWindowControl {
    fn new(pid: u32, options: GameWindowOptions) -> Self {
        Self {
            pid,
            title_done: options.title.is_empty(),
            maximize_done: !options.maximize,
            options,
            ready_at: None,
            next: None,
        }
    }
    fn tick(
        &mut self,
        now: std::time::Instant,
        tx: &Sender<Event>,
        mut apply: impl FnMut(u32, Option<&str>, bool) -> anyhow::Result<bool>,
    ) {
        if (self.title_done && self.maximize_done) || self.next.is_some_and(|next| now < next) {
            return;
        }
        let began = *self.ready_at.get_or_insert(now);
        self.next = Some(now + Duration::from_millis(500));
        let timed_out = now.duration_since(began) > Duration::from_secs(45);
        if !self.title_done {
            let date = chrono::Local::now();
            let title = self
                .options
                .title
                .replace("{date}", &date.format("%Y/%-m/%-d").to_string())
                .replace("{time}", &date.format("%H:%M:%S").to_string());
            match apply(self.pid, Some(&title), false) {
                Ok(true) => (), // Keep the title current, like the upstream watcher.
                Ok(false) if !timed_out => (),
                Ok(false) => {
                    self.title_done = true;
                    let _ = tx.send(Event::LaunchWarning(
                        "未找到本次游戏进程的可控制窗口，未修改游戏标题。".into(),
                    ));
                }
                Err(error) => {
                    self.title_done = true;
                    let _ = tx.send(Event::LaunchWarning(format!(
                        "设置游戏窗口标题失败：{error:#}"
                    )));
                }
            }
        }
        if !self.maximize_done && now.duration_since(began) >= Duration::from_secs(2) {
            match apply(self.pid, None, true) {
                Ok(true) => self.maximize_done = true,
                Ok(false) if !timed_out => (),
                Ok(false) => {
                    self.maximize_done = true;
                    let _ = tx.send(Event::LaunchWarning(
                        "未找到本次游戏进程的可控制窗口，未请求最大化。".into(),
                    ));
                }
                Err(error) => {
                    self.maximize_done = true;
                    let _ = tx.send(Event::LaunchWarning(format!(
                        "请求游戏窗口最大化失败：{error:#}"
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn tracked_launch_stages_precede_the_real_child_and_ready_lifecycle() {
        use crate::app::launch_ui::{LaunchEvent, Stage};
        use pcl_core::{
            config::Settings,
            launch::{build_plan_with_settings, LaunchOptions},
            model::Platform,
        };
        let root = tempfile::tempdir().unwrap();
        let version = root.path().join("versions/fixture");
        std::fs::create_dir_all(&version).unwrap();
        std::fs::write(
            version.join("fixture.json"),
            r#"{"mainClass":"Fixture","minecraftArguments":""}"#,
        )
        .unwrap();
        std::fs::write(version.join("fixture.jar"), "fixture").unwrap();
        let session = pcl_core::model::Session {
            user_type: "msa".into(),
            access_token: "FIXTURE_TOKEN".into(),
            ..pcl_core::auth::offline_session("Player").unwrap()
        };
        let mut plan = build_plan_with_settings(
            &LaunchOptions {
                root: root.path().into(),
                version_id: "fixture".into(),
                java: "/bin/sh".into(),
                memory_mb: 512,
                width: 854,
                height: 480,
            },
            &session,
            &Platform::current(),
            &Settings::default(),
            21,
        )
        .unwrap();
        // Only this fixture's harmless child runs; no Minecraft, account, or
        // user launch command is accessed. A real output marker signals ready.
        plan.args = vec![
            "-c".into(),
            "printf 'Created: textures fixture-atlas\\n'; sleep 0.3".into(),
        ];
        plan.behavior = behavior(root.path(), vec![]);
        let (tx, rx) = std::sync::mpsc::channel();
        run_game_with_launch_progress(
            plan,
            String::new(),
            tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            991,
        )
        .unwrap();
        let mut stages = Vec::new();
        let mut pid = None;
        let mut ready = false;
        let mut finished = false;
        for event in rx.try_iter() {
            match event {
                Event::Launch(LaunchEvent::Stage { request, stage }) => {
                    assert_eq!(request, 991);
                    assert!(pid.is_none());
                    stages.push(stage);
                }
                Event::GameStarted(value) => {
                    assert!(pid.replace(value).is_none());
                }
                Event::GameReady { pid: value, .. } => {
                    assert_eq!(pid, Some(value));
                    assert!(!ready);
                    ready = true;
                }
                Event::GameFinished {
                    pid: value,
                    success,
                    stopped,
                    ..
                } => {
                    assert_eq!(pid, Some(value));
                    assert!(ready && success && !stopped);
                    finished = true;
                }
                _ => (),
            }
        }
        assert_eq!(stages, [Stage::PreRun, Stage::Commands, Stage::Spawn]);
        assert!(finished);
    }

    #[cfg(unix)]
    #[test]
    fn exited_child_releases_game_state_before_inherited_log_pipe_closes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        // This test's own short-lived descendant inherits the pipes. The shell
        // exits immediately; the descendant emits a final line after two seconds.
        let child = Command::new("/bin/sh")
            .args(["-c", "(sleep 2; printf 'descendant closed pipe\\n') &"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let worker = std::thread::spawn(move || {
            let result = run_child(child, String::new(), tx, Arc::new(AtomicBool::new(false)));
            done_tx.send(result).unwrap();
        });
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Event::GameStarted(_)
        ));
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Event::GameFinished {message, ..} if message == "游戏已正常退出")
        );
        done_rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        // Drain the fixture's descendant and both reader threads before leaving.
        let mut log = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(4)) {
                Ok(Event::Log(line) | Event::GameLog { line, .. }) => log.push(line),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                event => panic!(
                    "unexpected post-exit event: {}",
                    match event {
                        Ok(_) => "non-log",
                        Err(_) => "timeout",
                    }
                ),
            }
        }
        assert_eq!(log, ["descendant closed pipe"]);
    }

    #[cfg(unix)]
    #[test]
    fn stop_only_kills_the_launched_child_and_waits_for_its_exit() {
        let mut sentinel = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let expected_pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || run_child(child, String::new(), tx, worker_stop));
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Event::GameStarted(pid) if pid == expected_pid)
        );
        stop.store(true, Ordering::Relaxed);
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Event::GameFinished {message, ..} if message == "已关闭运行中的 Minecraft！")
        );
        worker.join().unwrap().unwrap();
        assert!(
            sentinel.try_wait().unwrap().is_none(),
            "another process must remain alive"
        );
        sentinel.kill().unwrap();
        sentinel.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn late_stop_request_does_not_relabel_a_natural_exit() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        assert!(child.wait().unwrap().success());
        let (tx, rx) = std::sync::mpsc::channel();
        run_child(child, String::new(), tx, Arc::new(AtomicBool::new(true))).unwrap();
        assert!(matches!(rx.recv().unwrap(), Event::GameStarted(_)));
        assert!(
            matches!(rx.recv().unwrap(), Event::GameFinished {message, ..} if message == "游戏已正常退出")
        );
    }

    #[test]
    fn token_straddling_log_limit_never_leaks() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut bytes = vec![b'a'; 16380];
        bytes.extend_from_slice(b"secret-value\nsafe next line\n");
        read_output(std::io::Cursor::new(bytes), tx, "secret-value".into())
            .join()
            .unwrap();
        let values: Vec<String> = rx
            .try_iter()
            .filter_map(|v| match v {
                Event::Log(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(values, ["[超长日志行已省略]", "safe next line"]);
    }
    #[test]
    fn game_output_redacts_session_token() {
        let (tx, rx) = std::sync::mpsc::channel();
        read_output(
            std::io::Cursor::new(b"hello\ntoken=secret-value\n"),
            tx,
            "secret-value".into(),
        )
        .join()
        .unwrap();
        let values: Vec<String> = rx
            .try_iter()
            .filter_map(|v| match v {
                Event::Log(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(values, ["hello", "token=<redacted>"]);
    }
    #[cfg(unix)]
    fn behavior(root: &Path, commands: Vec<pcl_core::launch::PreLaunchCommand>) -> LaunchBehavior {
        LaunchBehavior {
            visibility: LauncherVisibility::Keep,
            priority: ProcessPriority::Normal,
            window: Default::default(),
            memory_optimize: false,
            auto_chinese: false,
            language_code: "zh_cn".into(),
            high_performance_gpu: false,
            offline_skin: None,
            warnings: vec![],
            command_cwd: root.into(),
            commands,
            variables: vec![],
        }
    }
    #[cfg(unix)]
    #[test]
    fn pre_commands_order_wait_and_nonzero_continue_without_logging_secrets() {
        use pcl_core::launch::PreLaunchCommand;
        let dir = tempfile::tempdir().unwrap();
        let plan = behavior(
            dir.path(),
            vec![
                PreLaunchCommand {
                    label: "全局",
                    text: "printf G > order; printf 'PRIVATE-MARKER'; exit 7".into(),
                    wait: true,
                },
                PreLaunchCommand {
                    label: "版本",
                    text: "printf V >> order".into(),
                    wait: true,
                },
            ],
        );
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(run_pre_launch(&plan, &tx, &AtomicBool::new(false))
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("order")).unwrap(),
            "GV"
        );
        let mut warnings = 0;
        for event in rx.try_iter() {
            match event {
                Event::Log(text) => assert!(!text.contains("PRIVATE-MARKER")),
                Event::LaunchWarning(text) => {
                    warnings += 1;
                    assert!(!text.contains("PRIVATE-MARKER"));
                }
                _ => panic!("unexpected event"),
            }
        }
        assert_eq!(warnings, 1);
    }
    #[cfg(unix)]
    #[test]
    fn path_markers_are_data_in_all_shell_quote_contexts() {
        use pcl_core::launch::PreLaunchCommand;
        let dir = tempfile::tempdir().unwrap();
        let value = "space ' \" $(touch unexpected) ; & ! %";
        let mut plan = behavior(
            dir.path(),
            vec![PreLaunchCommand {
                label: "全局",
                text: "printf '%s\\n' {name} \"{name}\" '{name}' > observed".into(),
                wait: true,
            }],
        );
        plan.variables.push(("{name}".into(), value.into()));
        let (tx, _) = std::sync::mpsc::channel();
        run_pre_launch(&plan, &tx, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("observed")).unwrap(),
            format!("{value}\n{value}\n{value}\n")
        );
        assert!(!dir.path().join("unexpected").exists());
        let windows = plan.shell_text("echo \"{name}\"", true);
        assert_eq!(windows, "echo \"!PCL_LAUNCH_0!\"");
        assert!(!windows.contains(value));
    }
    #[cfg(unix)]
    #[test]
    fn cancelling_waiting_command_stops_its_descendant_before_delayed_write() {
        use pcl_core::launch::PreLaunchCommand;
        let dir = tempfile::tempdir().unwrap();
        let plan = behavior(
            dir.path(),
            vec![PreLaunchCommand {
                label: "全局",
                text: "printf started > begun; (sleep 0.7; printf bad > late) & wait".into(),
                wait: true,
            }],
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, _) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || run_pre_launch(&plan, &tx, &worker_cancel));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !dir.path().join("begun").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(worker
            .join()
            .unwrap()
            .unwrap_err()
            .is::<pcl_core::model::OperationCancelled>());
        std::thread::sleep(Duration::from_millis(850));
        assert!(!dir.path().join("late").exists());
    }
    #[cfg(unix)]
    #[test]
    fn no_wait_returns_while_owned_command_continues_and_low_priority_is_real() {
        use pcl_core::launch::PreLaunchCommand;
        let dir = tempfile::tempdir().unwrap();
        let plan = behavior(
            dir.path(),
            vec![PreLaunchCommand {
                label: "全局",
                text: "sleep 0.3; printf complete > result".into(),
                wait: false,
            }],
        );
        let (tx, _) = std::sync::mpsc::channel();
        let began = std::time::Instant::now();
        let mut children = run_pre_launch(&plan, &tx, &AtomicBool::new(false)).unwrap();
        assert!(began.elapsed() < Duration::from_millis(250));
        assert_eq!(children.len(), 1);
        apply_priority(&children[0], ProcessPriority::Low).unwrap();
        unsafe extern "C" {
            fn getpriority(which: i32, who: u32) -> i32;
        }
        assert_eq!(unsafe { getpriority(0, children[0].id()) }, 5);
        children[0].wait().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("result")).unwrap(),
            "complete"
        );
    }
    #[test]
    fn game_readiness_requires_source_loading_marker_and_is_sent_once() {
        let (tx, rx) = std::sync::mpsc::channel();
        let signal = ReadySignal {
            pid: 55,
            visibility: LauncherVisibility::HideThenRestore,
            sent: Arc::new(AtomicBool::new(false)),
            defer: false,
        };
        read_game_output(std::io::Cursor::new(b"[CHAT] Created textures x-atlas\nSetting user: Player\nLWJGL Version\nCreated: textures x-atlas\nFound animation info\n"),tx,String::new(),Some(signal)).join().unwrap();
        let ready = rx
            .try_iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::GameReady {
                        pid: 55,
                        visibility: LauncherVisibility::HideThenRestore
                    }
                )
            })
            .count();
        assert_eq!(ready, 1);
    }
    #[test]
    fn window_control_keeps_pid_delays_maximize_and_does_not_retry_a_failed_title() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut control = GameWindowControl::new(
            771,
            GameWindowOptions {
                title: "Fixture {date} {time}".into(),
                maximize: true,
            },
        );
        let now = std::time::Instant::now();
        let mut calls = Vec::new();
        control.tick(now, &tx, |pid, title, maximize| {
            calls.push((pid, title.map(str::to_owned), maximize));
            anyhow::bail!("fixture read-only title")
        });
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 771);
        assert!(!calls[0].1.as_ref().unwrap().contains("{date}"));
        assert!(!calls[0].2);
        control.tick(now + Duration::from_millis(100), &tx, |_, _, _| {
            panic!("must be throttled")
        });
        control.tick(now + Duration::from_secs(2), &tx, |pid, title, maximize| {
            assert_eq!(pid, 771);
            assert!(title.is_none() && maximize);
            Ok(true)
        });
        control.tick(now + Duration::from_secs(3), &tx, |_, _, _| {
            panic!("completed controls must not toggle zoom again")
        });
        assert_eq!(
            rx.try_iter()
                .filter(|event| matches!(event, Event::LaunchWarning(_)))
                .count(),
            1
        );
    }
    #[test]
    fn empty_window_settings_never_touch_native_apis() {
        let (tx, _) = std::sync::mpsc::channel();
        GameWindowControl::new(771, GameWindowOptions::default()).tick(
            std::time::Instant::now(),
            &tx,
            |_, _, _| panic!("no user window request"),
        );
        assert!(crate::native_window::game_window(0, None, true).is_err());
        assert!(crate::native_window::game_window(std::process::id(), Some("x"), false).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn a_real_child_publishes_ready_once_before_exit_without_window_requests() {
        let (tx, rx) = std::sync::mpsc::channel();
        let child = Command::new("/bin/sh")
            .args(["-c", "printf 'Created textures fixture-atlas\n'; sleep 0.3"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        run_child(child, "0".into(), tx, Arc::new(AtomicBool::new(false))).unwrap();
        let events = rx.try_iter().collect::<Vec<_>>();
        let ready = events
            .iter()
            .position(|e| matches!(e,Event::GameReady{pid:p,..} if *p==pid))
            .unwrap();
        let finished = events
            .iter()
            .position(|e| matches!(e,Event::GameFinished{pid:p,..} if *p==pid))
            .unwrap();
        assert!(ready < finished);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::GameReady { .. }))
                .count(),
            1
        );
    }
}
