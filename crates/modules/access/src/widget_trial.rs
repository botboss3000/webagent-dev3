//! Website chat widgets as an owner-tier value (`OwnerLimits::widgets`): included, a trial
//! once per owner, or not included. The trial starts when the owner first turns a widget on
//! (the account's plain `widget_trial_started`, written once) and covers every agent they own;
//! it never starts again. Widgets serve while the tier includes them or the trial runs; after
//! that they pause, with their settings and visitor chats kept.
use crate::OwnerLimits;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DAY: u64 = 86_400;

/// Whether an owner's tier includes widgets; a lower value is the more restricted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Widgets {
    No,
    Trial,
    Yes,
}

/// Where an owner stands with widgets at one moment (times in Unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetState {
    Included,
    NotIncluded,
    TrialNotStarted { days: u32 },
    TrialRunning { started_at: u64, ends_at: u64 },
    TrialEnded { started_at: u64, ends_at: u64 },
}

impl WidgetState {
    /// The state for an owner whose effective tier has `limits` and whose trial started at
    /// `started` (`None`: never). A trial without a length counts as not included (fail closed).
    #[must_use]
    pub fn of(limits: &OwnerLimits, started: Option<u64>, now: u64) -> Self {
        match limits.widgets() {
            Widgets::Yes => Self::Included,
            Widgets::No => Self::NotIncluded,
            Widgets::Trial => match (limits.widget_trial_days, started) {
                (None, _) => Self::NotIncluded,
                (Some(days), None) => Self::TrialNotStarted { days },
                (Some(days), Some(started_at)) => {
                    let ends_at = trial_end(started_at, days);
                    if now < ends_at {
                        Self::TrialRunning {
                            started_at,
                            ends_at,
                        }
                    } else {
                        Self::TrialEnded {
                            started_at,
                            ends_at,
                        }
                    }
                }
            },
        }
    }
    /// Whether the owner's widgets serve visitors now.
    #[must_use]
    pub fn serves(self) -> bool {
        matches!(self, Self::Included | Self::TrialRunning { .. })
    }
    /// Whether the owner may turn a widget on now (turning one on starts a trial not started).
    #[must_use]
    pub fn may_turn_on(self) -> bool {
        matches!(
            self,
            Self::Included | Self::TrialNotStarted { .. } | Self::TrialRunning { .. }
        )
    }
    /// The plan part of an agent's widget settings, for the owner's settings page.
    #[must_use]
    pub fn view(self) -> Value {
        match self {
            Self::Included => json!({"plan":"included","serves":true}),
            Self::NotIncluded => json!({"plan":"not_included","serves":false}),
            Self::TrialNotStarted { days } => {
                json!({"plan":"trial","state":"not_started","days":days,"serves":false})
            }
            Self::TrialRunning {
                started_at,
                ends_at,
            } => json!({"plan":"trial","state":"running","started_at":started_at,
                "ends_at":ends_at,"serves":true}),
            Self::TrialEnded {
                started_at,
                ends_at,
            } => json!({"plan":"trial","state":"ended","started_at":started_at,
                "ends_at":ends_at,"serves":false}),
        }
    }
}

/// When a trial of `days` days that started at `started` ends.
#[must_use]
pub fn trial_end(started: u64, days: u32) -> u64 {
    started.saturating_add(u64::from(days).saturating_mul(DAY))
}

/// The owner notices a trial brings, each sent once from an `account_due` visit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialNotice {
    /// `widget_trial_notice_days` before the end.
    Ending,
    /// At the end.
    Ended,
}

impl TrialNotice {
    /// The fixed notification kind.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::Ending => "widget_trial_ending",
            Self::Ended => "widget_trial_ended",
        }
    }
    #[must_use]
    pub fn severity(self) -> &'static str {
        match self {
            Self::Ending => "info",
            Self::Ended => "warning",
        }
    }
}

