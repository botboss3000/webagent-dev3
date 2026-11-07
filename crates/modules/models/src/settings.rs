//! Live models-module tunables: `crates/modules/models/models-runtime.json` (listed in
//! `live-files.json`), read on use. An invalid edit keeps the last good version; a file that has
//! never been readable gives the defaults below, which match the shipped file.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const LIVE: &str = "crates/modules/models/models-runtime.json";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub retry: Retry,
    pub streaming: Streaming,
    pub harness_bridge: Bridge,
}

/// Same-model retries after a transient provider failure, before failover or a stop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub same_model_retries: u8,
    /// Wait before each retry; the last entry repeats if there are more retries than entries.
    pub delays_seconds: Vec<u64>,
    /// Up to this share of a delay is added at random so parallel runs do not retry in step.
    pub jitter_percent: u64,
    /// A provider's own Retry-After wins when longer, up to this cap.
    pub max_retry_after_seconds: u64,
    /// After retries run out on a call that is safe to resume, resume it once after this wait.
    pub auto_resume_after_seconds: u64,
    pub auto_resume_attempts: u8,
}

/// Direct API streaming limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Streaming {
    /// How long a provider may take to start responding.
    pub response_start_seconds: u64,
    /// How long a started stream may stay silent.
    pub stall_seconds: u64,
}

/// Harness bridge server timers; `client` is sent to paired clients with every poll.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    /// A queued call no client fetches within this window fails.
    pub unclaimed_call_seconds: u64,
    /// A link that polled within this window is online.
    pub client_online_seconds: u64,
    /// A fetched call is offered this long; unacknowledged, it returns to the queue.
    pub offer_lease_seconds: u64,
    /// An acknowledged call from a client that sends no heartbeats waits this long for its reply.
    pub answer_seconds: u64,
    /// Client long-poll window.
    pub poll_hold_seconds: u64,
    pub pairing_code_seconds: u64,
    /// How often a waiting side re-reads the call document.
    pub store_poll_ms: u64,
    /// A heartbeating client that goes quiet this long has stopped responding.
    pub heartbeat_timeout_seconds: u64,
    pub client: BridgeClient,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeClient {
    pub heartbeat_seconds: u64,
    /// Silence on the harness's event stream before a probe.
    pub stall_seconds: u64,
    pub probe_seconds: u64,
    /// A chat's warm harness server stops after this long unused.
    pub server_idle_seconds: u64,
    /// After Ctrl-C, how long a harness may take to exit before it is killed.
    pub stop_grace_seconds: u64,
    /// Longest one harness run may take.
    pub harness_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            retry: Retry {
                same_model_retries: 2,
                delays_seconds: vec![2, 8],
                jitter_percent: 25,
                max_retry_after_seconds: 60,
                auto_resume_after_seconds: 180,
                auto_resume_attempts: 1,
            },
            streaming: Streaming {
                response_start_seconds: 60,
                stall_seconds: 120,
            },
            harness_bridge: Bridge {
                unclaimed_call_seconds: 60,
                client_online_seconds: 120,
                offer_lease_seconds: 60,
                answer_seconds: 1800,
                poll_hold_seconds: 25,
                pairing_code_seconds: 600,
                store_poll_ms: 250,
                heartbeat_timeout_seconds: 60,
                client: BridgeClient {
                    heartbeat_seconds: 20,
                    stall_seconds: 120,
                    probe_seconds: 30,
                    server_idle_seconds: 900,
                    stop_grace_seconds: 5,
                    harness_seconds: 1680,
                },
            },
        }
    }
}

