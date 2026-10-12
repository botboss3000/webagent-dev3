//! Wake hints contain no authority or content. Durable scans remain the fallback.
use crate::*;
use std::sync::OnceLock;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{watch, Notify},
};
fn signal() -> &'static Notify {
    static SIGNAL: OnceLock<Notify> = OnceLock::new();
    SIGNAL.get_or_init(Notify::new)
}
fn broadcasts() -> &'static watch::Sender<u64> {
    static BROADCAST: OnceLock<watch::Sender<u64>> = OnceLock::new();
    BROADCAST.get_or_init(|| watch::channel(0).0)
}
pub(crate) fn subscribe() -> watch::Receiver<u64> {
    broadcasts().subscribe()
}
fn emit() {
    signal().notify_one();
    broadcasts().send_modify(|sequence| *sequence = sequence.wrapping_add(1));
}
fn socket(app: &App) -> PathBuf {
    app.control_dir.join("dispatch-wake.sock")
}
pub(crate) async fn notify(app: &App) {
    emit();
    let path = socket(app);
    let _ = tokio::time::timeout(Duration::from_millis(100), async move {
        if let Ok(mut stream) = UnixStream::connect(path).await {
            let _ = stream.write_all(b"W").await;
        }
    })
    .await;
}
pub(crate) async fn listener(app: &App) -> Result<tokio::task::JoinHandle<()>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let path = socket(app);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()) {
            return Err(anyhow!("Wake path is not a socket"));
        }
        if UnixStream::connect(&path).await.is_ok() {
            return Err(anyhow!("Wake listener already active"));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
    let expected_uid = std::fs::metadata(&path)?.uid();
    let stop = app.stop.clone();
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                _=stop.cancelled()=>break,
                incoming=listener.accept()=>if let Ok((mut stream,_))=incoming {
                    if stream.peer_cred().is_ok_and(|cred|cred.uid()==expected_uid){
                        let mut byte=[0u8;1];if tokio::time::timeout(Duration::from_millis(100),stream.read_exact(&mut byte)).await.is_ok()&&byte==[b'W']{emit();}
                    }
                }
            }
        }
        let _ = std::fs::remove_file(path);
    }))
}
pub(crate) async fn wait(app: &App) {
    let fallback = if std::env::var("V3_DISPATCH_POLL_ONLY").as_deref() == Ok("1") {
        app.runtime.read().await.scheduler.poll_interval_ms
    } else {
        5000
    };
    tokio::select! {_=signal().notified()=>{},_=tokio::time::sleep(Duration::from_millis(fallback.min(5000)))=>{},_=app.stop.cancelled()=>{}}
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn wake_broadcast_reaches_all_consumers() {
        let mut one = super::subscribe();
        let mut two = super::subscribe();
        super::emit();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), one.changed())
                .await
                .unwrap()
                .is_ok()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), two.changed())
                .await
                .unwrap()
                .is_ok()
        );
    }
}
