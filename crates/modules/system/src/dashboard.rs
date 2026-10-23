//! Pure dashboard window and counter calculations; host adapters own storage and authorization.
pub fn window_seconds(window: &str) -> Option<i64> {
    match window {
        "5m" => Some(300),
        "1h" => Some(3600),
        "24h" => Some(86400),
        "7d" => Some(604800),
        _ => None,
    }
}

pub fn quantile(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[((values.len() * 95).div_ceil(100)).saturating_sub(1)])
}
pub fn rate(current: f64, previous: Option<f64>, elapsed: f64) -> Option<f64> {
    let previous = previous?;
    (elapsed > 0.0 && current >= previous).then_some((current - previous) / elapsed)
}

/// Project only runs whose sessions were admitted for this agent by the host.
pub fn scoped_runs(
    runs: Vec<serde_json::Value>,
    sessions: &[serde_json::Value],
    agent: &str,
) -> Vec<serde_json::Value> {
    let admitted = sessions
        .iter()
        .filter(|session| session["agent_id"] == agent)
        .filter_map(|session| session["id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    runs.into_iter()
        .filter(|run| {
            run["session_id"]
                .as_str()
                .is_some_and(|id| admitted.contains(id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runs_require_an_admitted_session_for_the_current_agent() {
        use serde_json::json;
        let sessions = vec![
            json!({"id":"current","agent_id":"a"}),
            json!({"id":"other","agent_id":"b"}),
        ];
        let runs = vec![
            json!({"session_id":"current"}),
            json!({"session_id":"other"}),
            json!({"session_id":"revoked"}),
            json!({}),
        ];
        assert_eq!(
            scoped_runs(runs, &sessions, "a"),
            vec![json!({"session_id":"current"})]
        );
    }
    #[test]
    fn bounded_windows_and_rates_are_truthful() {
        assert_eq!(window_seconds("7d"), Some(604800));
        assert_eq!(window_seconds("../"), None);
        assert_eq!(rate(15.0, Some(5.0), 2.0), Some(5.0));
        assert_eq!(rate(1.0, Some(5.0), 2.0), None);
        assert_eq!(rate(1.0, None, 0.0), None);
        assert_eq!(quantile(vec![1.0, 9.0, 2.0]), Some(9.0));
    }
}
