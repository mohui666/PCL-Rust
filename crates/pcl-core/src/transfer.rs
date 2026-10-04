//! Explicit per-install telemetry. No globals, thread-local state, or inferred network sizes.
use crate::model::{Progress, ProgressStage, TransferProgress};
use anyhow::Result;
use std::{
    io::{self, Read},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_millis(150);

pub(crate) struct TransferTracker<'a> {
    callback: &'a (dyn Fn(Progress) + Sync),
    start: Instant,
    state: Mutex<State>,
}

struct State {
    message: String,
    stage: ProgressStage,
    stage_completed: u64,
    stage_total: u64,
    completed: u64,
    total: u64,
    bytes: u64,
    remaining: Option<u64>,
    active: u32,
    concurrency_limit: Option<u32>,
}

impl<'a> TransferTracker<'a> {
    fn new(callback: &'a (dyn Fn(Progress) + Sync), stage: ProgressStage) -> Self {
        Self {
            callback,
            start: Instant::now(),
            state: Mutex::new(State {
                message: String::new(),
                stage,
                stage_completed: 0,
                stage_total: 0,
                completed: 0,
                total: 0,
                bytes: 0,
                remaining: None,
                active: 0,
                concurrency_limit: None,
            }),
        }
    }

    fn event(&self, state: &State, plan: Option<Vec<ProgressStage>>) -> Progress {
        Progress {
            message: state.message.clone(),
            completed: state.completed,
            total: state.total,
            stage: Some(state.stage),
            stage_progress: Some((state.stage_completed, state.stage_total)),
            plan,
            transfer: Some(TransferProgress {
                downloaded_bytes: state.bytes,
                elapsed_ms: self.start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                remaining_files: state.remaining,
                active_downloads: Some(state.active),
                concurrency_limit: state.concurrency_limit,
            }),
        }
    }

    fn emit(&self, plan: Option<Vec<ProgressStage>>) {
        // Serialize both snapshots and delivery so parallel workers cannot emit an
        // older sample after a newer sample. Callbacks never receive this tracker.
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (self.callback)(self.event(&state, plan));
    }

    pub(crate) fn begin_stage(
        &self,
        stage: ProgressStage,
        message: impl Into<String>,
        count: u64,
        concurrency_limit: Option<u32>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stage = stage;
        state.message = message.into();
        state.stage_completed = 0;
        state.stage_total = count;
        state.concurrency_limit = concurrency_limit;
        (self.callback)(self.event(&state, None));
    }

    pub(crate) fn set_concurrency_limit(&self, limit: Option<u32>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.concurrency_limit = limit;
        (self.callback)(self.event(&state, None));
    }

    pub(crate) fn message(&self, message: impl Into<String>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).message = message.into();
    }

    pub(crate) fn plan_files(&self, total: u64, pending_downloads: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.total = total;
        state.remaining = Some(pending_downloads);
    }

    pub(crate) fn finished_item(&self, overall: bool, download: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stage_completed += 1;
        if overall {
            state.completed += 1;
        }
        if download {
            state.remaining = state.remaining.map(|remaining| remaining.saturating_sub(1));
        }
    }

    pub(crate) fn finish_stage(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Indeterminate metadata/local verification represents one finished operation.
        if state.stage_total == 0 {
            state.stage_total = 1;
            state.stage_completed = 1;
        }
        debug_assert_eq!(state.stage_completed, state.stage_total);
        (self.callback)(self.event(&state, None));
    }

    pub(crate) fn begin_download(&self) -> ActiveDownload<'_, 'a> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).active += 1;
        ActiveDownload { tracker: self }
    }

    pub(crate) fn record_bytes(&self, count: usize) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).bytes += count as u64;
    }
}

pub(crate) struct ActiveDownload<'t, 'a> {
    tracker: &'t TransferTracker<'a>,
}
impl Drop for ActiveDownload<'_, '_> {
    fn drop(&mut self) {
        let mut state = self.tracker.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_sub(1);
    }
}

pub(crate) struct TrackedReader<'r, 't, R> {
    pub(crate) source: R,
    pub(crate) tracker: Option<&'r TransferTracker<'t>>,
    pub(crate) active: Option<ActiveDownload<'r, 't>>,
}
impl<R: Read> Read for TrackedReader<'_, '_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read(output)?;
        if let Some(tracker) = self.tracker {
            tracker.record_bytes(count);
        }
        if count == 0 {
            self.active.take();
        }
        Ok(count)
    }
}

