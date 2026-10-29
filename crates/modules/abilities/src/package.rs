//! Version-one isolated tool packages; engine admission remains authoritative.
use crate::{Catalog, Host};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::path::{Component, Path as FsPath};
use webagent_access as access;
use webagent_configuration::digest;
#[derive(Clone, Debug, serde::Serialize)]
pub struct Package {
    #[serde(skip_serializing)]
    pub root: PathBuf,
    pub manifest: Value,
    pub schemas: Vec<Value>,
    pub generation: String,
    #[serde(skip_serializing)]
    pub legacy_generation: String,
}
/// Read a bounded regular plugin input; reject symlinks and unexpected file types.
fn read(path: &FsPath, limit: u64) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
        anyhow::bail!("Unsafe plugin file");
    }
    Ok(std::fs::read(path)?)
}
/// Resolve a manifest path inside its package without allowing absolute paths or traversal.
fn relative(root: &FsPath, value: &Value) -> Result<PathBuf> {
    let name = value
        .as_str()
        .ok_or_else(|| anyhow!("Missing plugin path"))?;
    if name.is_empty()
        || FsPath::new(name)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        anyhow::bail!("Invalid plugin path");
    }
    let path = root.join(name);
    if !path.canonicalize()?.starts_with(root.canonicalize()?) {
        anyhow::bail!("Plugin path escaped package");
    }
    Ok(path)
}
/// Hash declared locked Rust inputs and the build contract to locate an independently cached artifact.
fn build_key(root: &FsPath, manifest: &Value, legacy: bool) -> Result<String> {
    let inputs = manifest["build"]["inputs"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing build inputs"))?;
    if inputs.len() > 1024
        || manifest["build"]["dependencies"] != json!([])
        || manifest["build"]["flags"] != json!([])
    {
        anyhow::bail!("Unsupported build contract");
    }
    let entry = manifest["entrypoint"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing entrypoint"))?;
    if !access::valid_id(entry) {
        anyhow::bail!("Invalid artifact name");
    }
    let mut files = std::collections::BTreeMap::new();
    for name in inputs {
        let path = relative(root, name)?;
        let name = name
            .as_str()
            .ok_or_else(|| anyhow!("Invalid build input"))?;
        if files
            .insert(
                name.to_owned(),
                hex::encode(Sha256::digest(read(&path, 16 * 1024 * 1024)?)),
            )
            .is_some()
        {
            anyhow::bail!("Duplicate build input");
        }
    }
    if !files.contains_key("Cargo.toml") || !files.contains_key("Cargo.lock") {
        anyhow::bail!("Locked standalone crate required");
    }
    let mut build = manifest["build"].clone();
    if let Some(build) = build.as_object_mut().filter(|_| !legacy) {
        build.remove("assets");
    }
    Ok(digest(
        &json!({"build":build,"entrypoint":entry,"inputs":files}),
    ))
}
/// Fingerprint declared runtime helpers/assets without changing the executable build-cache key.
fn runtime_input_hashes(
    root: &FsPath,
    manifest: &Value,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut files = std::collections::BTreeMap::new();
    if let Some(build) = manifest.get("build") {
        if !build.is_object() {
            anyhow::bail!("Invalid package build declaration");
        }
        for key in ["inputs", "assets"] {
            if let Some(values) = build.get(key) {
                let values = values
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid runtime input list"))?;
                for value in values {
                    let path = relative(root, value)?;
                    let name = value
                        .as_str()
                        .ok_or_else(|| anyhow!("Invalid runtime input"))?;
                    files.insert(
                        name.to_owned(),
                        hex::encode(Sha256::digest(read(&path, 16 * 1024 * 1024)?)),
                    );
                    if files.len() > 1024 {
                        anyhow::bail!("Runtime input bound exceeded");
                    }
                }
            }
        }
    }
    Ok(files)
}
// @code-index {"version":1,"feature":"automation-drop-in","aliases":["standalone automation","automation ability plugin"],"purpose":"Fixed automation_v1 identity, bounded schemas and content-addressed native artifact resolution.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Native host registration, activation identity and confined execution are authority invariants; settings cannot bypass them."}}
/// Fixed native bridge registration; a package cannot request another host service.
pub const AUTOMATION_TOOLS: &[&str] = &[
    "list_event_sources",
    "list_delivery_channels",
    "event_subscribe",
    "list_event_subscriptions",
    "event_unsubscribe",
    "schedule_task",
    "remind_me",
    "list_automations",
    "update_automation",
    "pause_automation",
    "resume_automation",
    "cancel_automation",
    "run_automation_now",
    "remember_automation_state",
];
/// Automation's runtime assets besides its tool definitions.
const AUTOMATION_ASSETS: [&str; 6] = [
    "automation.json",
    "automation.skill.md",
    "service/package.json",
    "service/page.json",
    "service/page.js",
    "service/page.css",
];
/// Every runtime asset, each tool's `tools/<tool>/tool.json` included, so a definition edit
/// changes the package's runtime identity without changing its build key.
fn automation_assets() -> Vec<String> {
    AUTOMATION_ASSETS
        .iter()
        .map(|asset| (*asset).to_owned())
        .chain(
            AUTOMATION_TOOLS
                .iter()
                .map(|tool| format!("tools/{tool}/tool.json")),
        )
        .collect()
}
/// The host executes this reviewed reducer; it never receives state paths or credentials.
#[derive(Clone)]
pub struct AutomationService {
    pub root: PathBuf,
    pub executable: PathBuf,
    pub descriptor: Value,
    pub implementation: String,
}
/// Validate the native Automation interface independently of installation or artifact availability.
/// Calls: build_key and confined input reads. Package declarations confer no runtime authority.
fn automation_contract(
    root: &FsPath,
    id: &str,
    tools: &[String],
    manifest: &Value,
) -> Result<String> {
    if id != "automation" || json!(tools) != json!(AUTOMATION_TOOLS) {
        anyhow::bail!("Unsupported native Automation identity");
    }
    let expected = json!({
        "id":"automation", "dispatch":"isolated", "host_interface":"automation_v1",
        "protocol":1, "interface":1, "runtime":"executable", "entrypoint":"webagent_automation",
        "effect":"write", "tools":AUTOMATION_TOOLS,
        "build":{"toolchain":"rust-1.99", "inputs":["Cargo.toml","Cargo.lock","service/main.rs","service/engine.rs","service/contracts.rs"],
            "dependencies":[], "flags":[], "assets":automation_assets()}
    });
    if manifest != &expected {
        anyhow::bail!("Unsupported native Automation contract");
    }
    let key = build_key(root, manifest, false)?;
    runtime_input_hashes(root, manifest)?;
    let descriptor: Value =
        serde_json::from_slice(&read(&root.join("service/package.json"), 64 * 1024)?)?;
    let mut operations: Vec<&str> = AUTOMATION_TOOLS.to_vec();
    operations.extend(["history", "recycle", "restore", "delete"]);
    let expected_descriptor = json!({"id":"automation","protocol":1,"executable":"webagent_automation",
        "public_operations":operations,"read_operations":["list_event_sources","list_delivery_channels","list_event_subscriptions","list_automations","history"]});
    if descriptor != expected_descriptor {
        anyhow::bail!("Unsupported Automation reducer interface");
    }
    // Tool folders define the schemas; the reducer's read operations are exactly the
    // read tools plus its history read.
    let folders = crate::tool_definitions::read_folder(root)?
        .ok_or_else(|| anyhow!("Automation tools need tool folders"))?;
    let mut reads = std::collections::BTreeSet::from(["history"]);
    for tool in &folders {
        let definition = tool
            .definition
            .as_ref()
            .map_err(|error| anyhow!("{error:#}"))?;
        if definition.is_read() {
            reads.insert(definition.name.as_str());
        }
    }
    let names: std::collections::BTreeSet<&str> =
        folders.iter().map(|tool| tool.name.as_str()).collect();
    if names != AUTOMATION_TOOLS.iter().copied().collect() {
        anyhow::bail!("Incomplete Automation tool folders");
    }
    let declared: std::collections::BTreeSet<&str> = expected_descriptor["read_operations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if reads != declared {
        anyhow::bail!("Automation tool effects disagree with its reducer");
    }
    Ok(key)
}
/// A plugin that calls back into the app through declared host requests
/// (`webagent_extensions::requests`), running only tools its ability's folders define.
/// Returns the plugin's declared requests.
fn requests_contract(
    root: &FsPath,
    id: &str,
    folders: &[String],
    manifest: &Value,
) -> Result<webagent_extensions::requests::Declaration> {
    let fixed = [
        ("id", json!(id)),
        ("dispatch", json!("isolated")),
        ("host_interface", json!("requests_v1")),
        ("protocol", json!(webagent_extensions::plugin::PROTOCOL)),
        ("interface", json!(1)),
        ("runtime", json!("executable")),
        ("effect", json!("write")),
    ];
    if fixed.iter().any(|(key, value)| &manifest[*key] != value)
        || manifest["build"]["toolchain"] != "rust-1.99"
    {
        anyhow::bail!("Unsupported plugin contract");
    }
    let known = [
        "id",
        "dispatch",
        "host_interface",
        "protocol",
        "interface",
        "runtime",
        "entrypoint",
        "effect",
        "tools",
        "requests",
        "providers",
        "build",
    ];
    if manifest
        .as_object()
        .is_none_or(|fields| fields.keys().any(|key| !known.contains(&key.as_str())))
    {
        anyhow::bail!("Unknown plugin manifest field");
    }
    let tools = manifest["tools"]
        .as_array()
        .filter(|tools| !tools.is_empty() && tools.len() <= 64)
        .ok_or_else(|| anyhow!("Plugin tools required"))?;
    let mut seen = std::collections::BTreeSet::new();
    for tool in tools {
        let tool = tool
            .as_str()
            .ok_or_else(|| anyhow!("Invalid plugin tool"))?;
        if !folders.iter().any(|folder| folder == tool) || !seen.insert(tool) {
            anyhow::bail!("Plugin tools must be distinct tools of its ability");
        }
    }
    let declaration = webagent_extensions::requests::Declaration::parse(manifest)?;
    build_key(root, manifest, false)?;
    runtime_input_hashes(root, manifest)?;
    Ok(declaration)
}

/// A validated `requests_v1` plugin: its tools, declared requests, declared external
/// providers and artifact identity.
pub struct RequestsPlugin {
    pub tools: Vec<String>,
    pub requests: webagent_extensions::requests::Declaration,
    pub providers: webagent_extensions::providers::Providers,
    key: String,
    entry: String,
}

impl RequestsPlugin {
    /// Read and validate the `requests_v1` plugin in `root`, an ability folder of one
    /// published revision whose tool folders are `folders`.
    pub fn load(root: &FsPath, id: &str, folders: &[String]) -> Result<Self> {
        let manifest: Value = serde_json::from_slice(&read(&root.join("plugin.json"), 64 * 1024)?)?;
        let requests = requests_contract(root, id, folders, &manifest)?;
        let providers = webagent_extensions::providers::Providers::parse(&manifest, &requests)?;
        let entry = manifest["entrypoint"]
            .as_str()
            .filter(|entry| access::valid_id(entry))
            .ok_or_else(|| anyhow!("Invalid plugin entrypoint"))?
            .to_owned();
        let tools = manifest["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.as_str().map(str::to_owned))
            .collect();
        let key = build_key(root, &manifest, false)?;
        Ok(Self {
            tools,
            requests,
            providers,
            key,
            entry,
        })
    }

    /// The plugin's content-addressed executable under `artifacts`, if it was built.
    pub fn executable(&self, artifacts: &FsPath) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let executable = artifacts.join(&self.key).join(&self.entry);
        let meta = std::fs::symlink_metadata(&executable)?;
        let mode = meta.permissions().mode();
        if !meta.is_file() || mode & 0o111 == 0 || mode & 0o022 != 0 {
            anyhow::bail!("Plugin artifact unavailable");
        }
        Ok(executable)
    }
}

/// Resolve a content-addressed artifact from the same immutable ability revision as its assets.
/// Missing/invalid artifacts withdraw the service; there is no image-distribution fallback.
pub fn automation_service(root: &FsPath) -> Result<AutomationService> {
    automation_service_at(root, FsPath::new("/opt/v3/assets/artifacts"))
}
/// Keep the artifact registry explicit for private contract tests; callers never supply it through an API.
mod automation_cache;
fn automation_service_at(root: &FsPath, artifacts: &FsPath) -> Result<AutomationService> {
    automation_cache::get(root, artifacts)
}
fn load_automation_service_at(root: &FsPath, artifacts: &FsPath) -> Result<AutomationService> {
    use std::os::unix::fs::PermissionsExt;
    let manifest: Value = serde_json::from_slice(&read(&root.join("plugin.json"), 64 * 1024)?)?;
    let declaration: Value =
        serde_json::from_slice(&read(&root.join("automation.json"), 64 * 1024)?)?;
    let tools: Vec<String> = serde_json::from_value(declaration["tools"].clone())?;
    if declaration["required_module"] != "automation" {
        anyhow::bail!("Missing Automation host binding");
    }
    let key = automation_contract(root, "automation", &tools, &manifest)?;
    let directory = artifacts.join(&key);
    let meta = std::fs::symlink_metadata(&directory)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || !directory
            .canonicalize()?
            .starts_with(artifacts.canonicalize()?)
    {
        anyhow::bail!("Unsafe Automation artifact directory");
    }
    let executable = directory.join("webagent_automation");
    let code = read(&executable, 32 * 1024 * 1024)?;
    let mode = std::fs::symlink_metadata(&executable)?.permissions().mode();
    if mode & 0o111 == 0 || mode & 0o022 != 0 {
        anyhow::bail!("Unsafe Automation executable");
    }
    let descriptor: Value =
        serde_json::from_slice(&read(&root.join("service/package.json"), 64 * 1024)?)?;
    let implementation = digest(&json!([
        key,
        hex::encode(Sha256::digest(code)),
        runtime_input_hashes(root, &manifest)?
    ]));
    Ok(AutomationService {
        root: root.join("service"),
        executable,
        descriptor,
        implementation,
    })
}

impl Package {
    /// Validate protocol, ownership, schema subset, and executable generation before catalog installation.
    pub fn load<H: Host>(root: &FsPath, id: &str, tools: &[String]) -> Result<Option<Self>> {
        Self::load_for_scheme::<H>(root, id, tools, false)
    }
    /// Preserve original artifacts for legacy durable bindings; new keys exclude runtime assets.
    fn load_for_scheme<H: Host>(
        root: &FsPath,
        id: &str,
        tools: &[String],
        legacy: bool,
    ) -> Result<Option<Self>> {
        let path = root.join("plugin.json");
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let bytes = read(&path, 64 * 1024)?;
        let manifest: Value = serde_json::from_slice(&bytes)?;
        // Plugins with a host interface run through the app's own dispatch, never as
        // generic isolated packages.
        if let Some(interface) = manifest.get("host_interface") {
            match interface.as_str() {
                Some("automation_v1") => {
                    automation_contract(root, id, tools, &manifest)?;
                }
                Some("requests_v1") => {
                    requests_contract(root, id, tools, &manifest)?;
                }
                _ => anyhow::bail!("Unsupported plugin host interface"),
            }
            return Ok(None);
        }
        // Existing native package adapters retain their own contract.
        if matches!(id, "codebase_admin" | "diagnostics" | "git_control")
            && manifest.get("dispatch").is_none()
        {
            if id == "git_control" {
                let expected = json!({"version":1,"id":"git_control","protocol":1,"interface":1,"runtime":"python3","entrypoint":"transport.py",
                    "tools":tools,"build":{"inputs":["adapter.py","transport.py","file_manager.py","askpass.py"],
                    "dependencies":[],"toolchain":"image-python3","flags":["-I","-B"],
                    "assets":["git_control.json","git_control.skill.md","tools"]}});
                if manifest != expected {
                    anyhow::bail!("Unsupported native Git Control contract");
                }
                for name in [
                    "adapter.py",
                    "transport.py",
                    "file_manager.py",
                    "askpass.py",
                ] {
                    read(&root.join(name), 2 * 1024 * 1024)?;
                }
            }
            if id == "diagnostics" {
                let expected = json!({"version":1,"id":"diagnostics","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py",
                    "tools":tools,"build":{"inputs":["runner.py","adapter.py"],"dependencies":[],"toolchain":"image-python3","flags":["-I","-B"],
                    "assets":["diagnostics.json","diagnostics.skill.md","tools","skills"]}});
                if manifest != expected {
                    anyhow::bail!("Unsupported native Diagnostics contract");
                }
                for name in ["runner.py", "adapter.py"] {
                    read(&root.join(name), 2 * 1024 * 1024)?;
                }
            }
            return Ok(None);
        }
        if manifest["dispatch"] != "isolated"
            || manifest["protocol"].as_u64() != Some(1)
            || manifest["interface"].as_u64() != Some(1)
            || manifest["id"] != id
            || manifest["tools"] != json!(tools)
            || !matches!(manifest["runtime"].as_str(), Some("python3" | "executable"))
            || manifest["effect"] != "write"
        {
            anyhow::bail!("Unsupported isolated plugin contract");
        }
        let executable = if manifest["build"]["toolchain"] == "rust-1.99" {
            let build_key = build_key(root, &manifest, legacy)?;
            PathBuf::from("/opt/v3/assets/artifacts")
                .join(build_key)
                .join(
                    manifest["entrypoint"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Missing executable"))?,
                )
        } else {
            relative(root, &manifest["entrypoint"])?
        };
        let code = read(&executable, 32 * 1024 * 1024)?;
        if manifest["runtime"] == "executable" {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&executable)?.permissions().mode() & 0o111 == 0 {
                anyhow::bail!("Plugin is not executable");
            }
        }
        let schema_bytes = read(&relative(root, &manifest["schemas"])?, 256 * 1024)?;
        let schemas: Vec<Value> = serde_json::from_slice(&schema_bytes)?;
        let mut names = std::collections::BTreeSet::new();
        let native = H::native_schemas()
            .into_iter()
            .chain(H::mcp_schemas())
            .collect::<Vec<_>>();
        for schema in &schemas {
            let name = schema["name"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing schema name"))?;
            if !tools.iter().any(|t| t == name)
                || !names.insert(name)
                || native.iter().any(|s| s["name"] == name)
                || crate::is_discovery(name)
                || matches!(
                    name,
                    "publish_main_message" | "set_closer_status" | "codebase_admin_protocol"
                )
                || ["administrator_", "v3_", "ability_plugin", "mcp_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                || schema["type"] != "function"
                || schema["description"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.len() > 8192)
                || schema["parameters"]["type"] != "object"
            {
                anyhow::bail!("Invalid plugin schema owner");
            }
            validate_schema(&schema["parameters"], 0)?;
        }
        if names.len() != tools.len() || tools.is_empty() {
            anyhow::bail!("Incomplete plugin schemas");
        }
        let hashes = json!([
            hex::encode(Sha256::digest(bytes)),
            hex::encode(Sha256::digest(code)),
            hex::encode(Sha256::digest(schema_bytes))
        ]);
        let legacy_generation = digest(&hashes);
        let generation = digest(&json!([hashes, runtime_input_hashes(root, &manifest)?]));
        Ok(Some(Self {
            root: root.to_owned(),
            manifest,
            schemas,
            generation,
            legacy_generation,
        }))
    }
    /// Check ownership against the package's validated tool schemas.
    pub fn owns(&self, name: &str) -> bool {
        self.schemas.iter().any(|s| s["name"] == name)
    }
    /// Persist the admitted implementation root, generation, and schema for one model-call tool.
    pub fn binding(&self, name: &str) -> Option<Value> {
        self.owns(name).then(|| {
            json!({"root":self.root,"generation":self.generation,"generation_scheme":2,
            "schema":self.schemas.iter().find(|s|s["name"]==name),"id":self.manifest["id"]})
        })
    }
}
/// Validate the bounded closed schema subset supported by isolated protocol 1.
fn validate_schema(schema: &Value, depth: usize) -> Result<()> {
    let Some(object) = schema.as_object().filter(|_| depth <= 24) else {
        anyhow::bail!("Invalid plugin parameter schema");
    };
    let allowed = [
        "type",
        "description",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
    ];
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        anyhow::bail!("Unsupported plugin schema keyword");
    }
    if !matches!(
        schema["type"].as_str(),
        Some("object" | "array" | "string" | "integer" | "number" | "boolean" | "null")
    ) {
        anyhow::bail!("Unsupported plugin schema type");
    }
    if let Some(values) = schema.get("enum") {
        if values
            .as_array()
            .is_none_or(|v| v.is_empty() || v.len() > 128)
        {
            anyhow::bail!("Invalid enum");
        }
    }
    for key in ["minimum", "maximum"] {
        if schema.get(key).is_some_and(|v| !v.is_number()) {
            anyhow::bail!("Invalid numeric bound");
        }
    }
    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if schema.get(key).is_some_and(|v| v.as_u64().is_none()) {
            anyhow::bail!("Invalid length bound");
        }
    }
    if schema["type"] == "object" {
        let props = schema["properties"]
            .as_object()
            .ok_or_else(|| anyhow!("Object properties required"))?;
        if props.len() > 128 || schema["additionalProperties"] != false {
            anyhow::bail!("Closed plugin object required");
        }
        for (name, s) in props {
            if !access::valid_id(name) {
                anyhow::bail!("Invalid parameter name");
            }
            validate_schema(s, depth + 1)?;
        }
        if let Some(required) = schema.get("required") {
            let list = required
                .as_array()
                .ok_or_else(|| anyhow!("Invalid required list"))?;
            let mut seen = std::collections::BTreeSet::new();
            for name in list {
                let name = name
                    .as_str()
                    .ok_or_else(|| anyhow!("Invalid required key"))?;
                if !props.contains_key(name) || !seen.insert(name) {
                    anyhow::bail!("Invalid required key");
                }
            }
        }
    } else if schema["type"] == "array" {
        validate_schema(&schema["items"], depth + 1)?;
    }
    Ok(())
}
/// Check invocation arguments against the admitted schema before any worker effect.
fn accepts(schema: &Value, value: &Value, depth: usize) -> bool {
    if depth > 24 {
        return false;
    }
    // validate_schema admitted only array enums; anything else accepts nothing.
    if schema
        .get("enum")
        .is_some_and(|e| e.as_array().is_none_or(|e| !e.contains(value)))
    {
        return false;
    }
    let in_range = |actual: u64, min: &str, max: &str| {
        schema[min].as_u64().is_none_or(|v| actual >= v)
            && schema[max].as_u64().is_none_or(|v| actual <= v)
    };
    match schema["type"].as_str() {
        Some("object") => value.as_object().is_some_and(|v| {
            // A schema without an object of properties accepts no object.
            let Some(props) = schema["properties"].as_object() else {
                return false;
            };
            v.iter()
                .all(|(k, v)| props.get(k).is_some_and(|s| accepts(s, v, depth + 1)))
                && schema["required"].as_array().is_none_or(|keys| {
                    keys.iter()
                        .all(|k| k.as_str().is_some_and(|k| v.contains_key(k)))
                })
        }),
        Some("array") => value.as_array().is_some_and(|v| {
            in_range(v.len() as u64, "minItems", "maxItems")
                && v.iter().all(|v| accepts(&schema["items"], v, depth + 1))
        }),
        Some("string") => value
            .as_str()
            .is_some_and(|v| in_range(v.chars().count() as u64, "minLength", "maxLength")),
        Some("number" | "integer") => value.as_f64().is_some_and(|v| {
            v.is_finite()
                && (schema["type"] != "integer" || value.is_i64() || value.is_u64())
                && schema["minimum"].as_f64().is_none_or(|m| v >= m)
                && schema["maximum"].as_f64().is_none_or(|m| v <= m)
        }),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => false,
    }
}
/// The worker receives only this validated pinned binding after the caller's existing admission fences.
pub enum Invocation {
    Denied(Value),
    Ready(Value),
}
pub fn prepare_invocation<H: Host>(
    current: &Catalog<H>,
    name: &str,
    args: &Value,
    invocation: &str,
    binding: &Value,
) -> Result<Invocation> {
    let root = PathBuf::from(
        binding["root"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing plugin binding"))?,
    );
    let id = binding["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing plugin identity"))?;
    // Check current installation independently from the pinned implementation.
    if current.plugin_binding(name).is_none_or(|b| b["id"] != id) {
        return Ok(Invocation::Denied(
            json!({"status":"denied","output":"Plugin removed or ownership changed"}),
        ));
    }
    let manifest: Value =
        serde_json::from_slice(&read(&root.join(format!("{id}.json")), 64 * 1024)?)?;
    let tools: Vec<String> = serde_json::from_value(manifest["tools"].clone())?;
    let scheme = match binding.get("generation_scheme") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|v| matches!(v, 1 | 2))
            .ok_or_else(|| anyhow!("Unsupported binding generation scheme"))?,
    };
    let package = Package::load_for_scheme::<H>(&root, id, &tools, scheme == 1)?
        .ok_or_else(|| anyhow!("Plugin binding unavailable"))?;
    let expected = if scheme == 1 {
        &package.legacy_generation
    } else {
        &package.generation
    };
    if expected != binding["generation"].as_str().unwrap_or("")
        || !package.owns(name)
        || !accepts(&binding["schema"]["parameters"], args, 0)
    {
        return Ok(Invocation::Denied(
            json!({"status":"denied","output":"Plugin binding or arguments invalid"}),
        ));
    }
    let trusted = json!({"ability_plugin":{"root":root,"generation":expected,"generation_scheme":scheme,"id":id,"name":name,"invocation_id":invocation}});
    Ok(Invocation::Ready(trusted))
}

