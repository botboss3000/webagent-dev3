//! Chromium through chromedriver's W3C WebDriver HTTP JSON API, with a small HTTP/1.1
//! client over a loopback TCP connection (one connection per command; chromedriver runs
//! on this machine). [`Driver`] is the one primitive the browser service uses, so tests
//! can stand in a scripted driver; the helpers below are the Selenium calls the Python
//! adapter made, each mapped to the WebDriver endpoint Selenium sends.
use super::url::urlsplit;
use super::{Fault, PAGE_TIMEOUT_MS, SCRIPT_TIMEOUT_MS};
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A boxed driver future.
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T, Fault>> + Send + 'a>>;

/// The W3C key a web element reference is sent and returned under.
const ELEMENT: &str = "element-6066-11e4-a52e-4f735466cecf";
/// Largest WebDriver response read; a full-page PNG in base64 is far smaller.
const MAX_RESPONSE: usize = 32 * 1024 * 1024;
const CHROMEDRIVER: &str = "/usr/bin/chromedriver";
const CHROMIUM: &str = "/usr/bin/chromium";

/// One browser session behind WebDriver.
pub trait Driver: Send + Sync {
    /// Runs one session command: `method` on `/session/{id}/{path}` with a JSON `body`
    /// (`Null` sends none) and returns the response's `value`.
    fn command<'a>(&'a self, method: &'static str, path: &'a str, body: Value) -> Fut<'a, Value>;
    /// The chromedriver process id, recorded so a later start can reap it after a crash.
    fn pid(&self) -> Option<i32>;
    /// Ends the session and its processes; never fails.
    fn quit(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// How a browser is launched: signed out through the egress guard on `proxy`, or with no
/// network beyond loopback when `offline`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Launch {
    pub offline: bool,
    pub proxy: Option<u16>,
}

/// Starts a driver: the profile folder, the signed-in origin (`None` signed out) and how.
pub type Factory =
    Arc<dyn Fn(PathBuf, Option<String>, Launch) -> Fut<'static, Arc<dyn Driver>> + Send + Sync>;

/// A web element reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element(pub String);

impl Element {
    fn of(value: &Value) -> Result<Self, Fault> {
        value[ELEMENT]
            .as_str()
            .map(|id| Self(id.to_owned()))
            .ok_or_else(|| Fault::backend("WebDriverException"))
    }

    /// The reference as a script argument.
    #[must_use]
    pub fn reference(&self) -> Value {
        json!({ELEMENT: self.0})
    }
}

/// `driver.get(url)`: `POST /url`.
pub async fn get(driver: &dyn Driver, url: &str) -> Result<(), Fault> {
    driver
        .command("POST", "url", json!({"url":url}))
        .await
        .map(drop)
}

/// `driver.current_url`: `GET /url`.
pub async fn current_url(driver: &dyn Driver) -> Result<String, Fault> {
    let value = driver.command("GET", "url", Value::Null).await?;
    Ok(value.as_str().unwrap_or_default().to_owned())
}

/// `driver.execute_script(script, *args)`: `POST /execute/sync`.
pub async fn execute(driver: &dyn Driver, script: &str, args: Vec<Value>) -> Result<Value, Fault> {
    driver
        .command("POST", "execute/sync", json!({"script":script,"args":args}))
        .await
}

/// `driver.execute_cdp_cmd(cmd, params)`: chromedriver's `POST /goog/cdp/execute`.
pub async fn cdp(driver: &dyn Driver, cmd: &str, params: Value) -> Result<Value, Fault> {
    driver
        .command(
            "POST",
            "goog/cdp/execute",
            json!({"cmd":cmd,"params":params}),
        )
        .await
}

/// `driver.add_cookie(cookie)`: `POST /cookie`.
pub async fn add_cookie(driver: &dyn Driver, cookie: Value) -> Result<(), Fault> {
    driver
        .command("POST", "cookie", json!({"cookie":cookie}))
        .await
        .map(drop)
}

/// `driver.find_element("css selector", selector)`: `POST /element`. Selenium sends "tag
/// name" lookups as CSS selectors too.
pub async fn find_element(driver: &dyn Driver, selector: &str) -> Result<Element, Fault> {
    let value = driver
        .command(
            "POST",
            "element",
            json!({"using":"css selector","value":selector}),
        )
        .await?;
    Element::of(&value)
}

