//! Caller-owned global Agents page favorites, stored in the user's database.
use super::*;
use axum::Extension;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update { page_ids:Vec<String>, expected_revision:u64 }
pub async fn read(State(app):State<App>,Extension(identity):Extension<Value>)->Api {
    let user=identity["_user_id"].as_str().ok_or_else(||bad("Missing account identity"))?;
    Ok(Json(app.store.for_user(user).await?.agent_page_preferences().await?))
}
pub async fn save(State(app):State<App>,Extension(identity):Extension<Value>,Json(input):Json<Update>)->Api {
    let user=identity["_user_id"].as_str().ok_or_else(||bad("Missing account identity"))?;
    if input.page_ids.len()>64 || !input.page_ids.iter().any(|id|id=="config")
        || input.page_ids.iter().any(|id|id.is_empty()||id.len()>64||!id.bytes().all(|c|c.is_ascii_lowercase()||c.is_ascii_digit()||c==b'-'))
        || input.page_ids.iter().collect::<std::collections::HashSet<_>>().len()!=input.page_ids.len() {
        return Err(bad("Invalid agent page order"));
    }
    let store=app.store.for_user(user).await?;
    let saved=store.save_agent_page_preferences(&input.page_ids,input.expected_revision).await?
        .ok_or_else(||conflict("Agent page favorites changed; reload before saving"))?;
    Ok(Json(saved))
}
#[cfg(test)]
mod tests {
    use super::*;
use axum::Extension;
    #[tokio::test]
    async fn preferences_persist_in_user_plane_and_fence_stale_saves(){
        let (_root,app)=crate::runtime_tests::fixture().await;
        let identity=json!({"_user_id":"admin"});
        assert_eq!(read(State(app.clone()),Extension(identity.clone())).await.unwrap().0["revision"],0);
        let pages=vec!["abilities".into(),"config".into()];
        let saved=save(State(app.clone()),Extension(identity.clone()),Json(Update{page_ids:pages.clone(),expected_revision:0})).await.unwrap().0;
        assert_eq!(saved["revision"],1);
        assert_eq!(read(State(app.clone()),Extension(identity.clone())).await.unwrap().0["page_ids"],json!(pages));
        assert!(save(State(app.clone()),Extension(identity.clone()),Json(Update{page_ids:vec!["config".into()],expected_revision:0})).await.is_err());
        assert!(save(State(app.clone()),Extension(identity),Json(Update{page_ids:vec!["config".into(),"config".into()],expected_revision:1})).await.is_err());
        let store=app.store.for_user("admin").await.unwrap();
        assert!(sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key='user_agent_page_preferences:v1'").fetch_optional(&store.app_pool).await.unwrap().is_none());
        let other=app.store.create_user_account("tabs-other","long-placeholder-password-hash").await.unwrap();
        let other_store=app.store.for_user(other["user_id"].as_str().unwrap()).await.unwrap();
        assert_eq!(other_store.agent_page_preferences().await.unwrap()["revision"],0);
        assert!(other_store.save_agent_page_preferences(&["identity".into(),"config".into()],0).await.unwrap().is_some());
        assert_eq!(store.agent_page_preferences().await.unwrap()["page_ids"],json!(pages));
        let blob=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key='user_agent_page_preferences:v1'").fetch_one(&store.pool).await.unwrap();
        assert_eq!(store.unseal(&blob).unwrap(),saved);
        assert!(read(State(app),Extension(json!({"_user_id":"missing"}))).await.is_err());
    }
}
