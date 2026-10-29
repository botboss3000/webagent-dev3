//! Background Administration passes for every user: renew or revoke admitted broker
//! jobs, and record content-free repository notices. Neither executes model
//! instructions or creates assistant replies; the passes live in the Administration
//! module and this drives them.
use crate::tools::administrator::Host;
use crate::App;
use std::sync::atomic::Ordering;
use std::time::Duration;
use webagent_accounts_host::AccountsStore;

/// Run `pass` for every user every `period` until the application stops.
async fn every_user(app: App, period: Duration, pass: Pass) {
    while app.ready.load(Ordering::Acquire) && !app.stop.is_cancelled() {
        if let Ok(users) = app.store.all_user_ids().await {
            for uid in users {
                if let Ok(store) = app.store.for_user(&uid).await {
                    let mut caller = app.clone();
                    caller.store = store;
                    let host = Host(&caller);
                    let _ = match pass {
                        Pass::RenewJobs => {
                            webagent_administration_host::watch::renew_jobs(&host).await
                        }
                        Pass::RepositoryNotices => {
                            webagent_administration_host::watch::repository_notices(&host).await
                        }
                    };
                }
            }
        }
        tokio::select! {
            () = app.stop.cancelled() => break,
            () = tokio::time::sleep(period) => {}
        }
    }
}

#[derive(Clone, Copy)]
enum Pass {
    RenewJobs,
    RepositoryNotices,
}

pub(crate) async fn watch(app: App) {
    tokio::join!(
        every_user(app.clone(), Duration::from_secs(5), Pass::RenewJobs),
        every_user(app, Duration::from_secs(10), Pass::RepositoryNotices)
    );
}