/// `driver.find_elements("css selector", selector)`: `POST /elements`.
pub async fn find_elements(driver: &dyn Driver, selector: &str) -> Result<Vec<Element>, Fault> {
    let value = driver
        .command(
            "POST",
            "elements",
            json!({"using":"css selector","value":selector}),
        )
        .await?;
    value
        .as_array()
        .map(|items| items.iter().map(Element::of).collect())
        .unwrap_or_else(|| Ok(Vec::new()))
}

/// `driver.switch_to.active_element`: `GET /element/active`.
pub async fn active_element(driver: &dyn Driver) -> Result<Element, Fault> {
    Element::of(&driver.command("GET", "element/active", Value::Null).await?)
}

/// `element.text`: `GET /element/{id}/text`.
pub async fn text(driver: &dyn Driver, element: &Element) -> Result<String, Fault> {
    let path = format!("element/{}/text", element.0);
    let value = driver.command("GET", &path, Value::Null).await?;
    Ok(value.as_str().unwrap_or_default().to_owned())
}

/// `element.is_displayed()`: chromedriver's `GET /element/{id}/displayed`.
pub async fn displayed(driver: &dyn Driver, element: &Element) -> Result<bool, Fault> {
    let path = format!("element/{}/displayed", element.0);
    Ok(driver.command("GET", &path, Value::Null).await? == true)
}

/// `element.is_enabled()`: `GET /element/{id}/enabled`.
pub async fn enabled(driver: &dyn Driver, element: &Element) -> Result<bool, Fault> {
    let path = format!("element/{}/enabled", element.0);
    Ok(driver.command("GET", &path, Value::Null).await? == true)
}

/// `element.click()`: `POST /element/{id}/click`.
pub async fn click(driver: &dyn Driver, element: &Element) -> Result<(), Fault> {
    let path = format!("element/{}/click", element.0);
    driver.command("POST", &path, json!({})).await.map(drop)
}

/// `element.send_keys(text)`: `POST /element/{id}/value` with Selenium's text and value
/// fields.
pub async fn send_keys(driver: &dyn Driver, element: &Element, keys: &str) -> Result<(), Fault> {
    let path = format!("element/{}/value", element.0);
    let value: Vec<String> = keys.chars().map(String::from).collect();
    driver
        .command("POST", &path, json!({"text":keys,"value":value}))
        .await
        .map(drop)
}

/// `driver.get_screenshot_as_png()`: `GET /screenshot`, decoded.
pub async fn screenshot(driver: &dyn Driver) -> Result<Vec<u8>, Fault> {
    let value = driver.command("GET", "screenshot", Value::Null).await?;
    super::base64::decode(value.as_str().unwrap_or_default()).ok_or_else(|| Fault::backend("Error"))
}

/// `driver.get_log("browser")`: `POST /se/log`.
pub async fn browser_log(driver: &dyn Driver) -> Result<Vec<Value>, Fault> {
    let value = driver
        .command("POST", "se/log", json!({"type":"browser"}))
        .await?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

/// The Selenium exception class a W3C error code raises; failures are reported by this
/// name only, since WebDriver messages can echo page text.
fn exception(error: &str) -> &'static str {
    match error {
        "element click intercepted" => "ElementClickInterceptedException",
        "element not interactable" => "ElementNotInteractableException",
        "insecure certificate" => "InsecureCertificateException",
        "invalid argument" => "InvalidArgumentException",
        "invalid cookie domain" => "InvalidCookieDomainException",
        "invalid element state" => "InvalidElementStateException",
        "invalid selector" => "InvalidSelectorException",
        "invalid session id" => "InvalidSessionIdException",
        "javascript error" => "JavascriptException",
        "move target out of bounds" => "MoveTargetOutOfBoundsException",
        "no such alert" => "NoAlertPresentException",
        "no such cookie" => "NoSuchCookieException",
        "no such element" => "NoSuchElementException",
        "no such frame" => "NoSuchFrameException",
        "no such shadow root" => "NoSuchShadowRootException",
        "no such window" => "NoSuchWindowException",
        "script timeout" | "timeout" => "TimeoutException",
        "session not created" => "SessionNotCreatedException",
        "stale element reference" => "StaleElementReferenceException",
        "detached shadow root" => "DetachedShadowRootException",
        "unable to capture screen" => "ScreenshotException",
        "unable to set cookie" => "UnableToSetCookieException",
        "unexpected alert open" => "UnexpectedAlertPresentException",
        "unknown method" => "UnknownMethodException",
        _ => "WebDriverException",
    }
}

