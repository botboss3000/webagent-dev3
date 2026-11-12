//! The v3 tool worker. It owns the tool socket the API and loop call and accepts only the
//! application identity. File tools ([`files`]), workspace commands ([`commands`]),
//! terminals, preview tunnels, ability plugins and the private browsers ([`browser`]) all
//! run here; a request for any other tool fails as unknown.
pub mod browser;
pub mod commands;
pub mod files;
pub mod git;
pub mod launch;
pub mod manager;
pub mod plugins;
pub mod runner;
pub mod sandbox;
pub mod terminal;
pub mod tunnel;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;

/// Longest request line accepted.
const REQUEST_LIMIT: u64 = 2 * 1024 * 1024;
/// A caller has this long to send its request line.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the worker listens and whom it serves.
#[derive(Debug, Clone)]
pub struct Config {
    /// The socket the API and loop connect to.
    pub socket: PathBuf,
    /// The only caller UID served; other callers are disconnected unanswered.
    pub client_uid: u32,
    /// Where the native file tools work.
    pub files: files::Roots,
    /// The per-command supervisor: its executable and leading arguments.
    pub runner: Vec<String>,
    /// The per-plugin supervisor: its executable and leading arguments.
    pub plugin_runner: Vec<String>,
    /// Where published ability packages live.
    pub packages: plugins::Packages,
    /// Where private browsers keep their state and which app origin they may sign in to.
    pub browser: browser::Settings,
}

impl Config {
    /// Reads `V3_TOOL_SOCKET` and `V3_TOOL_CLIENT_UID`, defaulting to the container layout.
    ///
    /// # Errors
    /// When `V3_TOOL_CLIENT_UID` is not a number.
    pub fn from_env() -> io::Result<Self> {
        let path = |name: &str, default: &str| {
            PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()))
        };
        let client_uid = std::env::var("V3_TOOL_CLIENT_UID")
            .unwrap_or_else(|_| "1100".into())
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid client UID"))?;
        Ok(Self {
            socket: path("V3_TOOL_SOCKET", "/runtime/worker/tools.sock"),
            client_uid,
            files: files::Roots::container(),
            // The app executable's supervisor mode, dispatched before its async runtime.
            runner: vec!["/usr/local/bin/webagent-v3".into(), "--tool-runner".into()],
            plugin_runner: vec![
                "/usr/local/bin/webagent-v3".into(),
                "--plugin-runner".into(),
            ],
            packages: plugins::Packages::container(),
            browser: browser::Settings::container(),
        })
    }
}

/// Serves the tool socket until the listener fails.
///
/// # Errors
/// When the socket cannot be replaced, bound or given its mode.
pub async fn serve(config: Config) -> io::Result<()> {
    match std::fs::remove_file(&config.socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(&config.socket)?;
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o660))?;
    let active = Arc::new(Active::default());
    let persistent = Arc::new(Persistent::new(&config));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            // Running out of descriptors is temporary; keep serving.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        if !stream
            .peer_cred()
            .is_ok_and(|peer| peer.uid() == config.client_uid)
        {
            continue;
        }
        let config = config.clone();
        let active = Arc::clone(&active);
        let persistent = Arc::clone(&persistent);
        tokio::spawn(async move {
            // A failed write means the caller left; there is nobody to tell.
            let _ = handle(stream, &config, &active, &persistent).await;
        });
    }
}

/// Terminals, preview tunnels and private browsers outlive their calls: one registry of
/// each per worker.
struct Persistent {
    terminals: Arc<terminal::Terminals>,
    tunnels: tunnel::Tunnels,
    browsers: Arc<browser::Browsers>,
}

impl Persistent {
    fn new(config: &Config) -> Self {
        let terminals = Arc::new(terminal::Terminals::default());
        let registry = Arc::clone(&terminals);
        let claims = config.files.port_claims.clone();
        // A preview serves only one of the workspace's own ports that one of its running
        // terminals listens on.
        let validator: tunnel::Validator = Arc::new(move |base: PathBuf, port: u16| {
            let registry = Arc::clone(&registry);
            let claims = claims.clone();
            Box::pin(async move {
                let owned = {
                    let base = base.clone();
                    tokio::task::spawn_blocking(move || commands::network_ports(&claims, &base))
                        .await
                };
                if !matches!(owned, Ok(Ok(ports)) if ports.contains(&port)) {
                    return false;
                }
                let roots = registry.roots(&base).await;
                tokio::task::spawn_blocking(move || tunnel::terminal_listens(&roots, port))
                    .await
                    .unwrap_or(false)
            })
        });
        Self {
            terminals,
            tunnels: tunnel::Tunnels::new("/usr/local/bin/cloudflared".into(), validator),
            browsers: Arc::new(browser::Browsers::new(config.browser.clone())),
        }
    }
}

