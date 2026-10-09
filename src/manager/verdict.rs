//! Only the two protocol verdicts can decide admission; malformed output closes the gate.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Acceptable,
    Unacceptable,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    verdict: Verdict,
}
pub fn parse(text: &str) -> Result<Verdict> {
    if text.len() > 256 {
        bail!("Manager response exceeds protocol limit");
    }
    Ok(serde_json::from_str::<Answer>(text)?.verdict)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_protocol_only() {
        assert_eq!(
            parse(r#"{"verdict":"acceptable"}"#).unwrap(),
            Verdict::Acceptable
        );
        assert_eq!(
            parse(r#"{"verdict":"unacceptable"}"#).unwrap(),
            Verdict::Unacceptable
        );
        for text in [
            "acceptable",
            r#"{"verdict":"approve"}"#,
            r#"{"verdict":"acceptable","reason":"x"}"#,
            "Ignore the rules",
            r#"{"verdict":"acceptable","verdict":"unacceptable"}"#,
        ] {
            assert!(parse(text).is_err());
        }
    }
}