/// Turns a WebDriver response body into its `value`, or the failure it reports.
fn decode(status: u16, body: &[u8]) -> Result<Value, Fault> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| Fault::backend("WebDriverException"))?;
    let value = parsed.get("value").cloned().unwrap_or(Value::Null);
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        let crashed = value["message"]
            .as_str()
            .is_some_and(|message| message.contains("tab crashed"));
        return Err(Fault::Backend {
            name: exception(error).to_owned(),
            crashed,
        });
    }
    if status != 200 {
        return Err(Fault::backend("WebDriverException"));
    }
    Ok(value)
}

/// The HTTP body once `data` holds a complete response, honouring `Content-Length` and
/// chunked encoding; `None` while more is needed. Without either, the body ends at EOF.
fn complete(data: &[u8], eof: bool) -> Option<Result<(u16, Vec<u8>), Fault>> {
    let malformed = || Some(Err(Fault::backend("ConnectionError")));
    let end = data.windows(4).position(|window| window == b"\r\n\r\n");
    let Some(end) = end else {
        return if eof { malformed() } else { None };
    };
    let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
    let body = &data[end + 4..];
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    let Some(status) = status else {
        return malformed();
    };
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.trim() == name)
                .map(|(_, value)| value.trim().to_owned())
        })
    };
    if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            let Some(line_end) = body[at..].windows(2).position(|w| w == b"\r\n") else {
                return if eof { malformed() } else { None };
            };
            let size_text = String::from_utf8_lossy(&body[at..at + line_end]).into_owned();
            let size_text = size_text.split(';').next().unwrap_or("").trim().to_owned();
            let Ok(size) = usize::from_str_radix(&size_text, 16) else {
                return malformed();
            };
            at += line_end + 2;
            if size == 0 {
                return Some(Ok((status, out)));
            }
            if body.len() < at + size + 2 {
                return if eof { malformed() } else { None };
            }
            out.extend_from_slice(&body[at..at + size]);
            at += size + 2;
        }
    }
    if let Some(length) = header("content-length") {
        let Ok(length) = length.parse::<usize>() else {
            return malformed();
        };
        if body.len() >= length {
            return Some(Ok((status, body[..length].to_vec())));
        }
        return if eof { malformed() } else { None };
    }
    eof.then(|| Ok((status, body.to_vec())))
}

