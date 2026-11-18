//! Saved UI elements: small pages an agent builds in its sandbox and keeps across its
//! sessions. Each is one folder in the agent's folder holding `index.html` and
//! `element.json` (name, title, description, revision, times, source session):
//!
//! - `agent_data/<agent>/ui_elements/users/<user>/<name>/`: one user's elements with
//!   this agent, never seen by another user.
//! - `agent_data/<agent>/ui_elements/users/<user>/.proposals/<id>/`: proposals waiting
//!   for that user's approval on the agent's Artifacts page.
//! - `agent_data/<agent>/ui_elements/shared/<name>/`: agent-wide elements an operator
//!   shared with every user of the agent.
//!
//! Nothing here renders or runs an element: the chat shows it in a sandboxed frame with no
//! network and no app access. Limits come from the ability's `elements.json`.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A short name part no other call in this process uses.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let digest = sha2::Sha256::digest(
        format!(
            "{nanos}:{}:{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
        .as_bytes(),
    );
    hex::encode(&digest[..4])
}

/// Where an element lives: the caller's own, or shared with every user of the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Mine,
    Shared,
}

impl Scope {
    /// `mine` or `shared`.
    ///
    /// # Errors
    /// For any other text.
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "mine" => Ok(Self::Mine),
            "shared" => Ok(Self::Shared),
            _ => bail!("scope must be mine or shared"),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Mine => "mine",
            Self::Shared => "shared",
        }
    }
}

/// The ability's element limits (`abilities/Core/sandbox/elements.json`).
#[derive(Clone, Debug)]
pub struct Config {
    pub max_element_kb: u64,
    pub max_elements: usize,
    pub max_shared: usize,
    pub max_proposals: usize,
    pub max_params_kb: u64,
    pub prompt_countdown_seconds: u64,
    /// App routes an element may open, as path-and-query prefixes such as `/app?tab=agents`.
    pub routes: Vec<String>,
}

impl Config {
    /// Reads `elements.json`; read on every use so an edit applies at once.
    ///
    /// # Errors
    /// When the file is missing or a field is absent or of the wrong type.
    pub fn read() -> Result<Self> {
        let root = webagent_abilities::tool_definitions::root()
            .ok_or_else(|| anyhow!("UI element settings unavailable"))?;
        let path = root.join("Core/sandbox/elements.json");
        let value: Value = serde_json::from_slice(
            &std::fs::read(&path)
                .with_context(|| format!("UI element settings unavailable: {}", path.display()))?,
        )
        .context("UI element settings invalid")?;
        Self::from_value(&value)
    }

    fn from_value(value: &Value) -> Result<Self> {
        let number = |key: &str| {
            value[key]
                .as_u64()
                .ok_or_else(|| anyhow!("UI element setting {key} must be a whole number"))
        };
        let count = |key: &str| number(key).map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let routes = value["routes"]
            .as_array()
            .ok_or_else(|| anyhow!("UI element setting routes must be a list"))?
            .iter()
            .map(|route| {
                route
                    .as_str()
                    .filter(|route| route.starts_with('/') && !route.starts_with("//"))
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("Each UI element route must be an app path"))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            max_element_kb: number("max_element_kb")?.clamp(1, 512),
            max_elements: count("max_elements")?,
            max_shared: count("max_shared")?,
            max_proposals: count("max_proposals")?,
            max_params_kb: number("max_params_kb")?.clamp(1, 16),
            prompt_countdown_seconds: number("prompt_countdown_seconds")?.clamp(1, 30),
            routes,
        })
    }

    /// What the chat's element frame needs: the prompt countdown and the allowed routes.
    #[must_use]
    pub fn bridge(&self) -> Value {
        json!({"prompt_countdown_seconds":self.prompt_countdown_seconds,"routes":self.routes})
    }
}