/// What a due visit at `now` does for an owner with `limits` whose trial started at `started`:
/// the notice to send, if any, and when to look again (`None`: remove the row). Owners whose
/// tier has no trial now get no notice and no further visit.
#[must_use]
pub fn due_visit(
    limits: &OwnerLimits,
    started: Option<u64>,
    now: u64,
) -> (Option<TrialNotice>, Option<u64>) {
    let (Widgets::Trial, Some(days), Some(started)) =
        (limits.widgets(), limits.widget_trial_days, started)
    else {
        return (None, None);
    };
    let ends = trial_end(started, days);
    let notice_days = u64::from(limits.widget_trial_notice_days.unwrap_or(0));
    let warn = ends
        .checked_sub(notice_days.saturating_mul(DAY))
        .filter(|warn| notice_days > 0 && *warn > started);
    if now >= ends {
        (Some(TrialNotice::Ended), None)
    } else if warn.is_some_and(|warn| now >= warn) {
        (Some(TrialNotice::Ending), Some(ends))
    } else {
        (None, Some(warn.unwrap_or(ends)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial() -> OwnerLimits {
        OwnerLimits {
            widgets: Some(Widgets::Trial),
            widget_trial_days: Some(30),
            widget_trial_notice_days: Some(7),
            ..OwnerLimits::default()
        }
    }

    #[test]
    fn a_trial_serves_from_its_start_for_its_days_and_never_again() {
        let limits = trial();
        let start = 1_000_000;
        let end = start + 30 * DAY;
        assert_eq!(
            WidgetState::of(&limits, None, start),
            WidgetState::TrialNotStarted { days: 30 }
        );
        assert!(!WidgetState::of(&limits, None, start).serves());
        assert!(WidgetState::of(&limits, None, start).may_turn_on());
        let running = WidgetState::of(&limits, Some(start), end - 1);
        assert!(running.serves() && running.may_turn_on());
        assert_eq!(running.view()["ends_at"], end);
        for now in [end, end + 365 * DAY] {
            let ended = WidgetState::of(&limits, Some(start), now);
            assert_eq!(
                ended,
                WidgetState::TrialEnded {
                    started_at: start,
                    ends_at: end
                }
            );
            assert!(!ended.serves() && !ended.may_turn_on());
            assert_eq!(ended.view()["state"], "ended");
        }
    }

    #[test]
    fn included_and_not_included_ignore_the_trial_start() {
        for started in [None, Some(0)] {
            let yes = WidgetState::of(&OwnerLimits::default(), started, 10 * 365 * DAY);
            assert!(yes.serves() && yes.may_turn_on());
            let no = OwnerLimits {
                widgets: Some(Widgets::No),
                ..OwnerLimits::default()
            };
            let no = WidgetState::of(&no, started, 0);
            assert!(!no.serves() && !no.may_turn_on());
        }
        // A trial without a length is refused, not endless.
        let broken = OwnerLimits {
            widgets: Some(Widgets::Trial),
            ..OwnerLimits::default()
        };
        assert_eq!(
            WidgetState::of(&broken, Some(0), 0),
            WidgetState::NotIncluded
        );
    }

    #[test]
    fn due_visits_bring_the_warning_then_the_end_once_each() {
        let limits = trial();
        let start = 5_000_000;
        let end = start + 30 * DAY;
        let warn = end - 7 * DAY;
        assert_eq!(due_visit(&limits, Some(start), start), (None, Some(warn)));
        assert_eq!(
            due_visit(&limits, Some(start), warn - 1),
            (None, Some(warn))
        );
        assert_eq!(
            due_visit(&limits, Some(start), warn),
            (Some(TrialNotice::Ending), Some(end))
        );
        assert_eq!(
            due_visit(&limits, Some(start), end),
            (Some(TrialNotice::Ended), None)
        );
        // A late visit after the end sends only the end notice.
        assert_eq!(
            due_visit(&limits, Some(start), end + DAY),
            (Some(TrialNotice::Ended), None)
        );
        // No start, or a tier without a trial (Plus): nothing to do, the row goes.
        assert_eq!(due_visit(&limits, None, end), (None, None));
        assert_eq!(
            due_visit(&OwnerLimits::default(), Some(start), end),
            (None, None)
        );
        // No warning days: the only visit is the end.
        let quiet = OwnerLimits {
            widget_trial_notice_days: None,
            ..limits
        };
        assert_eq!(due_visit(&quiet, Some(start), start), (None, Some(end)));
        assert_eq!(TrialNotice::Ending.kind(), "widget_trial_ending");
        assert_eq!(TrialNotice::Ended.kind(), "widget_trial_ended");
    }

    #[test]
    fn an_upper_tier_caps_widgets_and_the_trial_length() {
        let free = trial();
        let plus = OwnerLimits {
            widgets: Some(Widgets::Yes),
            ..OwnerLimits::default()
        };
        assert_eq!(free.within(&plus).widgets(), Widgets::Trial);
        assert_eq!(free.within(&plus).widget_trial_days, Some(30));
        let anonymous = OwnerLimits {
            widgets: Some(Widgets::No),
            ..OwnerLimits::default()
        };
        assert_eq!(anonymous.within(&free).widgets(), Widgets::No);
        assert_eq!(plus.within(&free).widgets(), Widgets::Trial);
        assert!(OwnerLimits::default()
            .within(&OwnerLimits::default())
            .unlimited());
        let short = OwnerLimits {
            widget_trial_days: Some(10),
            ..free
        };
        assert_eq!(free.within(&short).widget_trial_days, Some(10));
    }
}
