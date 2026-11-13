//! Adapter contracts do not advertise unimplemented integration support.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Debug, Serialize, Deserialize)]
pub struct Availability {
    pub id: String,
    pub available: bool,
    pub operations: Vec<String>,
}
pub trait EventSource {
    fn availability(&self) -> Availability;
    fn validate_filter(&self, filter: &Value) -> Result<(), String>;
    fn register(&self, subscription: &Value) -> Result<Value, String>;
    fn renew(&self, receipt: &Value) -> Result<Value, String>;
    fn unsubscribe(&self, receipt: &Value) -> Result<(), String>;
    fn verify_push(&self, envelope: &Value) -> Result<Value, String>;
    fn poll(&self, cursor: &Value) -> Result<(Vec<Value>, Value), String>;
}
pub trait SchedulerProvider {
    fn availability(&self) -> Availability;
    fn reconcile(&self, definition: &Value) -> Result<Value, String>;
    fn remove(&self, receipt: &Value) -> Result<(), String>;
    fn verify_callback(&self, envelope: &Value) -> Result<Value, String>;
}
pub trait DeliveryDestination {
    fn availability(&self) -> Availability;
    fn prepare(&self, binding: &Value) -> Result<Value, String>;
    fn dispatch(&self, receipt: &Value) -> Result<Value, String>;
    fn reconcile(&self, receipt: &Value) -> Result<Value, String>;
    fn cancel(&self, receipt: &Value) -> Result<Value, String>;
}
pub trait Runner {
    fn availability(&self) -> Availability;
    fn prepare(&self, occurrence: &Value) -> Result<Value, String>;
    fn recover(&self, receipt: &Value) -> Result<Value, String>;
}