/// The workspace folder a control request names, when it exists. Run stop and resource
/// counts work while execution is disabled and never create a workspace.
fn existing_workspace(roots: &files::Roots, request: &Value) -> Option<PathBuf> {
    let session = request["workspace"].as_str()?;
    let parsed = uuid::Uuid::parse_str(session).ok()?;
    if parsed.hyphenated().to_string() != session {
        return None;
    }
    let path = std::fs::canonicalize(&roots.workspaces).ok()?.join(session);
    path.is_dir().then_some(path)
}

/// The commands running in each workspace, so a run stop can end them.
/// A running command's workspace and its stop signal.
type Running = (String, Arc<Notify>);

#[derive(Default)]
struct Active {
    /// The last call number handed out, and the running commands by number.
    calls: Mutex<(u64, HashMap<u64, Running>)>,
}

impl Active {
    fn start(&self, workspace: &str) -> (u64, Arc<Notify>) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        calls.0 += 1;
        let (id, stop) = (calls.0, Arc::new(Notify::new()));
        calls
            .1
            .insert(id, (workspace.to_owned(), Arc::clone(&stop)));
        (id, stop)
    }

    fn end(&self, id: u64) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        calls.1.remove(&id);
    }

    /// Stops every command in `workspace`; a stop sent before the command waits is kept.
    fn stop(&self, workspace: &str) {
        let calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        for (owner, stop) in calls.1.values() {
            if owner == workspace {
                stop.notify_one();
            }
        }
    }
}

/// Removes a command from [`Active`] however its call ends.
struct Registered<'a> {
    active: &'a Active,
    id: u64,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.active.end(self.id);
    }
}

async fn write_reply(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    id: &Value,
    result: Value,
) -> io::Result<()> {
    let mut line = json!({"version":2,"id":id,"result":result})
        .to_string()
        .into_bytes();
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.shutdown().await
}

