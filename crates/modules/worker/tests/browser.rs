//! Browser Control through a running worker and its session processes (this test
//! executable in `--browser-session` mode). The journey against a real Chromium is skipped
//! where `/usr/bin/chromedriver` is absent.
mod harness;
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use webagent_worker::{browser::Settings, files::Roots, serve, Config};

const SESSION: &str = "4f0b7c2e-9d1a-4c3b-8e5f-6a7b8c9d0e1f";

struct Worker {
    _dir: tempfile::TempDir,
    socket: PathBuf,
}

async fn worker(origin: &str) -> Worker {
    let dir = tempfile::tempdir().expect("temporary directory");
    let control = dir.path().join("control");
    std::fs::create_dir_all(&control).expect("control");
    std::fs::write(control.join("dispatch.lock"), "").expect("lock");
    std::fs::write(control.join("shutoff"), "0").expect("shutoff");
    std::fs::create_dir_all(dir.path().join("workspaces")).expect("workspaces");
    let socket = dir.path().join("tools.sock");
    let config = Config {
        socket: socket.clone(),
        client_uid: std::fs::metadata(dir.path()).expect("directory").uid(),
        files: Roots {
            workspaces: dir.path().join("workspaces"),
            live_checkout: dir.path().join("absent-checkout"),
            control,
            context_config: dir.path().join("absent-context.json"),
            port_claims: dir.path().join("port-claims"),
        },
        runner: harness::runner(),
        plugin_runner: harness::plugin_runner(),
        packages: webagent_worker::plugins::Packages::container(),
        browser: Settings {
            root: dir.path().join("browser"),
            origin: origin.to_owned(),
        },
    };
    tokio::spawn(serve(config));
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Worker { _dir: dir, socket }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

fn signed_in(cookie: &str) -> Value {
    json!({"session_id":SESSION,"user_id":"admin","agent_id":"shared_default",
        "cookie":cookie,"generation":"fixture","expires_at":now() + 300.0,
        "allowed_session_ids":[SESSION]})
}

impl Worker {
    async fn call(&self, name: &str, arguments: Value, trusted: &Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).await.expect("connect");
        let request = json!({"version":2,"id":"b","name":name,"workspace":SESSION,
            "arguments":arguments,"trusted":trusted});
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("request");
        let mut line = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut line)
            .await
            .expect("reply");
        let reply: Value = serde_json::from_slice(&line).expect("reply is JSON");
        assert_eq!(reply["id"], "b");
        reply["result"].clone()
    }
}

fn chromium() -> bool {
    let present = std::path::Path::new("/usr/bin/chromedriver").exists();
    if !present {
        eprintln!("skipped: /usr/bin/chromedriver is absent");
    }
    present
}

async fn the_protocol_and_trusted_session_checks_answer_without_a_browser() {
    let worker = worker("http://127.0.0.1:9").await;
    let protocol = worker
        .call("browser_protocol", json!({}), &Value::Null)
        .await;
    assert_eq!(
        protocol,
        json!({"status":"success","output":{"protocol":1,"interface":1,"stateful":true}})
    );
    let mut other = signed_in("9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d");
    other["allowed_session_ids"] = json!([]);
    let refused = worker
        .call("browser_read", json!({"action":"read"}), &other)
        .await;
    assert_eq!(
        refused["output"], "Trusted browser session required",
        "{refused}"
    );
    let mut expired = signed_in("9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d");
    expired["expires_at"] = json!(now() - 1.0);
    let refused = worker
        .call("browser_read", json!({"action":"read"}), &expired)
        .await;
    assert_eq!(refused["output"], "Browser authorization expired");
    let closed = worker
        .call(
            "browser_read",
            json!({"action":"close"}),
            &signed_in("9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d"),
        )
        .await;
    assert_eq!(closed, json!({"status":"success","closed":true}));
    let missing = worker
        .call(
            "administrator_artifact_get",
            json!({"artifact_id":"2244991a-c2e3-415b-ae5c-054570c91d32"}),
            &signed_in("9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d"),
        )
        .await;
    assert_eq!(
        (
            missing["status"].clone(),
            missing["uncertain"].clone(),
            missing["error_kind"].clone()
        ),
        (
            json!("error"),
            json!(false),
            json!("browser_transport_failed")
        ),
        "{missing}"
    );
}

