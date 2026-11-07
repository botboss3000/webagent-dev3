//! The per-session browser process, `webagent-v3 --browser-session`, ported from the
//! Python `runner.py`. It reads one line-framed JSON request at a time on stdin and writes
//! one reply line on stdout, keeping the session's Chromium, observations and recorder
//! alive between calls. It serves one owner: a frame with a different authority, an
//! expired lease, three idle minutes, a `close`, SIGTERM or the worker's death ends it,
//! and its browser is closed and every descendant ended on the way out.
use super::rules::validate_trusted;
use super::service::{self, Limits, Service};
use super::webdriver::{Chrome, Factory};
use super::{now, Settings, IMPLEMENTATION_VERSION};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Largest request or reply line.
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
const TOOLS: [&str; 5] = [
    "browser_read",
    "browser_input",
    "browser_record",
    "administrator_artifact_get",
    "close",
];
const IDLE: Duration = Duration::from_secs(180);

static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_signal: libc::c_int) {
    STOPPING.store(true, Ordering::SeqCst);
}

/// The production driver factory: chromedriver and Chromium from the image.
#[must_use]
pub fn chrome() -> Factory {
    Arc::new(|profile, origin, launch| Box::pin(Chrome::start(profile, origin, launch)))
}

/// Runs the session process on stdin and stdout and returns its exit code: 0 when it
/// ended normally, 1 on a malformed or refused frame.
#[must_use]
pub fn run() -> i32 {
    // SAFETY: prctl and sigaction with plain integers and a static handler; this process is
    // single-threaded here (dispatched before any runtime).
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0 {
            return 1;
        }
        // Chromium processes chromedriver leaves behind reparent here and are ended here.
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as extern "C" fn(libc::c_int) as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return 1;
    };
    let service = Service::new(&Settings::from_env(), chrome(), Limits::default());
    let code = runtime.block_on(serve(tokio::io::stdin(), tokio::io::stdout(), service));
    // The stdin reader may still block on its thread; do not wait for it.
    runtime.shutdown_background();
    end_descendants();
    code
}

/// SIGKILLs and reaps every remaining descendant.
fn end_descendants() {
    for _ in 0..50 {
        let children = crate::runner::descendants();
        if children.is_empty() {
            return;
        }
        for pid in children {
            // SAFETY: kill takes plain integers; a vanished pid is ignored.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        // SAFETY: reaping this process's own children.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One frame failed in a way that ends the process with exit code 1.
struct Broken;

impl From<super::Fault> for Broken {
    fn from(_: super::Fault) -> Self {
        Self
    }
}

/// Serves frames from `input` to `output` until the session ends; returns the exit code.
pub async fn serve(
    input: impl AsyncRead + Unpin + Send + 'static,
    mut output: impl AsyncWrite + Unpin,
    service: Arc<Service>,
) -> i32 {
    // Lines are read ahead on their own task, so the idle check never cancels a read.
    let (lines, mut incoming) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    tokio::spawn(async move {
        let mut reader = BufReader::new(input);
        loop {
            let mut line = Vec::new();
            let read = (&mut reader)
                .take(MAX_FRAME as u64 + 1)
                .read_until(b'\n', &mut line)
                .await;
            let last = !matches!(read, Ok(read) if read > 0) || !line.ends_with(b"\n");
            if !line.is_empty() && lines.send(line).await.is_err() {
                return;
            }
            if last {
                return;
            }
        }
    });
    let mut owner: Option<(String, String, String, String, Option<String>)> = None;
    let mut base: Option<PathBuf> = None;
    let mut expires = 0.0;
    let mut touched = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let code = loop {
        tokio::select! {
            raw = incoming.recv() => {
                let Some(raw) = raw else { break 0 };
                if raw.len() > MAX_FRAME || !raw.ends_with(b"\n") {
                    break 0;
                }
                let Ok(request) = serde_json::from_slice::<Value>(&raw) else { break 1 };
                let Some(frame_base) = request["base"].as_str().map(PathBuf::from) else {
                    break 1;
                };
                let Ok(trusted) = validate_trusted(&frame_base, &request["trusted"]) else {
                    break 1;
                };
                let identity = trusted.identity();
                if owner.as_ref().is_some_and(|owner| *owner != identity) {
                    break 0;
                }
                owner = Some(identity);
                base = Some(frame_base.clone());
                expires = trusted.expires_at;
                touched = Instant::now();
                let Ok(reply) = exchange(&service, &request, &frame_base).await else {
                    break 1;
                };
                let mut data = reply.to_string().into_bytes();
                data.push(b'\n');
                if data.len() > MAX_FRAME {
                    break 0;
                }
                if output.write_all(&data).await.is_err() || output.flush().await.is_err() {
                    break 1;
                }
                if request["tool"] == "close" {
                    break 0;
                }
            }
            _ = tick.tick() => {
                if STOPPING.load(Ordering::SeqCst)
                    || (base.is_some() && (now() >= expires || touched.elapsed() > IDLE))
                {
                    break 0;
                }
            }
        }
    };
    if let Some(base) = base {
        service.close_all(&base).await;
    }
    code
}

/// Checks the transport fields, then runs the tool; the browser validates the caller and
/// session itself.
async fn exchange(service: &Arc<Service>, request: &Value, base: &Path) -> Result<Value, Broken> {
    let tool = request["tool"].as_str().unwrap_or_default();
    if request["protocol"].as_u64() != Some(1) || !TOOLS.contains(&tool) {
        return Err(Broken);
    }
    let invocation = request["invocation_id"]
        .as_str()
        .filter(|id| (1..=80).contains(&id.chars().count()))
        .ok_or(Broken)?;
    let raw_trusted = &request["trusted"];
    validate_trusted(base, raw_trusted)?;
    let empty = json!({});
    let args = request.get("arguments").unwrap_or(&empty);
    let result = match tool {
        "administrator_artifact_get" => {
            let artifact = args.get("artifact_id").ok_or(Broken)?;
            let content = service::read_artifact(
                service.root(),
                service.limits(),
                base,
                raw_trusted,
                artifact,
            )?;
            json!({"status":"success",
                "png_base64":super::base64::encode(&content)})
        }
        "close" => {
            service.close_all(base).await;
            json!({"status":"success","closed":true})
        }
        "browser_record" => service.record(args, base, raw_trusted).await,
        _ => service.call(tool, args, base, raw_trusted).await,
    };
    Ok(
        json!({"protocol":1,"invocation_id":invocation,"result":result,
        "implementation_version":IMPLEMENTATION_VERSION}),
    )
}