/// Answers one call. A caller that hangs up (or sends more) before the reply stops its
/// command, plugin or browser call. A file call always completes, so a write is never
/// left half done. An unknown tool or a request outside protocol 2 fails unanswered by
/// any tool.
async fn handle(
    client: UnixStream,
    config: &Config,
    active: &Active,
    persistent: &Persistent,
) -> io::Result<()> {
    let (client_read, mut client_write) = client.into_split();
    let mut client_read = BufReader::new(client_read);
    let mut request = Vec::new();
    let problem = {
        let mut limited = (&mut client_read).take(REQUEST_LIMIT + 1);
        let read = limited.read_until(b'\n', &mut request);
        match tokio::time::timeout(REQUEST_TIMEOUT, read).await {
            Err(_) => Some(""),
            Ok(Err(_)) => Some("Request unreadable"),
            Ok(Ok(_)) if request.len() as u64 > REQUEST_LIMIT => Some("Request too large"),
            Ok(Ok(_)) => None,
        }
    };
    if let Some(problem) = problem {
        return reply(&mut client_write, None, problem).await;
    }
    let parsed = serde_json::from_slice::<Value>(&request)
        .ok()
        .filter(|request| request["version"] == 2);
    let name = parsed
        .as_ref()
        .and_then(|request| request["name"].as_str())
        .unwrap_or_default();
    if let Some(parsed) = parsed.as_ref().filter(|_| files::handles(name)) {
        let roots = config.files.clone();
        let request = parsed.clone();
        let result = tokio::task::spawn_blocking(move || files::call(&roots, &request))
            .await
            .unwrap_or_else(|_| json!({"status":"failed","output":"File tool failed"}));
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| git::handles(name)) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        let result = tokio::select! {
            result = git_call(&config.files, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            // A call its workspace's run stop cancelled.
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| {
        matches!(
            name,
            "administrator_file_manager" | "administrator_file_git"
        )
    }) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        let result = tokio::select! {
            result = file_manager_call(&config.files, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| commands::handles(name)) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        // Dropping the call (hang-up or run stop) ends the command's session.
        let result = tokio::select! {
            result = commands::call(&config.files, &config.runner, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            // A call its workspace's run stop cancelled.
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| name == plugins::TOOL) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        let result = tokio::select! {
            result = plugin_call(config, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed
        .as_ref()
        .filter(|_| terminal::handles(name) || tunnel::handles(name))
    {
        // Bounded calls (a wait lasts at most 30 s); an open is never abandoned halfway.
        let result = persistent_call(config, persistent, parsed).await;
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| browser::handles(name)) {
        if name == "browser_protocol" {
            let output = json!({"protocol":1,"interface":1,"stateful":true});
            let result = protocol(&config.files, output).await;
            return write_reply(&mut client_write, &parsed["id"], result).await;
        }
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        // A call that ends early closes the session's browser; a screenshot read holds no
        // browser.
        let cancel = || async {
            if name != "administrator_artifact_get" {
                persistent.browsers.close(workspace).await;
            }
        };
        let result = tokio::select! {
            result = browser_call(config, persistent, parsed) => result,
            () = hang_up(&mut client_read) => {
                cancel().await;
                return Ok(());
            }
            () = stop.notified() => {
                cancel().await;
                json!({"status":"failed","output":"Session not found"})
            }
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed
        .as_ref()
        .filter(|_| name == "administrator_resources")
    {
        // Counts the resources that survive a conversation stop.
        let (terminals, tunnels) = match existing_workspace(&config.files, parsed) {
            Some(base) => (
                persistent.terminals.count(&base),
                persistent.tunnels.count(&base),
            ),
            None => (0, 0),
        };
        let result = json!({"status":"success","output":{"terminals":terminals,"tunnels":tunnels}});
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed
        .as_ref()
        .filter(|_| name == "administrator_browser_purge")
    {
        // The chat session was deleted: its private browser folder goes with it.
        if let Some(sid) = parsed["workspace"].as_str() {
            persistent.browsers.purge(sid).await;
        }
        let result = json!({"status":"success"});
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed
        .as_ref()
        .filter(|_| name == "administrator_browser_sweep")
    {
        // Every chat session that still exists, from the API at startup; other folders
        // are orphans of deletions made before folders were deleted with their session.
        let keep: Option<Vec<String>> = parsed["arguments"]["keep"].as_array().map(|keep| {
            keep.iter()
                .filter_map(|sid| sid.as_str().map(str::to_owned))
                .collect()
        });
        let result = match keep {
            Some(keep) => {
                json!({"status":"success","output":{"removed":persistent.browsers.sweep(&keep).await}})
            }
            None => json!({"status":"error","output":"keep list required"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed
        .as_ref()
        .filter(|_| name == "ability_plugins_protocol")
    {
        // The ability plugin contract the broker checks before a hot swap.
        let output = json!({"protocol":1,"catalog":1,"dispatch":1});
        let result = protocol(&config.files, output).await;
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| name == "run_stop") {
        // Stops the workspace's commands, terminals, tunnels and browsers.
        if let Some(workspace) = parsed["workspace"].as_str() {
            active.stop(workspace);
        }
        if let Some(base) = existing_workspace(&config.files, parsed) {
            persistent.terminals.close_all(&base).await;
            persistent.tunnels.close_workspace(&base).await;
            if let Some(sid) = base.file_name().and_then(|name| name.to_str()) {
                persistent.browsers.close(sid).await;
            }
        }
        return write_reply(
            &mut client_write,
            &parsed["id"],
            json!({"status":"success"}),
        )
        .await;
    }
    let id = serde_json::from_slice::<Value>(&request)
        .ok()
        .and_then(|value| value.get("id").cloned());
    let problem = if parsed.is_some() {
        "Unknown tool"
    } else {
        "Worker RPC protocol 2 required; no operation dispatched"
    };
    reply(&mut client_write, id, problem).await
}

/// Admits and runs one Git Control request or repository watch.
async fn git_call(roots: &files::Roots, request: &Value) -> Value {
    let admitted = {
        let roots = roots.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, _, _)| base)
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    let base = match admitted {
        Ok(Ok(base)) => base,
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"Git admission failed"}),
    };
    if request["name"] == "administrator_git_watch" {
        return git::watch(&base).await;
    }
    let name = request["name"].as_str().unwrap_or_default();
    let empty = json!({});
    let args = request.get("arguments").unwrap_or(&empty);
    git::execute(
        &base,
        name,
        args,
        request.get("trusted").unwrap_or(&Value::Null),
    )
    .await
}

/// Admits and runs one administrator File Manager request on the live checkout. The host
/// admits only an administrator; the worker confines every path to the checkout.
async fn file_manager_call(roots: &files::Roots, request: &Value) -> Value {
    let admitted = {
        let roots = roots.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|_| ())
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    match admitted {
        Ok(Ok(())) => {}
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"File Manager admission failed"}),
    }
    let empty = json!({});
    let args = request.get("arguments").unwrap_or(&empty);
    let root = roots.live_checkout.clone();
    if request["name"] == "administrator_file_git" {
        return git::manager::action(&root, args, request.get("trusted").unwrap_or(&Value::Null))
            .await;
    }
    let args = args.clone();
    tokio::task::spawn_blocking(move || manager::execute(&root, &args))
        .await
        .unwrap_or_else(|_| json!({"status":"error","output":"File Manager operation failed"}))
}

/// A contract the broker checks before a hot swap, `output`, behind the same shutoff
/// switch as every call.
async fn protocol(roots: &files::Roots, output: Value) -> Value {
    let control = roots.control.clone();
    let admitted = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        let lock = std::fs::File::open(control.join("dispatch.lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(std::fs::read_to_string(control.join("shutoff"))?.trim() == "0")
    })
    .await;
    match admitted {
        Ok(Ok(true)) => json!({"status":"success","output":output}),
        Ok(Ok(false)) => json!({"status":"failed","output":"execution disabled"}),
        Ok(Err(error)) => json!({"status":"failed","output":error.to_string()}),
        Err(_) => json!({"status":"failed","output":"Protocol admission failed"}),
    }
}

/// Admits and runs one browser request in the session's private browser.
async fn browser_call(config: &Config, persistent: &Persistent, request: &Value) -> Value {
    let admitted = {
        let roots = config.files.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, _, _)| base)
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    let base = match admitted {
        Ok(Ok(base)) => base,
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"Browser admission failed"}),
    };
    let name = request["name"].as_str().unwrap_or_default();
    let empty = json!({});
    let args = request.get("arguments").unwrap_or(&empty);
    let Some(supervisor) = config.runner.first() else {
        return json!({"status":"failed","output":"Browser supervisor not configured"});
    };
    persistent
        .browsers
        .call(
            supervisor,
            name,
            args,
            &base,
            request.get("trusted").unwrap_or(&Value::Null),
        )
        .await
}

/// Admits and runs one terminal or preview tunnel request.
async fn persistent_call(config: &Config, persistent: &Persistent, request: &Value) -> Value {
    let admitted = {
        let roots = config.files.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, _, _)| base)
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    let base = match admitted {
        Ok(Ok(base)) => base,
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"Terminal admission failed"}),
    };
    let name = request["name"].as_str().unwrap_or_default();
    let empty = json!({});
    let args = request.get("arguments").unwrap_or(&empty);
    if tunnel::handles(name) {
        return persistent.tunnels.execute(name, args, &base).await;
    }
    let ports = {
        let claims = config.files.port_claims.clone();
        let base = base.clone();
        tokio::task::spawn_blocking(move || commands::network_ports(&claims, &base)).await
    };
    let supervisor = config
        .runner
        .first()
        .cloned()
        .into_iter()
        .chain(["--terminal-supervisor".to_owned()])
        .collect();
    let launch = move || match ports {
        Ok(Ok(ports)) => Ok(terminal::Launch { supervisor, ports }),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("Port claim failed".to_owned()),
    };
    match persistent
        .terminals
        .execute(name, args, &base, launch)
        .await
    {
        Ok(output) => json!({"status":"success","output":output}),
        Err(output) => json!({"status":"failed","output":output}),
    }
}

/// Admits and runs one `ability_plugin_invoke` request.
async fn plugin_call(config: &Config, request: &Value) -> Value {
    let binding = &request["trusted"]["ability_plugin"];
    if !binding.is_object() {
        return json!({"status":"failed","output":"Trusted plugin binding required"});
    }
    let admitted = {
        let roots = config.files.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, _, _)| base)
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    let base = match admitted {
        Ok(Ok(base)) => base,
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"Plugin admission failed"}),
    };
    let args = request
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    plugins::invoke(
        &config.packages,
        &config.plugin_runner,
        binding,
        &args,
        &base,
    )
    .await
    .unwrap_or_else(|refused| json!({"status":"failed","output":refused.to_string()}))
}