async fn a_session_process_answers_with_the_browser_result() {
    // Nothing listens on the origin: without Chromium the browser cannot start; with it,
    // the signed-in browser cannot prove its identity. Either way the session process
    // answered through the frame protocol.
    let worker = worker("http://127.0.0.1:9").await;
    let result = worker
        .call(
            "browser_read",
            json!({"action":"navigate","url":"/app"}),
            &signed_in("9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d"),
        )
        .await;
    assert_eq!(result["status"], "error", "{result}");
    assert!(
        result["error_kind"] == "browser_startup_failed"
            || result["error_kind"] == "browser_authentication_failed",
        "{result}"
    );
    assert_eq!(result["implementation_version"], "3.0.0");
    assert!(result.get("plugin_revision").is_some());
}

/// A tiny app: the identity route honours only the session cookie it was given.
async fn fixture_app(cookie: String) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let cookie = cookie.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(read) if read > 0 => head.extend_from_slice(&chunk[..read]),
                        _ => return,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let authenticated = head
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case(&format!("cookie: v3_session={cookie}")));
                let (media, body) = if path == "/api/v3/auth/me" {
                    let user = if authenticated {
                        "admin"
                    } else {
                        "unauthenticated"
                    };
                    ("application/json", json!({"user_id":user}).to_string())
                } else if path.starts_with("/app") {
                    (
                        "text/html",
                        concat!(
                            "<html><body><script>window.__boot={revealed:true}</script>",
                            "<p id=\"text\">Fixture ready</p><button id=\"go\" onclick=\"",
                            "document.getElementById('text').textContent='Clicked'\">Go</button>",
                            "<input id=\"password\" type=\"password\" value=\"PRIVATE_VALUE\">",
                            "</body></html>"
                        )
                        .to_owned(),
                    )
                } else {
                    ("text/plain", "ready".to_owned())
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    port
}

async fn a_real_browser_signs_in_reads_clicks_and_captures() {
    if !chromium() {
        return;
    }
    let cookie = uuid::Uuid::new_v4().to_string();
    let port = fixture_app(cookie.clone()).await;
    let worker = worker(&format!("http://127.0.0.1:{port}")).await;
    let trusted = signed_in(&cookie);
    let login = worker
        .call(
            "browser_read",
            json!({"action":"navigate","url":"/app"}),
            &trusted,
        )
        .await;
    assert_eq!(login["status"], "success", "{login}");
    assert!(!login.to_string().contains(&cookie));
    let read = worker
        .call("browser_read", json!({"action":"read"}), &trusted)
        .await;
    assert_eq!(read["status"], "success", "{read}");
    let text = read["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("Fixture ready") && !text.contains("PRIVATE_VALUE"),
        "{read}"
    );
    let clicked = worker
        .call(
            "browser_input",
            json!({"action":"click","selector":"#go"}),
            &trusted,
        )
        .await;
    assert_eq!(clicked["status"], "success", "{clicked}");
    let read = worker
        .call("browser_read", json!({"action":"read"}), &trusted)
        .await;
    assert!(
        read["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Clicked"),
        "{read}"
    );
    let shot = worker
        .call("browser_read", json!({"action":"screenshot"}), &trusted)
        .await;
    assert_eq!(shot["status"], "success", "{shot}");
    let image = worker
        .call(
            "administrator_artifact_get",
            json!({"artifact_id":shot["artifact_id"]}),
            &trusted,
        )
        .await;
    assert_eq!(image["status"], "success", "{image}");
    assert!(image["png_base64"]
        .as_str()
        .unwrap_or_default()
        .starts_with("iVBORw0KGgo"));
    let closed = worker
        .call("browser_read", json!({"action":"close"}), &trusted)
        .await;
    assert_eq!(closed["closed"], true, "{closed}");
}

fn main() {
    harness::main(&[
        (
            "the_protocol_and_trusted_session_checks_answer_without_a_browser",
            || Box::pin(the_protocol_and_trusted_session_checks_answer_without_a_browser()),
        ),
        ("a_session_process_answers_with_the_browser_result", || {
            Box::pin(a_session_process_answers_with_the_browser_result())
        }),
        ("a_real_browser_signs_in_reads_clicks_and_captures", || {
            Box::pin(a_real_browser_signs_in_reads_clicks_and_captures())
        }),
    ]);
}
