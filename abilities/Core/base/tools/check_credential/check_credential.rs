//! check_credential: whether a named vault credential is configured; never its value.

use super::public;
use serde_json::{json, Value};

pub(super) fn run(name: &str, prior: &Value) -> Value {
    json!({"status":"success","output":public(name,prior)})
}