/// An element or proposal name: 1–40 lowercase letters, digits, `-` and `_`.
#[must_use]
pub fn name_ok(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn agent_ok(agent: &str) -> bool {
    (1..=64).contains(&agent.len())
        && agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn user_ok(user: &str) -> bool {
    (1..=128).contains(&user.len())
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'))
}

/// The agent's element folder (`<root>/agent_data/<agent>/ui_elements`).
fn base(root: &Path, agent: &str) -> Result<PathBuf> {
    if !agent_ok(agent) {
        bail!("Invalid agent");
    }
    Ok(root.join("agent_data").join(agent).join("ui_elements"))
}

fn user_dir(root: &Path, agent: &str, user: &str) -> Result<PathBuf> {
    if !user_ok(user) {
        bail!("Invalid user");
    }
    Ok(base(root, agent)?.join("users").join(user))
}

fn scope_dir(root: &Path, agent: &str, user: &str, scope: Scope) -> Result<PathBuf> {
    match scope {
        Scope::Mine => user_dir(root, agent, user),
        Scope::Shared => Ok(base(root, agent)?.join("shared")),
    }
}

/// The title, description and HTML of a new element or proposal, checked against `config`.
pub struct Draft<'a> {
    pub name: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub html: &'a str,
}

impl Draft<'_> {
    fn check(&self, config: &Config) -> Result<()> {
        if !name_ok(self.name) {
            bail!("name must be 1-40 lowercase letters, digits, - or _");
        }
        if self.title.trim().is_empty() || self.title.chars().count() > 80 {
            bail!("title must be 1-80 characters");
        }
        if self.description.chars().count() > 300 {
            bail!("description must be at most 300 characters");
        }
        if self.html.trim().is_empty() {
            bail!("index.html is empty");
        }
        if self.html.len() as u64 > config.max_element_kb * 1024 {
            bail!("index.html must be at most {} KB", config.max_element_kb);
        }
        Ok(())
    }
}

fn meta_of(dir: &Path) -> Option<Value> {
    let meta: Value =
        serde_json::from_slice(&std::fs::read(dir.join("element.json")).ok()?).ok()?;
    meta["name"].as_str().filter(|name| name_ok(name))?;
    Some(meta)
}

/// Folders directly in `dir` that hold an element, sorted by name, symlinks skipped.
fn entries(dir: &Path) -> Vec<Value> {
    let mut found: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name().to_str().is_some_and(name_ok))
        .filter_map(|entry| meta_of(&entry.path()))
        .collect();
    found.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    found
}

/// Writes `meta` and `html` into a new folder `dir`, through a temporary sibling so a
/// reader never sees half an element; an existing `dir` is replaced.
fn write_element(dir: &Path, meta: &Value, html: &str) -> Result<()> {
    let parent = dir
        .parent()
        .ok_or_else(|| anyhow!("Invalid element folder"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".tmp-{}-{}",
        dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("element"),
        unique()
    ));
    std::fs::create_dir(&temporary)?;
    std::fs::write(temporary.join("index.html"), html)?;
    std::fs::write(
        temporary.join("element.json"),
        serde_json::to_vec_pretty(meta)?,
    )?;
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::rename(&temporary, dir)?;
    Ok(())
}

/// This user's elements, the shared ones and this user's pending proposals for `agent`.
///
/// # Errors
/// For an invalid agent or user.
pub fn list(root: &Path, agent: &str, user: &str) -> Result<Value> {
    let mine = user_dir(root, agent, user)?;
    Ok(
        json!({"mine":entries(&mine),"shared":entries(&base(root, agent)?.join("shared")),
        "proposals":proposals(&mine)}),
    )
}

fn proposals(mine: &Path) -> Vec<Value> {
    let mut found: Vec<Value> = std::fs::read_dir(mine.join(".proposals"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let mut meta = meta_of(&entry.path())?;
            meta["id"] = json!(entry.file_name().to_str()?);
            meta["html"] = json!(std::fs::read_to_string(entry.path().join("index.html")).ok()?);
            Some(meta)
        })
        .collect();
    found.sort_by_key(|meta| meta["proposed_at"].as_u64().unwrap_or(0));
    found
}

