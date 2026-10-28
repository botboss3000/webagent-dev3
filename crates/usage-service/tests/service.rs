//! The usage-reports service process answers exactly as the linked implementation.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use webagent_module_service::{Limits, Process, Services};
use webagent_usage::{Sample, UsageReports, UsageReportsClient};
use webagent_usage_reports::Reports;

fn sample(at: i64, agent: &str, entry: &str, model: &str, input: u64, usd: f64) -> Sample {
    Sample {
        at,
        session: "s1".into(),
        agent: agent.into(),
        entry: entry.into(),
        model: model.into(),
        usage: json!({"input_tokens": input, "output_tokens": 2, "known_cost_usd": usd}),
    }
}

async fn exercise(reports: &impl UsageReports) -> Vec<Value> {
    let now = 20_000 * 86_400;
    let samples = vec![
        sample(now - 86_400, "a", "e1", "m1", 10, 0.5),
        sample(now, "b", "", "m2", 7, 0.25),
    ];
    let labels = HashMap::from([("b".to_string(), "Agent B".to_string())]);
    let resets = reports.mark_reset(&Value::Null, "model", "m1", now - 10).await.unwrap();
    let kept = reports.apply_resets(&samples, resets.as_ref().unwrap()).await.unwrap();
    vec![
        json!(reports.analytics(&samples, "day", "agents", &labels, now).await.unwrap()),
        json!(reports.analytics(&samples, "week", "models", &labels, now).await.unwrap()),
        reports.model_totals(&samples, "e2", "m2", true).await.unwrap(),
        json!(resets),
        json!(kept.iter().map(|s| s.model.clone()).collect::<Vec<_>>()),
        json!(reports.mark_reset(&Value::Null, "undo", "", now).await.unwrap()),
    ]
}

#[tokio::test]
async fn service_process_matches_linked_reports() {
    let command = tokio::process::Command::new(env!("CARGO_BIN_EXE_webagent-usage-service"));
    let no_host_services: Services = Arc::new(|_| Vec::new());
    let process = Process::spawn(command, Limits::default(), &no_host_services)
        .expect("usage service starts");
    let live = exercise(&UsageReportsClient(process.peer().clone())).await;
    assert_eq!(live, exercise(&Reports).await);
}
