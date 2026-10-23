//! Borrowed user-plane navigation repository. No pools or encryption keys are created here.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::SqlitePool;
pub trait RecordCodec: Sync {
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>>;
    fn unseal(&self, value: &[u8]) -> Result<Value>;
}
#[derive(Clone, Copy)]
pub enum Navigation {
    App,
    Agent,
}
impl Navigation {
    fn key(self) -> &'static str {
        match self {
            Self::App => "user_app_page_preferences:v1",
            Self::Agent => "user_agent_page_preferences:v1",
        }
    }
}
pub fn validate_pages(kind: Navigation, pages: &[String]) -> bool {
    pages.len() <= 64
        && pages.iter().any(|id| id == "config")
        && pages.iter().collect::<std::collections::HashSet<_>>().len() == pages.len()
        && pages.iter().all(|id| match kind {
            Navigation::App => [
                "config",
                "functions",
                "modules",
                "settings",
                "files",
                "users",
                "user-policies",
                "page-access",
                "models",
            ]
            .contains(&id.as_str()),
            Navigation::Agent => {
                !id.is_empty()
                    && id.len() <= 64
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            }
        })
}
fn default_preferences() -> Value {
    json!({"page_ids":["config"],"revision":0,"configured":false})
}
pub struct Repository<'a, C: RecordCodec> {
    pool: &'a SqlitePool,
    codec: &'a C,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    pub fn new(pool: &'a SqlitePool, codec: &'a C) -> Self {
        Self { pool, codec }
    }
    pub async fn read(&self, kind: Navigation) -> Result<Value> {
        let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(kind.key())
            .fetch_optional(self.pool)
            .await?;
        blob.map(|bytes| self.codec.unseal(&bytes))
            .transpose()
            .map(|v| v.unwrap_or_else(default_preferences))
    }
    pub async fn save(
        &self,
        kind: Navigation,
        pages: &[String],
        expected: u64,
    ) -> Result<Option<Value>> {
        let key = kind.key();
        let mut tx = self.pool.begin().await?;
        // Reserve the user-plane writer before reading the revision, including across processes.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO NOTHING")
            .bind(key)
            .bind(self.codec.encode_user_record(&default_preferences())?)
            .execute(&mut *tx)
            .await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
        let current = self.codec.unseal(&blob)?;
        if current["revision"].as_u64() != Some(expected) {
            tx.rollback().await?;
            return Ok(None);
        }
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| anyhow!("Preference revision exhausted"))?;
        let saved = json!({"page_ids":pages,"revision":revision,"configured":true});
        sqlx::query("UPDATE docs SET value=? WHERE key=?")
            .bind(self.codec.encode_user_record(&saved)?)
            .bind(key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Codec;
    impl RecordCodec for Codec {
        fn encode_user_record(&self, v: &Value) -> Result<Vec<u8>> {
            Ok(serde_json::to_vec(v)?)
        }
        fn unseal(&self, v: &[u8]) -> Result<Value> {
            Ok(serde_json::from_slice(v)?)
        }
    }
    #[tokio::test]
    async fn sqlite_user_planes_navigation_and_stale_writes_are_independent() {
        let root = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(root.path().join("user.db"))
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        let second = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let a = Repository::new(&pool, &Codec);
        let b = Repository::new(&second, &Codec);
        assert_eq!(
            a.read(Navigation::App).await.unwrap(),
            default_preferences()
        );
        let pages = vec!["files".into(), "config".into()];
        assert_eq!(
            a.save(Navigation::App, &pages, 0).await.unwrap().unwrap()["revision"],
            1
        );
        assert!(b
            .save(Navigation::App, &["config".into()], 0)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            b.read(Navigation::App).await.unwrap()["page_ids"],
            json!(pages)
        );
        assert_eq!(
            a.read(Navigation::Agent).await.unwrap(),
            default_preferences()
        );
        assert!(b
            .save(Navigation::Agent, &["abilities".into(), "config".into()], 0)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            a.read(Navigation::App).await.unwrap()["page_ids"],
            json!(pages)
        );
        let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(Navigation::App.key())
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE docs SET value=? WHERE key=?")
            .bind(b"invalid".to_vec())
            .bind(Navigation::App.key())
            .execute(&pool)
            .await
            .unwrap();
        assert!(a.save(Navigation::App, &pages, 1).await.is_err());
        sqlx::query("UPDATE docs SET value=? WHERE key=?")
            .bind(blob)
            .bind(Navigation::App.key())
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(a.read(Navigation::App).await.unwrap()["revision"], 1);
        assert!(!validate_pages(
            Navigation::App,
            &["config".into(), "config".into()]
        ));
        assert!(!validate_pages(
            Navigation::Agent,
            &["config".into(), "../escape".into()]
        ));
    }
}
