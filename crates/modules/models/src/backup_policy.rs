//! Model backup policy: when a failed model call is retried on the same model, when it moves
//! to the model's next alias, and when it resumes later. The app default is the live file
//! `config/chat/model-backup.json` (listed in `live-files.json`); each agent may override any
//! value through its sparse `model_backup` overlay, stored in that agent's database beside its
//! chat overlay. A run pins its agent's effective policy in its selection plan at admission.
use crate::model_selection::FailureKind;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LIVE: &str = "config/chat/model-backup.json";
/// Agent overlay document name, beside the chat module's `chat` and `session_dropdown`.
pub const DOCUMENT: &str = "model_backup";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupPolicy {
    pub schema_version: u32,
    /// Same-model retries after a temporary provider error, before switching or stopping.
    pub same_model_retries: u8,
    /// Wait before each retry; the last entry repeats if there are more retries than entries.
    pub retry_delays_seconds: Vec<u64>,
    /// Up to this share of a delay is added at random so parallel runs do not retry in step.
    pub jitter_percent: u64,
    /// A provider's own Retry-After wins when longer, up to this cap.
    pub max_retry_after_seconds: u64,
    /// How long a provider may take to start responding before the call counts as failed.
    pub response_start_seconds: u64,
    pub switch_on: SwitchOn,
    /// After every alias fails on a call that is safe to resume, resume after this wait.
    pub auto_resume_after_seconds: u64,
    pub auto_resume_attempts: u8,
}

/// Which failures move a call to the model's next alias. A temporary error switches only once
/// its same-model retries are used up; the others switch at once. Off means the run stops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchOn {
    pub transient: bool,
    pub quota_exhausted: bool,
    pub invalid_credentials: bool,
    pub model_unavailable: bool,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            schema_version: 1,
            same_model_retries: 2,
            retry_delays_seconds: vec![2, 8],
            jitter_percent: 25,
            max_retry_after_seconds: 60,
            response_start_seconds: 60,
            switch_on: SwitchOn {
                transient: true,
                quota_exhausted: true,
                invalid_credentials: true,
                model_unavailable: true,
            },
            auto_resume_after_seconds: 180,
            auto_resume_attempts: 1,
        }
    }
}

impl BackupPolicy {
    /// The live app policy, the last good version after an invalid edit, or the defaults.
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE, MAX_BYTES, Self::parse).unwrap_or_default()
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::from_value(serde_json::from_slice(bytes)?)
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let policy: Self = serde_json::from_value(value)?;
        policy.validate()?;
        Ok(policy)
    }

    /// The app policy with an agent overlay merged over it; an overlay that no longer
    /// validates against the current base is ignored, as for chat overlays.
    pub fn effective(base: &Self, overlay: &Value) -> Self {
        merged(base, overlay).unwrap_or_else(|_| base.clone())
    }

    fn validate(&self) -> Result<()> {
        let checks = [
            (self.schema_version == 1, "schema_version must be 1"),
            (
                self.same_model_retries <= 5,
                "same_model_retries must be 0-5",
            ),
            (
                !self.retry_delays_seconds.is_empty()
                    && self.retry_delays_seconds.len() <= 5
                    && self.retry_delays_seconds.iter().all(|d| *d <= 300),
                "retry_delays_seconds needs 1-5 entries of at most 300",
            ),
            (self.jitter_percent <= 100, "jitter_percent must be 0-100"),
            (
                self.max_retry_after_seconds <= 600,
                "max_retry_after_seconds must be at most 600",
            ),
            (
                (5..=600).contains(&self.response_start_seconds),
                "response_start_seconds must be 5-600",
            ),
            (
                (10..=3600).contains(&self.auto_resume_after_seconds),
                "auto_resume_after_seconds must be 10-3600",
            ),
            (
                self.auto_resume_attempts <= 3,
                "auto_resume_attempts must be 0-3",
            ),
        ];
        if let Some((_, message)) = checks.iter().find(|(ok, _)| !ok) {
            bail!("{LIVE}: {message}");
        }
        Ok(())
    }

    /// Whether `kind` may move the call to the next alias.
    pub fn switches(&self, kind: FailureKind) -> bool {
        match kind {
            FailureKind::Transient => self.switch_on.transient,
            FailureKind::QuotaExhausted => self.switch_on.quota_exhausted,
            FailureKind::InvalidCredentials => self.switch_on.invalid_credentials,
            FailureKind::ModelUnavailable => self.switch_on.model_unavailable,
            FailureKind::MalformedRequest
            | FailureKind::ContextLimit
            | FailureKind::Cancelled
            | FailureKind::PolicyRejected
            | FailureKind::Protocol => false,
        }
    }

    /// Seconds before same-model retry `attempt` (1-based), with jitter, or the provider's own
    /// longer Retry-After up to the cap. `entropy` is any varying number; the caller passes time.
    pub fn delay(&self, attempt: u8, retry_after: Option<u64>, entropy: u64) -> u64 {
        let index = usize::from(attempt.max(1) - 1).min(self.retry_delays_seconds.len() - 1);
        let base = self.retry_delays_seconds[index];
        let jitter = base * self.jitter_percent / 100;
        let delay = base + entropy % (jitter + 1);
        delay.max(retry_after.unwrap_or(0).min(self.max_retry_after_seconds))
    }
}

