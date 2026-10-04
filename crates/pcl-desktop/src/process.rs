use crate::app::Event;
use pcl_core::launch::LaunchPlan;
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

pub fn run_game(
    plan: LaunchPlan,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&plan.cwd)?;
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
    run_child(command.spawn()?, token, tx, stop)
}

fn run_child(
    mut child: Child,
    token: String,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let _ = tx.send(Event::GameStarted(child.id()));
    if let Some(stream) = child.stdout.take() {
        let _ = read_output(stream, tx.clone(), token.clone());
    }
    if let Some(stream) = child.stderr.take() {
        let _ = read_output(stream, tx.clone(), token);
    }
    let mut killed = false;
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
                    let _ = tx.send(Event::Error(format!(
                        "关闭 Minecraft 失败：{error}。进程仍在运行，可以重试。"
                    )));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // A descendant can inherit stdout/stderr and keep their pipes open after
    // this child exits. Readers finish independently at EOF; they may drain
    // remaining redacted logs, but must not hold the game's UI state open.
    let _ = tx.send(Event::GameExited(if status.success() {
        "游戏已正常退出".into()
    } else if killed {
        "已关闭运行中的 Minecraft！".into()
    } else {
        format!("游戏已退出（{status}），请查看运行日志")
    }));
    Ok(())
}

fn read_output(
    stream: impl std::io::Read + Send + 'static,
    tx: Sender<Event>,
    token: String,
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
                if tx.send(Event::Log("[超长日志行已省略]".into())).is_err() {
                    break;
                }
                continue;
            }
            let line = String::from_utf8_lossy(&line);
            let safe = if token.is_empty() || token == "0" {
                line.into_owned()
            } else {
                line.replace(&token, "<redacted>")
            };
            if tx.send(Event::Log(safe.trim_end().to_owned())).is_err() {
                break;
            }
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;

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
            matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Event::GameExited(message) if message == "游戏已正常退出")
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
                Ok(Event::Log(line)) => log.push(line),
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
            matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Event::GameExited(message) if message == "已关闭运行中的 Minecraft！")
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
            matches!(rx.recv().unwrap(), Event::GameExited(message) if message == "游戏已正常退出")
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
}