struct StopTimer(mpsc::Sender<()>);
impl Drop for StopTimer {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

pub(crate) fn with_transfer<T>(
    callback: &(dyn Fn(Progress) + Sync),
    plan: Vec<ProgressStage>,
    message: &str,
    work: impl FnOnce(&TransferTracker<'_>) -> Result<T>,
) -> Result<T> {
    let tracker = TransferTracker::new(callback, plan[0]);
    tracker.message(message);
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        // The guard stops the sampling thread on success, error, or unwinding.
        let stop = StopTimer(sender);
        tracker.emit(Some(plan));
        let tracker_ref = &tracker;
        let timer = scope.spawn(move || {
            while matches!(
                receiver.recv_timeout(SAMPLE_INTERVAL),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                tracker_ref.emit(None);
            }
        });
        let result = work(&tracker);
        drop(stop);
        timer
            .join()
            .map_err(|_| anyhow::anyhow!("进度采样线程异常退出"))?;
        tracker.emit(None);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Barrier,
    };

    #[test]
    fn periodic_samples_use_actual_body_bytes_and_monotonic_time() {
        let events = Mutex::new(Vec::new());
        let (release, released) = mpsc::channel();
        let signaled = AtomicBool::new(false);
        let callback = |event: Progress| {
            let sample = event.transfer.as_ref().unwrap();
            if sample.downloaded_bytes == 4 && !signaled.swap(true, Ordering::Relaxed) {
                release.send(()).unwrap();
            }
            events.lock().unwrap().push(event);
        };
        struct PausedStream {
            step: u8,
            released: mpsc::Receiver<()>,
        }
        impl Read for PausedStream {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let bytes: &[u8] = match self.step {
                    0 => b"body",
                    1 => {
                        self.released
                            .recv_timeout(Duration::from_secs(3))
                            .map_err(|e| io::Error::new(io::ErrorKind::TimedOut, e))?;
                        b"bytes"
                    }
                    _ => return Ok(0),
                };
                output[..bytes.len()].copy_from_slice(bytes);
                self.step += 1;
                Ok(bytes.len())
            }
        }
        with_transfer(
            &callback,
            vec![ProgressStage::AssetFiles],
            "stream",
            |tracker| {
                tracker.plan_files(1, 1);
                tracker.begin_stage(ProgressStage::AssetFiles, "stream", 1, Some(1));
                {
                    let _active = tracker.begin_download();
                    let mut source = TrackedReader {
                        source: PausedStream { step: 0, released },
                        tracker: Some(tracker),
                        active: None,
                    };
                    let mut output = Vec::new();
                    source.read_to_end(&mut output)?;
                    assert_eq!(output, b"bodybytes");
                }
                tracker.finished_item(true, true);
                tracker.finish_stage();
                Ok(())
            },
        )
        .unwrap();
        let events = events.lock().unwrap();
        assert_eq!(
            events.iter().filter(|event| event.plan.is_some()).count(),
            1
        );
        let samples: Vec<_> = events
            .iter()
            .map(|event| event.transfer.as_ref().unwrap())
            .collect();
        let periodic = samples
            .iter()
            .find(|sample| sample.downloaded_bytes == 4)
            .unwrap();
        assert!(periodic.elapsed_ms >= 100);
        assert_eq!(periodic.active_downloads, Some(1));
        assert_eq!(periodic.concurrency_limit, Some(1));
        // Rate comes from bytes/time, not the one file's completion counter.
        let bytes_per_second =
            periodic.downloaded_bytes as f64 * 1000.0 / periodic.elapsed_ms as f64;
        assert!(bytes_per_second > 0.0 && bytes_per_second <= 40.0);
        assert!(samples
            .windows(2)
            .all(|pair| pair[0].elapsed_ms <= pair[1].elapsed_ms
                && pair[0].downloaded_bytes <= pair[1].downloaded_bytes));
        let last = samples.last().unwrap();
        assert_eq!(last.downloaded_bytes, 9);
        assert_eq!(last.active_downloads, Some(0));
        assert_eq!(last.remaining_files, Some(0));
    }