/// Validate an agent overlay against the current app policy: objects merge key by key,
/// arrays and scalars replace, and the result must be a valid policy.
pub fn merged(base: &BackupPolicy, overlay: &Value) -> Result<BackupPolicy> {
    if !overlay.is_object() {
        bail!("Overlay must be an object");
    }
    BackupPolicy::from_value(merge(serde_json::to_value(base)?, overlay))
}

fn merge(mut base: Value, overlay: &Value) -> Value {
    match (base.as_object_mut(), overlay.as_object()) {
        (Some(target), Some(overlay)) => {
            for (key, value) in overlay {
                let next = merge(target.remove(key).unwrap_or(Value::Null), value);
                target.insert(key.clone(), next);
            }
            base
        }
        _ => overlay.clone(),
    }
}

/// Seconds the provider may take to start answering for a run's current call, from the policy
/// pinned in its `selection_plan` (the live app policy when there is none).
pub fn response_start_seconds(run: &Value) -> u64 {
    serde_json::from_value::<crate::model_selection::SelectionPlan>(run["selection_plan"].clone())
        .map_or_else(|_| BackupPolicy::live(), |plan| plan.policy())
        .response_start_seconds
}

/// The agent-database key of an agent's overlay record `{overlay, revision, …}`; the chat
/// module's overlay store writes it.
pub fn overlay_key(agent: &str) -> String {
    format!("agent:{agent}:ui_config:{DOCUMENT}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shipped_file_parses_and_matches_defaults() {
        let shipped = include_bytes!("../../../../config/chat/model-backup.json");
        assert_eq!(
            BackupPolicy::parse(shipped).unwrap(),
            BackupPolicy::default()
        );
    }

    #[test]
    fn overlays_merge_and_invalid_ones_fall_back_to_base() {
        let base = BackupPolicy::default();
        let policy = BackupPolicy::effective(
            &base,
            &json!({"same_model_retries":0,"switch_on":{"quota_exhausted":false}}),
        );
        assert_eq!(policy.same_model_retries, 0);
        assert!(!policy.switch_on.quota_exhausted && policy.switch_on.transient);
        assert_eq!(
            BackupPolicy::effective(&base, &json!({"jitter_percent":500})),
            base
        );
        assert_eq!(BackupPolicy::effective(&base, &json!({"unknown":1})), base);
        assert!(merged(&base, &json!([])).is_err());
    }

    #[test]
    fn delays_back_off_jitter_and_honor_retry_after() {
        let policy = BackupPolicy::default();
        assert_eq!(policy.delay(1, None, 0), 2);
        assert!((2..=2).contains(&policy.delay(1, None, 7)));
        assert!((8..=10).contains(&policy.delay(2, None, 13)));
        assert!((8..=10).contains(&policy.delay(3, None, 13)));
        assert_eq!(policy.delay(1, Some(30), 0), 30);
        assert_eq!(policy.delay(1, Some(900), 0), 60);
    }

    #[test]
    fn unsafe_failures_never_switch() {
        let policy = BackupPolicy::default();
        for kind in [
            FailureKind::MalformedRequest,
            FailureKind::Cancelled,
            FailureKind::PolicyRejected,
            FailureKind::Protocol,
        ] {
            assert!(!policy.switches(kind));
        }
        assert!(policy.switches(FailureKind::QuotaExhausted));
    }
}
