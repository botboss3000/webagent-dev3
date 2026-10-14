/// Pin one catalog per request; authentication keeps its existing caller routing.
struct State<T>(T);
impl axum::extract::FromRequestParts<App> for State<App> {
    type Rejection=Infallible;
    async fn from_request_parts(parts:&mut axum::http::request::Parts,state:&App)->std::result::Result<Self,Self::Rejection> {
        let catalog=if let Some(snapshot)=parts.extensions.get::<Arc<abilities::Catalog>>() {snapshot.clone()}
            else {let snapshot=live_assets::catalog(&state.abilities);parts.extensions.insert(snapshot.clone());snapshot};
        let mut app=state.clone();app.abilities=catalog;
        if let Ok(config)=loop_runtime::Config::read(&app.runtime_path) {
            let mut previous=app.runtime.write().await;
            if previous.cache.document_budget_bytes!=config.cache.document_budget_bytes
                ||previous.cache.context_budget_bytes!=config.cache.context_budget_bytes {
                app.store.configure_cache(config.cache.document_budget_bytes,config.cache.context_budget_bytes);
            }
            *previous=config;
        }
        let _=live_assets::record_adoption(&app.store.root,"api",&app.abilities);
        let _=crate::engine_components::adoption(&app.store.root,"api");
        Ok(Self(app))
    }
}
