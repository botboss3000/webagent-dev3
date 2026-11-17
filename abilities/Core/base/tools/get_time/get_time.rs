//! get_time: argument shape for the clock time.
use super::utc;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TimeArgs {
    #[serde(default = "utc")]
    pub(super) timezone: String,
}
