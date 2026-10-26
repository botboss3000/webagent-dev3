//! Files attachment repository on the existing user-plane pool, cache and Store codec.
use super::*;

impl Store {
    /// Remove only an unclaimed upload owned by this session and decrement its quota atomically.
    pub(crate) async fn delete_pending_upload(&self, sid: &str, aid: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        if !webagent_files::repository::delete_pending_upload(self, &mut tx, sid, aid).await? {
            return Ok(());
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[webagent_files::attachment_key(aid), webagent_files::QUOTA_KEY.into()]);
        Ok(())
    }
}
