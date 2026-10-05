//! Explicit opt-in debug delays; no UI thread sleeps or changes to real deadlines.
use super::SystemSettings;
use anyhow::Result;
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static ADD_DELAY: AtomicBool = AtomicBool::new(false);
static SKIP_COPY: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn configure_debug(settings: &SystemSettings) {
    ADD_DELAY.store(settings.debug_delay, Ordering::Relaxed);
    SKIP_COPY.store(settings.debug_skip_copy, Ordering::Relaxed);
}

/// Upstream ModAnimation.AniSpeed. The last slider position means "off".
pub fn animation_speed(raw: u8) -> f32 {
    if raw >= 30 {
        200.0
    } else {
        f32::from(raw + 1) / 10.0
    }
}

/// Disable reuse from a shared or different game directory, not a valid target file.
pub fn debug_skip_copy() -> bool {
    SKIP_COPY.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug)]
pub enum DebugPhase {
    Request,
    JobStart,
    JobFinish,
}
impl DebugPhase {
    fn bounds(self) -> (u64, u64) {
        match self {
            Self::Request => (50, 3000),
            Self::JobStart => (200, 3000),
            Self::JobFinish => (100, 2000),
        }
    }
    fn duration(self, sample: u64) -> Duration {
        let (low, high) = self.bounds();
        Duration::from_millis(low + sample % (high - low + 1))
    }
}

/// Call only in a worker. Cancellation remains typed and is checked at most 20 ms apart.
pub fn debug_delay(cancel: &AtomicBool, phase: DebugPhase) -> Result<()> {
    crate::install::cancelled(cancel)?;
    if !ADD_DELAY.load(Ordering::Relaxed) {
        return Ok(());
    }
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Timing jitter for diagnostics only; never used for identifiers or security.
    let sample = (time.as_nanos() as u64).wrapping_add(
        SEQUENCE
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9e3779b97f4a7c15),
    );
    wait(phase.duration(sample), cancel)
}

fn wait(duration: Duration, cancel: &AtomicBool) -> Result<()> {
    let until = Instant::now() + duration;
    loop {
        crate::install::cancelled(cancel)?;
        let Some(left) = until.checked_duration_since(Instant::now()) else {
            return Ok(());
        };
        std::thread::sleep(left.min(Duration::from_millis(20)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_source_and_delay_bounds_are_real() {
        let defaults = SystemSettings::default();
        assert_eq!(animation_speed(defaults.debug_animation), 1.0);
        assert_eq!(animation_speed(0), 0.1);
        assert_eq!(animation_speed(29), 3.0);
        assert_eq!(animation_speed(30), 200.0);
        assert!(!defaults.debug_delay && !defaults.debug_skip_copy);
        for phase in [
            DebugPhase::Request,
            DebugPhase::JobStart,
            DebugPhase::JobFinish,
        ] {
            let (min, max) = phase.bounds();
            for sample in [0, 1, 2345, u64::MAX] {
                let delay = phase.duration(sample).as_millis();
                assert!((u128::from(min)..=u128::from(max)).contains(&delay));
            }
        }
        assert!(SystemSettings {
            debug_animation: 31,
            ..defaults
        }
        .validate()
        .is_err());
    }

    #[test]
    fn debug_sleep_is_cancellable_without_waiting_for_original_delay() {
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(25));
                cancel.store(true, Ordering::Relaxed);
            });
            let started = Instant::now();
            let error = wait(Duration::from_secs(3), &cancel).unwrap_err();
            assert!(error.is::<crate::model::OperationCancelled>());
            assert!(started.elapsed() < Duration::from_secs(1));
        });
    }
}
