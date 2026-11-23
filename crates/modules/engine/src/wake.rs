//! Wake hints contain no authority or content. Durable scans remain the fallback.
//!
//! A hint may name one account (a validated id, never trusted for anything but "look at this
//! account sooner") and a kind. Each process keeps a bounded table of accounts named since
//! start with the sequence of their latest hint; consumers keep their own cursor and visit
//! only the accounts named after it. A hint that names no account (settings changes, older
//! senders) or a full table means "look at everything", as every hint did before.
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{watch, Notify},
};
use tokio_util::sync::CancellationToken;

/// What a named hint is about. Only the account's own consumers need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A run was queued, changed state or ended (the scheduler; automation follows run ends).
    Run,
    /// A background task was queued or settled.
    Background,
    /// Module input (events, webhooks, main notes).
    Module,
}
impl Kind {
    fn byte(self) -> u8 {
        match self {
            Kind::Run => b'r',
            Kind::Background => b'b',
            Kind::Module => b'm',
        }
    }
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'r' => Some(Kind::Run),
            b'b' => Some(Kind::Background),
            b'm' => Some(Kind::Module),
            _ => None,
        }
    }
}

/// Most accounts the table holds; past it the table empties and counts as a global hint.
const LIMIT: usize = 4096;
/// Longest account id a hint may carry.
const MAX_ID: usize = 128;

