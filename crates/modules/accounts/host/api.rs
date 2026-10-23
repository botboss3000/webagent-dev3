//! Route extraction and live Access/Storage composition for the native Accounts controllers.
use crate::*;
use axum::{extract::Path, Extension, Json};
pub(crate) use webagent_accounts::api::{NewAccount, ProfileChange};
impl From<webagent_accounts::api::Error> for ApiError {
    fn from(error: webagent_accounts::api::Error) -> Self {
        match error {
            webagent_accounts::api::Error::Public(status, message) => Self(status, message),
            webagent_accounts::api::Error::Storage(error) => Self::from(error),
        }
    }
}
impl webagent_accounts::api::Host for App {
    fn initial_user_access(&self) -> webagent_accounts::InitialAccess<'_> {
        access::initial_user_access()
    }
    fn administrator<'a>(
        &'a self,
        identity: &'a Value,
    ) -> impl std::future::Future<Output = webagent_accounts::api::Result<()>> + Send + 'a {
        async move {
            phase3_api::administrator(self, identity)
                .await
                .map_err(|ApiError(status, message)| {
                    webagent_accounts::api::Error::Public(status, message)
                })
        }
    }
    fn account_view(&self, record: &Value) -> webagent_accounts::api::Result<Value> {
        let principal = access::Principal::from_account(record)?;
        Ok(
            json!({"user_id":principal.user_id,"username":record["username"],"status":principal.status,"role":principal.role,"tier_id":principal.tier_id,"access_revision":principal.access_revision}),
        )
    }
    fn user_counts<'a>(
        &'a self,
        user: &'a str,
    ) -> impl std::future::Future<Output = webagent_accounts::api::Result<(i64, i64)>> + Send + 'a
    {
        async move {
            let view = self.store.for_user(user).await?;
            let sessions =
                sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key LIKE 'session:%'")
                    .fetch_one(&view.pool)
                    .await
                    .map_err(anyhow::Error::from)?;
            let messages = sqlx::query_scalar("SELECT COUNT(*) FROM chat")
                .fetch_one(&view.pool)
                .await
                .map_err(anyhow::Error::from)?;
            Ok((sessions, messages))
        }
    }
    fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }
}
pub(crate) async fn accounts(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_accounts::api::accounts(&app.store.accounts_repository(), &app, &identity)
        .await
        .map_err(Into::into)
}
pub(crate) async fn create_account(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<NewAccount>,
) -> Api {
    webagent_accounts::api::create_account(&app.store.accounts_repository(), &app, &identity, body)
        .await
        .map_err(Into::into)
}
pub(crate) async fn change_profile(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<ProfileChange>,
) -> Api {
    webagent_accounts::api::change_profile(
        &app.store.accounts_repository(),
        &app,
        &identity,
        &id,
        body,
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn account_sessions(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    webagent_accounts::api::account_sessions(
        &app.store.accounts_repository(),
        &app,
        &identity,
        &id,
        crate::now(),
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn revoke_account_session(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, session_id)): Path<(String, String)>,
) -> Api {
    webagent_accounts::api::revoke_account_session(
        &app.store.accounts_repository(),
        &app,
        &identity,
        &id,
        &session_id,
        crate::now(),
    )
    .await
    .map_err(Into::into)
}
