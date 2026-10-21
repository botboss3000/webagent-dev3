//! Fail-stop supervision uses dispatcher progress, never a lease-renewal task.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const WARN_AFTER: Duration = Duration::from_secs(120);
const EXIT_AFTER: Duration = Duration::from_secs(180);

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Stage {
    Startup = 0,
    Heartbeat,
    Recovery,
    DispatchGate,
    UserScan,
    Commands,
    RunScan,
    Claim,
    Idle,
}

fn stage_name(stage: u8) -> &'static str {
    match stage {
        0 => "startup_recovery",
        1 => "heartbeat",
        2 => "recovery",
        3 => "dispatch_gate",
        4 => "user_scan",
        5 => "command_validation",
        6 => "run_scan",
        7 => "claim",
        8 => "idle",
        _ => "unknown",
    }
}

#[derive(Clone)]
pub struct Progress {
    revision: Arc<AtomicU64>,
    stage: Arc<AtomicU8>,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            revision: Arc::new(AtomicU64::new(0)),
            stage: Arc::new(AtomicU8::new(Stage::Startup as u8)),
        }
    }

    pub fn at(&self, stage: Stage) {
        self.stage.store(stage as u8, Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
    }
}

#[derive(Debug, PartialEq)]
enum Observation {
    Progress,
    Waiting,
    Warn,
    Exit,
}

struct Monitor {
    revision: u64,
    last_progress: Instant,
    warned: bool,
}

impl Monitor {
    fn new(at: Instant) -> Self {
        Self {
            revision: 0,
            last_progress: at,
            warned: false,
        }
    }

    fn observe(&mut self, revision: u64, at: Instant) -> Observation {
        if revision != self.revision {
            self.revision = revision;
            self.last_progress = at;
            self.warned = false;
            return Observation::Progress;
        }
        let elapsed = at.duration_since(self.last_progress);
        if elapsed >= EXIT_AFTER {
            Observation::Exit
        } else if elapsed >= WARN_AFTER && !self.warned {
            self.warned = true;
            Observation::Warn
        } else {
            Observation::Waiting
        }
    }
}

pub struct Watchdog {
    stopped: Arc<AtomicBool>,
}

impl Watchdog {
    pub fn start(progress: Progress) -> std::io::Result<Self> {
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stop = stopped.clone();
        // A separate OS thread also notices a blocked async executor. It never
        // takes runtime locks or reads private storage to decide liveness.
        std::thread::Builder::new()
            .name("loop-watchdog".into())
            .spawn(move || {
                let mut monitor = Monitor::new(Instant::now());
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let revision = progress.revision.load(Ordering::Acquire);
                    let stage = stage_name(progress.stage.load(Ordering::Relaxed));
                    match monitor.observe(revision, Instant::now()) {
                        Observation::Warn => tracing::warn!(
                            stage,
                            stalled_seconds = 120,
                            "Loop dispatcher has stopped progressing"
                        ),
                        Observation::Exit => {
                            tracing::error!(
                                stage,
                                stalled_seconds = 180,
                                "Loop dispatcher stalled; exiting for supervised restart"
                            );
                            // Graceful shutdown would wait on the same stalled gate.
                            // Exit drops the process; startup recovery fences old leases
                            // and holds uncertain writes instead of replaying them.
                            std::process::exit(70);
                        }
                        _ => {}
                    }
                }
            })?;
        Ok(Self { stopped })
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_warns_once_then_exits_on_missing_dispatcher_progress() {
        let start = Instant::now();
        let mut monitor = Monitor::new(start);
        assert_eq!(
            monitor.observe(0, start + Duration::from_secs(119)),
            Observation::Waiting
        );
        assert_eq!(monitor.observe(0, start + WARN_AFTER), Observation::Warn);
        assert_eq!(
            monitor.observe(0, start + Duration::from_secs(179)),
            Observation::Waiting
        );
        assert_eq!(monitor.observe(0, start + EXIT_AFTER), Observation::Exit);
    }

    #[test]
    fn dispatcher_progress_resets_deadline_and_warning() {
        let start = Instant::now();
        let mut monitor = Monitor::new(start);
        assert_eq!(monitor.observe(0, start + WARN_AFTER), Observation::Warn);
        assert_eq!(
            monitor.observe(1, start + Duration::from_secs(179)),
            Observation::Progress
        );
        assert_eq!(
            monitor.observe(1, start + Duration::from_secs(298)),
            Observation::Waiting
        );
        assert_eq!(
            monitor.observe(1, start + Duration::from_secs(299)),
            Observation::Warn
        );
    }

    #[test]
    fn idle_dispatcher_progress_does_not_trigger_watchdog() {
        let start = Instant::now();
        let mut monitor = Monitor::new(start);
        for revision in 1..100 {
            assert_eq!(
                monitor.observe(revision, start + Duration::from_secs(revision * 60)),
                Observation::Progress
            );
        }
    }
}
