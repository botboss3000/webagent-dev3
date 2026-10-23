//! Read-only Linux namespace metrics, sampled at most once per second on demand.
pub mod dashboard;
mod metrics;
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Cache {
    sampler: metrics::Sampler,
    sample: Value,
    sampled_at: Instant,
    timestamp_ms: u128,
}

/// Return one consistent sample; callers must run this blocking collector off the async executor.
/// Missing counters stay unavailable. Proc counters are namespace values, not cgroup quotas.
pub fn snapshot() -> Value {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        let mut sampler = metrics::Sampler::new(vec!["/".into()]);
        let sample = sampler.sample();
        Mutex::new(Cache {
            sampler,
            sample,
            sampled_at: Instant::now(),
            timestamp_ms: metrics::timestamp_ms(),
        })
    });
    let Ok(mut cache) = cache.lock() else {
        return json!({"schema_version":1,"available":false,"reason":"sampler_unavailable"});
    };
    if cache.sampled_at.elapsed() >= Duration::from_secs(1) {
        cache.sample = cache.sampler.sample();
        cache.sampled_at = Instant::now();
        cache.timestamp_ms = metrics::timestamp_ms();
    }
    json!({"schema_version":1,"sampled_at_unix_ms":cache.timestamp_ms,
        "sample_age_ms":cache.sampled_at.elapsed().as_millis(),
        "sampling":"on_demand_cached","minimum_sample_interval_ms":1000,
        "scope":{"source":"linux_proc_namespace","cgroup_limits_included":false,
            "disk_mounts":["/"],"network":"process_network_namespace"},
        "metrics":cache.sample})
}

/// Project a fixed metric name from the same sample envelope; unknown names are rejected.
pub fn select(mut sample: Value, name: &str) -> Option<Value> {
    if !["cpu", "memory", "disks", "network", "uptime"].contains(&name) {
        return None;
    }
    let metric = sample.get_mut("metrics")?.get_mut(name)?.take();
    sample["metrics"] = Value::Object([(name.to_owned(), metric)].into_iter().collect());
    Some(sample)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_preserves_scope_and_rejects_unknown_names() {
        let sample = json!({"schema_version":1,"scope":{"source":"linux_proc_namespace"},
            "metrics":{"cpu":{"available":false},"memory":{"available":true}}});
        let cpu = select(sample.clone(), "cpu").unwrap();
        assert_eq!(cpu["scope"], sample["scope"]);
        assert_eq!(cpu["metrics"].as_object().unwrap().len(), 1);
        assert_eq!(cpu["metrics"]["cpu"]["available"], false);
        assert!(select(sample, "processes").is_none());
    }
    #[test]
    fn real_snapshot_labels_counter_scope_and_all_groups() {
        let sample = snapshot();
        assert_eq!(sample["schema_version"], 1);
        assert_eq!(sample["scope"]["cgroup_limits_included"], false);
        for name in ["cpu", "memory", "disks", "network", "uptime"] {
            assert!(sample["metrics"][name].is_object(), "{name}");
        }
    }
}
