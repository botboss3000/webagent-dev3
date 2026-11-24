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
/// Loop `index`'s hint socket: loop 0 keeps `dispatch-wake.sock`, loop k `dispatch-wake-k.sock`.
fn socket(control_dir: &Path, index: u32) -> PathBuf {
    control_dir.join(crate::partition::indexed_name("dispatch-wake.sock", index))
}
async fn send(control_dir: &Path, index: u32, message: &[u8]) {
    let path = socket(control_dir, index);
    let _ = tokio::time::timeout(Duration::from_millis(100), async move {
        if let Ok(mut stream) = webagent_platform::ipc::connect(&path).await {
            let _ = stream.write_all(message).await;
        }
    })
    .await;
}
/// A hint about everything (settings and other changes that are not one account's): every
/// loop gets it and looks at its own accounts.
pub async fn notify(control_dir: &Path) {
    emit(None);
    for index in targets(None, crate::partition::routes()) {
        send(control_dir, index, b"W").await;
    }
}
/// The loops a hint goes to: the one serving the named account, or all of them.
fn targets(user: Option<&str>, loops: u32) -> Vec<u32> {
    match user {
        Some(user) => vec![crate::partition::loop_for(user, loops)],
        None => (0..loops.max(1)).collect(),
    }
}
/// A hint naming one account, sent to the loop that serves it. An id that is not a valid
/// account id is sent as a hint about everything instead. A loop process takes the hint in
/// itself only for an account it serves.
pub async fn notify_account(control_dir: &Path, user: &str, kind: Kind) {
    if !webagent_access::valid_id(user) || user.len() > MAX_ID {
        return notify(control_dir).await;
    }
    if crate::partition::owns(user) {
        emit(Some((user, kind)));
    }
    let message = named_message(user, kind);
    for index in targets(Some(user), crate::partition::routes()) {
        send(control_dir, index, &message).await;
    }
}
fn named_message(user: &str, kind: Kind) -> Vec<u8> {
    let mut message = vec![b'W', kind.byte()];
    message.extend_from_slice(user.as_bytes());
    message.push(b'\n');
    message
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
    let own = crate::partition::own().unwrap_or_default();
    let path = socket(control_dir, own.index);
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
    let control = control_dir.to_path_buf();
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
                                Some(Some((user,kind))) if crate::partition::owns(&user)=>emit(Some((&user,kind))),
                                // A sender that routed over a stale loop count: pass the hint
                                // on to the loop that serves the account (one hop).
                                Some(Some((user,kind)))=>{
                                    let index=crate::partition::loop_for(&user,own.loops);
                                    if index!=own.index{
                                        send(&control,index,&named_message(&user,kind)).await;
                                    }
                                }
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
    fn named_hints_go_to_the_serving_loop_and_others_to_every_loop() {
        assert_eq!(targets(Some("guest_ab12"), 1), vec![0]);
        assert_eq!(targets(Some("guest_ab12"), 2), vec![1]);
        assert_eq!(targets(Some("user_one"), 2), vec![0]);
        assert_eq!(targets(Some("admin"), 4), vec![0]);
        assert_eq!(targets(None, 1), vec![0]);
        assert_eq!(targets(None, 3), vec![0, 1, 2]);
        assert_eq!(
            socket(Path::new("/c"), 0),
            PathBuf::from("/c/dispatch-wake.sock")
        );
        assert_eq!(
            socket(Path::new("/c"), 2),
            PathBuf::from("/c/dispatch-wake-2.sock")
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hint_reaches_only_the_socket_of_its_loop() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let first = tokio::net::UnixListener::bind(socket(dir.path(), 0)).unwrap();
        let second = tokio::net::UnixListener::bind(socket(dir.path(), 1)).unwrap();
        let message = named_message("guest_ab12", Kind::Run);
        for index in targets(Some("guest_ab12"), 2) {
            send(dir.path(), index, &message).await;
        }
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(1), second.accept())
            .await
            .unwrap()
            .unwrap();
        let mut received = Vec::new();
        stream.read_to_end(&mut received).await.unwrap();
        assert_eq!(
            parse(&received),
            Some(Some(("guest_ab12".into(), Kind::Run)))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), first.accept())
                .await
                .is_err()
        );
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