/// Completes when the caller closes its side or sends anything after its request.
async fn hang_up(reader: &mut (impl AsyncRead + Unpin)) {
    let mut byte = [0_u8; 1];
    let _ = reader.read(&mut byte).await;
}

/// Writes the worker's failure envelope for a call that never reached a tool.
async fn reply(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    id: Option<Value>,
    output: &str,
) -> io::Result<()> {
    let envelope = json!({"version":2,"id":id,"result":{"status":"failed","output":output}});
    let mut line = envelope.to_string().into_bytes();
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    struct Fixture {
        _dir: tempfile::TempDir,
        socket: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("temporary directory");
        let socket = dir.path().join("tools.sock");
        let control = dir.path().join("control");
        std::fs::create_dir_all(&control).expect("control");
        std::fs::write(control.join("dispatch.lock"), "").expect("lock");
        std::fs::write(control.join("shutoff"), "0").expect("shutoff");
        std::fs::create_dir_all(dir.path().join("workspaces")).expect("workspaces");
        let config = Config {
            files: files::Roots {
                workspaces: dir.path().join("workspaces"),
                live_checkout: dir.path().join("absent-checkout"),
                control,
                context_config: dir.path().join("absent-context.json"),
                port_claims: dir.path().join("port-claims"),
            },
            runner: vec!["/nonexistent-runner".into()],
            plugin_runner: vec!["/nonexistent-runner".into()],
            packages: plugins::Packages::container(),
            browser: browser::Settings {
                root: dir.path().join("browser"),
                origin: "http://127.0.0.1:9".into(),
                host: None,
                sandbox: false,
            },
            socket: socket.clone(),
            // The test process owns the directory it just created.
            client_uid: std::fs::metadata(dir.path()).expect("directory").uid(),
        };
        tokio::spawn(serve(config));
        Fixture { _dir: dir, socket }
    }

    async fn connect(path: &Path) -> UnixStream {
        for _ in 0..100 {
            if let Ok(stream) = UnixStream::connect(path).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("worker socket never appeared");
    }

    async fn read_line(stream: &mut UnixStream) -> Value {
        let mut line = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut line)
            .await
            .expect("reply");
        serde_json::from_slice(&line).expect("reply is JSON")
    }

    #[tokio::test]
    async fn a_file_tool_runs_in_the_worker_without_the_python_worker() {
        let fixture = fixture();
        let mut client = connect(&fixture.socket).await;
        let request = json!({"version":2,"id":"n","name":"file_list",
            "workspace":"2244991a-c2e3-415b-ae5c-054570c91d32","arguments":{"path":"."}});
        client
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("request");
        let reply = read_line(&mut client).await;
        assert_eq!(reply["id"], "n");
        assert_eq!(reply["result"]["status"], "success", "{reply}");
        assert_eq!(reply["result"]["implementation_version"], "2.0.0");
    }

    async fn call(socket: &Path, request: Value) -> Value {
        let mut client = connect(socket).await;
        client
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("request");
        read_line(&mut client).await
    }

    #[tokio::test]
    async fn an_unknown_tool_fails_with_its_id() {
        let fixture = fixture();
        let reply = call(
            &fixture.socket,
            json!({"version":2,"id":"b","name":"retired_tool"}),
        )
        .await;
        assert_eq!(reply["id"], "b");
        assert_eq!(
            reply["result"],
            json!({"status":"failed","output":"Unknown tool"})
        );
        let old = call(&fixture.socket, json!({"id":"c","name":"file_list"})).await;
        assert_eq!(old["id"], "c");
        assert_eq!(old["result"]["status"], "failed");
    }

    #[tokio::test]
    async fn the_ability_plugin_protocol_is_answered_behind_the_shutoff_switch() {
        let fixture = fixture();
        let request = json!({"version":2,"id":"p","name":"ability_plugins_protocol",
            "workspace":"2244991a-c2e3-415b-ae5c-054570c91d32","arguments":{}});
        let reply = call(&fixture.socket, request.clone()).await;
        assert_eq!(reply["id"], "p");
        assert_eq!(
            reply["result"],
            json!({"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}})
        );
        let control = fixture.socket.parent().expect("root").join("control");
        std::fs::write(control.join("shutoff"), "1").expect("shutoff");
        let disabled = call(&fixture.socket, request).await;
        assert_eq!(
            disabled["result"],
            json!({"status":"failed","output":"execution disabled"})
        );
    }

    #[tokio::test]
    async fn run_stop_succeeds_for_present_and_absent_workspaces() {
        let fixture = fixture();
        let workspace = "2244991a-c2e3-415b-ae5c-054570c91d32";
        let workspaces = fixture.socket.parent().expect("root").join("workspaces");
        for present in [false, true] {
            if present {
                std::fs::create_dir_all(workspaces.join(workspace)).expect("workspace");
            }
            let reply = call(
                &fixture.socket,
                json!({"version":2,"id":"q","name":"run_stop","workspace":workspace}),
            )
            .await;
            assert_eq!(reply["id"], "q");
            assert_eq!(reply["result"], json!({"status":"success"}));
        }
    }
}