    #[test]
    fn concurrent_download_guards_count_requests_and_drain_before_return() {
        let events = Mutex::new(Vec::new());
        let callback = |event| events.lock().unwrap().push(event);
        with_transfer(
            &callback,
            vec![ProgressStage::AssetFiles],
            "parallel",
            |tracker| {
                tracker.plan_files(8, 8);
                tracker.begin_stage(ProgressStage::AssetFiles, "parallel", 8, Some(8));
                let ready = Barrier::new(9);
                let go = Barrier::new(9);
                std::thread::scope(|scope| {
                    for _ in 0..8 {
                        let ready = &ready;
                        let go = &go;
                        scope.spawn(move || {
                            let _active = tracker.begin_download();
                            ready.wait();
                            go.wait();
                            let mut read = TrackedReader {
                                source: &b"payload"[..],
                                tracker: Some(tracker),
                                active: None,
                            };
                            io::copy(&mut read, &mut io::sink()).unwrap();
                            tracker.finished_item(true, true);
                        });
                    }
                    ready.wait();
                    tracker.emit(None);
                    go.wait();
                });
                tracker.finish_stage();
                Ok(())
            },
        )
        .unwrap();
        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| {
            let sample = event.transfer.as_ref().unwrap();
            sample.active_downloads == Some(8) && sample.concurrency_limit == Some(8)
        }));
        let last = events.last().unwrap();
        let sample = last.transfer.as_ref().unwrap();
        assert_eq!(sample.active_downloads, Some(0));
        assert_eq!(sample.downloaded_bytes, 8 * 7);
        assert_eq!(sample.remaining_files, Some(0));
        assert_eq!(last.stage_progress, Some((8, 8)));
        assert_eq!((last.completed, last.total), (8, 8));
    }

    #[test]
    fn failed_stream_keeps_partial_byte_count_and_releases_active_request() {
        let events = Mutex::new(Vec::new());
        let callback = |event| events.lock().unwrap().push(event);
        let error = with_transfer(
            &callback,
            vec![ProgressStage::VersionMetadata],
            "failure",
            |tracker| {
                let _active = tracker.begin_download();
                let mut source = TrackedReader {
                    source: &b"partial"[..],
                    tracker: Some(tracker),
                    active: None,
                };
                io::copy(&mut source, &mut io::sink())?;
                Err::<(), _>(
                    io::Error::new(io::ErrorKind::ConnectionReset, "fixture network error").into(),
                )
            },
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::ConnectionReset
        );
        let events = events.lock().unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.stage_progress, Some((0, 0)));
        assert_eq!(last.transfer.as_ref().unwrap().downloaded_bytes, 7);
        assert_eq!(last.transfer.as_ref().unwrap().active_downloads, Some(0));
        assert_eq!(last.transfer.as_ref().unwrap().remaining_files, None);
    }

    #[test]
    fn stage_limits_are_explicit_and_do_not_leak_into_local_or_unknown_work() {
        let events = Mutex::new(Vec::new());
        let callback = |event| events.lock().unwrap().push(event);
        with_transfer(
            &callback,
            vec![ProgressStage::AssetFiles, ProgressStage::NativeLibraries],
            "fixture",
            |tracker| {
                tracker.begin_stage(ProgressStage::AssetFiles, "network", 0, Some(2));
                {
                    let _first = tracker.begin_download();
                    let _second = tracker.begin_download();
                    tracker.emit(None);
                }
                tracker.begin_stage(ProgressStage::NativeLibraries, "local", 0, Some(0));
                tracker.begin_stage(ProgressStage::CoreLibraries, "unknown", 0, None);
                let _active = tracker.begin_download();
                tracker.emit(None);
                Ok(())
            },
        )
        .unwrap();
        let events = events.lock().unwrap();
        let observed: Vec<_> = events
            .iter()
            .map(|event| {
                let transfer = event.transfer.as_ref().unwrap();
                (
                    event.message.as_str(),
                    transfer.active_downloads,
                    transfer.concurrency_limit,
                )
            })
            .collect();
        assert!(observed.contains(&("network", Some(2), Some(2))));
        assert!(observed.contains(&("local", Some(0), Some(0))));
        assert!(observed.contains(&("unknown", Some(1), None)));
        assert_eq!(observed.last(), Some(&("unknown", Some(0), None)));
    }
}
