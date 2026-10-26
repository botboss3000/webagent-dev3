//! Model endpoint services; the root router supplies authenticated identity and browser middleware.
use axum::{http::StatusCode,Json};
use serde_json::Value;
pub enum Error {Public(StatusCode,String),Internal(anyhow::Error)}
impl std::fmt::Debug for Error {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        match self {Self::Public(s,m)=>f.debug_tuple("ModelApiError").field(s).field(m).finish(),
            Self::Internal(_)=>f.write_str("ModelApiError(Internal)")}
    }
}
impl From<anyhow::Error> for Error {fn from(error:anyhow::Error)->Self {Self::Internal(error)}}
pub type ApiError=Error;
#[allow(non_snake_case)] pub fn ApiError(status:StatusCode,message:String)->Error {Error::Public(status,message)}
pub type Api=Result<Json<Value>,Error>;
pub fn bad(message:&str)->Error {Error::Public(StatusCode::BAD_REQUEST,message.into())}
pub fn conflict(message:&str)->Error {Error::Public(StatusCode::CONFLICT,message.into())}
pub async fn administrator<C:crate::storage::RuntimeStorage>(app:&crate::runtime::Runtime<'_,C>,identity:&Value)->Result<(),Error> {
    let id=identity["_user_id"].as_str().unwrap_or("");
    let account=app.store.account_by_id(id).await?.ok_or_else(||ApiError(StatusCode::UNAUTHORIZED,"Sign in required".into()))?;
    webagent_access::require_administrator(&account).map_err(|_|ApiError(StatusCode::FORBIDDEN,"Administrator access required".into()))?;
    Ok(())
}
pub mod selection;
pub mod credentials;
pub mod account_catalog;
pub mod platform_catalog;