/// One HTTP/1.1 request to the local chromedriver.
async fn http(port: u16, method: &str, path: &str, body: &Value) -> Result<Value, Fault> {
    let failed = |_| Fault::backend("ConnectionError");
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(failed)?;
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n"
    );
    if !body.is_null() {
        request.push_str(&format!(
            "Content-Type: application/json;charset=UTF-8\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    request.push_str("\r\n");
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(&payload);
    stream.write_all(&bytes).await.map_err(failed)?;
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let read = stream.read(&mut chunk).await.map_err(failed)?;
        data.extend_from_slice(&chunk[..read]);
        if data.len() > MAX_RESPONSE {
            return Err(Fault::backend("ConnectionError"));
        }
        if let Some(done) = complete(&data, read == 0) {
            let (status, body) = done?;
            return decode(status, &body);
        }
    }
}

/// The capabilities `chrome_factory` gave Selenium, as the new-session payload.
#[must_use]
pub fn capabilities(profile: &Path, origin: Option<&str>, launch: Launch) -> Value {
    let mut args: Vec<String> = [
        "--headless=new",
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--disable-background-networking",
        "--disable-extensions",
        "--disable-sync",
        "--no-first-run",
        "--no-default-browser-check",
        "--disk-cache-size=1048576",
        "--media-cache-size=1048576",
        "--window-size=1280,900",
        // A runaway page then crashes its own tab instead of exhausting the shared
        // container's memory.
        "--js-flags=--max-old-space-size=512",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    args.push(format!("--user-data-dir={}", profile.display()));
    // Unrelated origins cannot receive browser network traffic. Only the fixed origin
    // bypasses an intentionally unavailable local proxy.
    if let Some(proxy) = launch.proxy {
        // Signed out: every request, loopback included, goes through the egress guard.
        args.push(format!("--proxy-server=http://127.0.0.1:{proxy}"));
        args.push("--proxy-bypass-list=<-loopback>".into());
        args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
        args.push("--disable-quic".into());
        args.push("--dns-prefetch-disable".into());
    } else {
        let parsed = urlsplit(origin.unwrap_or_default()).unwrap_or_default();
        let port = match parsed.port() {
            Ok(Some(port)) if port != 0 => port,
            _ if parsed.scheme == "https" => 443,
            _ => 80,
        };
        args.push("--proxy-server=http://127.0.0.1:9".into());
        args.push(if launch.offline {
            "--proxy-bypass-list=<-loopback>".into()
        } else {
            format!(
                "--proxy-bypass-list=<-loopback>;{}:{port}",
                parsed.hostname().unwrap_or_default()
            )
        });
    }
    if launch.offline {
        args.push("--host-resolver-rules=MAP * ~NOTFOUND".into());
        args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
        args.push("--disable-quic".into());
    }
    json!({"capabilities":{"firstMatch":[{}],"alwaysMatch":{
        "browserName":"chrome","pageLoadStrategy":"normal",
        "goog:chromeOptions":{"extensions":[],"args":args,"binary":CHROMIUM,
            "prefs":{"download_restrictions":3,"credentials_enable_service":false,
                "profile.password_manager_enabled":false}},
        // Reads report uncaught page errors from this console log.
        "goog:loggingPrefs":{"browser":"ALL"}}}})
}

/// A chromedriver process and its one session.
pub struct Chrome {
    port: u16,
    session: String,
    child: Mutex<Option<Child>>,
    pid: i32,
}

impl Chrome {
    /// Starts chromedriver on a free loopback port with the scrubbed environment Selenium
    /// was given, waits for it, and opens one session with [`capabilities`].
    ///
    /// # Errors
    /// [`Fault::Startup`] for any failure; a partial start is cleaned up.
    pub async fn start(
        profile: PathBuf,
        origin: Option<String>,
        launch: Launch,
    ) -> Result<Arc<dyn Driver>, Fault> {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|listener| listener.local_addr())
            .map_err(|_| Fault::Startup)?
            .port();
        let child = Command::new(CHROMEDRIVER)
            .arg(format!("--port={port}"))
            .arg("--log-path=/dev/null")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &profile)
            .env("TMPDIR", "/tmp")
            .env("XDG_CONFIG_HOME", profile.join("config"))
            .env("XDG_CACHE_HOME", profile.join("cache"))
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Fault::Startup)?;
        let pid = i32::try_from(child.id()).unwrap_or(0);
        let mut chrome = Self {
            port,
            session: String::new(),
            child: Mutex::new(Some(child)),
            pid,
        };
        match chrome.open(&profile, origin.as_deref(), launch).await {
            Ok(()) => Ok(Arc::new(chrome)),
            Err(_) => {
                chrome.quit().await;
                Err(Fault::Startup)
            }
        }
    }

    fn running(&self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    async fn open(
        &mut self,
        profile: &Path,
        origin: Option<&str>,
        launch: Launch,
    ) -> Result<(), Fault> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if !self.running() {
                return Err(Fault::Startup);
            }
            if TcpStream::connect(("127.0.0.1", self.port)).await.is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Fault::Startup);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let created = http(
            self.port,
            "POST",
            "/session",
            &capabilities(profile, origin, launch),
        )
        .await?;
        self.session = created["sessionId"]
            .as_str()
            .ok_or(Fault::Startup)?
            .to_owned();
        self.command("POST", "timeouts", json!({"pageLoad":PAGE_TIMEOUT_MS}))
            .await?;
        self.command("POST", "timeouts", json!({"script":SCRIPT_TIMEOUT_MS}))
            .await?;
        self.command("POST", "window/rect", json!({"width":1280,"height":900}))
            .await?;
        Ok(())
    }
}

