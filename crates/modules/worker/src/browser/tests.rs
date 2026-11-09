//! The browser service against a scripted WebDriver, ported from the Python package's
//! `test_runner.py`: every rule, message and result shape the Python tests pinned.
use super::rules::{
    origin_of, valid_origin, validate_args, validate_trusted, validate_url, Scope, Sites,
};
use super::service::{read_artifact, Limits, Service};
use super::webdriver::{Driver, Factory, Fut, Launch};
use super::{scripts, Fault, Settings};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const ELEMENT: &str = "element-6066-11e4-a52e-4f735466cecf";
const ORIGIN: &str = "https://dev.webagent.live";
const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ4cAAAAASUVORK5CYII=";
const CREDENTIAL_FIELDS: [&str; 3] = ["#password", "#user", "#code"];

type ScriptHook = Box<dyn Fn(&str, &[Value]) -> Option<Result<Value, Fault>> + Send + Sync>;
type CdpHook = Box<dyn Fn(&str, &Value) -> Option<Value> + Send + Sync>;

/// What the scripted browser has seen and will answer.
#[derive(Default)]
struct Page {
    current_url: String,
    identity: String,
    csrf: String,
    cookies: Vec<Value>,
    jar: Vec<Value>,
    cdp: Vec<(String, Value)>,
    clicks: u32,
    typed: Vec<String>,
    ready: String,
    escape_on_click: bool,
    closed: bool,
    failure: bool,
    signin: bool,
    /// Asks to sign in until this value has been typed.
    signin_until_typed: Option<String>,
    masked: Option<Vec<Value>>,
    log: Option<Vec<Value>>,
    refuse: Option<String>,
    crash: bool,
}

struct Mock {
    page: Mutex<Page>,
    script: Mutex<Option<ScriptHook>>,
    cdp: Mutex<Option<CdpHook>>,
}

impl Mock {
    fn page(&self) -> std::sync::MutexGuard<'_, Page> {
        self.page.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn on_script(&self, hook: ScriptHook) {
        *self.script.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    fn on_cdp(&self, hook: CdpHook) {
        *self.cdp.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    fn element(id: &str) -> Value {
        json!({ELEMENT: id})
    }

    fn script(&self, source: &str, args: &[Value]) -> Result<Value, Fault> {
        if let Some(hook) = &*self.script.lock().unwrap_or_else(PoisonError::into_inner) {
            if let Some(answer) = hook(source, args) {
                return answer;
            }
        }
        let mut page = self.page();
        if page.crash {
            return Err(Fault::Backend {
                name: "WebDriverException".into(),
                crashed: true,
            });
        }
        let arg = |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
        Ok(if source.contains("V3_BOOT") {
            json!(page.ready)
        } else if source.contains("V3_READ") {
            json!({"text":format!("Visible bounded text {} {}", arg(1), page.csrf),
                "missing":false,"truncated":false})
        } else if source.contains("V3_TARGET") {
            json!({"allowed":true,"secret":arg(0) == "#password",
                "credential":CREDENTIAL_FIELDS.contains(&arg(0)),
                "href":if arg(0) == "#external" {"https://outside.invalid/"} else {""},"tag":"input"})
        } else if source.contains("V3_SIGNIN") {
            match &page.signin_until_typed {
                Some(value) => json!(!page.typed.contains(value)),
                None => json!(page.signin),
            }
        } else if source.contains("V3_CREDENTIAL") {
            let id = args[0][ELEMENT].as_str().unwrap_or("");
            json!(CREDENTIAL_FIELDS.contains(&id))
        } else if source.contains("V3_CHECK") {
            json!(true)
        } else if source.contains("V3_MASK") {
            page.masked = Some(args.to_vec());
            json!(1)
        } else if source.contains("V3_UNMASK") {
            Value::Null
        } else {
            panic!("Unexpected native script");
        })
    }

    fn answer(&self, method: &str, path: &str, body: &Value) -> Result<Value, Fault> {
        if path == "execute/sync" {
            let args = body["args"].as_array().cloned().unwrap_or_default();
            return self.script(body["script"].as_str().unwrap_or(""), &args);
        }
        if path == "goog/cdp/execute" {
            let command = body["cmd"].as_str().unwrap_or("").to_owned();
            let params = body["params"].clone();
            self.page().cdp.push((command.clone(), params.clone()));
            if let Some(hook) = &*self.cdp.lock().unwrap_or_else(PoisonError::into_inner) {
                if let Some(answer) = hook(&command, &params) {
                    return Ok(answer);
                }
            }
            let mut page = self.page();
            return Ok(match command.as_str() {
                "Network.getAllCookies" => json!({"cookies":page.jar}),
                "Network.setCookies" => {
                    let cookies = params["cookies"].as_array().cloned().unwrap_or_default();
                    page.jar.extend(cookies);
                    json!({})
                }
                "Network.clearBrowserCookies" => {
                    page.jar.clear();
                    json!({})
                }
                _ => json!({}),
            });
        }
        let mut page = self.page();
        let element = path
            .strip_prefix("element/")
            .and_then(|rest| rest.split_once('/'));
        Ok(match (method, path, element) {
            ("POST", "url", _) => {
                let url = body["url"].as_str().unwrap_or("").to_owned();
                page.current_url = match &page.refuse {
                    Some(host) if url.contains(host.as_str()) => {
                        "chrome-error://chromewebdata/".into()
                    }
                    _ => url,
                };
                Value::Null
            }
            ("GET", "url", _) => json!(page.current_url),
            ("POST", "cookie", _) => {
                page.cookies.push(body["cookie"].clone());
                Value::Null
            }
            ("POST", "element", _) => {
                if page.failure {
                    return Err(Fault::backend("RuntimeError"));
                }
                Self::element(body["value"].as_str().unwrap_or(""))
            }
            ("POST", "elements", _) => {
                json!([Self::element(body["value"].as_str().unwrap_or(""))])
            }
            ("GET", "element/active", _) => Self::element("active"),
            ("GET", _, Some((_, "text"))) => {
                json!(json!({"user_id":page.identity,"csrf_token":page.csrf}).to_string())
            }
            ("GET", _, Some((_, "displayed" | "enabled"))) => json!(true),
            ("POST", _, Some((_, "click"))) => {
                page.clicks += 1;
                if page.escape_on_click {
                    page.current_url = "https://outside.invalid/".into();
                }
                Value::Null
            }
            ("POST", _, Some((_, "value"))) => {
                let text = body["text"].as_str().unwrap_or("").to_owned();
                page.typed.push(text);
                Value::Null
            }
            ("GET", "screenshot", _) => json!(PNG),
            ("POST", "se/log", _) => match &page.log {
                Some(entries) => json!(entries),
                None => return Err(Fault::backend("WebDriverException")),
            },
            _ => panic!("unexpected WebDriver command {method} {path}"),
        })
    }
}

impl Driver for Mock {
    fn command<'a>(&'a self, method: &'static str, path: &'a str, body: Value) -> Fut<'a, Value> {
        Box::pin(async move { self.answer(method, path, &body) })
    }

    fn pid(&self) -> Option<i32> {
        None
    }

    fn quit(&self) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.page().closed = true;
        })
    }
}

/// How the next scripted browser starts.
#[derive(Default)]
struct Start {
    identity: Option<String>,
    broken: bool,
    refuse: Option<String>,
}

type Drivers = Arc<Mutex<Vec<Arc<Mock>>>>;
type Launches = Arc<Mutex<Vec<(Option<String>, Launch)>>>;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    sid: String,
    base: PathBuf,
    trusted: Value,
    service: Arc<Service>,
    drivers: Drivers,
    launches: Launches,
    start: Arc<Mutex<Start>>,
}

