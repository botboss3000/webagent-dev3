use anyhow::{bail, Result};
use serde_json::Value;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
mod config;
async fn handle(mut stream: UnixStream, pool: SqlitePool, root: &Path) -> Result<()> {
    if stream.peer_cred()?.uid() != unsafe { getuid() } {
        bail!("Peer rejected")
    }
    let n = stream.read_u32().await? as usize;
    if n > 136 * 1024 * 1024 {
        bail!("Frame rejected")
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).await?;
    let request: Value = serde_json::from_slice(&bytes)?;
    if request["version"] != 1 {
        bail!("Version rejected")
    }
    let (c, digest) = config::Config::read(&root.join("config/diagnostics.json"))?;
    let event = &request["event"];
    match request["operation"].as_str() {
        Some("heartbeat") => {
            let role = event["role"]
                .as_str()
                .filter(|v| matches!(*v, "api" | "loop"))
                .ok_or_else(|| anyhow::anyhow!("Role rejected"))?;
            sqlx::query("INSERT INTO diagnostic_processes(role,ts,payload) VALUES(?,?,?) ON CONFLICT(role) DO UPDATE SET ts=excluded.ts,payload=excluded.payload").bind(role).bind(now() as i64).bind(event.to_string()).execute(&pool).await?;
        }
        Some("event" | "snapshot") => {
            if event["owner"] != "admin" || event["config_digest"] != digest || !c.active() {
                bail!("Capture withdrawn")
            }
            let allowed = match event["category"].as_str() {
                Some("provider") => c.categories.provider,
                Some("context_assembly") => c.categories.context_assembly,
                Some("run") => c.categories.run,
                Some("tool") => c.categories.tool,
                Some("recovery") => c.categories.recovery,
                _ => false,
            };
            if !allowed {
                bail!("Category withdrawn")
            }
            if request["operation"] == "event" {
                sqlx::query("INSERT INTO diagnostics(ts,category,run_id,session_id,payload) VALUES(?,?,?,?,?)").bind(now() as i64).bind(event["category"].as_str()).bind(event["run_id"].as_str()).bind(event["session_id"].as_str()).bind(event.to_string()).execute(&pool).await?;
            } else {
                let size = request["bytes"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("Size rejected"))?;
                if !c.selected(event["session_id"].as_str().unwrap_or(""), now())
                    || size > c.full_context.max_snapshot_bytes as u64
                {
                    bail!("Snapshot withdrawn")
                }
                let sealed = hex::decode(
                    request["sealed"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Ciphertext required"))?,
                )?;
                if sealed.len() > c.full_context.max_snapshot_bytes + 1024 {
                    bail!("Ciphertext rejected")
                }
                let (current, current_digest) =
                    config::Config::read(&root.join("config/diagnostics.json"))?;
                if current_digest != digest
                    || !current.selected(event["session_id"].as_str().unwrap_or(""), now())
                {
                    bail!("Snapshot withdrawn")
                }
                sqlx::query("INSERT OR IGNORE INTO context_snapshots(attempt_id,ts,expires_at,session_id,run_id,bytes,sealed) VALUES(?,?,?,?,?,?,?)").bind(event["attempt_id"].as_str()).bind(now() as i64).bind((now()+c.full_context.retention_hours*3600) as i64).bind(event["session_id"].as_str()).bind(event["run_id"].as_str()).bind(size as i64).bind(sealed).execute(&pool).await?;
            }
        }
        _ => bail!("Operation rejected"),
    }
    stream.write_all(&[1]).await?;
    Ok(())
}
unsafe extern "C" {
    fn getuid() -> u32;
}
#[tokio::main]
async fn main() -> Result<()> {
    let root = std::path::PathBuf::from(std::env::var("V3_PLUGIN_STATE")?);
    let pool = open(&root).await?;
    let socket = root.join("diagnostics/plugin.sock");
    let ready = root.join("diagnostics/plugin.ready");
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::write(
        root.join("diagnostics/plugin.pid"),
        std::process::id().to_string(),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let alive = ready.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let temp = alive.with_extension("next");
            if tokio::fs::write(&temp, now().to_string()).await.is_ok() {
                let _ = tokio::fs::rename(temp, &alive).await;
            }
        }
    });
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut ticks = 0u32;
    loop {
        tokio::select! {
         _=interval.tick()=>{
          if ticks%60==0 {if let Ok((c,_))=config::Config::read(&root.join("config/diagnostics.json")){let _=prune(&pool,&c).await;}}ticks=ticks.wrapping_add(1);},
         accepted=listener.accept()=>{let (stream,_)=accepted?;
          // One bounded frame at a time; excess clients remain in the bounded socket backlog.
          let _=tokio::time::timeout(Duration::from_secs(10),handle(stream,pool.clone(),&root)).await;
         }
        }
    }
}
async fn open(root: &Path) -> anyhow::Result<SqlitePool> {
    let dir = root.join("diagnostics");
    tokio::fs::create_dir_all(&dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let options = SqliteConnectOptions::new()
        .filename(dir.join("logs.db"))
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(2));
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    for statement in [
  "CREATE TABLE IF NOT EXISTS diagnostics(id INTEGER PRIMARY KEY,ts INTEGER NOT NULL,category TEXT NOT NULL,run_id TEXT,session_id TEXT,payload TEXT NOT NULL)",
  "CREATE INDEX IF NOT EXISTS diagnostics_ts ON diagnostics(ts)",
  "CREATE INDEX IF NOT EXISTS diagnostics_run ON diagnostics(run_id,id)",
  "CREATE INDEX IF NOT EXISTS diagnostics_session ON diagnostics(session_id,id)",
  "CREATE TABLE IF NOT EXISTS context_snapshots(attempt_id TEXT PRIMARY KEY,ts INTEGER NOT NULL,expires_at INTEGER NOT NULL,session_id TEXT NOT NULL,run_id TEXT NOT NULL,bytes INTEGER NOT NULL,sealed BLOB NOT NULL)",
  "CREATE INDEX IF NOT EXISTS context_snapshots_age ON context_snapshots(ts)",
  "CREATE TABLE IF NOT EXISTS diagnostic_processes(role TEXT PRIMARY KEY,ts INTEGER NOT NULL,payload TEXT NOT NULL)",
 ] {sqlx::query(statement).execute(&pool).await?;}
    Ok(pool)
}
async fn prune(pool: &SqlitePool, config: &config::Config) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM diagnostics WHERE id IN (SELECT id FROM diagnostics WHERE ts<? ORDER BY id LIMIT 1000)")
  .bind(now() as i64-(config.retention.hours*3600) as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM diagnostics WHERE id IN (SELECT id FROM diagnostics WHERE id<=COALESCE((SELECT id FROM diagnostics ORDER BY id DESC LIMIT 1 OFFSET ?),0) ORDER BY id LIMIT 1000)")
  .bind(config.retention.max_rows as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM context_snapshots WHERE attempt_id IN (SELECT attempt_id FROM context_snapshots WHERE expires_at<=? OR ts<? ORDER BY ts LIMIT 1000)")
  .bind(now() as i64).bind(now() as i64-(config.full_context.retention_hours*3600) as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM context_snapshots WHERE attempt_id IN (SELECT attempt_id FROM (SELECT attempt_id,SUM(bytes) OVER (ORDER BY ts DESC,attempt_id DESC) retained FROM context_snapshots) WHERE retained>? LIMIT 1000)")
  .bind(config.full_context.max_retained_bytes as i64).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn protocol_rejects_public_owner_and_unknown_version() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        let mut c = config::Config::disabled();
        c.enabled = true;
        std::fs::write(
            root.path().join("config/diagnostics.json"),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        let (_, digest) =
            config::Config::read(&root.path().join("config/diagnostics.json")).unwrap();
        let pool = open(root.path()).await.unwrap();
        for (version, owner, success) in [
            (1, "public", false),
            (2, "admin", false),
            (1, "admin", true),
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let p = pool.clone();
            let path = root.path().to_path_buf();
            let task = tokio::spawn(async move { handle(server, p, &path).await });
            let bytes=serde_json::to_vec(&serde_json::json!({"version":version,"operation":"event","event":{"owner":owner,"category":"provider","config_digest":digest}})).unwrap();
            client.write_u32(bytes.len() as u32).await.unwrap();
            client.write_all(&bytes).await.unwrap();
            assert_eq!(task.await.unwrap().is_ok(), success);
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM diagnostics")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let (mut client, server) = UnixStream::pair().unwrap();
        let p = pool.clone();
        let path = root.path().to_path_buf();
        let task = tokio::spawn(async move { handle(server, p, &path).await });
        client.write_u32(136 * 1024 * 1024 + 1).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }
}