/// Records `draft` as a proposal from session `sid`, replacing this user's earlier pending
/// proposal of the same name. Returns its id.
///
/// # Errors
/// When the draft breaks a limit or the folder cannot be written.
pub fn propose(
    root: &Path,
    agent: &str,
    user: &str,
    sid: &str,
    draft: &Draft<'_>,
    config: &Config,
    now: u64,
) -> Result<String> {
    draft.check(config)?;
    let pending = user_dir(root, agent, user)?.join(".proposals");
    let earlier: Vec<Value> = proposals(&user_dir(root, agent, user)?);
    for old in earlier.iter().filter(|old| old["name"] == draft.name) {
        if let Some(id) = old["id"].as_str() {
            std::fs::remove_dir_all(pending.join(id))?;
        }
    }
    if earlier
        .iter()
        .filter(|old| old["name"] != draft.name)
        .count()
        >= config.max_proposals
    {
        bail!(
            "{} proposals are already waiting; ask the user to approve or reject them first",
            config.max_proposals
        );
    }
    let id = format!("{}-{}", draft.name, unique());
    let meta = json!({"name":draft.name,"title":draft.title.trim(),"description":draft.description.trim(),
        "proposed_at":now,"origin_session_id":sid,"bytes":draft.html.len()});
    write_element(&pending.join(&id), &meta, draft.html)?;
    Ok(id)
}

/// Approves (saves exactly as proposed, replacing this user's element of the same name and
/// raising its revision) or rejects proposal `id`.
///
/// # Errors
/// When the proposal is missing or saving would pass the element limit.
pub fn decide(
    root: &Path,
    agent: &str,
    user: &str,
    id: &str,
    approve: bool,
    config: &Config,
    now: u64,
) -> Result<Value> {
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
        || id.is_empty()
        || id.len() > 64
    {
        bail!("Invalid proposal");
    }
    let mine = user_dir(root, agent, user)?;
    let dir = mine.join(".proposals").join(id);
    let meta = meta_of(&dir).ok_or_else(|| anyhow!("Proposal not found"))?;
    if !approve {
        std::fs::remove_dir_all(&dir)?;
        return Ok(json!({"status":"rejected"}));
    }
    let name = meta["name"].as_str().unwrap_or_default().to_owned();
    let existing = entries(&mine);
    let earlier = existing.iter().find(|old| old["name"] == name.as_str());
    if earlier.is_none() && existing.len() >= config.max_elements {
        bail!(
            "This agent already keeps {} elements for you; delete one first",
            config.max_elements
        );
    }
    let html = std::fs::read_to_string(dir.join("index.html"))?;
    let revision = earlier
        .and_then(|old| old["revision"].as_u64())
        .unwrap_or(0)
        + 1;
    let saved = json!({"name":name,"title":meta["title"],"description":meta["description"],
        "revision":revision,"saved_at":now,"origin_session_id":meta["origin_session_id"],
        "bytes":html.len()});
    write_element(&mine.join(&name), &saved, &html)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(json!({"status":"saved","element":saved}))
}

/// One element's description and HTML.
///
/// # Errors
/// When it does not exist.
pub fn read(
    root: &Path,
    agent: &str,
    user: &str,
    scope: Scope,
    name: &str,
) -> Result<(Value, String)> {
    if !name_ok(name) {
        bail!("Element not found");
    }
    let dir = scope_dir(root, agent, user, scope)?.join(name);
    let meta = meta_of(&dir).ok_or_else(|| anyhow!("Element not found"))?;
    Ok((meta, std::fs::read_to_string(dir.join("index.html"))?))
}

