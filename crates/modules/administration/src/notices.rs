//! Checks on the build broker's retained-work notices before the app shows them.
use serde_json::Value;

/// A notice ID: 64 lowercase hex digits.
#[must_use]
pub fn valid_notice_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_task_id(value: &str) -> bool {
    (8..=80).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether a `v3_build_leftovers` result is a bounded page of well-formed notices.
#[must_use]
pub fn valid_page(output: &Value) -> bool {
    let Some(rows) = output["notices"].as_array().filter(|rows| rows.len() <= 20) else {
        return false;
    };
    let total_fits = output["total"]
        .as_u64()
        .is_some_and(|total| total >= rows.len() as u64 && total <= 1000);
    total_fits
        && rows.iter().all(|row| {
            row["id"].as_str().is_some_and(valid_notice_id)
                && row["task_id"].as_str().is_some_and(valid_task_id)
                && matches!(
                    row["status"].as_str(),
                    Some("stale_open" | "failed" | "interrupted" | "uncertain" | "stale")
                )
                && row["notification"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty() && text.len() <= 512)
                && row["observed_at"].as_u64().is_some()
                && row["since"].as_u64().is_some()
                && (row["retained_bytes"].is_null() || row["retained_bytes"].as_u64().is_some())
        })
}

/// Whether a `v3_build_leftover_review` result acknowledges exactly `notice_id`.
#[must_use]
pub fn reviewed(output: &Value, notice_id: &str) -> bool {
    output["status"] == "reviewed" && output["notice_id"] == notice_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> Value {
        json!({"id":"a".repeat(64),"task_id":"retained-task","status":"uncertain",
            "notification":"Investigate retained work.","since":1,"observed_at":2,
            "retained_bytes":null})
    }

    #[test]
    fn projection_rejects_bad_revision_and_negative_space() {
        assert!(valid_page(&json!({"notices":[row()],"total":1})));
        assert!(!valid_page(&json!({"notices":[row()],"total":0})));
        for (key, value) in [
            ("id", json!("A".repeat(64))),
            ("id", json!("../foreign")),
            ("task_id", json!("short")),
            ("status", json!("completed")),
            ("notification", json!("")),
            ("retained_bytes", json!(-1)),
            ("since", json!(-1)),
        ] {
            let mut bad = row();
            bad[key] = value;
            assert!(!valid_page(&json!({"notices":[bad],"total":1})), "{key}");
        }
        let many: Vec<_> = (0..21).map(|_| row()).collect();
        assert!(!valid_page(&json!({"notices":many,"total":21})));
    }

    #[test]
    fn a_review_must_name_the_reviewed_notice() {
        let id = "b".repeat(64);
        assert!(reviewed(&json!({"status":"reviewed","notice_id":id}), &id));
        assert!(!reviewed(
            &json!({"status":"reviewed","notice_id":"c".repeat(64)}),
            &id
        ));
        assert!(!valid_notice_id("short") && valid_notice_id(&id));
    }
}
