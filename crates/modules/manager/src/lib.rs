//! Manager settings IO, durable admission and bounded System-model review.
pub mod configuration;
pub mod incoming_message;
pub mod verdict;
pub use verdict::Verdict;

pub mod storage;
pub mod settings;
pub mod repository;
pub mod runtime;
pub mod scheduler;
pub fn public(record:&serde_json::Value,usage:impl FnOnce(&serde_json::Value)->serde_json::Value)->serde_json::Value {
 serde_json::json!({"review_id":record["id"],"gate":"incoming_message","status":record["status"],"verdict":record["verdict"],"error":record["error"],"usage":usage(&record["usage"]),"run_id":if record["status"]=="acceptable"{record["id"].clone()}else{serde_json::Value::Null}})
}

#[cfg(test)]mod tests;
