//! A Browser Control call refused because every private browser is busy waits here instead
//! of going back to the model: the host re-issues it every [`POLL_SECONDS`], which keeps the
//! worker's queue place (it forgets a waiter after 90 seconds), until it runs, the run is
//! cancelled or the live `max_wait_seconds` passes.
use serde_json::{json, Value};
use std::future::Future;

/// Seconds between re-issues of a queued call; under the worker's 90-second place TTL.
pub const POLL_SECONDS: u64 = 30;

/// What the host does with one worker result.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// The call ran (or failed for another reason): return this result to the model.
    Done,
    /// Still queued: wait this many seconds, then re-issue the same call.
    Wait(u64),
    /// Still queued when the wait ran out: return [`busy`] to the model.
    Busy,
}

/// The step for `result` after the call has waited `waited` of `max_wait` seconds.
#[must_use]
pub fn step(result: &Value, waited: u64, max_wait: u64) -> Step {
    if result["error_kind"] != "browser_queued" {
        Step::Done
    } else if waited >= max_wait {
        Step::Busy
    } else {
        Step::Wait(POLL_SECONDS.min(max_wait - waited))
    }
}

/// The model's result when no browser came free in time: nothing ran.
#[must_use]
pub fn busy(waited: u64) -> Value {
    json!({"status":"error","error_kind":"browser_busy",
        "output":format!("All private browsers stayed busy for {} minutes; nothing ran. \
            Try again later, or tell the user the browser is busy.", waited.div_ceil(60))})
}

/// Runs `call` until it is not queued. `sleep(seconds)` waits between re-issues and returns
/// false when the run was cancelled; the run stop itself removes the session from the queue.
///
/// # Errors
/// The first error `call` returns.
pub async fn wait<C, CF, S, SF>(max_wait: u64, mut call: C, mut sleep: S) -> anyhow::Result<Value>
where
    C: FnMut() -> CF,
    CF: Future<Output = anyhow::Result<Value>>,
    S: FnMut(u64) -> SF,
    SF: Future<Output = bool>,
{
    let mut waited = 0;
    loop {
        let result = call().await?;
        match step(&result, waited, max_wait) {
            Step::Done => return Ok(result),
            Step::Busy => return Ok(busy(waited)),
            Step::Wait(seconds) => {
                if !sleep(seconds).await {
                    return Ok(json!({"status":"failed","output":"Browser action cancelled"}));
                }
                waited += seconds;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::task::{Context, Poll, Waker};

    /// Polls a future whose awaits are all ready.
    fn ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("pending"),
        }
    }

    /// Runs `wait` over scripted worker results, recording each sleep.
    fn scripted(
        results: Vec<Value>,
        max_wait: u64,
        cancel_after: usize,
    ) -> (Value, Vec<u64>, usize) {
        let results = RefCell::new(results.into_iter());
        let calls = RefCell::new(0);
        let sleeps = RefCell::new(Vec::new());
        let out = ready(wait(
            max_wait,
            || {
                *calls.borrow_mut() += 1;
                let next = results.borrow_mut().next().unwrap();
                async move { Ok(next) }
            },
            |seconds| {
                sleeps.borrow_mut().push(seconds);
                let go = sleeps.borrow().len() <= cancel_after;
                async move { go }
            },
        ))
        .unwrap();
        (out, sleeps.into_inner(), calls.into_inner())
    }

    fn queued() -> Value {
        json!({"status":"error","error_kind":"browser_queued","queue_position":1})
    }

    #[test]
    fn queued_calls_are_reissued_and_only_the_real_result_returns() {
        let ran = json!({"status":"success","output":"page"});
        let (out, sleeps, calls) = scripted(vec![queued(), queued(), ran.clone()], 600, 99);
        assert_eq!(out, ran);
        assert_eq!(sleeps, [POLL_SECONDS, POLL_SECONDS]);
        assert_eq!(calls, 3);
    }

    #[test]
    fn cancel_stops_the_wait() {
        let (out, sleeps, calls) = scripted(vec![queued(), queued()], 600, 1);
        assert_eq!(out["output"], "Browser action cancelled");
        assert_eq!((sleeps.len(), calls), (2, 2));
    }

    #[test]
    fn cap_returns_busy() {
        let (out, sleeps, _) = scripted(vec![queued(), queued(), queued()], 45, 99);
        assert_eq!(out["error_kind"], "browser_busy");
        assert_eq!(sleeps, [30, 15]);
    }

    #[test]
    fn queued_result_waits_and_other_results_return() {
        let queued = json!({"status":"error","error_kind":"browser_queued","queue_position":2});
        assert_eq!(step(&queued, 0, 600), Step::Wait(POLL_SECONDS));
        assert_eq!(step(&queued, 590, 600), Step::Wait(10));
        assert_eq!(step(&json!({"status":"success"}), 0, 600), Step::Done);
        let other = json!({"status":"error","error_kind":"browser_transport_failed"});
        assert_eq!(step(&other, 0, 600), Step::Done);
    }

    #[test]
    fn wait_cap_answers_busy() {
        let queued = json!({"status":"error","error_kind":"browser_queued","queue_position":1});
        assert_eq!(step(&queued, 600, 600), Step::Busy);
        let result = busy(600);
        assert_eq!(result["error_kind"], "browser_busy");
        assert!(result["output"].as_str().unwrap().contains("10 minutes"));
    }
}
