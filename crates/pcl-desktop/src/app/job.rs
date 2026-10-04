//! Identity and ownership for the single active download/install writer.
//! Browsing generations remain separate: a stale view must not strand a writer.
use super::Event;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{SendError, Sender},
        Arc,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct JobId(u64);
#[derive(Clone, Debug)]
pub(super) struct JobContext {
    /// Actual write root, which differs from the game root for Java downloads.
    pub root: PathBuf,
    /// The game-root selection at kickoff, used only to guard UI side effects.
    pub game_root: PathBuf,
    pub target: Option<String>,
}
impl JobContext {
    pub fn applies_to(&self, game_root: &Path) -> bool {
        self.game_root == game_root
    }
    pub fn description(&self) -> String {
        format!(
            "{} · {}",
            self.target.as_deref().unwrap_or("下载"),
            self.root.display()
        )
    }
}
pub(crate) struct JobMessage {
    pub(super) id: JobId,
    pub(super) event: Box<Event>,
}
#[derive(Clone)]
pub(super) struct JobSender {
    id: JobId,
    tx: Sender<Event>,
}
impl JobSender {
    pub fn send(&self, event: Event) -> Result<(), Box<SendError<Event>>> {
        self.tx
            .send(Event::Job(JobMessage {
                id: self.id,
                event: Box::new(event),
            }))
            .map_err(Box::new)
    }
}
#[derive(Clone)]
pub(super) struct ActiveJob {
    pub id: JobId,
    pub context: JobContext,
    pub cancel: Arc<AtomicBool>,
}
impl ActiveJob {
    pub fn label(&self) -> String {
        format!("任务 #{} · {}", self.id.0, self.context.description())
    }
    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}
#[derive(Default)]
pub(super) struct Jobs {
    next: u64,
    active: Option<ActiveJob>,
}
impl Jobs {
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
    pub fn begin(
        &mut self,
        context: JobContext,
        tx: Sender<Event>,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<JobSender> {
        anyhow::ensure!(self.active.is_none(), "已有下载作业尚未结束");
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("下载作业编号已耗尽，请重新打开启动器"))?;
        let id = JobId(self.next);
        self.active = Some(ActiveJob {
            id,
            context,
            cancel,
        });
        Ok(JobSender { id, tx })
    }
    /// Releasing the writer depends only on its actual terminal event, never
    /// the cancellation flag, a request generation, or the selected directory.
    pub fn route(&mut self, id: JobId, terminal: bool) -> Option<ActiveJob> {
        let active = self
            .active
            .as_ref()
            .filter(|active| active.id == id)?
            .clone();
        if terminal {
            self.active = None;
        }
        Some(active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    fn context(root: &str, target: &str) -> JobContext {
        JobContext {
            root: root.into(),
            game_root: root.into(),
            target: Some(target.into()),
        }
    }
    fn unwrap_job(event: Event) -> JobMessage {
        match event {
            Event::Job(message) => message,
            _ => panic!("all download sends must be scoped"),
        }
    }
    #[test]
    fn a_late_progress_log_and_terminal_cannot_touch_b_or_release_its_lock() {
        let (tx, rx) = mpsc::channel();
        let mut jobs = Jobs::default();
        let a = jobs
            .begin(
                context("/a", "first"),
                tx.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        a.send(Event::Installed("first".into())).unwrap();
        let event = unwrap_job(rx.recv().unwrap());
        assert!(jobs
            .route(event.id, event.event.is_download_terminal())
            .is_some());
        let b = jobs
            .begin(
                context("/b", "second"),
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let before = b.id;
        for payload in [
            Event::Progress(Default::default()),
            Event::Log("late a".into()),
            Event::Installed("first".into()),
        ] {
            a.send(payload).unwrap();
            let message = unwrap_job(rx.recv().unwrap());
            assert!(jobs
                .route(message.id, message.event.is_download_terminal())
                .is_none());
            assert_eq!(jobs.active.as_ref().unwrap().id, before);
        }
        assert!(jobs.is_active());
    }
    #[test]
    fn cancelled_writer_holds_lock_until_real_terminal_and_new_cancel_is_independent() {
        let (tx, rx) = mpsc::channel();
        let mut jobs = Jobs::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let a = jobs
            .begin(context("/old", "instance"), tx.clone(), cancel.clone())
            .unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(jobs
            .begin(
                context("/new", "other"),
                tx.clone(),
                Arc::new(AtomicBool::new(false))
            )
            .is_err());
        // A genuine error racing with cancellation remains a failure.
        a.send(Event::download_failed(
            "下载失败",
            anyhow::anyhow!("SHA-1 mismatch"),
        ))
        .unwrap();
        let message = unwrap_job(rx.recv().unwrap());
        let active = jobs
            .route(message.id, message.event.is_download_terminal())
            .unwrap();
        assert!(active.cancel_requested());
        assert!(matches!(
            *message.event,
            Event::DownloadFailed {
                cancelled: false,
                ..
            }
        ));
        let new_cancel = Arc::new(AtomicBool::new(false));
        jobs.begin(context("/new", "other"), tx, new_cancel.clone())
            .unwrap();
        assert!(!new_cancel.load(Ordering::Relaxed));
        assert!(cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn root_switch_and_stale_resource_view_do_not_hide_terminal_or_retarget_it() {
        let (tx, rx) = mpsc::channel();
        let mut jobs = Jobs::default();
        let sender = jobs
            .begin(
                context("/old", "old-instance"),
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        sender
            .send(Event::Resource(
                super::super::resource_ui::ResourceEvent::Installed(
                    "old-instance".into(),
                    "one.jar".into(),
                ),
            ))
            .unwrap();
        let message = unwrap_job(rx.recv().unwrap());
        assert!(message.event.is_download_terminal());
        let active = jobs.route(message.id, true).unwrap();
        assert!(!active.context.applies_to(Path::new("/new")));
        assert_eq!(active.context.target.as_deref(), Some("old-instance"));
        assert!(!jobs.is_active());
    }
    #[test]
    fn successful_commit_is_not_relabelled_cancelled_by_a_late_click() {
        let (tx, rx) = mpsc::channel();
        let mut jobs = Jobs::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let sender = jobs
            .begin(context("/game", "done"), tx, cancel.clone())
            .unwrap();
        sender.send(Event::Installed("done".into())).unwrap();
        cancel.store(true, Ordering::Relaxed);
        let message = unwrap_job(rx.recv().unwrap());
        let active = jobs
            .route(message.id, message.event.is_download_terminal())
            .unwrap();
        assert!(active.cancel_requested());
        assert!(matches!(*message.event,Event::Installed(ref id) if id=="done"));
        assert!(!jobs.is_active());
    }
}
