//! The loop container's supervisor when several agent-loop processes run (multi-loop).
//!
//! With `scheduler.loops` above 1 the loop container's program starts that many copies of
//! itself, loop `k` of `n` (`V3_LOOP_INDEX`, `V3_LOOPS`), and does nothing else: it forwards a
//! stop request to every child and waits for their drains, restarts a child that exits on its
//! own after a growing pause, and gives up (exits with an error, so Docker restarts the whole
//! container) when one child keeps exiting soon after it starts. Health comes from the
//! children's own member rows (see [`crate::partition`]). With one loop there is no supervisor.
use anyhow::{anyhow, Result};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// A child that exits within this long of starting counts as a quick exit.
const QUICK: Duration = Duration::from_secs(60);
/// Quick exits in a row of one child after which the supervisor gives up.
const GIVE_UP_AFTER: u32 = 5;
/// Longest pause before restarting a child.
const MAX_PAUSE: Duration = Duration::from_secs(30);
/// Pause between starting one loop and the next, so they do not set up the shared databases at
/// the same moment (two processes doing so at once can meet "database is locked"); a loop's
/// start-up takes about 2 s (measured on development).
const START_GAP: Duration = Duration::from_secs(3);

/// What to do after a child exited on its own.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Start it again after this pause.
    Restart(Duration),
    /// It keeps exiting soon after starting: stop the others and exit.
    GiveUp,
}

/// One child's record of recent exits.
#[derive(Debug, Default)]
pub struct Exits {
    quick: u32,
}

impl Exits {
    /// Record an exit after the child ran for `ran`. A child that ran past [`QUICK`] starts the
    /// count again; each quick exit doubles the pause (1 s, 2 s, 4 s ... at most 30 s).
    pub fn exited(&mut self, ran: Duration) -> Verdict {
        if ran >= QUICK {
            self.quick = 0;
        }
        self.quick += 1;
        if self.quick >= GIVE_UP_AFTER {
            return Verdict::GiveUp;
        }
        let pause = Duration::from_secs(1u64 << (self.quick - 1).min(5));
        Verdict::Restart(pause.min(MAX_PAUSE))
    }
}

enum Event {
    Exited {
        index: u32,
        ran: Duration,
        status: String,
    },
    Restart(u32),
}

fn start(program: &Path, index: u32, loops: u32) -> Result<tokio::process::Child> {
    Ok(tokio::process::Command::new(program)
        .env("V3_LOOP_INDEX", index.to_string())
        .env("V3_LOOPS", loops.to_string())
        .kill_on_drop(false)
        .spawn()?)
}

/// Run `loops` loop children of `program` until a stop signal, then forward it and wait for
/// every child to finish its drain. A child that cannot be started counts as one that exited at
/// once. Returns an error when a child keeps exiting (or failing to start) quickly.
pub async fn supervise(loops: u32, program: &Path) -> Result<()> {
    let (events, mut received) = mpsc::unbounded_channel();
    let mut pids: Vec<Option<u32>> = vec![None; loops as usize];
    let mut exits: Vec<Exits> = (0..loops).map(|_| Exits::default()).collect();
    let watch =
        |index: u32, mut child: tokio::process::Child, events: mpsc::UnboundedSender<Event>| {
            let started = Instant::now();
            tokio::spawn(async move {
                let status = match child.wait().await {
                    Ok(status) => status.to_string(),
                    Err(error) => error.to_string(),
                };
                let _ = events.send(Event::Exited {
                    index,
                    ran: started.elapsed(),
                    status,
                });
            });
        };
    for index in 0..loops {
        if index > 0 {
            tokio::time::sleep(START_GAP).await;
        }
        match start(program, index, loops) {
            Ok(child) => {
                pids[index as usize] = child.id();
                watch(index, child, events.clone());
            }
            // A loaded machine can refuse a new process for a moment: counted like an exit.
            Err(error) => {
                let _ = events.send(Event::Exited {
                    index,
                    ran: Duration::ZERO,
                    status: format!("not started: {error}"),
                });
            }
        }
    }
    tracing::info!(loops, "Loop supervisor started its loops");
    let mut signal = webagent_platform::signal::terminate()?;
    let mut failure = None;
    loop {
        let event = tokio::select! {
            _ = signal.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            event = received.recv() => event,
        };
        match event {
            Some(Event::Exited { index, ran, status }) => {
                pids[index as usize] = None;
                match exits[index as usize].exited(ran) {
                    Verdict::Restart(pause) => {
                        tracing::warn!(index, %status, seconds = pause.as_secs(), "Loop exited; restarting");
                        let events = events.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(pause).await;
                            let _ = events.send(Event::Restart(index));
                        });
                    }
                    Verdict::GiveUp => {
                        tracing::error!(index, %status, "Loop keeps exiting; stopping the container");
                        failure = Some(anyhow!(
                            "Loop {index} exited {GIVE_UP_AFTER} times soon after starting"
                        ));
                        break;
                    }
                }
            }
            Some(Event::Restart(index)) => match start(program, index, loops) {
                Ok(child) => {
                    pids[index as usize] = child.id();
                    watch(index, child, events.clone());
                }
                // Counted like an exit: tried again after a pause, given up after five.
                Err(error) => {
                    let _ = events.send(Event::Exited {
                        index,
                        ran: Duration::ZERO,
                        status: format!("not started: {error}"),
                    });
                }
            },
            None => break,
        }
    }
    // Forward the stop to every running child and wait for their drains (Docker's stop timeout
    // bounds the whole container). A restart still pausing is simply not started.
    for pid in pids.iter().flatten() {
        if let Err(error) = webagent_platform::process::request_stop(*pid) {
            tracing::warn!(pid, %error, "Stop request to a loop failed");
        }
    }
    while pids.iter().any(Option::is_some) {
        match received.recv().await {
            Some(Event::Exited { index, .. }) => pids[index as usize] = None,
            Some(Event::Restart(_)) => {}
            None => break,
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_that_keeps_exiting_quickly_is_given_up_after_five_tries() {
        let mut exits = Exits::default();
        let quick = Duration::from_secs(2);
        assert_eq!(
            exits.exited(quick),
            Verdict::Restart(Duration::from_secs(1))
        );
        assert_eq!(
            exits.exited(quick),
            Verdict::Restart(Duration::from_secs(2))
        );
        assert_eq!(
            exits.exited(quick),
            Verdict::Restart(Duration::from_secs(4))
        );
        assert_eq!(
            exits.exited(quick),
            Verdict::Restart(Duration::from_secs(8))
        );
        assert_eq!(exits.exited(quick), Verdict::GiveUp);
    }

    #[test]
    fn a_child_that_ran_a_while_starts_its_count_again() {
        let mut exits = Exits::default();
        for _ in 0..4 {
            exits.exited(Duration::from_secs(1));
        }
        assert_eq!(
            exits.exited(Duration::from_secs(600)),
            Verdict::Restart(Duration::from_secs(1))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_cannot_start_is_retried_then_given_up() {
        // A program that does not exist fails every start: five tries, then the supervisor stops.
        let result = supervise(1, Path::new("/nonexistent/webagent-loop")).await;
        assert!(result.is_err_and(|e| e.to_string().contains("5 times")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_supervisor_restarts_an_exited_child() {
        // `false` exits at once with failure; five quick exits make the supervisor give up.
        let started = Instant::now();
        let result = supervise(1, Path::new("/bin/false")).await;
        assert!(result.is_err_and(|e| e.to_string().contains("5 times")));
        // Pauses of 1 + 2 + 4 + 8 s between the five starts.
        assert!(started.elapsed() >= Duration::from_secs(15));
    }
}
