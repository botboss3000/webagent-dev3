//! Live models-module tunables: `config/models/models-runtime.json` (listed in
//! `live-files.json`), read on use. An invalid edit keeps the last good version; a file that has
//! never been readable gives the defaults below, which match the shipped file.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const LIVE: &str = "config/models/models-runtime.json";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub streaming: Streaming,
    pub harness_bridge: Bridge,
}

/// Direct API streaming limits. How long a provider may take to start responding is part
/// of the model backup policy (`crate::backup_policy`), which agents can override.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Streaming {
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
            streaming: Streaming { stall_seconds: 120 },
            harness_bridge: Bridge {
                unclaimed_call_seconds: 15,
                client_online_seconds: 120,
                offer_lease_seconds: 15,
                answer_seconds: 1800,
                poll_hold_seconds: 10,
                pairing_code_seconds: 600,
                store_poll_ms: 100,
                heartbeat_timeout_seconds: 15,
                client: BridgeClient {
                    heartbeat_seconds: 5,
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
        let (s, b, c) = (
            &self.streaming,
            &self.harness_bridge,
            &self.harness_bridge.client,
        );
        let checks = [
            (self.schema_version == 1, "schema_version must be 1"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_file_matches_defaults_and_validates() {
        let shipped = include_bytes!("../../../../config/models/models-runtime.json");
        assert_eq!(Settings::parse(shipped).unwrap(), Settings::default());
    }

    #[test]
    fn invalid_ranges_are_rejected() {
        let mut settings = Settings::default();
        settings.harness_bridge.heartbeat_timeout_seconds = 9;
        assert!(settings.validate().is_err());
        let mut settings = Settings::default();
        settings.harness_bridge.client.harness_seconds = 1800;
        assert!(settings.validate().is_err());
    }
}
