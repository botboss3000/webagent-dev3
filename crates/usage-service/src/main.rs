//! Usage-reports module service process: answers the host's `UsageReports` calls.
use std::sync::Arc;
use webagent_module_service::{Limits, Service};
use webagent_usage::UsageReportsServer;
use webagent_usage_reports::Reports;

fn main() {
    let result = webagent_module_service::run(
        Limits::default(),
        Arc::new(|_| vec![Arc::new(UsageReportsServer(Arc::new(Reports))) as Arc<dyn Service>]),
    );
    if result.is_err() {
        std::process::exit(1);
    }
}
