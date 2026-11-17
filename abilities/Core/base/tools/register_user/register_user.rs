//! register_user: queues a registration through app control.
use super::{Call, NativeHost};
use crate::app_control;
use anyhow::Result;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    app_control::enqueue(host, call, name, args, "registration", cancel).await
}