impl Settings {
    /// The live settings, the last good version after an invalid edit, or the defaults.
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE, MAX_BYTES, Self::parse).unwrap_or_default()
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let settings: Self = serde_json::from_slice(bytes)?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<()> {
        let (r, s, b, c) = (
            &self.retry,
            &self.streaming,
            &self.harness_bridge,
            &self.harness_bridge.client,
        );
        let checks = [
            (self.schema_version == 1, "schema_version must be 1"),
            (r.same_model_retries <= 5, "retry.same_model_retries must be 0-5"),
            (
                !r.delays_seconds.is_empty()
                    && r.delays_seconds.len() <= 5
                    && r.delays_seconds.iter().all(|d| *d <= 300),
                "retry.delays_seconds needs 1-5 entries of at most 300",
            ),
            (r.jitter_percent <= 100, "retry.jitter_percent must be 0-100"),
            (r.max_retry_after_seconds <= 600, "retry.max_retry_after_seconds must be at most 600"),
            (
                (10..=3600).contains(&r.auto_resume_after_seconds),
                "retry.auto_resume_after_seconds must be 10-3600",
            ),
            (r.auto_resume_attempts <= 3, "retry.auto_resume_attempts must be 0-3"),
            (
                (5..=600).contains(&s.response_start_seconds),
                "streaming.response_start_seconds must be 5-600",
            ),
            ((10..=1800).contains(&s.stall_seconds), "streaming.stall_seconds must be 10-1800"),
            (
                (5..=60).contains(&b.poll_hold_seconds),
                "harness_bridge.poll_hold_seconds must be 5-60",
            ),
            (
                b.unclaimed_call_seconds > b.poll_hold_seconds && b.unclaimed_call_seconds <= 600,
                "harness_bridge.unclaimed_call_seconds must exceed poll_hold_seconds, at most 600",
            ),
            (
                b.client_online_seconds >= b.unclaimed_call_seconds
                    && b.client_online_seconds <= 3600,
                "harness_bridge.client_online_seconds must be at least unclaimed_call_seconds",
            ),
            (
                (10..=600).contains(&b.offer_lease_seconds),
                "harness_bridge.offer_lease_seconds must be 10-600",
            ),
            (
                (60..=7200).contains(&b.answer_seconds),
                "harness_bridge.answer_seconds must be 60-7200",
            ),
            (
                (60..=86400).contains(&b.pairing_code_seconds),
                "harness_bridge.pairing_code_seconds must be 60-86400",
            ),
            (
                (50..=5000).contains(&b.store_poll_ms),
                "harness_bridge.store_poll_ms must be 50-5000",
            ),
            (
                c.heartbeat_seconds >= 1 && b.heartbeat_timeout_seconds >= 2 * c.heartbeat_seconds,
                "harness_bridge.heartbeat_timeout_seconds must be at least twice client.heartbeat_seconds",
            ),
            (
                b.heartbeat_timeout_seconds <= 1800,
                "harness_bridge.heartbeat_timeout_seconds must be at most 1800",
            ),
            (
                (10..=3600).contains(&c.stall_seconds),
                "harness_bridge.client.stall_seconds must be 10-3600",
            ),
            (
                (5..=300).contains(&c.probe_seconds),
                "harness_bridge.client.probe_seconds must be 5-300",
            ),
            (
                (60..=86400).contains(&c.server_idle_seconds),
                "harness_bridge.client.server_idle_seconds must be 60-86400",
            ),
            (
                (1..=60).contains(&c.stop_grace_seconds),
                "harness_bridge.client.stop_grace_seconds must be 1-60",
            ),
            (
                c.harness_seconds >= 60 && c.harness_seconds < b.answer_seconds,
                "harness_bridge.client.harness_seconds must be at least 60 and below answer_seconds",
            ),
        ];
        if let Some((_, message)) = checks.iter().find(|(ok, _)| !ok) {
            bail!("{LIVE}: {message}");
        }
        Ok(())
    }
}

impl Retry {
    /// Seconds before same-model retry `attempt` (1-based), with jitter, or the provider's own
    /// longer Retry-After up to the cap. `entropy` is any varying number; the caller passes time.
    pub fn delay(&self, attempt: u8, retry_after: Option<u64>, entropy: u64) -> u64 {
        let index = usize::from(attempt.max(1) - 1).min(self.delays_seconds.len() - 1);
        let base = self.delays_seconds[index];
        let jitter = base * self.jitter_percent / 100;
        let delay = base + entropy % (jitter + 1);
        delay.max(retry_after.unwrap_or(0).min(self.max_retry_after_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_file_matches_defaults_and_validates() {
        let shipped = include_bytes!("../models-runtime.json");
        assert_eq!(Settings::parse(shipped).unwrap(), Settings::default());
    }

    #[test]
    fn invalid_ranges_are_rejected() {
        let mut settings = Settings::default();
        settings.harness_bridge.heartbeat_timeout_seconds = 30;
        assert!(settings.validate().is_err());
        let mut settings = Settings::default();
        settings.harness_bridge.client.harness_seconds = 1800;
        assert!(settings.validate().is_err());
        let mut settings = Settings::default();
        settings.retry.delays_seconds.clear();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn delays_back_off_jitter_and_honor_retry_after() {
        let retry = Settings::default().retry;
        assert_eq!(retry.delay(1, None, 0), 2);
        assert!((2..=2).contains(&retry.delay(1, None, 7)));
        assert!((8..=10).contains(&retry.delay(2, None, 13)));
        assert!((8..=10).contains(&retry.delay(3, None, 13)));
        assert_eq!(retry.delay(1, Some(30), 0), 30);
        assert_eq!(retry.delay(1, Some(900), 0), 60);
    }
}
