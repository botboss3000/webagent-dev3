//! get_date: argument shape and default format for the clock date.
use super::utc;
use serde::Deserialize;

fn full() -> String {
    "full".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DateArgs {
    #[serde(default = "utc")]
    pub(super) timezone: String,
    #[serde(default = "full")]
    pub(super) format: String,
}