impl Driver for Chrome {
    fn command<'a>(&'a self, method: &'static str, path: &'a str, body: Value) -> Fut<'a, Value> {
        Box::pin(async move {
            let path = format!("/session/{}/{path}", self.session);
            http(self.port, method, &path, &body).await
        })
    }

    fn pid(&self) -> Option<i32> {
        Some(self.pid)
    }

    /// Selenium's `quit`: delete the session (Chromium closes), ask chromedriver to shut
    /// down, then end it: SIGTERM, SIGKILL after a second. Chromium processes it leaves
    /// behind reparent to the session process (a subreaper) and are ended there.
    fn quit(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if !self.session.is_empty() {
                let path = format!("/session/{}", self.session);
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    http(self.port, "DELETE", &path, &Value::Null),
                )
                .await;
            }
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                http(self.port, "GET", "/shutdown", &Value::Null),
            )
            .await;
            for (signal, wait) in [(libc::SIGTERM, 1000), (libc::SIGKILL, 1000)] {
                if !self.running() {
                    break;
                }
                // SAFETY: kill takes plain integers; the child is unreaped while running,
                // so its id is still its own.
                unsafe {
                    libc::kill(self.pid, signal);
                }
                let deadline = Instant::now() + Duration::from_millis(wait);
                while self.running() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(mut process) = child.take() {
                let _ = process.try_wait();
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_complete_by_length_chunks_or_end() {
        let fixed = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n{\"value\":42}x";
        let (status, body) = complete(fixed, false).expect("complete").expect("valid");
        assert_eq!((status, body.as_slice()), (200, &b"{\"value\":42}x"[..]));
        assert!(complete(&fixed[..fixed.len() - 2], false).is_none());
        let chunked = b"HTTP/1.1 500 Err\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"va\r\n9\r\nlue\":null\r\n1\r\n}\r\n0\r\n\r\n";
        let (status, body) = complete(chunked, false).expect("complete").expect("valid");
        assert_eq!(status, 500);
        assert_eq!(body, b"{\"value\":null}");
        assert!(complete(b"HTTP/1.1 200 OK\r\n\r\nabc", false).is_none());
        assert_eq!(
            complete(b"HTTP/1.1 200 OK\r\n\r\nabc", true)
                .expect("complete")
                .expect("valid")
                .1,
            b"abc"
        );
    }

    #[test]
    fn errors_name_their_selenium_exception_and_never_carry_page_text() {
        let error = decode(
            404,
            br#"{"value":{"error":"no such element","message":"secret page text","stacktrace":""}}"#,
        );
        assert!(
            matches!(error, Err(Fault::Backend{ref name, crashed: false}) if name == "NoSuchElementException")
        );
        let crashed = decode(
            500,
            br#"{"value":{"error":"unknown error","message":"unknown error: session deleted as the browser has closed the connection from tab crashed"}}"#,
        );
        assert!(matches!(crashed, Err(Fault::Backend { crashed: true, .. })));
        assert_eq!(decode(200, br#"{"value":"x"}"#).expect("value"), "x");
    }

    #[test]
    fn capabilities_match_the_python_chrome_factory() {
        let profile = Path::new("/runtime/worker/browser/s/chrome");
        let signed_in = capabilities(
            profile,
            Some("https://dev.webagent.live"),
            Launch::default(),
        );
        let options = &signed_in["capabilities"]["alwaysMatch"]["goog:chromeOptions"];
        let args: Vec<&str> = options["args"]
            .as_array()
            .expect("args")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(args[0], "--headless=new");
        assert!(args.contains(&"--user-data-dir=/runtime/worker/browser/s/chrome"));
        assert_eq!(
            &args[args.len() - 2..],
            [
                "--proxy-server=http://127.0.0.1:9",
                "--proxy-bypass-list=<-loopback>;dev.webagent.live:443"
            ]
        );
        assert_eq!(options["binary"], CHROMIUM);
        assert_eq!(options["prefs"]["download_restrictions"], 3);
        assert_eq!(
            signed_in["capabilities"]["alwaysMatch"]["goog:loggingPrefs"]["browser"],
            "ALL"
        );
        let signed_out = capabilities(
            profile,
            None,
            Launch {
                offline: false,
                proxy: Some(4321),
            },
        );
        let args = signed_out["capabilities"]["alwaysMatch"]["goog:chromeOptions"]["args"]
            .as_array()
            .expect("args")
            .clone();
        assert!(args.contains(&json!("--proxy-server=http://127.0.0.1:4321")));
        assert!(args.contains(&json!("--dns-prefetch-disable")));
        let offline = capabilities(
            profile,
            Some("http://127.0.0.1:8081"),
            Launch {
                offline: true,
                proxy: None,
            },
        );
        let args = offline["capabilities"]["alwaysMatch"]["goog:chromeOptions"]["args"]
            .as_array()
            .expect("args")
            .clone();
        assert!(args.contains(&json!("--proxy-bypass-list=<-loopback>")));
        assert!(args.contains(&json!("--host-resolver-rules=MAP * ~NOTFOUND")));
    }
}