/// Deletes one element (`Shared` only for an operator; the caller checks) or one of this
/// user's pending proposals of that name.
///
/// # Errors
/// When neither exists.
pub fn delete(root: &Path, agent: &str, user: &str, scope: Scope, name: &str) -> Result<()> {
    if !name_ok(name) {
        bail!("Element not found");
    }
    let dir = scope_dir(root, agent, user, scope)?.join(name);
    if meta_of(&dir).is_some() {
        std::fs::remove_dir_all(dir)?;
        return Ok(());
    }
    if scope == Scope::Mine {
        let pending = user_dir(root, agent, user)?.join(".proposals");
        let ids: Vec<String> = proposals(&user_dir(root, agent, user)?)
            .into_iter()
            .filter(|p| p["name"] == name)
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        if !ids.is_empty() {
            for id in ids {
                std::fs::remove_dir_all(pending.join(id))?;
            }
            return Ok(());
        }
    }
    bail!("Element not found")
}

/// Shares this user's element `name` with every user of the agent (copying it, so later
/// changes to the user's copy need sharing again), or stops sharing it.
///
/// # Errors
/// When the element is missing or the shared limit is reached.
pub fn share(
    root: &Path,
    agent: &str,
    user: &str,
    name: &str,
    shared: bool,
    config: &Config,
    now: u64,
) -> Result<Value> {
    if !shared {
        delete(root, agent, user, Scope::Shared, name)?;
        return Ok(json!({"status":"unshared","name":name}));
    }
    let (meta, html) = read(root, agent, user, Scope::Mine, name)?;
    let shared_dir = base(root, agent)?.join("shared");
    let existing = entries(&shared_dir);
    let earlier = existing.iter().find(|old| old["name"] == name);
    if earlier.is_none() && existing.len() >= config.max_shared {
        bail!("This agent already shares {} elements", config.max_shared);
    }
    let revision = earlier
        .and_then(|old| old["revision"].as_u64())
        .unwrap_or(0)
        + 1;
    let copy = json!({"name":name,"title":meta["title"],"description":meta["description"],
        "revision":revision,"saved_at":now,"shared_by":user,"bytes":html.len()});
    write_element(&shared_dir.join(name), &copy, &html)?;
    Ok(json!({"status":"shared","element":copy}))
}

