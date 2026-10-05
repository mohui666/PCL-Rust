//! Scoped download jobs. Conflicting directories queue; independent roots run concurrently.
use super::Event;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{SendError, Sender},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct JobId(pub(super) u64);
#[derive(Clone, Debug)]
pub(super) struct JobContext {
    pub root: PathBuf,
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
fn normalized(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        return normalized(parent).join(name);
    }
    path.to_path_buf()
}
fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (normalized(a), normalized(b));
    a.starts_with(&b) || b.starts_with(&a)
}
#[derive(Default)]
struct Scheduler {
    queue: Mutex<BTreeMap<JobId, (Vec<PathBuf>, bool)>>,
    changed: Condvar,
}
struct Permit {
    id: JobId,
    scheduler: Arc<Scheduler>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.scheduler
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        self.scheduler.changed.notify_all();
    }
}
impl Scheduler {
    fn acquire(self: &Arc<Self>, id: JobId, cancel: &AtomicBool) -> anyhow::Result<Permit> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancel.load(Ordering::Relaxed) {
                queue.remove(&id);
                self.changed.notify_all();
                return Err(pcl_core::model::OperationCancelled.into());
            }
            let root = queue
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("任务排队信息不存在"))?
                .0
                .clone();
            let eligible = queue.values().filter(|(_, running)| *running).count() < 4
                && !queue.iter().any(|(other, (path, running))| {
                    *other != id
                        && (*running || *other < id)
                        && path.iter().any(|a| root.iter().any(|b| overlaps(a, b)))
                });
            if eligible {
                queue.get_mut(&id).unwrap().1 = true;
                return Ok(Permit {
                    id,
                    scheduler: self.clone(),
                });
            }
            queue = self
                .changed
                .wait_timeout(queue, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
pub(crate) struct JobMessage {
    pub(super) id: JobId,
    pub(super) event: Box<Event>,
}
pub(super) type JobWork = Arc<dyn Fn(JobSender) + Send + Sync>;
#[derive(Clone)]
pub(super) struct JobSender {
    pub(super) id: JobId,
    tx: Sender<Event>,
    scheduler: Arc<Scheduler>,
    cancel: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    work: Arc<Mutex<BTreeMap<JobId, JobWork>>>,
}
impl JobSender {
    pub fn send(&self, event: Event) -> Result<(), Box<SendError<Event>>> {
        if event.is_download_terminal() {
            // Cancelling a diagnostic wait cannot undo a completed file commit.
            let _ = pcl_core::system::debug_delay(
                &self.cancel,
                pcl_core::system::DebugPhase::JobFinish,
            );
            self.terminal.store(true, Ordering::Release);
        }
        self.tx
            .send(Event::Job(JobMessage {
                id: self.id,
                event: Box::new(event),
            }))
            .map_err(Box::new)
    }
    pub fn disable_retry(&self) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(recipe) = work.get(&self.id).cloned() {
            work.retain(|_, candidate| !Arc::ptr_eq(candidate, &recipe));
        }
    }
    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    pub fn spawn<F: FnOnce(JobSender) + Clone + Send + 'static>(self, work: F) {
        let saved = Mutex::new(work);
        let work: JobWork = Arc::new(move |sender| {
            let f = saved.lock().unwrap_or_else(|e| e.into_inner()).clone();
            f(sender);
        });
        self.launch(work);
    }
    pub fn launch(self, work: JobWork) {
        self.work
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.id, work.clone());
        std::thread::spawn(move || {
            let permit = match self.scheduler.acquire(self.id, &self.cancel) {
                Ok(permit) => permit,
                Err(error) => {
                    let _ = self.send(Event::download_failed("排队任务已取消", error));
                    return;
                }
            };
            if let Err(error) =
                pcl_core::system::debug_delay(&self.cancel, pcl_core::system::DebugPhase::JobStart)
            {
                let _ = self.send(Event::download_failed("任务开始前已取消", error));
                return;
            }
            let _ = self.send(Event::JobStarted);
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self.clone())));
            // A terminal is required even when a worker panics, otherwise its card never releases.
            if !self.terminal.load(Ordering::Acquire) {
                let message = if result.is_err() {
                    "下载线程异常退出"
                } else {
                    "下载线程未返回任务结果"
                };
                let _ = self.send(Event::DownloadFailed {
                    message: message.into(),
                    cancelled: false,
                });
            }
            drop(permit);
        });
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
    active: BTreeMap<JobId, ActiveJob>,
    scheduler: Arc<Scheduler>,
    work: Arc<Mutex<BTreeMap<JobId, JobWork>>>,
}
impl Jobs {
    pub fn cancel_all(&self) {
        for job in self.active.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn retry(&self, id: JobId) -> Option<JobWork> {
        self.work
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }
    pub fn forget(&mut self, id: JobId) {
        if !self.active.contains_key(&id) {
            self.work
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
        }
    }
    pub fn is_active(&self) -> bool {
        !self.active.is_empty()
    }
    pub fn get(&self, id: JobId) -> Option<&ActiveJob> {
        self.active.get(&id)
    }
    pub fn conflicts_with(&self, root: &Path) -> bool {
        self.scheduler
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|(paths, _)| paths.iter().any(|path| overlaps(path, root)))
    }
    pub fn begin(
        &mut self,
        context: JobContext,
        tx: Sender<Event>,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<JobSender> {
        anyhow::ensure!(
            !self
                .active
                .values()
                .any(|a| normalized(&a.context.root) == normalized(&context.root)
                    && a.context.target == context.target),
            "此目标已有待完成的任务"
        );
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("任务编号已耗尽"))?;
        let id = JobId(self.next);
        self.scheduler
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id,
                (vec![context.root.clone(), context.game_root.clone()], false),
            );
        self.active.insert(
            id,
            ActiveJob {
                id,
                context,
                cancel: cancel.clone(),
            },
        );
        Ok(JobSender {
            id,
            tx,
            scheduler: self.scheduler.clone(),
            cancel,
            terminal: Arc::new(AtomicBool::new(false)),
            work: self.work.clone(),
        })
    }
    pub fn route(&mut self, id: JobId, terminal: bool) -> Option<ActiveJob> {
        if terminal {
            self.active.remove(&id)
        } else {
            self.active.get(&id).cloned()
        }
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
            assert_eq!(jobs.active.values().next().unwrap().id, before);
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
                context("/old", "instance"),
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

#[cfg(test)]
mod scheduler_tests {
    use super::*;
    fn context(root: &Path, target: &str) -> JobContext {
        JobContext {
            root: root.into(),
            game_root: root.into(),
            target: Some(target.into()),
        }
    }
    #[test]
    fn conflicting_writes_wait_for_body_cleanup_but_independent_roots_run() {
        let dirs = tempfile::tempdir().unwrap();
        let root = dirs.path().join("game");
        let other = dirs.path().join("other");
        let (tx, rx) = std::sync::mpsc::channel();
        let (begin, events) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = Arc::new(Mutex::new(wait));
        let mut jobs = Jobs::default();
        let a = jobs
            .begin(
                context(&root, "a"),
                tx.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let started = begin.clone();
        a.spawn(move |tx| {
            started.send("a").unwrap();
            tx.send(Event::Done("a".into())).unwrap();
            wait.lock().unwrap().recv().unwrap();
        });
        assert_eq!(events.recv_timeout(Duration::from_secs(2)).unwrap(), "a");
        for _ in 0..2 {
            let Event::Job(message) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
                panic!()
            };
            jobs.route(message.id, message.event.is_download_terminal());
        }
        assert!(
            jobs.conflicts_with(&root),
            "terminal UI event cannot release a worker still cleaning up"
        );
        let b = jobs
            .begin(
                context(&root, "b"),
                tx.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let started = begin.clone();
        b.spawn(move |tx| {
            started.send("b").unwrap();
            let _ = tx.send(Event::Done("b".into()));
        });
        let c = jobs
            .begin(context(&other, "c"), tx, Arc::new(AtomicBool::new(false)))
            .unwrap();
        c.spawn(move |tx| {
            begin.send("c").unwrap();
            let _ = tx.send(Event::Done("c".into()));
        });
        assert_eq!(events.recv_timeout(Duration::from_secs(2)).unwrap(), "c");
        assert!(events.recv_timeout(Duration::from_millis(80)).is_err());
        release.send(()).unwrap();
        assert_eq!(events.recv_timeout(Duration::from_secs(2)).unwrap(), "b");
    }
    #[test]
    fn queue_cancellation_never_runs_writer_and_retry_uses_fresh_cancel_token() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut jobs = Jobs::default();
        let cancelled = Arc::new(AtomicBool::new(true));
        let worked = Arc::new(AtomicBool::new(false));
        let a = jobs
            .begin(context(dir.path(), "a"), tx.clone(), cancelled)
            .unwrap();
        let id = a.id;
        let did_work = worked.clone();
        a.spawn(move |tx| {
            assert!(!tx.cancel_token().load(Ordering::Relaxed));
            did_work.store(true, Ordering::Relaxed);
            let _ = tx.send(Event::Done("done".into()));
        });
        let Event::Job(message) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!()
        };
        assert!(matches!(
            *message.event,
            Event::DownloadFailed {
                cancelled: true,
                ..
            }
        ));
        assert!(!worked.load(Ordering::Relaxed));
        jobs.route(id, true);
        let retry = jobs.retry(id).unwrap();
        let b = jobs
            .begin(
                context(dir.path(), "a"),
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        b.launch(retry);
        let mut done = false;
        for _ in 0..2 {
            let Event::Job(message) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
                panic!()
            };
            done |= matches!(*message.event, Event::Done(_));
        }
        assert!(done && worked.load(Ordering::Relaxed));
    }
    #[test]
    fn panicked_writer_reports_failure_and_releases_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut jobs = Jobs::default();
        let a = jobs
            .begin(
                context(dir.path(), "a"),
                tx.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        a.spawn(|_| panic!("fixture"));
        let mut failed = false;
        for _ in 0..2 {
            let Event::Job(m) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
                panic!()
            };
            failed |= matches!(
                *m.event,
                Event::DownloadFailed {
                    cancelled: false,
                    ..
                }
            );
        }
        assert!(failed);
        let b = jobs
            .begin(
                context(dir.path(), "b"),
                tx,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        b.spawn(|tx| {
            let _ = tx.send(Event::Done("b".into()));
        });
        let mut done = false;
        for _ in 0..2 {
            let Event::Job(m) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
                panic!()
            };
            done |= matches!(*m.event, Event::Done(_));
        }
        assert!(done);
    }
}
