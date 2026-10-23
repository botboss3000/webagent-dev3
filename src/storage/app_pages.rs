use super::*;
impl Store {
    pub async fn app_page_preferences(&self)->Result<Value> {
        webagent_webapp::preferences::Repository::new(&self.pool,self)
            .read(webagent_webapp::preferences::Navigation::App).await
    }
    pub async fn save_app_page_preferences(&self,pages:&[String],expected:u64)->Result<Option<Value>> {
        webagent_webapp::preferences::Repository::new(&self.pool,self)
            .save(webagent_webapp::preferences::Navigation::App,pages,expected).await
    }
}