#[cfg(feature = "test-support")]
pub mod registered_tests {
    use super::*;
    pub mod tests {
        use super::*;
        pub fn closed_schema_rejects_unknown_and_wrong_types<H: crate::TestHost>() {
            let s = json!({"type":"object","properties":{"text":{"type":"string","maxLength":3}},"required":["text"],"additionalProperties":false});
            validate_schema(&s, 0).unwrap();
            assert!(accepts(&s, &json!({"text":"yes"}), 0));
            for v in [
                json!({}),
                json!({"text":true}),
                json!({"text":"long"}),
                json!({"text":"yes","extra":1}),
            ] {
                assert!(!accepts(&s, &v, 0));
            }
            assert!(validate_schema(
                &json!({"type":"object","properties":{},"additionalProperties":true}),
                0
            )
            .is_err());
        }
    }

    pub mod catalog_tests {
        use super::*;
        pub fn package<H: crate::TestHost>(root: &FsPath) -> PathBuf {
            let path = root.join("Custom/fixture_ability");
            std::fs::create_dir_all(path.join("skills")).unwrap();
            std::fs::write(path.join("fixture_ability.json"),serde_json::to_vec(&json!({"display_name":"Fixture","kind":"ability","description":"New drop-in ability","readiness":"ready","tools":["fixture_echo"]})).unwrap()).unwrap();
            std::fs::write(path.join("fixture_ability.skill.md"), "First guidance").unwrap();
            std::fs::write(path.join("runner.py"), "print('fixture')").unwrap();
            std::fs::write(path.join("plugin.json"),serde_json::to_vec(&json!({"id":"fixture_ability","dispatch":"isolated","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py","schemas":"schemas.json","effect":"write","tools":["fixture_echo"]})).unwrap()).unwrap();
            std::fs::write(path.join("schemas.json"),serde_json::to_vec(&json!([{"type":"function","name":"fixture_echo","description":"Echo supplied text","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}}])).unwrap()).unwrap();
            path
        }
        pub fn new_tools_and_guidance_are_loaded_without_native_registration<H: crate::TestHost>() {
            let dir = tempfile::tempdir().unwrap();
            let path = package::<H>(dir.path());
            let catalog = Catalog::<H>::load_snapshot(dir.path()).unwrap();
            assert!(catalog.known("fixture_ability"));
            assert!(catalog
                .all_schemas()
                .iter()
                .any(|s| s["name"] == "fixture_echo"));
            assert_eq!(
                catalog.configuration_skill("fixture_ability").unwrap(),
                "First guidance"
            );
            let old_version = catalog.version().to_owned();
            std::fs::write(path.join("fixture_ability.skill.md"), "Updated guidance").unwrap();
            let next = Catalog::<H>::load_snapshot(dir.path()).unwrap();
            assert_ne!(old_version, next.version());
            assert_eq!(
                next.configuration_skill("fixture_ability").unwrap(),
                "Updated guidance"
            );
            let before = next.plugin_binding("fixture_echo").unwrap();
            std::fs::write(path.join("runner.py"), "print('next')").unwrap();
            assert_ne!(
                before["generation"],
                Catalog::<H>::load_snapshot(dir.path())
                    .unwrap()
                    .plugin_binding("fixture_echo")
                    .unwrap()["generation"]
            );
        }
        pub fn helper_and_runtime_asset_updates_change_generation_while_legacy_bindings_remain_verifiable<
            H: crate::TestHost,
        >() {
            let dir = tempfile::tempdir().unwrap();
            let path = package::<H>(dir.path());
            let p = path.join("plugin.json");
            let mut manifest: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            manifest["build"] =
                json!({"inputs":["runner.py","helper.py"],"assets":["fixture_ability.skill.md"]});
            std::fs::write(path.join("helper.py"), "VALUE = 1").unwrap();
            std::fs::write(&p, serde_json::to_vec(&manifest).unwrap()).unwrap();
            let before = Package::load::<H>(&path, "fixture_ability", &["fixture_echo".into()])
                .unwrap()
                .unwrap();
            assert_eq!(
                before.binding("fixture_echo").unwrap()["generation_scheme"],
                2
            );
            std::fs::write(path.join("helper.py"), "VALUE = 2").unwrap();
            let next = Package::load::<H>(&path, "fixture_ability", &["fixture_echo".into()])
                .unwrap()
                .unwrap();
            assert_ne!(before.generation, next.generation);
            assert_eq!(before.legacy_generation, next.legacy_generation);
            std::fs::write(path.join("fixture_ability.skill.md"), "Changed asset").unwrap();
            let asset = Package::load::<H>(&path, "fixture_ability", &["fixture_echo".into()])
                .unwrap()
                .unwrap();
            assert_ne!(next.generation, asset.generation);
        }
        pub fn pending_packages_withdraw_bindings_and_cannot_declare_trusted_core_kind<
            H: crate::TestHost,
        >() {
            let dir = tempfile::tempdir().unwrap();
            let path = package::<H>(dir.path());
            let declaration = path.join("fixture_ability.json");
            let mut value: Value =
                serde_json::from_slice(&std::fs::read(&declaration).unwrap()).unwrap();
            value["readiness"] = json!("pending");
            std::fs::write(&declaration, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Catalog::<H>::load_snapshot(dir.path())
                .unwrap()
                .plugin_binding("fixture_echo")
                .is_none());
            value["readiness"] = json!("ready");
            value["kind"] = json!("virtual_core");
            std::fs::write(&declaration, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Catalog::<H>::load_snapshot(dir.path()).is_err());
        }
        pub fn runtime_asset_declarations_do_not_change_the_compile_cache_key<
            H: crate::TestHost,
        >() {
            let dir = tempfile::tempdir().unwrap();
            for name in ["Cargo.toml", "Cargo.lock", "main.rs"] {
                std::fs::write(dir.path().join(name), "fixed input").unwrap();
            }
            let mut manifest = json!({"entrypoint":"fixture","build":{"toolchain":"rust-1.99","inputs":["Cargo.toml","Cargo.lock","main.rs"],"dependencies":[],"flags":[],"assets":[]}});
            let before = build_key(dir.path(), &manifest, false).unwrap();
            let legacy = build_key(dir.path(), &manifest, true).unwrap();
            manifest["build"]["assets"] = json!(["new-guidance.md"]);
            assert_eq!(before, build_key(dir.path(), &manifest, false).unwrap());
            assert_ne!(legacy, build_key(dir.path(), &manifest, true).unwrap());
        }
        pub fn incompatible_protocol_and_core_schema_collision_are_rejected<H: crate::TestHost>() {
            let dir = tempfile::tempdir().unwrap();
            let path = package::<H>(dir.path());
            let p = path.join("plugin.json");
            let bytes = std::fs::read(&p).unwrap();
            let mut value: Value = serde_json::from_slice(&bytes).unwrap();
            value["protocol"] = json!(true);
            std::fs::write(&p, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Catalog::<H>::load_snapshot(dir.path()).is_err());
            value["protocol"] = json!(1);
            value["tools"] = json!(["file_read"]);
            std::fs::write(&p, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Package::load::<H>(&path, "fixture_ability", &["file_read".into()]).is_err());
        }
    }

    pub mod diagnostics_native_contract_tests {
        use super::*;

        /// Browser credentials remain behind native dispatch; malformed or incomplete contracts cannot install.
        pub fn diagnostics_retains_native_authority_and_rejects_incompatible_packages<
            H: crate::TestHost,
        >() {
            let root = tempfile::tempdir().unwrap();
            let tools = vec![
                "read_diagnostics".to_owned(),
                "login_as_current_user".to_owned(),
                "dev_browser_url".to_owned(),
                "dev_browser_action".to_owned(),
                "dev_browser_record".to_owned(),
            ];
            let manifest = json!({"version":1,"id":"diagnostics","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py",
            "tools":tools,"build":{"inputs":["runner.py","adapter.py"],"dependencies":[],"toolchain":"image-python3","flags":["-I","-B"],
            "assets":["diagnostics.json","diagnostics.skill.md","tools","skills"]}});
            for name in ["runner.py", "adapter.py"] {
                std::fs::write(root.path().join(name), "# native fixture\n").unwrap();
            }
            std::fs::write(
                root.path().join("plugin.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(Package::load::<H>(root.path(), "diagnostics", &tools)
                .unwrap()
                .is_none());
            let mut invalid = manifest.clone();
            invalid["protocol"] = json!(true);
            std::fs::write(
                root.path().join("plugin.json"),
                serde_json::to_vec(&invalid).unwrap(),
            )
            .unwrap();
            assert!(Package::load::<H>(root.path(), "diagnostics", &tools).is_err());
            std::fs::write(
                root.path().join("plugin.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            std::fs::remove_file(root.path().join("adapter.py")).unwrap();
            assert!(Package::load::<H>(root.path(), "diagnostics", &tools).is_err());
        }
    }

    pub mod native_automation_contract_tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        /// Build private interface inputs; this fixture asserts contracts, never a Cargo compilation proof.
        pub fn fixture<H: crate::TestHost>() -> (tempfile::TempDir, Value, Vec<String>) {
            fixture_data()
        }
        pub fn fixture_data() -> (tempfile::TempDir, Value, Vec<String>) {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let tools: Vec<String> = AUTOMATION_TOOLS.iter().map(|v| (*v).to_owned()).collect();
            let manifest = json!({"id":"automation","dispatch":"isolated","host_interface":"automation_v1",
            "protocol":1,"interface":1,"runtime":"executable","entrypoint":"webagent_automation","effect":"write",
            "tools":tools,
            "build":{"toolchain":"rust-1.99","inputs":["Cargo.toml","Cargo.lock","service/main.rs","service/engine.rs","service/contracts.rs"],"dependencies":[],"flags":[],
                "assets":automation_assets()}});
            for name in manifest["build"]["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .chain(manifest["build"]["assets"].as_array().unwrap())
            {
                let path = root.join(name.as_str().unwrap());
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"private contract fixture").unwrap();
            }
            let mut operations = tools.clone();
            operations.extend(["history", "recycle", "restore", "delete"].map(str::to_owned));
            let descriptor = json!({"id":"automation","protocol":1,"executable":"webagent_automation","public_operations":operations,
            "read_operations":["list_event_sources","list_delivery_channels","list_event_subscriptions","list_automations","history"]});
            for name in &tools {
                let effect = if [
                    "list_event_sources",
                    "list_delivery_channels",
                    "list_event_subscriptions",
                    "list_automations",
                ]
                .contains(&name.as_str())
                {
                    "read"
                } else {
                    "write"
                };
                let tool = json!({"name":name,"description":"private schema fixture","effect":effect,"default_access":{},
                "parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}});
                std::fs::write(
                    root.join(format!("tools/{name}/tool.json")),
                    serde_json::to_vec(&tool).unwrap(),
                )
                .unwrap();
            }
            for (name, value) in [
                ("plugin.json", manifest.clone()),
                (
                    "automation.json",
                    json!({"tools":tools,"required_module":"automation"}),
                ),
                ("service/package.json", descriptor),
            ] {
                std::fs::write(root.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
            }
            (temp, manifest, tools)
        }
        /// Reject forged bridge identities, interface changes and tool authority expansions before loading code.
        pub fn native_registration_rejects_host_and_tool_escalation<H: crate::TestHost>() {
            let (root, manifest, tools) = fixture::<H>();
            assert!(automation_contract(root.path(), "automation", &tools, &manifest).is_ok());
            assert!(automation_contract(root.path(), "other_ability", &tools, &manifest).is_err());
            for (field, value) in [
                ("host_interface", json!("codebase_admin")),
                ("interface", json!(2)),
                ("entrypoint", json!("../outside")),
            ] {
                let mut bad = manifest.clone();
                bad[field] = value;
                assert!(
                    automation_contract(root.path(), "automation", &tools, &bad).is_err(),
                    "{field}"
                );
            }
            let mut bad_tools = tools.clone();
            bad_tools.push("build_work".into());
            assert!(automation_contract(root.path(), "automation", &bad_tools, &manifest).is_err());
        }
        /// A symlinked compile input cannot certify an artifact, even when its bytes are readable.
        pub fn native_inputs_reject_filesystem_substitution<H: crate::TestHost>() {
            let (root, manifest, tools) = fixture::<H>();
            let path = root.path().join("service/engine.rs");
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
            assert!(automation_contract(root.path(), "automation", &tools, &manifest).is_err());
        }
        /// Asset updates change runtime identity without recompilation; missing/new-input artifacts never fall back.
        pub fn native_artifact_and_assets_have_independent_version_bindings<H: crate::TestHost>() {
            let (root, manifest, tools) = fixture::<H>();
            let registry = tempfile::tempdir().unwrap();
            assert!(automation_service_at(root.path(), registry.path()).is_err());
            let key = automation_contract(root.path(), "automation", &tools, &manifest).unwrap();
            let directory = registry.path().join(&key);
            std::fs::create_dir(&directory).unwrap();
            let executable = directory.join("webagent_automation");
            std::fs::write(&executable, b"fixture artifact identity only").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
            let first = automation_service_at(root.path(), registry.path()).unwrap();
            std::fs::write(
                root.path().join("service/page.js"),
                b"updated runtime asset",
            )
            .unwrap();
            let next = automation_service_at(root.path(), registry.path()).unwrap();
            assert_eq!(first.executable, next.executable);
            assert_ne!(first.implementation, next.implementation);
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o775)).unwrap();
            assert!(automation_service_at(root.path(), registry.path()).is_err());
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
            std::fs::write(root.path().join("service/engine.rs"), b"new compiled input").unwrap();
            assert!(automation_service_at(root.path(), registry.path()).is_err());
        }
    }
}

#[cfg(test)]
mod requests_contract_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A plugin folder with tool folders `read_a` and `write_b`, serving `read_a`.
    fn fixture() -> (tempfile::TempDir, Value, Vec<String>) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let manifest = json!({"id":"demo","dispatch":"isolated","host_interface":"requests_v1",
            "protocol":2,"interface":1,"runtime":"executable","entrypoint":"webagent_demo",
            "effect":"write","tools":["read_a"],"requests":["agents.list"],
            "build":{"toolchain":"rust-1.99","inputs":["Cargo.toml","Cargo.lock","plugin/lib.rs"],
                "dependencies":[],"flags":[],"assets":["demo.json"]}});
        for name in ["Cargo.toml", "Cargo.lock", "plugin/lib.rs", "demo.json"] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, name).unwrap();
        }
        std::fs::write(
            root.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        (temp, manifest, vec!["read_a".into(), "write_b".into()])
    }

    #[test]
    fn a_valid_plugin_declares_offered_requests_for_its_own_tools() {
        let (root, _, folders) = fixture();
        let plugin = RequestsPlugin::load(root.path(), "demo", &folders).unwrap();
        assert_eq!(plugin.tools, ["read_a"]);
        assert!(plugin.requests.kind("agents.list").is_ok());
        assert!(plugin.requests.kind("sessions.list").is_err());
    }

    #[test]
    fn foreign_tools_unoffered_requests_and_unknown_fields_are_refused() {
        let (root, manifest, folders) = fixture();
        let mut cases = Vec::new();
        for (field, value) in [
            ("tools", json!(["read_a", "build_work"])),
            ("tools", json!(["read_a", "read_a"])),
            ("requests", json!(["accounts.delete"])),
            ("protocol", json!(1)),
            ("host_interface", json!("automation_v1")),
            ("id", json!("other")),
            ("credentials", json!("x")),
        ] {
            let mut bad = manifest.clone();
            bad[field] = value;
            cases.push(requests_contract(root.path(), "demo", &folders, &bad).is_err());
        }
        assert!(cases.iter().all(|refused| *refused), "{cases:?}");
        assert!(requests_contract(root.path(), "demo", &folders, &manifest).is_ok());
    }

    #[test]
    fn the_executable_must_be_its_built_artifact() {
        use std::os::unix::fs::PermissionsExt;
        let (root, manifest, folders) = fixture();
        let plugin = RequestsPlugin::load(root.path(), "demo", &folders).unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        assert!(plugin.executable(artifacts.path()).is_err());
        let directory = artifacts
            .path()
            .join(build_key(root.path(), &manifest, false).unwrap());
        std::fs::create_dir(&directory).unwrap();
        let binary = directory.join("webagent_demo");
        std::fs::write(&binary, b"artifact").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(plugin.executable(artifacts.path()).unwrap(), binary);
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            plugin.executable(artifacts.path()).is_err(),
            "group/world writable"
        );
    }
}