/// Bytes of the elements and proposals `user` keeps with any agent, for their storage.
#[must_use]
pub fn user_bytes(root: &Path, user: &str) -> u64 {
    if !user_ok(user) {
        return 0;
    }
    let mut total = 0u64;
    for agent in std::fs::read_dir(root.join("agent_data"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let mine = agent.path().join("ui_elements").join("users").join(user);
        let mut pending = vec![mine];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if meta.is_dir() {
                    pending.push(entry.path());
                } else {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

/// The elements a session's sandbox mirrors read-only, as `mine/<name>` and
/// `shared/<name>` with each one's revision.
///
/// # Errors
/// For an invalid agent or user.
pub fn mirror_manifest(root: &Path, agent: &str, user: &str) -> Result<Value> {
    let mut manifest = serde_json::Map::new();
    for scope in [Scope::Mine, Scope::Shared] {
        for meta in entries(&scope_dir(root, agent, user, scope)?) {
            let name = meta["name"].as_str().unwrap_or_default();
            manifest.insert(format!("{}/{name}", scope.name()), meta["revision"].clone());
        }
    }
    Ok(Value::Object(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system temporary folder, removed when dropped.
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("ui-elements-{}", unique()));
            std::fs::create_dir_all(&dir).expect("temporary folder");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config() -> Config {
        Config::from_value(&json!({"max_element_kb":1,"max_elements":2,"max_shared":1,
            "max_proposals":2,"max_params_kb":4,"prompt_countdown_seconds":3,
            "routes":["/app?tab=agents"]}))
        .expect("config")
    }

    fn draft<'a>(name: &'a str, html: &'a str) -> Draft<'a> {
        Draft {
            name,
            title: "Preview toggle",
            description: "Turns a preview on and off",
            html,
        }
    }

    #[test]
    fn settings_refuse_routes_outside_the_app() {
        let bad = json!({"max_element_kb":1,"max_elements":2,"max_shared":1,"max_proposals":2,
            "max_params_kb":4,"prompt_countdown_seconds":3,"routes":["//evil.example"]});
        assert!(Config::from_value(&bad).is_err());
        assert_eq!(config().bridge()["prompt_countdown_seconds"], 3);
    }

    #[test]
    fn proposals_wait_for_approval_and_stay_per_user() {
        let dir = Temp::new();
        let root = dir.path();
        let config = config();
        let id = propose(
            root,
            "agent",
            "alice",
            "s1",
            &draft("toggle", "<p>a</p>"),
            &config,
            1,
        )
        .expect("proposed");
        assert!(list(root, "agent", "alice").expect("list")["mine"]
            .as_array()
            .expect("mine")
            .is_empty());
        assert!(list(root, "agent", "bob").expect("list")["proposals"]
            .as_array()
            .expect("p")
            .is_empty());
        // A second proposal of the same name replaces the first.
        let id2 = propose(
            root,
            "agent",
            "alice",
            "s1",
            &draft("toggle", "<p>b</p>"),
            &config,
            2,
        )
        .expect("proposed again");
        assert!(decide(root, "agent", "alice", &id, true, &config, 3).is_err());
        decide(root, "agent", "alice", &id2, true, &config, 3).expect("approved");
        let (meta, html) = read(root, "agent", "alice", Scope::Mine, "toggle").expect("saved");
        assert_eq!(
            (meta["revision"].as_u64(), html.as_str()),
            (Some(1), "<p>b</p>")
        );
        assert!(read(root, "agent", "bob", Scope::Mine, "toggle").is_err());
        assert_eq!(
            mirror_manifest(root, "agent", "alice").expect("manifest"),
            json!({"mine/toggle":1})
        );
        assert!(user_bytes(root, "alice") > 0);
        assert_eq!(user_bytes(root, "bob"), 0);
    }

    #[test]
    fn limits_and_names_are_enforced() {
        let dir = Temp::new();
        let config = config();
        let big = "x".repeat(2048);
        assert!(propose(
            dir.path(),
            "agent",
            "u",
            "s",
            &draft("big", &big),
            &config,
            1
        )
        .is_err());
        assert!(propose(
            dir.path(),
            "agent",
            "u",
            "s",
            &draft("../x", "<p/>"),
            &config,
            1
        )
        .is_err());
        assert!(propose(
            dir.path(),
            "../agent",
            "u",
            "s",
            &draft("x", "<p/>"),
            &config,
            1
        )
        .is_err());
        assert!(list(dir.path(), "agent", "../u").is_err());
        propose(
            dir.path(),
            "agent",
            "u",
            "s",
            &draft("a", "<p/>"),
            &config,
            1,
        )
        .expect("a");
        propose(
            dir.path(),
            "agent",
            "u",
            "s",
            &draft("b", "<p/>"),
            &config,
            1,
        )
        .expect("b");
        assert!(propose(
            dir.path(),
            "agent",
            "u",
            "s",
            &draft("c", "<p/>"),
            &config,
            1
        )
        .is_err());
    }

    #[test]
    fn sharing_copies_for_every_user_and_unsharing_removes_it() {
        let dir = Temp::new();
        let (root, config) = (dir.path(), config());
        let id = propose(
            root,
            "agent",
            "op",
            "s",
            &draft("toggle", "<p/>"),
            &config,
            1,
        )
        .expect("p");
        decide(root, "agent", "op", &id, true, &config, 2).expect("saved");
        share(root, "agent", "op", "toggle", true, &config, 3).expect("shared");
        assert!(read(root, "agent", "someone", Scope::Shared, "toggle").is_ok());
        share(root, "agent", "op", "toggle", false, &config, 4).expect("unshared");
        assert!(read(root, "agent", "someone", Scope::Shared, "toggle").is_err());
    }
}