#[derive(Default)]
struct Named {
    accounts: HashMap<String, u64>,
    /// Sequence of the latest hint that named no account, or of an overflow.
    global: u64,
}
fn named() -> &'static Mutex<Named> {
    static NAMED: OnceLock<Mutex<Named>> = OnceLock::new();
    NAMED.get_or_init(Default::default)
}
fn signal() -> &'static Notify {
    static SIGNAL: OnceLock<Notify> = OnceLock::new();
    SIGNAL.get_or_init(Notify::new)
}
fn broadcasts() -> &'static watch::Sender<u64> {
    static BROADCAST: OnceLock<watch::Sender<u64>> = OnceLock::new();
    BROADCAST.get_or_init(|| watch::channel(0).0)
}
pub fn subscribe() -> watch::Receiver<u64> {
    broadcasts().subscribe()
}
/// How many wake hints this process has seen; a result computed before the latest hint is stale.
pub fn sequence() -> u64 {
    *broadcasts().borrow()
}
/// Accounts named by hints after `cursor`, with the cursor to pass next time. `None`: a hint
/// after `cursor` named no account (or the table overflowed), so look at every account.
pub fn named_since(cursor: u64) -> (u64, Option<HashSet<String>>) {
    let Ok(named) = named().lock() else {
        return (sequence(), None);
    };
    // Read under the table lock, which every emit holds while it moves the sequence.
    let now = sequence();
    if named.global > cursor {
        return (now, None);
    }
    let accounts = named
        .accounts
        .iter()
        .filter(|(_, at)| **at > cursor)
        .map(|(user, _)| user.clone())
        .collect();
    (now, Some(accounts))
}
fn emit(account: Option<(&str, Kind)>) {
    if let Ok(mut named) = named().lock() {
        let next = sequence().wrapping_add(1);
        match account {
            Some((user, _))
                if named.accounts.len() < LIMIT || named.accounts.contains_key(user) =>
            {
                named.accounts.insert(user.to_owned(), next);
            }
            Some(_) => {
                named.accounts.clear();
                named.global = next;
            }
            None => named.global = next,
        }
        broadcasts().send_modify(|sequence| *sequence = next);
    } else {
        broadcasts().send_modify(|sequence| *sequence = sequence.wrapping_add(1));
    }
    signal().notify_one();
}
fn socket(control_dir: &Path) -> PathBuf {
    control_dir.join("dispatch-wake.sock")
}
async fn send(control_dir: &Path, message: Vec<u8>) {
    let path = socket(control_dir);
    let _ = tokio::time::timeout(Duration::from_millis(100), async move {
        if let Ok(mut stream) = webagent_platform::ipc::connect(&path).await {
            let _ = stream.write_all(&message).await;
        }
    })
    .await;
}
/// A hint about everything (settings and other changes that are not one account's).
pub async fn notify(control_dir: &Path) {
    emit(None);
    send(control_dir, b"W".to_vec()).await;
}
/// A hint naming one account. An id that is not a valid account id is sent as a hint about
/// everything instead.
pub async fn notify_account(control_dir: &Path, user: &str, kind: Kind) {
    if !webagent_access::valid_id(user) || user.len() > MAX_ID {
        return notify(control_dir).await;
    }
    emit(Some((user, kind)));
    let mut message = vec![b'W', kind.byte()];
    message.extend_from_slice(user.as_bytes());
    message.push(b'\n');
    send(control_dir, message).await;
}
/// Parse one message read from the socket: `W` alone, or `W`, a kind byte, an account id and a
/// newline. Anything else is ignored; an invalid id is not trusted to name anything.
fn parse(message: &[u8]) -> Option<Option<(String, Kind)>> {
    match message {
        [b'W'] => Some(None),
        [b'W', kind, rest @ .., b'\n'] => {
            let kind = Kind::from_byte(*kind)?;
            let user = std::str::from_utf8(rest).ok()?;
            (user.len() <= MAX_ID && webagent_access::valid_id(user))
                .then(|| Some((user.to_owned(), kind)))
        }
        _ => None,
    }
}
/// Off Unix the API and loop share one process, so `emit` alone reaches the loop.
#[cfg(not(unix))]
pub async fn listener(
    _control_dir: &Path,
    stop: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    Ok(tokio::spawn(async move { stop.cancelled().await }))
}
#[cfg(unix)]
pub async fn listener(
    control_dir: &Path,
    stop: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tokio::{
        io::AsyncReadExt,
        net::{UnixListener, UnixStream},
    };
    let path = socket(control_dir);
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
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                _=stop.cancelled()=>break,
                incoming=listener.accept()=>if let Ok((stream,_))=incoming {
                    // Same account only (same uid), as before; a named account is only a hint.
                    if stream.peer_cred().is_ok_and(|cred|cred.uid()==expected_uid){
                        let mut message=Vec::with_capacity(MAX_ID+3);
                        let read=tokio::time::timeout(Duration::from_millis(100),
                            stream.take((MAX_ID+3) as u64).read_to_end(&mut message)).await;
                        if read.is_ok(){
                            match parse(&message){
                                Some(Some((user,kind)))=>emit(Some((&user,kind))),
                                Some(None)=>emit(None),
                                None=>{}
                            }
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_file(path);
    }))
}
pub async fn wait(poll_interval_ms: u64, stop: &CancellationToken) {
    tokio::select! {_=signal().notified()=>{},_=tokio::time::sleep(Duration::from_millis(poll_interval_ms.min(5000)))=>{},_=stop.cancelled()=>{}}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn wake_broadcast_reaches_all_consumers() {
        let mut one = super::subscribe();
        let mut two = super::subscribe();
        super::emit(None);
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
    #[test]
    fn hints_name_only_valid_accounts() {
        assert_eq!(parse(b"W"), Some(None));
        assert_eq!(
            parse(b"Wrguest_ab12\n"),
            Some(Some(("guest_ab12".to_owned(), Kind::Run)))
        );
        assert_eq!(parse(b"Wr../etc\n"), None);
        assert_eq!(parse(b"Wxadmin\n"), None);
        assert_eq!(parse(b"Wradmin"), None);
        assert_eq!(parse(b"X"), None);
    }
    #[test]
    fn named_accounts_follow_each_cursor() {
        let (cursor, _) = named_since(u64::MAX);
        let start = sequence();
        emit(Some(("named_cursor_a", Kind::Run)));
        let (next, named) = named_since(start);
        // Other tests may emit concurrently; this account is among those named.
        if let Some(named) = named {
            assert!(named.contains("named_cursor_a"));
        }
        assert!(next > start);
        let _ = cursor;
    }
}
