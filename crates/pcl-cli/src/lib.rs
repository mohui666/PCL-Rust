//! Window-free frontend shared by the console executable and desktop argument entry.
mod args;
mod game;
mod operations;

use anyhow::{Context, Result};
use args::*;
use clap::Parser;
use pcl_core::{
    config, metadata,
    model::{OperationCancelled, Platform, Progress},
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// Returns an OS exit code. Argument parsing happens before settings or signal setup.
pub fn main_entry() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return code;
        }
    };
    let output = Output { json: cli.json };
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let result = (|| {
        ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
            .context("无法安装取消信号处理器")?;
        let config_path = cli.config.unwrap_or_else(config::settings_path);
        let settings = config::load_settings(&config_path)?;
        pcl_core::network::configure(&settings.downloads)?;
        let root = cli.root.unwrap_or_else(|| settings.game_root.clone());
        anyhow::ensure!(root.is_absolute(), "--root 必须是绝对路径");
        let context = RuntimeContext {
            root,
            settings,
            config_path,
            cancel,
            output,
            platform: Platform::current(),
        };
        let result = operations::execute(&context, cli.command)?;
        context.check_cancel()?;
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &result)?;
        writeln!(stdout)?;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            let code = if error.is::<OperationCancelled>() {
                130
            } else if let Some(exit) = error.downcast_ref::<game::GameExit>() {
                exit.0
            } else {
                1
            };
            output.event(
                "error",
                &pcl_core::crash::redact(&format!("{error:#}"), &[]),
            );
            code
        }
    }
}

pub(crate) struct RuntimeContext {
    root: PathBuf,
    settings: config::Settings,
    config_path: PathBuf,
    platform: Platform,
    cancel: Arc<AtomicBool>,
    output: Output,
}
impl RuntimeContext {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(OperationCancelled.into());
        }
        Ok(())
    }
    fn progress(&self, progress: Progress) {
        if self.output.json {
            self.output
                .value(json!({"type":"progress", "message":progress.message,
                "completed":progress.completed,"total":progress.total,
                "stage":progress.stage.map(|stage| format!("{stage:?}")),
                "stage_progress":progress.stage_progress}));
        } else {
            self.output.event(
                "progress",
                &format!(
                    "{} / {}  {}",
                    progress.completed, progress.total, progress.message
                ),
            );
        }
    }
    fn instance(&self, id: &str) -> Result<PathBuf> {
        metadata::resolve_version(&self.root, id)?;
        config::instance_game_dir(&self.root, id)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Output {
    json: bool,
}
impl Output {
    fn value(&self, value: Value) {
        // A closed progress pipe must not panic inside a parallel download worker.
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{value}");
    }
    fn event(&self, kind: &str, message: &str) {
        if self.json {
            self.value(json!({"type":kind,"message":message}));
        } else {
            let _ = writeln!(std::io::stderr().lock(), "{message}");
        }
    }
}

fn value(data: impl Serialize) -> Result<Value> {
    Ok(serde_json::to_value(data)?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    use std::io::Read;
    let file =
        std::fs::File::open(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 16 * 1024 * 1024, "JSON 文件超过 16 MiB");
    Ok(serde_json::from_slice(&bytes)?)
}

fn redacted(mut data: Value) -> Value {
    match &mut data {
        Value::String(text) => *text = pcl_core::crash::redact(text, &[]),
        Value::Array(values) => {
            for item in values {
                *item = redacted(item.take());
            }
        }
        Value::Object(values) => {
            for (key, item) in values {
                if ["password", "access_token", "refresh_token", "api_key"].contains(&key.as_str())
                {
                    *item = json!("<redacted>");
                } else {
                    *item = redacted(item.take());
                }
            }
        }
        _ => (),
    }
    data
}
