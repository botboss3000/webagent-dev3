//! Compiled, local Core utilities. JSON metadata never chooses executable code.
//! Rust policy authorizes calls before this adapter; no worker, network or eval.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

#[path = "../calculate/calculate.rs"]
mod calculate;
#[path = "../get_date/get_date.rs"]
mod get_date;
#[path = "../get_time/get_time.rs"]
mod get_time;

/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("calculate", calculate::Arithmetic),
    webagent_abilities::argument_type!("get_date", get_date::DateArgs),
    webagent_abilities::argument_type!("get_time", get_time::TimeArgs),
];

pub fn owns(name: &str) -> bool {
    matches!(name, "get_time" | "get_date" | "calculate")
}
fn utc() -> String {
    "UTC".into()
}
fn offset(zone: &str) -> Result<i64> {
    if zone == "UTC" {
        return Ok(0);
    }
    let b = zone.as_bytes();
    if b.len() != 9
        || &b[..3] != b"UTC"
        || !matches!(b[3], b'+' | b'-')
        || b[6] != b':'
        || ![b[4], b[5], b[7], b[8]].iter().all(u8::is_ascii_digit)
    {
        bail!("Unsupported timezone; use UTC or UTC+HH:MM/UTC-HH:MM");
    }
    let hours = i64::from((b[4] - b'0') * 10 + b[5] - b'0');
    let minutes = i64::from((b[7] - b'0') * 10 + b[8] - b'0');
    if hours > 14 || minutes > 59 || (hours == 14 && minutes != 0) {
        bail!("Timezone offset must be within -14:00..+14:00");
    }
    Ok((hours * 3600 + minutes * 60) * if b[3] == b'-' { -1 } else { 1 })
}
// Civil-date conversion from a Gregorian day ordinal; arithmetic is bounded
// to years 0001..9999 before conversion so unsupported clocks fail explicitly.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}
fn clock(name: &str, args: &Value, timestamp: i64) -> Result<Value> {
    let (zone, format) = if name == "get_time" {
        let a: get_time::TimeArgs =
            serde_json::from_value(args.clone()).context("Invalid time arguments")?;
        (a.timezone, "time".to_owned())
    } else {
        let a: get_date::DateArgs =
            serde_json::from_value(args.clone()).context("Invalid date arguments")?;
        if !matches!(a.format.as_str(), "full" | "short" | "iso") {
            bail!("Invalid date format");
        }
        (a.timezone, a.format)
    };
    let delta = offset(&zone)?;
    let local = timestamp.checked_add(delta).context("Clock out of range")?;
    if !(-62135596800..=253402300799).contains(&local) {
        bail!("Clock out of range");
    }
    let days = local.div_euclid(86400);
    let seconds = local.rem_euclid(86400);
    let (year, month, day) = civil(days);
    let date = format!("{year:04}-{month:02}-{day:02}");
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    let text = match format.as_str() {
        "time" => time,
        "short" => date,
        "iso" => format!(
            "{date}T{time}{}",
            if zone == "UTC" { "+00:00" } else { &zone[3..] }
        ),
        _ => {
            let weekdays = [
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ];
            let months = [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ];
            format!(
                "{}, {} {}, {}",
                weekdays[(days + 4).rem_euclid(7) as usize],
                months[(month - 1) as usize],
                day,
                year
            )
        }
    };
    Ok(json!({"text":text,"timezone":zone,"unix_seconds":timestamp,"precision":"seconds"}))
}
pub async fn execute(name: &str, args: &Value, cancel: CancellationToken) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let output = match name {
        "get_time" | "get_date" => match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(now) => i64::try_from(now.as_secs())
                .context("Clock out of range")
                .and_then(|timestamp| clock(name, args, timestamp)),
            Err(_) => Err(anyhow::anyhow!("System clock before Unix epoch")),
        },
        "calculate" => webagent_abilities::tool_arguments::read("calculate", args)
            .and_then(calculate::arithmetic),
        _ => bail!("Unknown Core utility tool"),
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    Ok(match output {
        Ok(value) => json!({"status":"success","output":value}),
        Err(error) => {
            json!({"status":"error","output":error.to_string(),"error_kind":"invalid_input_or_clock"})
        }
    })
}
#[cfg(test)]
#[path = "../../tests/utilities.rs"]
mod tests;
