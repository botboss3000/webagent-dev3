//! Root compatibility for the one Engine wake channel and authenticated socket listener.
use crate::*;
pub(crate) use webagent_engine::wake::subscribe;
pub(crate) async fn notify(app:&App) {webagent_engine::wake::notify(&app.control_dir).await}
pub(crate) async fn listener(app:&App)->Result<tokio::task::JoinHandle<()>> {webagent_engine::wake::listener(&app.control_dir,app.stop.clone()).await}
pub(crate) async fn wait(app:&App) {
 let fallback=if std::env::var("V3_DISPATCH_POLL_ONLY").as_deref()==Ok("1") {app.runtime.read().await.scheduler.poll_interval_ms}else{5000};
 webagent_engine::wake::wait(fallback,&app.stop).await
}