fn scripted(drivers: &Drivers, launches: &Launches, start: &Arc<Mutex<Start>>) -> Factory {
    let (drivers, launches, start) = (Arc::clone(drivers), Arc::clone(launches), Arc::clone(start));
    Arc::new(move |_profile, origin, launch| {
        let (drivers, launches, start) = (
            Arc::clone(&drivers),
            Arc::clone(&launches),
            Arc::clone(&start),
        );
        Box::pin(async move {
            let start = start.lock().unwrap_or_else(PoisonError::into_inner);
            if start.broken {
                return Err(Fault::backend("RuntimeError"));
            }
            launches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((origin, launch));
            let mock = Arc::new(Mock {
                page: Mutex::new(Page {
                    identity: start.identity.clone().unwrap_or_else(|| "admin".into()),
                    csrf: uuid::Uuid::new_v4().to_string(),
                    ready: "ready".into(),
                    refuse: start.refuse.clone(),
                    ..Page::default()
                }),
                script: Mutex::new(None),
                cdp: Mutex::new(None),
            });
            drivers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Arc::clone(&mock));
            Ok(mock as Arc<dyn Driver>)
        })
    })
}

fn fixture_with(limits: Limits) -> Fixture {
    let dir = tempfile::tempdir().expect("temporary directory");
    let root = dir.path().to_owned();
    let sid = uuid::Uuid::new_v4().to_string();
    let base = root.join("workspace").join(&sid);
    std::fs::create_dir_all(&base).expect("workspace");
    let trusted = json!({"session_id":sid,"user_id":"admin","agent_id":"shared_default",
        "cookie":uuid::Uuid::new_v4().to_string(),"generation":"first",
        "expires_at":super::now() + 300.0,"allowed_session_ids":[sid]});
    let drivers = Drivers::default();
    let launches = Launches::default();
    let start = Arc::new(Mutex::new(Start::default()));
    let settings = Settings {
        root: root.join("private"),
        origin: ORIGIN.into(),
    };
    Fixture {
        service: Service::new(&settings, scripted(&drivers, &launches, &start), limits),
        _dir: dir,
        root,
        sid,
        base,
        trusted,
        drivers,
        launches,
        start,
    }
}

fn fixture() -> Fixture {
    fixture_with(Limits::default())
}

impl Fixture {
    /// Input actions belong to browser_input; signed-in routes are passed as `path`.
    async fn call(&self, args: Value) -> Value {
        self.call_with(args, &self.trusted).await
    }

    async fn call_with(&self, mut args: Value, trusted: &Value) -> Value {
        let input = ["click", "type", "press", "save_login"]
            .iter()
            .any(|action| args["action"] == *action);
        let name = if input {
            "browser_input"
        } else {
            "browser_read"
        };
        if let Some(path) = args.as_object_mut().and_then(|args| args.remove("path")) {
            if name == "browser_read" {
                args["url"] = path;
            } else {
                args["path"] = path;
            }
        }
        self.service.call(name, &args, &self.base, trusted).await
    }

    async fn tool(&self, name: &str, args: Value, trusted: &Value) -> Value {
        self.service.call(name, &args, &self.base, trusted).await
    }

    fn driver(&self, index: usize) -> Arc<Mock> {
        Arc::clone(&self.drivers.lock().unwrap_or_else(PoisonError::into_inner)[index])
    }

    fn last(&self) -> Arc<Mock> {
        let drivers = self.drivers.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(drivers.last().expect("a browser started"))
    }

