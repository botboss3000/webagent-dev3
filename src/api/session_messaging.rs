//! Owner-only route inspection and revocation; human approval is the sole grant path.
use super::*;
pub(crate) async fn list(State(app):State<App>,Path(sid):Path<String>)->Api{
 let value=crate::session_messaging::routes(&app,&sid).await.map_err(|_|ApiError(StatusCode::FORBIDDEN,"Private session owner required".into()))?;
 Ok(Json(value))
}
pub(crate) async fn revoke(State(app):State<App>,Path((sid,target)):Path<(String,String)>,Json(body):Json<Value>)->Api{
 crate::session_messaging::routes(&app,&sid).await.map_err(|_|ApiError(StatusCode::FORBIDDEN,"Private session owner required".into()))?;
 let request=required(&body,"client_request_id",128)?;
 let revision=body["expected_revision"].as_u64().ok_or_else(||bad("expected_revision required"))?;
 if body.as_object().is_none_or(|m|m.keys().any(|k|!matches!(k.as_str(),"client_request_id"|"expected_revision"))){return Err(bad("Unknown route field"));}
 Ok(Json(app.store.revoke_messaging_route(&sid,&target,revision,request).await.map_err(|e|conflict(&e.to_string()))?))
}
