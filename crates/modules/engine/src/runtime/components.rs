//! Component withdrawal/restore decisions stay with Engine run state.
use super::{config, models::ModelRecords, ports::*, Runtime};
use crate::components::*;
use crate::context::abilities::Catalog;
use crate::ports::*;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

/// Component pauses saved by this process. Paused runs are not in the scan's "work now", so
/// the scheduler sweeps the accounts that paused one (see [`take_paused`]) after this moves
/// (and once components are back).
pub static COMPONENT_PAUSES: AtomicU64 = AtomicU64::new(0);
/// C9: the accounts in which this process paused a run for a component, saved before
/// [`COMPONENT_PAUSES`] moves. After the start-up sweep the scheduler restores only these, so a
/// component outage no longer opens every account. Losing the list loses nothing: it lives
/// as long as the process, and a new loop process sweeps every account once at start.
static PAUSED_ACCOUNTS: std::sync::Mutex<std::collections::BTreeSet<(std::path::PathBuf, String)>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Remember that `user` (of the installation whose control folder is `control`) holds a
/// component-paused run, or one to offer again.
pub(super) fn note_paused(control: &std::path::Path, user: &str) {
    PAUSED_ACCOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert((control.to_path_buf(), user.to_owned()));
}

/// The accounts of `control` noted since the last call, taking them off the list.
pub(super) fn take_paused(control: &std::path::Path) -> Vec<String> {
    let mut noted = PAUSED_ACCOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mine: Vec<_> = noted
        .iter()
        .filter(|(dir, _)| dir == control)
        .cloned()
        .collect();
    mine.into_iter()
        .map(|entry| {
            noted.remove(&entry);
            entry.1
        })
        .collect()
}

impl<
        S: ScopedStorage
            + ModelRecords
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
        C: config::SchemaCatalog,
        B: Catalog,
        A: super::actions::Actions<S> + Clone + PartialEq,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S>
            + NativePolicy
            + CredentialBindings<S>
            + super::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    pub async fn component_pause(&self, run: &mut Value) -> Result<()> {
        let app = self;
        let component = run["component_pause"]["component"]
            .as_str()
            .filter(|id| working_component(id))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                WORKING_COMPONENTS
                    .into_iter()
                    .find(|id| {
                        !component_available(
                            crate::ports::AgentMetadata::root(app.store.records()),
                            id,
                        )
                    })
                    .map(str::to_owned)
                    // An expired cooldown no longer shows which component failed; the recorded failure does.
                    .or_else(|| {
                        last_failure()
                            .map(|(id, _)| id)
                            .filter(|id| working_component(id))
                    })
                    .unwrap_or_else(|| CONTEXT.to_owned())
            });
        let cause = last_failure()
            .filter(|(id, _)| *id == component)
            .map(|(_, cause)| cause);
        run["status"] = json!("paused");
        run["component_pause"] = json!({"component":component,"version":1});
        // Name the recorded cause so the pause is diagnosable from the run itself.
        run["error"]=json!(match cause{
  Some(cause)=>format!("Required engine component {component} unavailable ({cause}); work resumes when the component is restored"),
  None=>"Required engine component unavailable; work resumes when the component is restored".to_owned()});
        let saved = app.save(run).await;
        note_paused(
            &app.control_dir,
            crate::ports::Records::current_user_id(app.store.records()),
        );
        COMPONENT_PAUSES.fetch_add(1, Ordering::AcqRel);
        saved
    }
    pub async fn component_restore(&self, id: &str) -> Result<()> {
        let app = self;
        if !available(crate::ports::AgentMetadata::root(app.store.records()))
            || app.shutoff().await?
            || app.active.lock().await.contains_key(id)
        {
            return Ok(());
        }
        let mut run = app.run(id).await?;
        if !retained_pause(&run) {
            return Ok(());
        }
        if !app
            .chat
            .participation_allows_run(app.store.records(), &run)
            .await?
            || !app.run_model_authorized(&run).await
        {
            run["status"] = json!("stopped");
            run["component_pause"] = Value::Null;
            run["error"] = json!("Access changed while engine component was unavailable");
        } else {
            run["status"] = json!("queued");
            run["component_pause"] = Value::Null;
            run["error"] = Value::Null;
        }
        app.save(&mut run).await?;
        crate::wake::notify_account(
            &app.control_dir,
            crate::ports::Records::current_user_id(app.store.records()),
            crate::wake::Kind::Run,
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod paused_account_tests {
    #[test]
    fn noted_accounts_are_taken_once_per_installation_and_can_be_noted_again() {
        let here = std::path::Path::new("/c9-test/control-a");
        let there = std::path::Path::new("/c9-test/control-b");
        super::note_paused(here, "a");
        super::note_paused(here, "b");
        super::note_paused(here, "a");
        super::note_paused(there, "a");
        assert_eq!(super::take_paused(here), ["a", "b"]);
        assert!(super::take_paused(here).is_empty(), "taken once");
        // Another installation's accounts are its own.
        assert_eq!(super::take_paused(there), ["a"]);
        // An account a sweep could not finish is noted again and taken by the next sweep.
        super::note_paused(here, "b");
        assert_eq!(super::take_paused(here), ["b"]);
    }
}
