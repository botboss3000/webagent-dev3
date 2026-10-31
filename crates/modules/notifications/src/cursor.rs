//! Event-read query bounds: cursor, page size, long-poll wait and the SSE resume header.
use serde::Deserialize;
use std::fmt;

/// Default and largest page size.
pub const MAX_LIMIT: i64 = 200;
/// Default and longest long-poll wait, in seconds.
pub const MAX_WAIT: u64 = 30;

/// Query arguments of the event read and stream routes.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    /// Return events after this outbox sequence (default 0).
    pub since: Option<i64>,
    /// Page size, 1 to [`MAX_LIMIT`] (default [`MAX_LIMIT`]).
    pub limit: Option<i64>,
    /// Long-poll wait in seconds, at most [`MAX_WAIT`] (default [`MAX_WAIT`]).
    pub wait: Option<u64>,
}

/// Validated read bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Return events after this sequence.
    pub since: i64,
    /// Page size.
    pub limit: i64,
    /// Long-poll wait in seconds.
    pub wait: u64,
}

/// The query arguments are out of range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidBounds;

impl fmt::Display for InvalidBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid event cursor, limit or wait")
    }
}

impl std::error::Error for InvalidBounds {}

/// Apply defaults to `args` and check their ranges.
pub fn bounds(args: &Args) -> Result<Bounds, InvalidBounds> {
    let bounds = Bounds {
        since: args.since.unwrap_or(0),
        limit: args.limit.unwrap_or(MAX_LIMIT),
        wait: args.wait.unwrap_or(MAX_WAIT),
    };
    if bounds.since < 0 || !(1..=MAX_LIMIT).contains(&bounds.limit) || bounds.wait > MAX_WAIT {
        return Err(InvalidBounds);
    }
    Ok(bounds)
}

/// The non-negative sequence in a `Last-Event-ID` header value, if valid.
#[must_use]
pub fn last_event_id(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().filter(|v| *v >= 0)
}

#[cfg(test)]
mod tests {
    // The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn defaults_and_ranges() {
        assert_eq!(
            bounds(&Args::default()).unwrap(),
            Bounds {
                since: 0,
                limit: 200,
                wait: 30
            }
        );
        let args = |since, limit, wait| Args {
            since: Some(since),
            limit: Some(limit),
            wait: Some(wait),
        };
        assert!(bounds(&args(5, 1, 0)).is_ok());
        assert!(bounds(&args(0, 200, 30)).is_ok());
        for invalid in [
            args(-1, 1, 0),
            args(0, 0, 0),
            args(0, 201, 0),
            args(0, 1, 31),
        ] {
            assert_eq!(bounds(&invalid).unwrap_err(), InvalidBounds);
        }
        assert_eq!(
            InvalidBounds.to_string(),
            "Invalid event cursor, limit or wait"
        );
        assert!(serde_json::from_str::<Args>(r#"{"cursor":1}"#).is_err());
    }

    #[test]
    fn last_event_id_is_a_non_negative_integer() {
        assert_eq!(last_event_id("0"), Some(0));
        assert_eq!(last_event_id("42"), Some(42));
        assert_eq!(last_event_id("-1"), None);
        assert_eq!(last_event_id("4x"), None);
        assert_eq!(last_event_id(""), None);
    }
}