    fn started(&self) -> usize {
        self.drivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn cookie(&self) -> String {
        self.trusted["cookie"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn signed_out(&self, extra: Value) -> Value {
        let mut public = self.trusted.clone();
        if let Some(public) = public.as_object_mut() {
            public.remove("cookie");
        }
        public["sites"] = json!({"allow":[],"block":[]});
        if let (Value::Object(public), Value::Object(extra)) = (&mut public, extra) {
            public.extend(extra);
        }
        public
    }

    fn read(&self, artifact: &Value) -> Result<Vec<u8>, Fault> {
        read_artifact(
            &self.root.join("private"),
            self.service.limits(),
            &self.base,
            &self.trusted,
            artifact,
        )
    }
}

fn png() -> Vec<u8> {
    super::base64::decode(PNG).expect("fixture")
}

fn output(result: &Value) -> &str {
    result["output"].as_str().unwrap_or_default()
}

fn pair(left: &Value, right: &Value) -> (Value, Value) {
    (left.clone(), right.clone())
}

#[test]
fn page_scripts_are_byte_identical_to_the_python_adapter() {
    let mut digest = Sha256::new();
    for script in [
        scripts::READ,
        scripts::MEASURE,
        scripts::ANIMATIONS,
        scripts::CHECK,
        scripts::READY,
        scripts::SCROLL,
        scripts::FOCUS,
        scripts::PERFORMANCE,
        scripts::CENTER,
        scripts::TARGET,
        scripts::CREDENTIAL,
        scripts::SIGNIN,
        scripts::MASK,
        scripts::UNMASK,
    ] {
        digest.update(script.as_bytes());
    }
    assert_eq!(
        hex::encode(digest.finalize()),
        "a5ecfcece4bcbba1ea09ca33d17229f24e75d27257bee68dadbbe6e28ac91ac5"
    );
    // The page sees a JavaScript newline escape, not a raw newline in a string literal.
    assert!(scripts::READ.contains("text+=chunk+'\\n';"));
}

#[tokio::test]
async fn optional_action_schema_defaults_preserve_boundaries() {
    let f = fixture();
    let opened = f.call(json!({"action":"navigate","url":"/app"})).await;
    assert_eq!(opened["status"], "success", "{opened}");
    for action in ["read", "screenshot", "close"] {
        let result = f
            .call(json!({"action":action,"path":"","selector":"","text":""}))
            .await;
        assert_eq!(result["status"], "success", "{action} {result}");
    }
    let result = f
        .call(json!({"action":"navigate","path":"/api/v3/auth/me","selector":"","text":null}))
        .await;
    assert_eq!(result["status"], "success", "{result}");
    for args in [
        json!({"action":"screenshot","path":"/app","selector":"","text":""}),
        json!({"action":"read","path":"","selector":"","text":"substantive"}),
        json!({"action":"click","path":"","selector":"","text":""}),
        json!({"action":"navigate","path":"","selector":"","text":""}),
        json!({"action":"close","unknown":""}),
    ] {
        assert_eq!(f.call(args.clone()).await["status"], "error", "{args}");
    }
    let object =
        |value: Value| -> Map<String, Value> { serde_json::from_value(value).expect("object") };
    let mut args = object(json!({"action":"read","selector":null,"url":null,"text":null}));
    assert_eq!(validate_args("browser_read", &mut args).ok(), Some("read"));
    assert_eq!(Value::Object(args), json!({"action":"read"}));
    let mut zero = object(json!({"action":"read","expected_sequence":0}));
    assert_eq!(validate_args("browser_read", &mut zero).ok(), Some("read"));
    let mut one = object(json!({"action":"read","expected_sequence":1}));
    assert!(matches!(
        validate_args("browser_read", &mut one),
        Err(Fault::Rejected(_))
    ));
}

#[tokio::test]
async fn identity_csrf_is_masked_in_read_and_screenshot() {
    let f = fixture();
    f.call(json!({"action":"navigate","url":"/api/v3/auth/me"}))
        .await;
    let csrf = f.driver(0).page().csrf.clone();
    let read = f.call(json!({"action":"read"})).await;
    let text = read["text"].as_str().expect("text");
    assert!(
        !text.contains(&csrf) && !text.contains(&f.cookie()),
        "{text}"
    );
    let shot = f.call(json!({"action":"screenshot"})).await;
    assert_eq!(shot["status"], "success", "{shot}");
    assert_eq!(
        f.driver(0).page().masked,
        Some(vec![json!(f.cookie()), json!(csrf)])
    );
}

#[tokio::test]
async fn login_private_cookie_fixed_origin_and_current_routes() {
    let f = fixture();
    let result = f
        .call(json!({"action":"navigate",
            "url":"/app?tab=agents&agent=shared_default&agentView=abilities"}))
        .await;
    assert_eq!(result["status"], "success", "{result}");
    assert!(!result.to_string().contains(&f.cookie()));
    assert_eq!(result["ready"], true);
    let cookie = f.driver(0).page().cookies[0].clone();
    assert_eq!(cookie["name"], "v3_session");
    assert_eq!(
        pair(&cookie["httpOnly"], &cookie["secure"]),
        (json!(true), json!(true))
    );
    assert!(f
        .root
        .join("private")
        .join(&f.sid)
        .join("owner.json")
        .exists());
    for path in [
        "https://outside.invalid/".to_owned(),
        "//outside.invalid/".into(),
        "file:///etc/passwd".into(),
        format!("/app?session_id={}", uuid::Uuid::new_v4()),
        "/app?tab=agents&agent=another".into(),
        format!("/api/v3/sessions/{}", uuid::Uuid::new_v4()),
        "/app?tab=account&cookie=x".into(),
    ] {
        let result = f.call(json!({"action":"navigate","path":path})).await;
        assert_eq!(result["status"], "error", "{path}");
    }
    let me = f
        .call(json!({"action":"navigate","path":"/api/v3/auth/me"}))
        .await;
    assert_eq!(me["status"], "success");
}

#[tokio::test]
async fn current_app_routes_viewport_page_errors_and_named_rejections() {
    let f = fixture();
    f.call(json!({"action":"navigate","url":"/app"})).await;
    for path in [
        "/app?tab=app",
        "/app?tab=builds&job_id=job-1",
        "/app?tab=agents&agent=shared_default&agentView=identity&agentScope=sessions",
        "/app?tab=app&appView=files&path=/notes/today.md",
        "/app?tab=session-metrics",
    ] {
        let result = f.call(json!({"action":"navigate","path":path})).await;
        assert_eq!(result["status"], "success", "{path} {result}");
    }
    for (path, reason) in [
        (
            "/app?tab=app&chat_invite=x",
            "Unsupported app route parameters; allowed: agent, agentScope",
        ),
        (
            "/app?tab=app&embed_auth=x",
            "Unsupported app route parameters",
        ),
        ("/app?tab=Agents%20x", "Unsupported development page"),
        ("/app?agent=another", "Another agent is unavailable"),
    ] {
        let result = f.call(json!({"action":"navigate","path":path})).await;
        assert!(
            output(&result).starts_with(&format!("Private browser request rejected: {reason}")),
            "{path} {result}"
        );
    }
    // A page that drifted outside the routes blocks reads but navigation recovers.
    let driver = f.driver(0);
    driver.page().current_url = format!("{ORIGIN}/app?tab=agents&agent=another");
    assert_eq!(
        output(&f.call(json!({"action":"read"})).await),
        "Private browser request rejected: Another agent is unavailable"
    );
    let back = f
        .call(json!({"action":"navigate","path":"/app?tab=app"}))
        .await;
    assert_eq!(back["status"], "success");
    driver.page().cdp.clear();
    let phone = f.call(json!({"action":"viewport","preset":"phone"})).await;
    assert_eq!(
        pair(&phone["width"], &phone["height"]),
        (json!(390), json!(844))
    );
    assert_eq!(phone["navigate_required"], true);
    assert_eq!(
        driver.page().cdp,
        vec![
            (
                "Emulation.setDeviceMetricsOverride".into(),
                json!({"width":390,"height":844,"deviceScaleFactor":1,"mobile":true})
            ),
            (
                "Emulation.setTouchEmulationEnabled".into(),
                json!({"enabled":true})
            )
        ]
    );
    let desktop = f
        .call(json!({"action":"viewport","preset":"desktop"}))
        .await;
    assert_eq!(desktop["width"], 1280);
    let watch = f.call(json!({"action":"viewport","preset":"watch"})).await;
    assert!(output(&watch).contains("one of phone"));
    let tablet = f.call(json!({"action":"viewport","preset":"tablet"})).await;
    assert_eq!(tablet["width"], 768);
    assert_eq!(
        driver.page().cdp.last().cloned(),
        Some((
            "Emulation.setTouchEmulationEnabled".into(),
            json!({"enabled":true})
        ))
    );
    // The preset is re-applied after navigation and before a screenshot.
    driver.page().cdp.clear();
    f.call(json!({"action":"navigate","path":"/app?tab=app"}))
        .await;
    assert_eq!(driver.page().cdp[0].1["width"], 768);
    driver.page().cdp.clear();
    f.call(json!({"action":"screenshot"})).await;
    assert_eq!(driver.page().cdp[0].1["width"], 768);
    assert_eq!(
        f.call(json!({"action":"viewport"})).await["status"],
        "error"
    );
    driver.page().log = Some(vec![
        json!({"level":"SEVERE","message":format!("app.js 455 Uncaught TypeError {}", f.cookie())}),
        json!({"level":"INFO","message":"ignored"}),
        json!({"level":"SEVERE","message":"x".repeat(400)}),
    ]);
    let errors = f.call(json!({"action":"read"})).await["page_errors"].clone();
    assert_eq!(errors[0], "app.js 455 Uncaught TypeError [redacted]");
    assert_eq!(errors.as_array().map(Vec::len), Some(2));
    assert_eq!(errors[1].as_str().map(str::len), Some(300));
    assert!(!errors.to_string().contains(&f.cookie()));
    let negative = f.call(json!({"action":"read","offset":-1})).await;
    assert!(output(&negative).contains("whole number"));
    let far = f.call(json!({"action":"read","offset":12000})).await;
    assert_eq!(far["status"], "success");
    let clicked = f.call(json!({"action":"click","selector":"#button"})).await;
    assert_eq!(
        clicked["target"],
        json!({"tag":"input","label":""}),
        "{clicked}"
    );
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[tokio::test]
async fn measure_wait_animations_scroll_press_pixels_and_emulate() {
    let f = fixture();
    f.call(json!({"action":"navigate","url":"/app"})).await;
    let driver = f.driver(0);
    let cookie = f.cookie();
    let polls: Arc<Mutex<Vec<Vec<Value>>>> = Arc::default();
    let measured: Arc<Mutex<Vec<Vec<Value>>>> = Arc::default();
    let focus = Arc::new(Mutex::new(
        json!({"tag":"button","id":"next","label":"Next","secret":false}),
    ));
    {
        let (polls, measured, focus, cookie) = (
            Arc::clone(&polls),
            Arc::clone(&measured),
            Arc::clone(&focus),
            cookie.clone(),
        );
        driver.on_script(Box::new(move |source, args| {
            let arg = |index: usize| args.get(index).and_then(Value::as_str).unwrap_or("");
            let label = format!("Save {cookie}");
            Some(Ok(if source.contains("V3_MEASURE") {
                lock(&measured).push(args.to_vec());
                json!({"checks":{"viewport":{"width":390,"height":844},"page_overflow_x":0,
                    "matches":1,"rendered":1,"under_44px":{"count":1,
                    "smallest":[{"label":label,"width":40,"height":32}]},"outside_viewport":0,
                    "content_overflowing":{"count":0,"first":[]}},
                    "elements":[{"tag":"button","label":label,
                    "box":{"x":0,"y":0,"width":40,"height":32}}],"next_offset":null})
            } else if source.contains("V3_CENTER") {
                if arg(0) == "#none" {
                    Value::Null
                } else {
                    json!({"x":20,"y":16})
                }
            } else if source.contains("V3_PERFORMANCE") {
                json!({"time_to_first_byte_ms":80,"first_contentful_paint_ms":420})
            } else if source.contains("V3_ANIMATIONS") {
                json!({"missing":false,"count":1,
                    "animations":[{"type":"CSSTransition","name":"opacity","duration_ms":200}]})
            } else if source.contains("V3_CHECK") {
                let mut polls = lock(&polls);
                polls.push(args.to_vec());
                json!(polls.len() >= 3)
            } else if source.contains("V3_SCROLL") {
                json!({"missing":arg(0) == "#none","scroll_y":args[2]})
            } else if source.contains("V3_FOCUS") {
                lock(&focus).clone()
            } else {
                return None;
            }))
        }));
    }
    let bare = f.call(json!({"action":"measure"})).await;
    assert!(output(&bare).contains("requires a selector"));
    let result = f
        .call(json!({"action":"measure","selector":"button"}))
        .await;
    assert_eq!(result["elements"][0]["box"]["height"], 32, "{result}");
    assert_eq!(result["elements"][0]["label"], "Save [redacted]");
    assert_eq!(
        result["checks"]["under_44px"]["smallest"][0]["label"],
        "Save [redacted]"
    );
    assert_eq!(
        lock(&measured).last().cloned(),
        Some(vec![json!("button"), json!(20), json!(0), json!(false)])
    );
    f.call(json!({"action":"measure","selector":"button","offset":20,"styles":true}))
        .await;
    assert_eq!(
        lock(&measured).last().cloned(),
        Some(vec![json!("button"), json!(5), json!(20), json!(true)])
    );
    let styled = f
        .call(json!({"action":"measure","selector":"button","styles":"yes"}))
        .await;
    assert!(output(&styled).contains("styles must be"));
    let timing = f.call(json!({"action":"performance"})).await;
    assert_eq!(timing["first_contentful_paint_ms"], 420);
    // Pixels: one 1x1 capture per point, decoded from the PNG, under the screenshot masks.
    let one = super::png::tests::one_pixel(&[
        0x78, 0x9c, 0x63, 0x11, 0x32, 0x09, 0xfb, 0x0f, 0x00, 0x02, 0xa8, 0x01, 0xa0,
    ]);
    let encoded = super::base64::encode(&one);
    driver.on_cdp(Box::new(move |command, _| {
        (command == "Page.captureScreenshot").then(|| json!({"data":encoded}))
    }));
    let shot = f
        .call(json!({"action":"pixels","points":[{"x":5,"y":7},{"x":0,"y":0}]}))
        .await;
    let colors: Vec<Value> = shot["pixels"]
        .as_array()
        .expect("pixels")
        .iter()
        .map(|pixel| pixel["color"].clone())
        .collect();
    assert_eq!(colors, vec![json!("#123456"), json!("#123456")], "{shot}");
    let capture = driver
        .page()
        .cdp
        .iter()
        .find(|(command, _)| command == "Page.captureScreenshot")
        .map(|(_, params)| params["clip"].clone());
    assert_eq!(
        capture,
        Some(json!({"x":5,"y":7,"width":1,"height":1,"scale":1}))
    );
    let csrf = driver.page().csrf.clone();
    assert_eq!(driver.page().masked, Some(vec![json!(cookie), json!(csrf)]));
    let centred = f.call(json!({"action":"pixels","selector":"button"})).await;
    assert_eq!(centred["pixels"][0]["x"], 20);
    let missing = f.call(json!({"action":"pixels","selector":"#none"})).await;
    assert!(output(&missing).contains("No rendered element"));
    let outside = f
        .call(json!({"action":"pixels","points":[{"x":-1,"y":0}]}))
        .await;
    assert!(output(&outside).contains("points must be"));
    let nothing = f.call(json!({"action":"pixels"})).await;
    assert!(output(&nothing).contains("points or a selector"));
    let animations = f.call(json!({"action":"animations"})).await;
    assert_eq!(animations["animations"][0]["duration_ms"], 200);
    let visible = f
        .call(json!({"action":"wait_for","condition":"visible"}))
        .await;
    assert!(output(&visible).contains("requires a selector"));
    let long = f
        .call(json!({"action":"wait_for","condition":"quiet","timeout_ms":99999}))
        .await;
    assert!(output(&long).contains("timeout_ms"));
    let forever = f
        .call(json!({"action":"wait_for","condition":"forever"}))
        .await;
    assert!(output(&forever).contains("one of"));
    let waited = f
        .call(json!({"action":"wait_for","condition":"quiet","quiet_ms":200}))
        .await;
    assert_eq!(
        pair(&waited["met"], &waited["last_action"]),
        (json!(true), json!("navigate"))
    );
    assert_eq!(lock(&polls).len(), 3);
    assert!(waited["since_last_action_ms"].is_i64(), "{waited}");
    assert_eq!(
        lock(&polls)[0],
        vec![json!("quiet"), json!(""), json!(""), json!(200)]
    );
    let none = f.call(json!({"action":"scroll","selector":"#none"})).await;
    assert!(output(&none).contains("does not exist"));
    let still = f.call(json!({"action":"scroll"})).await;
    assert!(output(&still).contains("selector to bring into view"));
    assert_eq!(
        f.call(json!({"action":"scroll","dy":600})).await["scroll_y"],
        600
    );
    let unread = f.call(json!({"action":"press","key":"Tab"})).await;
    assert!(output(&unread).contains("Read the current page"));
    f.call(json!({"action":"read"})).await;
    let letter = f.call(json!({"action":"press","key":"a"})).await;
    assert!(output(&letter).contains("Key must be one of"));
    let pressed = f.call(json!({"action":"press","key":"Tab"})).await;
    assert_eq!(
        pair(&pressed["applied"], &pressed["focused"]["label"]),
        (json!(true), json!("Next")),
        "{pressed}"
    );
    assert_eq!(
        driver.page().typed.last().map(String::as_str),
        Some("\u{e004}")
    );
    let again = f.call(json!({"action":"press","key":"Tab"})).await;
    assert!(output(&again).contains("Read the current page"));
    // A key types nothing, so Enter may submit a password field; its label stays private.
    *lock(&focus) = json!({"tag":"input","id":"pw","label":"secret label","secret":true});
    f.call(json!({"action":"read"})).await;
    let pressed = f.call(json!({"action":"press","key":"Enter"})).await;
    assert_eq!(
        pair(&pressed["applied"], &pressed["target"]["label"]),
        (json!(true), json!(""))
    );
    let sepia = f
        .call(json!({"action":"emulate","color_scheme":"sepia"}))
        .await;
    assert!(output(&sepia).contains("color_scheme must be"));
    let empty = f.call(json!({"action":"emulate"})).await;
    assert!(output(&empty).contains("needs color_scheme"));
    let emulated = f
        .call(json!({"action":"emulate","color_scheme":"dark","reduced_motion":"reduce"}))
        .await;
    assert_eq!(
        pair(&emulated["color_scheme"], &emulated["reduced_motion"]),
        (json!("dark"), json!("reduce"))
    );
    assert_eq!(
        driver.page().cdp.last().cloned(),
        Some((
            "Emulation.setEmulatedMedia".into(),
            json!({"features":[{"name":"prefers-color-scheme","value":"dark"},
                {"name":"prefers-reduced-motion","value":"reduce"}]})
        ))
    );
    // Media emulation is re-applied after navigation, like the viewport.
    driver.page().cdp.clear();
    f.call(json!({"action":"navigate","path":"/app?tab=app"}))
        .await;
    assert_eq!(
        driver.page().cdp.last().map(|(command, _)| command.clone()),
        Some("Emulation.setEmulatedMedia".into())
    );
}

#[tokio::test]
async fn signed_out_public_urls_site_policy_guard_and_tool_split() {
    let f = fixture();
    let sites = Sites {
        allow: Vec::new(),
        block: vec!["blocked-by-policy.test".into()],
    };
    let credentialed = format!(
        concat!("https://", "{}:{}", "@example.com/"),
        "name", "value"
    );
    for (url, reason) in [
        ("http://localhost/", "Private and internal"),
        ("https://10.0.0.1/", "Private and internal"),
        (credentialed.as_str(), "user name or password"),
        ("ftp://example.com/", "full http"),
        ("https://example.com:8080/", "ports 80 and 443"),
        ("/app", "full http"),
        ("https://sub.blocked-by-policy.test/", "blocks"),
    ] {
        let result = validate_url(&json!(url), &Scope::SignedOut { sites: &sites }, ORIGIN);
        assert!(
            matches!(&result, Err(Fault::Rejected(message)) if message.contains(reason)),
            "{url} {result:?}"
        );
    }
    let allow = Sites {
        allow: vec!["example.com".into()],
        block: Vec::new(),
    };
    let scope = Scope::SignedOut { sites: &allow };
    assert_eq!(
        validate_url(&json!("https://www.example.com/a?b=1"), &scope, ORIGIN).ok(),
        Some("https://www.example.com/a?b=1".into())
    );
    assert!(matches!(
        validate_url(&json!("https://other.test/"), &scope, ORIGIN),
        Err(Fault::Rejected(message)) if message.contains("may only open: example.com")
    ));
    let bad = f.signed_out(json!({"sites":{"allow":["Bad Host"],"block":[]}}));
    assert!(validate_trusted(&f.base, &bad).is_err());
    let public = f.signed_out(json!({"sites":{"allow":[],"block":["blocked-by-policy.test"]}}));
    lock(&f.start).refuse = Some("refused.test".into());
    let opened = f
        .tool(
            "browser_read",
            json!({"action":"navigate","url":"https://example.com/"}),
            &public,
        )
        .await;
    assert_eq!(opened["status"], "success", "{opened}");
    assert_eq!(
        pair(&opened["signed_in"], &opened["url"]),
        (json!(false), json!("https://example.com/"))
    );
    let guard = f.service.egress().await.expect("guard");
    let (origin, launch) = lock(&f.launches)[0].clone();
    assert!(origin.is_none());
    assert_eq!(launch.proxy, Some(guard.port()));
    assert!(f.last().page().cookies.is_empty());
    guard.refused(
        "refused.test",
        "it resolves to a private or internal address",
    );
    let blocked = f
        .tool(
            "browser_read",
            json!({"action":"navigate","url":"https://refused.test/"}),
            &public,
        )
        .await;
    assert!(
        output(&blocked).contains("Blocked: refused.test"),
        "{blocked}"
    );
    let read = f
        .tool("browser_read", json!({"action":"read"}), &public)
        .await;
    assert!(output(&read).contains("navigate to another URL"));
    let next = f
        .tool(
            "browser_read",
            json!({"action":"navigate","url":"https://example.com/next"}),
            &public,
        )
        .await;
    assert_eq!(next["status"], "success");
    let read = f
        .tool("browser_read", json!({"action":"read"}), &public)
        .await;
    assert!(read["text"]
        .as_str()
        .is_some_and(|text| text.starts_with("Visible bounded text")));
    let misrouted = f
        .tool(
            "browser_read",
            json!({"action":"click","selector":"#button"}),
            &public,
        )
        .await;
    assert!(output(&misrouted).contains("browser_input"));
    let misrouted = f
        .tool("browser_input", json!({"action":"read"}), &public)
        .await;
    assert!(output(&misrouted).contains("browser_read"));
    let clicked = f
        .tool(
            "browser_input",
            json!({"action":"click","selector":"#button"}),
            &public,
        )
        .await;
    assert_eq!(clicked["status"], "success", "{clicked}");
    let typed = f
        .tool(
            "browser_read",
            json!({"action":"navigate","url":"https://example.com/","signed_in":"yes"}),
            &public,
        )
        .await;
    assert!(output(&typed).contains("signed_in must be"));
    // Signing in replaces the signed-out browser with one locked to the development origin.
    let signed = f.call(json!({"action":"navigate","url":"/app"})).await;
    assert_eq!(signed["status"], "success", "{signed}");
    assert_eq!(signed["signed_in"], true);
    let outside = f
        .call(json!({"action":"navigate","url":"https://example.com/"}))
        .await;
    assert!(output(&outside).contains("Only exact relative"));
}

#[tokio::test]
async fn fresh_read_before_each_input_and_secret_targets_blocked() {
    let f = fixture();
    let status = |value: Value| value["status"].clone();
    assert_eq!(
        status(f.call(json!({"action":"click","selector":"#button"})).await),
        "error"
    );
    let read = f.call(json!({"action":"read"})).await;
    assert_eq!(read["status"], "success");
    assert!(!read["text"]
        .as_str()
        .unwrap_or_default()
        .contains(&f.cookie()));
    assert_eq!(read["fresh_for_input"], true);
    assert_eq!(
        status(f.call(json!({"action":"click","selector":"#button"})).await),
        "success"
    );
    assert_eq!(
        status(
            f.call(json!({"action":"type","selector":"#text","text":"x"}))
                .await
        ),
        "error"
    );
    f.call(json!({"action":"read"})).await;
    assert_eq!(
        status(f.call(json!({"action":"click","selector":"#button"})).await),
        "success"
    );
    f.call(json!({"action":"read"})).await;
    assert_eq!(
        status(
            f.call(json!({"action":"type","selector":"#password","text":"secret"}))
                .await
        ),
        "error"
    );
    f.call(json!({"action":"read"})).await;
    assert_eq!(
        status(
            f.call(json!({"action":"click","selector":"#external"}))
                .await
        ),
        "error"
    );
    f.call(json!({"action":"read"})).await;
    assert_eq!(
        status(
            f.call(json!({"action":"type","selector":"#text","text":"literal"}))
                .await
        ),
        "success"
    );
    assert_eq!(f.driver(0).page().typed, vec!["literal".to_owned()]);
}

#[tokio::test]
async fn input_escape_is_uncertain_not_replayed_and_closes_profile() {
    let f = fixture();
    f.call(json!({"action":"read"})).await;
    f.driver(0).page().escape_on_click = true;
    let result = f.call(json!({"action":"click","selector":"#button"})).await;
    assert_eq!(result["status"], "uncertain", "{result}");
    assert_eq!(f.driver(0).page().clicks, 1);
    assert!(f.driver(0).page().closed);
    // The next call opens a fresh browser.
    f.call(json!({"action":"read"})).await;
    assert_eq!(f.started(), 2);
}

#[tokio::test]
async fn artifacts_are_real_private_pngs_owner_checked_and_bounded() {
    let f = fixture();
    let shot = f.call(json!({"action":"screenshot"})).await;
    assert_eq!(shot["status"], "success", "{shot}");
    assert_eq!(
        pair(&shot["media_type"], &shot["width"]),
        (json!("image/png"), json!(1))
    );
    assert!(!shot
        .to_string()
        .contains(&f.root.to_string_lossy().into_owned()));
    assert_eq!(f.read(&shot["artifact_id"]).ok(), Some(png()));
    let mut other = f.trusted.clone();
    other["user_id"] = json!("other");
    assert!(read_artifact(
        &f.root.join("private"),
        f.service.limits(),
        &f.base,
        &other,
        &shot["artifact_id"]
    )
    .is_err());
    assert!(matches!(
        f.read(&json!("../secret")),
        Err(Fault::Rejected(_))
    ));
    // A service bounded to 24-byte screenshots refuses the same capture.
    let small = fixture_with(Limits {
        png: 24,
        ..Limits::default()
    });
    assert_eq!(
        small.call(json!({"action":"screenshot"})).await["status"],
        "error"
    );
}

#[tokio::test]
async fn full_screenshot_folder_drops_the_oldest() {
    let f = fixture_with(Limits {
        artifacts: 3,
        ..Limits::default()
    });
    let mut shots = Vec::new();
    for _ in 0..5 {
        let shot = f.call(json!({"action":"screenshot"})).await;
        assert_eq!(shot["status"], "success", "{shot}");
        shots.push(shot["artifact_id"].clone());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let folder = f.root.join("private").join(&f.sid).join("artifacts");
    let mut kept: Vec<String> = std::fs::read_dir(&folder)
        .expect("artifacts")
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(str::to_owned)
        })
        .collect();
    kept.sort();
    let mut expected: Vec<String> = shots[2..]
        .iter()
        .map(|shot| shot.as_str().unwrap_or_default().to_owned())
        .collect();
    expected.sort();
    assert_eq!(kept, expected);
    assert!(matches!(f.read(&shots[0]), Err(Fault::Backend { .. })));
    assert_eq!(f.read(&shots[4]).ok(), Some(png()));
}

#[test]
fn origins_normalize_and_only_https_vault_origins_are_valid() {
    assert_eq!(
        origin_of("https://Example.com:443/a?b"),
        "https://example.com"
    );
    assert_eq!(
        origin_of("http://example.com:8080/"),
        "http://example.com:8080"
    );
    assert_eq!(origin_of("ftp://example.com/"), "");
    assert!(valid_origin(&json!("https://example.com")));
    assert!(!valid_origin(&json!("http://example.com")));
    assert!(!valid_origin(&json!("https://example.com/")));
}

#[tokio::test]
async fn vault_values_go_only_into_sign_in_fields_on_their_site_and_never_come_back() {
    let f = fixture();
    let value = format!("Pa55-{}", "PRIVATE");
    let trusted = f.signed_out(json!({"secrets":{
        "site_password":{"value":value,"origin":"https://example.com"},
        "site_totp":{"value":"GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ","origin":"https://example.com"}}}));
    let read = || f.tool("browser_read", json!({"action":"read"}), &trusted);
    let input = |args: Value| f.tool("browser_input", args, &trusted);
    let opened = f
        .tool(
            "browser_read",
            json!({"action":"navigate","url":"https://example.com/login"}),
            &trusted,
        )
        .await;
    assert_eq!(opened["status"], "success", "{opened}");
    let both =
        input(json!({"action":"type","selector":"#password","text":"x","vault":"site_password"}))
            .await;
    assert!(output(&both).contains("literal text or a vault"));
    let loose = input(json!({"action":"type","selector":"#user","text":"me","totp":true})).await;
    assert!(output(&loose).contains("goes with a vault"));
    read().await;
    let search = input(json!({"action":"type","selector":"#search","vault":"site_password"})).await;
    assert!(output(&search).contains("sign-in field"));
    read().await;
    let missing = input(json!({"action":"type","selector":"#password","vault":"missing"})).await;
    assert!(output(&missing).contains("unavailable"));
    read().await;
    let typed =
        input(json!({"action":"type","selector":"#password","vault":"site_password"})).await;
    assert_eq!(
        pair(&typed["status"], &typed["vault"]),
        (json!("success"), json!("site_password")),
        "{typed}"
    );
    let driver = f.last();
    assert_eq!(driver.page().typed.last(), Some(&value));
    read().await;
    let code =
        input(json!({"action":"type","selector":"#code","vault":"site_totp","totp":true})).await;
    assert_eq!(code["status"], "success", "{code}");
    let last = driver.page().typed.last().cloned().unwrap_or_default();
    assert!(last.len() == 6 && last.bytes().all(|b| b.is_ascii_digit()));
    // A page that echoes the value anywhere has it scrubbed, in plain and URL-encoded form.
    driver.page().log = Some(vec![
        json!({"level":"SEVERE","message":format!("bad login for {value} / Pa55-PRIVATE")}),
    ]);
    let echoed = read().await;
    assert!(!echoed.to_string().contains(&value));
    assert!(echoed["page_errors"][0]
        .as_str()
        .unwrap_or_default()
        .contains("[redacted]"));
    // Another site never receives it.
    f.tool(
        "browser_read",
        json!({"action":"navigate","url":"https://other.example.org/login"}),
        &trusted,
    )
    .await;
    read().await;
    let elsewhere =
        input(json!({"action":"type","selector":"#password","vault":"site_password"})).await;
    assert!(output(&elsewhere).contains("only for https://example.com"));
    let http = f.signed_out(json!({"secrets":{"x":{"value":"v","origin":"http://example.com"}}}));
    assert!(validate_trusted(&f.base, &http).is_err());
}

#[tokio::test]
async fn saved_sign_in_is_kept_restored_and_signs_in_again() {
    let owned = fixture();
    let f = &owned;
    let value = format!("Pa55-{}", "PRIVATE");
    let trusted = f.signed_out(
        json!({"secrets":{"site_password":{"value":value,"origin":"https://example.com"}}}),
    );
    let read = || f.tool("browser_read", json!({"action":"read"}), &trusted);
    let input = |args: Value| f.tool("browser_input", args, &trusted);
    let navigate = |url: &str, site: Option<&Value>| {
        let mut with = trusted.clone();
        if let Some(site) = site {
            with["site"] = site.clone();
        }
        let args = json!({"action":"navigate","url":url});
        async move { f.tool("browser_read", args, &with).await }
    };
    navigate("https://example.com/login", None).await;
    read().await;
    input(json!({"action":"type","selector":"#user","text":"me@example.com"})).await;
    read().await;
    input(json!({"action":"type","selector":"#password","vault":"site_password"})).await;
    read().await;
    input(json!({"action":"click","selector":"#submit"})).await;
    let driver = f.last();
    driver.page().jar = vec![
        json!({"name":"sid","value":"abc","domain":".example.com","path":"/","secure":true,
            "session":true,"expires":-1,"sameSite":"Lax"}),
        json!({"name":"tracker","value":"t","domain":"ads.test","path":"/"}),
    ];
    let wrong = input(json!({"action":"save_login","origin":"https://example.org"})).await;
    assert!(output(&wrong).contains("not on https://example.org"));
    let saved = input(json!({"action":"save_login","origin":"https://example.com"})).await;
    assert_eq!(saved["status"], "success", "{saved}");
    assert_eq!(
        pair(&saved["cookies"], &saved["steps"]),
        (json!(1), json!(3))
    );
    let login = saved["site_login"].clone();
    assert_eq!(
        login["cookies"],
        json!([{"name":"sid","value":"abc","domain":".example.com","path":"/","secure":true,
            "sameSite":"Lax"}])
    );
    assert_eq!(
        login["steps"][1],
        json!({"action":"type","selector":"#password","vault":"site_password","totp":false})
    );
    assert_eq!(login["login_url"], "https://example.com/login");
    assert!(!saved.to_string().contains(&value));
    // Opening another site clears the cookies; opening the saved site loads its own.
    navigate("https://other.example.org/", None).await;
    assert!(driver.page().jar.is_empty());
    let restored = navigate("https://example.com/account", Some(&login)).await;
    assert_eq!(
        pair(&restored["signed_in"], &restored["site_login"]),
        (json!(true), json!("restored")),
        "{restored}"
    );
    assert_eq!(json!(driver.page().jar), login["cookies"]);
    // Signed out by the site: the saved steps sign in again, and the new cookies come back
    // for the API to keep.
    navigate("https://other.example.org/", None).await;
    driver.page().typed.clear();
    driver.page().signin_until_typed = Some(value.clone());
    let again = navigate("https://example.com/account", Some(&login)).await;
    assert_eq!(again["status"], "success", "{again}");
    assert_eq!(
        pair(&again["site_login"], &again["url"]),
        (
            json!("signed_in_again"),
            json!("https://example.com/account")
        )
    );
    assert_eq!(
        driver.page().typed,
        vec!["me@example.com".to_owned(), value.clone()]
    );
    assert!(again.get("site_cookies").is_some());
    assert!(!again.to_string().contains(&value));
    navigate("https://other.example.org/", None).await;
    driver.page().signin_until_typed = None;
    driver.page().signin = true;
    let failed = navigate("https://example.com/account", Some(&login)).await;
    assert_eq!(failed["error_kind"], "signin_failed", "{failed}");
    navigate("https://other.example.org/", None).await;
    let mut no_steps = login.clone();
    no_steps["steps"] = json!([]);
    let expired = navigate("https://example.com/", Some(&no_steps)).await;
    assert_eq!(
        pair(&expired["signed_in"], &expired["site_login"]),
        (json!(false), json!("expired")),
        "{expired}"
    );
    let signed_in = f
        .call(json!({"action":"save_login","origin":"https://example.com"}))
        .await;
    assert!(output(&signed_in).contains("for external sites"));
}

#[tokio::test]
async fn rotation_expiry_unknown_fields_and_errors_never_echo_cookie() {
    let mut f = fixture();
    f.call(json!({"action":"navigate","url":"/app"})).await;
    let original = f.driver(0);
    f.trusted["cookie"] = json!(uuid::Uuid::new_v4().to_string());
    f.trusted["generation"] = json!("second");
    assert_eq!(f.call(json!({"action":"read"})).await["status"], "success");
    assert!(original.page().closed);
    f.last().page().failure = true;
    let result = f.call(json!({"action":"click","selector":"#button"})).await;
    assert!(!result.to_string().contains(&f.cookie()));
    let evaluate = f
        .call(json!({"action":"evaluate","js":"return document.cookie"}))
        .await;
    assert_eq!(evaluate["status"], "error");
    let foreign = f.call(json!({"action":"read","user_id":"foreign"})).await;
    assert_eq!(foreign["status"], "error");
    f.trusted["expires_at"] = json!(super::now() - 1.0);
    assert_eq!(f.call(json!({"action":"read"})).await["status"], "error");
}

#[tokio::test]
async fn signed_in_navigation_waits_for_boot_and_names_where_it_stopped() {
    let f = fixture_with(Limits {
        ready_ms: 300,
        ..Limits::default()
    });
    assert_eq!(
        f.call(json!({"action":"navigate","url":"/app"})).await["ready"],
        true
    );
    f.last().page().ready = "signed_out".into();
    let result = f
        .call(json!({"action":"navigate","url":"/app?tab=builds"}))
        .await;
    assert_eq!(result["error_kind"], "app_signed_out", "{result}");
    f.last().page().ready = "booting".into();
    let result = f
        .call(json!({"action":"navigate","url":"/app?tab=builds"}))
        .await;
    assert_eq!(result["error_kind"], "app_boot_stalled", "{result}");
    assert!(result.get("page_errors").is_some());
    // API routes are JSON, not the app, so they do not wait for boot.
    let me = f
        .call(json!({"action":"navigate","url":"/api/v3/auth/me"}))
        .await;
    assert!(me.get("ready").is_none(), "{me}");
}

#[tokio::test]
async fn crashed_tab_is_named_and_closed_and_other_failures_name_their_type() {
    let f = fixture();
    f.call(json!({"action":"navigate","url":"/app"})).await;
    let crashed = f.last();
    crashed.page().crash = true;
    let result = f.call(json!({"action":"read"})).await;
    assert_eq!(result["error_kind"], "browser_crashed", "{result}");
    assert!(crashed.page().closed);
    f.call(json!({"action":"navigate","url":"/app"})).await;
    f.last().on_script(Box::new(|source, _| {
        source
            .contains("V3_READ")
            .then(|| Err(Fault::backend("KeyError")))
    }));
    let result = f.call(json!({"action":"read"})).await;
    assert_eq!(result["error_kind"], "browser_backend_failed");
    assert!(output(&result).contains("(KeyError)"));
    assert!(!result.to_string().contains(&f.cookie()));
}

#[tokio::test]
async fn recorder_has_real_bounded_frames_and_cancellation_closes() {
    let f = fixture();
    let start = f
        .service
        .record(&json!({"action":"start"}), &f.base, &f.trusted)
        .await;
    assert_eq!(start["recording"], true, "{start}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let stopped = f
        .service
        .record(&json!({"action":"stop"}), &f.base, &f.trusted)
        .await;
    assert_eq!(stopped["recording"], false, "{stopped}");
    assert!(stopped["frames"].as_u64() >= Some(1));
    assert_eq!(
        pair(&stopped["video"], &stopped["format"]),
        (json!(false), json!("bounded_png_frames"))
    );
    assert_eq!(
        f.read(&stopped["artifacts"][0]["artifact_id"]).ok(),
        Some(png())
    );
    let driver = f.last();
    f.service.close_all(&f.base).await;
    assert!(driver.page().closed);
    // Closed: a read opens a new browser.
    f.call(json!({"action":"read"})).await;
    assert_eq!(f.started(), 2);
}

#[tokio::test]
async fn identity_mismatch_and_profile_symlink_fail_closed() {
    let f = fixture();
    lock(&f.start).identity = Some("another-user".into());
    let result = f.call(json!({"action":"navigate","url":"/app"})).await;
    assert_eq!(
        result["error_kind"], "browser_authentication_failed",
        "{result}"
    );
    assert!(f.last().page().closed);
    let folder = f.root.join("private").join(&f.sid).join("chrome");
    let _ = std::fs::remove_dir_all(&folder);
    std::os::unix::fs::symlink(&f.base, &folder).expect("symlink");
    let linked = f.call(json!({"action":"navigate","url":"/app"})).await;
    assert_eq!(linked["status"], "error");
    assert_eq!(std::fs::read_dir(&f.base).expect("workspace").count(), 0);
}

#[tokio::test]
async fn startup_and_authentication_errors_are_distinct_and_scrubbed() {
    let f = fixture();
    lock(&f.start).broken = true;
    let failed = f.call(json!({"action":"navigate","url":"/app"})).await;
    assert_eq!(failed["error_kind"], "browser_startup_failed", "{failed}");
    assert!(!failed.to_string().contains(&f.cookie()));
    {
        let mut start = lock(&f.start);
        start.broken = false;
        start.identity = Some("other".into());
    }
    let failed = f.call(json!({"action":"navigate","url":"/app"})).await;
    assert_eq!(failed["error_kind"], "browser_authentication_failed");
    assert!(f.last().page().closed);
}

/// The session process's frame loop over in-memory pipes.
async fn frames(f: &Fixture, requests: &[Value]) -> (i32, Vec<Value>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut to_session, input) = tokio::io::duplex(1 << 20);
    let (output, mut from_session) = tokio::io::duplex(1 << 20);
    let session = tokio::spawn(super::session::serve(input, output, Arc::clone(&f.service)));
    let mut text = String::new();
    for request in requests {
        text.push_str(&request.to_string());
        text.push('\n');
    }
    to_session.write_all(text.as_bytes()).await.expect("send");
    drop(to_session);
    let code = session.await.expect("session");
    let mut replies = String::new();
    from_session
        .read_to_string(&mut replies)
        .await
        .expect("replies");
    let replies = replies
        .lines()
        .map(|line| serde_json::from_str(line).expect("reply is JSON"))
        .collect();
    (code, replies)
}

fn frame(f: &Fixture, tool: &str, arguments: Value, trusted: &Value) -> Value {
    json!({"protocol":1,"invocation_id":"fixture","tool":tool,"arguments":arguments,
        "base":f.base.to_string_lossy(),"trusted":trusted})
}

#[tokio::test]
async fn session_frames_reply_in_order_and_close_ends_the_session() {
    let f = fixture();
    let (code, replies) = frames(
        &f,
        &[
            frame(
                &f,
                "browser_read",
                json!({"action":"navigate","url":"/app"}),
                &f.trusted,
            ),
            frame(
                &f,
                "browser_read",
                json!({"action":"screenshot"}),
                &f.trusted,
            ),
            frame(&f, "close", json!({}), &f.trusted),
            frame(&f, "browser_read", json!({"action":"read"}), &f.trusted),
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(replies.len(), 3, "a close ends the session");
    assert_eq!(
        pair(&replies[0]["protocol"], &replies[0]["invocation_id"]),
        (json!(1), json!("fixture"))
    );
    assert_eq!(
        replies[0]["implementation_version"],
        super::IMPLEMENTATION_VERSION
    );
    assert_eq!(replies[0]["result"]["status"], "success", "{}", replies[0]);
    assert_eq!(
        replies[2]["result"],
        json!({"status":"success","closed":true})
    );
    assert!(f.driver(0).page().closed);
}

#[tokio::test]
async fn session_frames_reject_unknown_tools_and_other_owners() {
    let f = fixture();
    let (code, replies) = frames(&f, &[frame(&f, "shell", json!({}), &f.trusted)]).await;
    assert_eq!(
        (code, replies.len()),
        (1, 0),
        "an unknown tool breaks the transport"
    );
    let mut other = f.trusted.clone();
    other["generation"] = json!("second");
    let (code, replies) = frames(
        &f,
        &[
            frame(
                &f,
                "browser_read",
                json!({"action":"navigate","url":"/app"}),
                &f.trusted,
            ),
            frame(&f, "browser_read", json!({"action":"read"}), &other),
        ],
    )
    .await;
    assert_eq!(
        (code, replies.len()),
        (0, 1),
        "another owner ends the session unanswered"
    );
    let mut expired = f.trusted.clone();
    expired["expires_at"] = json!(super::now() - 1.0);
    let (code, _) = frames(
        &f,
        &[frame(
            &f,
            "browser_read",
            json!({"action":"read"}),
            &expired,
        )],
    )
    .await;
    assert_eq!(code, 1);
    let (code, _) = frames(
        &f,
        &[frame(
            &f,
            "administrator_artifact_get",
            json!({"artifact_id":"../x"}),
            &f.trusted,
        )],
    )
    .await;
    assert_eq!(
        code, 1,
        "a refused artifact read breaks the transport, as in Python"
    );
}

#[test]
fn trusted_authority_rules() {
    let sid = "2244991a-c2e3-415b-ae5c-054570c91d32";
    let base = Path::new("/workspaces").join(sid);
    let good = json!({"session_id":sid,"user_id":"admin","agent_id":"shared_default",
        "cookie":"9b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d","generation":"g",
        "expires_at":super::now() + 60.0,"allowed_session_ids":[sid]});
    assert!(validate_trusted(&base, &good).is_ok());
    let rejected = |change: &dyn Fn(&mut Value), message: &str| {
        let mut trusted = good.clone();
        change(&mut trusted);
        let result = validate_trusted(&base, &trusted);
        assert!(
            matches!(&result, Err(Fault::Rejected(text)) if text == message),
            "{message}: {result:?}"
        );
    };
    rejected(
        &|t| t["session_id"] = json!(sid.to_uppercase()),
        "Browser session ownership mismatch",
    );
    rejected(&|t| t["user_id"] = json!("Admin"), "Invalid trusted caller");
    rejected(&|t| t["agent_id"] = json!(""), "Invalid trusted agent");
    rejected(
        &|t| t["cookie"] = json!("x"),
        "Invalid trusted authentication",
    );
    rejected(
        &|t| t["generation"] = json!(""),
        "Invalid trusted generation",
    );
    rejected(
        &|t| t["expires_at"] = json!(super::now() + 700.0),
        "Browser authorization expired or exceeds lease",
    );
    rejected(
        &|t| t["expires_at"] = json!(true),
        "Browser authorization expired or exceeds lease",
    );
    rejected(
        &|t| t["allowed_session_ids"] = json!([sid, sid]),
        "Browser routes require the current session",
    );
    rejected(
        &|t| t["secrets"] = Value::Null,
        "Invalid trusted vault values",
    );
    rejected(
        &|t| {
            t["site"] = json!({"origin":"https://example.com","cookies":[],"steps":[],
                "login_url":"https://example.com/"});
        },
        "Invalid trusted site sign-in",
    );
    rejected(
        &|t| {
            if let Some(t) = t.as_object_mut() {
                t.remove("cookie");
            }
        },
        "Invalid trusted site policy",
    );
    assert!(matches!(
        validate_trusted(&base, &json!([])),
        Err(Fault::Rejected(text)) if text == "Trusted browser authorization required"
    ));
}

#[test]
fn private_directories_are_owner_bound() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let sid = uuid::Uuid::new_v4().to_string();
    let first = super::service::directory(dir.path(), &sid, "admin").expect("created");
    assert!(first.join("owner.json").exists() && first.join("artifacts").is_dir());
    assert!(matches!(
        super::service::directory(dir.path(), &sid, "someone"),
        Err(Fault::Rejected(text)) if text == "Private browser belongs to another caller"
    ));
}

#[tokio::test]
async fn script_runs_steps_and_returns_only_marked_screenshots() {
    let f = fixture();
    let script = f
        .tool(
            "browser_read",
            json!({"action":"script","steps":[
                {"action":"navigate","url":"/api/v3/auth/me"},
                {"action":"read"},
                {"action":"read","screenshot":true,"label":"Option A · phone"}]}),
            &f.trusted,
        )
        .await;
    assert_eq!(script["status"], "success", "{script}");
    assert_eq!(script["steps"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        script["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{script}"
    );
    assert_eq!(script["steps"][2]["screenshot"], 0);
    assert_eq!(script["artifacts"][0]["label"], "Option A · phone");
    assert!(script["steps"][1].get("session_id").is_none());
    // Input steps need browser_input; a bad step stops the script before it runs.
    let refused = f
        .tool(
            "browser_read",
            json!({"action":"script","steps":[{"action":"click","selector":"button"}]}),
            &f.trusted,
        )
        .await;
    assert_eq!(
        refused["error_kind"], "browser_invalid_request",
        "{refused}"
    );
    let empty = f
        .tool(
            "browser_read",
            json!({"action":"script","steps":[]}),
            &f.trusted,
        )
        .await;
    assert_eq!(empty["error_kind"], "browser_invalid_request");
    // A failed step stops the script and reports which one.
    let failed = f
        .tool(
            "browser_read",
            json!({"action":"script","steps":[
                {"action":"read"},
                {"action":"navigate","url":"/api/v3/admin/secrets"},
                {"action":"read"}]}),
            &f.trusted,
        )
        .await;
    assert_eq!(failed["failed_step"], 1, "{failed}");
    assert_eq!(failed["steps"].as_array().map(Vec::len), Some(1));
}
