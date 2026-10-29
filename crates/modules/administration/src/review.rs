//! Source reviews and promotion records. A promotion applies exactly the patch a
//! reviewer saw: the review digest binds the baseline commit and the patch bytes.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Largest patch a review may carry into a promotion.
pub const MAX_PATCH: usize = 1024 * 1024;

/// Why a review cannot be promoted. `Changed` means the source moved since the caller
/// read it (retry after reloading); `Invalid` means the request itself is unusable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewError {
    Changed(&'static str),
    Invalid(&'static str),
}

impl ReviewError {
    /// The public message.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Changed(message) | Self::Invalid(message) => message,
        }
    }
}

/// A review digest: 64 lowercase hex digits.
#[must_use]
pub fn valid_review_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The `(patch, baseline)` of a `source_review` result whose digest is `expected`.
pub fn verified<'a>(review: &'a Value, expected: &str) -> Result<(&'a str, &'a str), ReviewError> {
    if !valid_review_digest(expected)
        || review["status"] != "success"
        || review["truncated"] != false
    {
        return Err(ReviewError::Changed("Source changed since review"));
    }
    let patch = review["diff"]
        .as_str()
        .ok_or(ReviewError::Changed("Review has no patch"))?;
    let baseline = review["baseline_commit"]
        .as_str()
        .ok_or(ReviewError::Changed("Review baseline is missing"))?;
    if !matches!(baseline.len(), 40 | 64)
        || !baseline
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ReviewError::Changed("Review baseline is invalid"));
    }
    let calculated = sha256(format!("{baseline}\0{patch}").as_bytes());
    if review["review_sha256"] != expected || calculated != expected {
        return Err(ReviewError::Changed("Source changed since review"));
    }
    if patch.is_empty() || patch.len() > MAX_PATCH {
        return Err(ReviewError::Invalid(
            "Review patch is empty or exceeds limit",
        ));
    }
    Ok((patch, baseline))
}

/// The fields of a promotion record a client may see; never the patch itself.
#[must_use]
pub fn public_promotion(record: &Value) -> Value {
    json!({"session_id":record["session_id"],"client_request_id":record["client_request_id"],
        "state":record["state"],"review_sha256":record["review_sha256"],
        "patch_sha256":record["patch_sha256"],"paths":record["paths"],
        "created_at":record["created_at"],"updated_at":record["updated_at"],
        "summary":record["summary"],"proposed_by_run":record["proposed_by_run"]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(baseline: &str, patch: &str) -> Value {
        let digest = sha256(format!("{baseline}\0{patch}").as_bytes());
        json!({"status":"success","truncated":false,"diff":patch,"baseline_commit":baseline,
            "review_sha256":digest})
    }

    #[test]
    fn a_review_binds_its_baseline_and_patch() {
        let baseline = "a".repeat(40);
        let reviewed = review(&baseline, "patch");
        let digest = reviewed["review_sha256"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        assert_eq!(
            verified(&reviewed, &digest),
            Ok(("patch", baseline.as_str()))
        );
        let mut moved = reviewed.clone();
        moved["diff"] = json!("other patch");
        assert_eq!(
            verified(&moved, &digest),
            Err(ReviewError::Changed("Source changed since review"))
        );
        let mut truncated = reviewed.clone();
        truncated["truncated"] = json!(true);
        assert!(matches!(
            verified(&truncated, &digest),
            Err(ReviewError::Changed(_))
        ));
        assert!(verified(&reviewed, &digest.to_uppercase()).is_err());
        let mut upper = review(&"A".repeat(40), "patch");
        let upper_digest = upper["review_sha256"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        assert_eq!(
            verified(&upper, &upper_digest),
            Err(ReviewError::Changed("Review baseline is invalid"))
        );
        upper["baseline_commit"] = Value::Null;
        assert!(matches!(
            verified(&upper, &upper_digest),
            Err(ReviewError::Changed(_))
        ));
    }

    #[test]
    fn empty_patches_are_invalid_not_changed() {
        let empty = review(&"b".repeat(64), "");
        let digest = empty["review_sha256"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        assert_eq!(
            verified(&empty, &digest),
            Err(ReviewError::Invalid(
                "Review patch is empty or exceeds limit"
            ))
        );
    }

    #[test]
    fn public_records_never_carry_the_patch() {
        let record = json!({"session_id":"s","patch":"secret diff","state":"applied"});
        let public = public_promotion(&record);
        assert_eq!(public["state"], "applied");
        assert!(public.get("patch").is_none());
    }
}
