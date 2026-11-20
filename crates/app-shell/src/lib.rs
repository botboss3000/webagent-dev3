//! The installed WebAgent app: unpack the bundled app files, start the server as a
//! companion process on a free loopback port, show it in the window, and stop it when
//! the app exits.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{Manager, RunEvent};

/// `app/` (UI, abilities, configuration) and `checkout/` (the editable settings files),
/// written by package.py.
static BUNDLE: &[u8] = include_bytes!("../bundle/app-bundle.zip");

/// How long a first start (database creation, catalog load) may take.
const START_LIMIT: Duration = Duration::from_secs(180);
/// How long the server may drain its runs after the window closes.
const STOP_LIMIT: Duration = Duration::from_secs(20);

struct Server(Mutex<Option<Child>>);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Server(Mutex::new(None)))
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let Some(window) = handle.get_webview_window("main") else {
                    return;
                };
                match start(&data) {
                    Ok((child, url)) => {
                        *handle
                            .state::<Server>()
                            .0
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(child);
                        let _ = window.navigate(url);
                    }
                    Err(error) => {
                        let text =
                            serde_json::to_string(&format!("WebAgent could not start: {error}"))
                                .unwrap_or_default();
                        let _ = window.eval(&format!(
                            "document.getElementById('status').textContent={text}"
                        ));
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("WebAgent window")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                let child = app
                    .state::<Server>()
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(child) = child {
                    stop(child);
                }
            }
        });
}

/// Unpack the app files, start the server and wait until it answers readiness.
fn start(data: &Path) -> Result<(Child, url::Url), String> {
    let text = |error: std::io::Error| error.to_string();
    std::fs::create_dir_all(data).map_err(text)?;
    unpack(data).map_err(text)?;
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map_err(text)?
        .port();
    let log = std::fs::File::create(data.join("server.log")).map_err(text)?;
    for folder in ["state", "runtime", "workspaces", "tmp"] {
        std::fs::create_dir_all(data.join(folder)).map_err(text)?;
    }
    let mut command = Command::new(server_binary().map_err(text)?);
    command
        .current_dir(data)
        .env("V3_PROCESS_ROLE", "desktop")
        .env("V3_STOP_ON_STDIN_EOF", "1")
        .env("V3_LISTEN", format!("127.0.0.1:{port}"))
        .env("V3_SECURE_COOKIE", "false")
        .env("V3_STATE_ROOT", data.join("state"))
        .env("V3_RUNTIME_ROOT", data.join("runtime"))
        .env("V3_WORKSPACE_ROOT", data.join("workspaces"))
        .env("V3_APP_ROOT", data.join("app"))
        .env("V3_ABILITIES_ROOT", data.join("app").join("abilities"))
        .env("V3_CHECKOUT_ROOT", data.join("checkout"))
        .env("TMPDIR", data.join("tmp"))
        .env("TEMP", data.join("tmp"))
        .env("TMP", data.join("tmp"))
        .stdin(Stdio::piped())
        .stdout(log.try_clone().map_err(text)?)
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(text)?;
    let started = Instant::now();
    while !ready(port) {
        if let Ok(Some(status)) = child.try_wait() {
            let log = std::fs::read_to_string(data.join("server.log")).unwrap_or_default();
            let tail: String = log.lines().rev().take(5).collect::<Vec<_>>().join(" | ");
            return Err(format!("the server stopped ({status}): {tail}"));
        }
        if started.elapsed() > START_LIMIT {
            stop(child);
            return Err("the server did not become ready".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let url =
        url::Url::parse(&format!("http://127.0.0.1:{port}/app")).map_err(|e| e.to_string())?;
    Ok((child, url))
}

/// Whether `/health/ready` answers 200.
fn ready(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(b"GET /health/ready HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut head = [0u8; 16];
    stream.read(&mut head).is_ok_and(|n| {
        head[..n].starts_with(b"HTTP/1.1 200") || head[..n].starts_with(b"HTTP/1.0 200")
    })
}

/// Closing standard input asks the server to drain and stop; it is killed after
/// [`STOP_LIMIT`].
fn stop(mut child: Child) {
    drop(child.stdin.take());
    let started = Instant::now();
    while started.elapsed() < STOP_LIMIT {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `app/` is replaced whenever the bundle changes. `checkout/` holds the editable files:
/// an upgrade replaces one only while it still equals the version last shipped (kept in
/// `.shipped/`), so files the user changed survive and unchanged ones get the new version.
fn unpack(data: &Path) -> std::io::Result<()> {
    let marker = data.join("app").join(".bundle-id");
    let id = format!("{:016x}-{}", fnv(BUNDLE), BUNDLE.len());
    let fresh = std::fs::read_to_string(&marker).is_ok_and(|current| current == id);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(BUNDLE))
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if fresh {
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(data.join("app"));
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let target = data.join(&relative);
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        if relative.starts_with("checkout") {
            let shipped = data.join(".shipped").join(&relative);
            let current = std::fs::read(&target).ok();
            let unchanged = current.is_none() || current == std::fs::read(&shipped).ok();
            if let Some(parent) = shipped.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&shipped, &bytes)?;
            if !unchanged {
                continue;
            }
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }
    std::fs::create_dir_all(data.join("app"))?;
    std::fs::write(marker, id)?;
    Ok(())
}

/// FNV-1a: tells bundle versions apart; not a security check.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The server executable: beside the app on desktops (Tauri's `externalBin`); on Android
/// the APK's native-library folder, the one app folder Android lets an app execute from.
fn server_binary() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "android")]
    {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: dladdr only reads the address of a function in this library and fills
        // `info`; the returned name stays valid while the library is loaded.
        let found = unsafe { libc::dladdr(server_binary as *const libc::c_void, &mut info) };
        if found == 0 || info.dli_fname.is_null() {
            return Err(std::io::Error::other("native library folder unknown"));
        }
        let library = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
        let library = PathBuf::from(library.to_string_lossy().into_owned());
        Ok(library
            .parent()
            .unwrap_or(Path::new("/"))
            .join("libwebagent_server.so"))
    }
    #[cfg(not(target_os = "android"))]
    {
        let exe = std::env::current_exe()?;
        Ok(exe
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("webagent-server{}", std::env::consts::EXE_SUFFIX)))
    }
}
