//! `ability_plugin_invoke`: one protocol frame to an installed ability package (Core agent
//! management, automation, web search and other plugins), ported from
//! `ability_plugin_worker.py`. The worker re-verifies the app's binding against the
//! immutable published package before anything runs: location, identity, manifest
//! contract, the compiled artifact for Rust packages, and the generation hash, which uses
//! the same compact, key-sorted JSON as the host. The plugin then runs under its own
//! supervisor (`webagent-v3 --plugin-runner`), confined to the workspace and its package.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// The tool this module runs.
pub const TOOL: &str = "ability_plugin_invoke";
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024 - 16384;
const ERROR_LIMIT: usize = 16384;
const UNCERTAIN: &str = "Plugin interrupted or returned invalid output; reconcile before retrying";

/// Where published packages and their compiled artifacts live.
#[derive(Debug, Clone)]
pub struct Packages {
    /// Immutable published asset releases.
    pub releases: PathBuf,
    /// Compiled Rust package artifacts, by build key.
    pub artifacts: PathBuf,
}

impl Packages {
    /// The installed release's assets (`webagent_configuration::paths::app`).
    #[must_use]
    pub fn container() -> Self {
        let assets = webagent_configuration::paths::app().join("assets");
        Self {
            releases: assets.join("releases"),
            artifacts: assets.join("artifacts"),
        }
    }
}

/// Why a binding was refused; its text is the tool's output.
#[derive(Debug)]
pub struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn refused(message: &str) -> Refused {
    Refused(message.to_owned())
}

impl From<io::Error> for Refused {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

/// Hex SHA-256 of `value` as compact JSON with sorted keys, as the host and Python hash it.
fn json_digest(value: &Value) -> String {
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A verified package: its root and the argv that runs it.
#[derive(Debug, PartialEq)]
pub struct Verified {
    pub root: PathBuf,
    pub argv: Vec<String>,
}

/// Resolves and verifies the binding's immutable package and admitted executable.
///
/// # Errors
/// When the package moved, changed, or is not one the binding admitted.
pub fn package(packages: &Packages, binding: &Value) -> Result<Verified, Refused> {
    let root = PathBuf::from(binding["root"].as_str().ok_or_else(|| refused("'root'"))?);
    let resolved = std::fs::canonicalize(&root)?;
    if resolved != root || root == packages.releases || !root.starts_with(&packages.releases) {
        return Err(refused("Plugin must use an immutable published package"));
    }
    let release: Vec<String> = root
        .strip_prefix(&packages.releases)
        .map_err(|_| refused("Invalid package location"))?
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    if release.len() != 6 || release[1..4] != ["v3", "plugins", "abilities"] {
        return Err(refused("Invalid package location"));
    }
    if crate::files::uuid_text(&release[0]).is_none() {
        return Err(refused("badly formed hexadecimal UUID string"));
    }
    let ident = binding["id"].as_str().ok_or_else(|| refused("'id'"))?;
    if release[5] != ident {
        return Err(refused("Plugin identity mismatch"));
    }
    let read = |name: &str, bound: u64| -> Result<Vec<u8>, Refused> {
        let path = root.join(name);
        let unsafe_name = name.is_empty()
            || Path::new(name).is_absolute()
            || Path::new(name)
                .components()
                .any(|part| matches!(part, Component::ParentDir));
        let meta = std::fs::symlink_metadata(&path);
        let unsafe_file = meta
            .as_ref()
            .map_or(true, |meta| !meta.is_file() || meta.len() > bound);
        if unsafe_name
            || unsafe_file
            || !std::fs::canonicalize(&path).is_ok_and(|real| real.starts_with(&root))
        {
            return Err(refused("Unsafe package file"));
        }
        Ok(std::fs::read(&path)?)
    };
    let raw = read("plugin.json", 65536)?;
    let manifest: Value =
        serde_json::from_slice(&raw).map_err(|error| Refused(error.to_string()))?;
    if manifest["id"] != ident
        || !manifest["protocol"].is_i64()
        || manifest["protocol"] != 1
        || manifest["dispatch"] != "isolated"
        || manifest["effect"] != "write"
    {
        return Err(refused("Unsupported package"));
    }
    let entrypoint = manifest["entrypoint"]
        .as_str()
        .ok_or_else(|| refused("'entrypoint'"))?;
    let scheme = match binding.get("generation_scheme") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|scheme| matches!(scheme, 1 | 2))
            .ok_or_else(|| refused("Unsupported binding generation scheme"))?,
    };
    let mut executable = root.join(entrypoint);
    let code = if manifest["build"]["toolchain"] == "rust-1.99" {
        let build = &manifest["build"];
        let mut inputs = BTreeMap::new();
        for name in build["inputs"]
            .as_array()
            .ok_or_else(|| refused("'inputs'"))?
        {
            let name = name
                .as_str()
                .ok_or_else(|| refused("Unsafe package file"))?;
            inputs.insert(name.to_owned(), sha256(&read(name, 16 * 1024 * 1024)?));
        }
        let mut compile_build = build.clone();
        if scheme == 2 {
            if let Some(build) = compile_build.as_object_mut() {
                build.remove("assets");
            }
        }
        let key =
            json_digest(&json!({"build":compile_build,"entrypoint":entrypoint,"inputs":inputs}));
        executable = packages.artifacts.join(key).join(entrypoint);
        let meta = std::fs::symlink_metadata(&executable);
        if meta.map_or(true, |meta| {
            !meta.is_file() || meta.len() > 32 * 1024 * 1024
        }) {
            return Err(refused("Plugin artifact unavailable"));
        }
        std::fs::read(&executable)?
    } else {
        read(entrypoint, 32 * 1024 * 1024)?
    };
    let schemas = read(
        manifest["schemas"]
            .as_str()
            .ok_or_else(|| refused("Unsafe package file"))?,
        256 * 1024,
    )?;
    let hashes = json!([sha256(&raw), sha256(&code), sha256(&schemas)]);
    let identity = if scheme == 2 {
        let build = match manifest.get("build") {
            None => json!({}),
            Some(build) if build.is_object() => build.clone(),
            Some(_) => return Err(refused("Invalid package build declaration")),
        };
        let mut files = BTreeMap::new();
        for field in ["inputs", "assets"] {
            let values = match build.get(field) {
                None => Vec::new(),
                Some(Value::Array(values)) => values.clone(),
                Some(_) => return Err(refused("Invalid runtime input list")),
            };
            for name in values {
                let name = name
                    .as_str()
                    .ok_or_else(|| refused("Unsafe package file"))?;
                files.insert(name.to_owned(), sha256(&read(name, 16 * 1024 * 1024)?));
                if files.len() > 1024 {
                    return Err(refused("Runtime input bound exceeded"));
                }
            }
        }
        json!([hashes, files])
    } else {
        hashes
    };
    let tools = manifest["tools"].as_array().cloned().unwrap_or_default();
    if json_digest(&identity) != binding["generation"].as_str().unwrap_or_default()
        || !tools.contains(&binding["name"])
    {
        return Err(refused("Stale package binding"));
    }
    let argv = match manifest["runtime"].as_str() {
        Some("python3") => vec![
            "/usr/bin/python3".into(),
            "-I".into(),
            "-B".into(),
            root.join(entrypoint).to_string_lossy().into_owned(),
        ],
        Some("executable") => vec![executable.to_string_lossy().into_owned()],
        _ => return Err(refused("Unsupported runtime")),
    };
    Ok(Verified { root, argv })
}

/// Stops a started plugin supervisor's session if the call ends before it does.
struct Session {
    child: Option<tokio::process::Child>,
    group: i32,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            crate::commands::stop_in_background(child, self.group);
        }
    }
}

async fn bounded(mut stream: impl AsyncRead + Unpin, bound: usize) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut chunk = vec![0_u8; 65536];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&chunk[..read]);
        if data.len() > bound {
            return Err(io::Error::other("Plugin output exceeds limit"));
        }
    }
}

/// Exchanges one bounded protocol frame with the bound plugin in `base`. Any failure after
/// the plugin may have started is reported as uncertain: it never implies a safe retry.
///
/// # Errors
/// When the binding is refused, before anything runs.
pub async fn invoke(
    packages: &Packages,
    runner: &[String],
    binding: &Value,
    args: &Value,
    base: &Path,
) -> Result<Value, Refused> {
    let verified = {
        let packages = packages.clone();
        let binding = binding.clone();
        tokio::task::spawn_blocking(move || package(&packages, &binding))
            .await
            .map_err(|_| refused("Plugin verification failed"))??
    };
    let frame = json!({"protocol":1,"invocation_id":binding["invocation_id"],
        "tool":binding["name"],"arguments":args});
    let invocation = binding["invocation_id"].clone();
    let generation = binding["generation"].clone();
    let result = exchange(runner, &verified, base, frame, &invocation).await;
    Ok(match result {
        Ok((status, output, kind)) => with_kind(
            json!({"status":status,"uncertain":status == "uncertain",
                "output":output,"plugin_revision":generation,"invocation_id":invocation}),
            &kind,
        ),
        Err(_) => json!({"status":"uncertain","output":UNCERTAIN,"uncertain":true}),
    })
}

/// A plugin's own cause class travels with its result so the review log can group it.
fn with_kind(mut result: Value, kind: &Value) -> Value {
    if kind.is_string() {
        result["error_kind"] = kind.clone();
    }
    result
}

async fn exchange(
    runner: &[String],
    verified: &Verified,
    base: &Path,
    frame: Value,
    invocation: &Value,
) -> io::Result<(String, Value, Value)> {
    let (program, leading) = runner
        .split_first()
        .ok_or_else(|| io::Error::other("plugin supervisor not configured"))?;
    let read = [
        verified.root.to_string_lossy().into_owned(),
        verified.argv.last().cloned().unwrap_or_default(),
    ];
    let mut command = tokio::process::Command::new(program);
    command
        .args(leading)
        .arg(json!({"argv":verified.argv,"read":read}).to_string())
        .current_dir(base)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", base)
        .env("TMPDIR", base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and touches no memory.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| io::Error::other("plugin supervisor exited at once"))?;
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let mut session = Session {
        child: Some(child),
        group,
    };
    let talk = async {
        let mut stdin = stdin.ok_or_else(|| io::Error::other("plugin input unavailable"))?;
        let mut line = frame.to_string().into_bytes();
        line.push(b'\n');
        stdin.write_all(&line).await?;
        drop(stdin);
        let stdout = stdout.ok_or_else(|| io::Error::other("plugin output unavailable"))?;
        let stderr = stderr.ok_or_else(|| io::Error::other("plugin output unavailable"))?;
        let (output, _) =
            tokio::try_join!(bounded(stdout, OUTPUT_LIMIT), bounded(stderr, ERROR_LIMIT))?;
        let status = match session.child.as_mut() {
            Some(child) => child.wait().await?,
            None => return Err(io::Error::other("plugin supervisor lost")),
        };
        session.child = None;
        if !status.success() {
            return Err(io::Error::other("Plugin process failed"));
        }
        let result: Value = serde_json::from_slice(&output).map_err(io::Error::other)?;
        let status = result["status"].as_str().unwrap_or_default();
        if !result["protocol"].is_i64()
            || result["protocol"] != 1
            || &result["invocation_id"] != invocation
            || !matches!(status, "success" | "failed" | "uncertain")
        {
            return Err(io::Error::other("Malformed plugin response"));
        }
        Ok((
            status.to_owned(),
            result["output"].clone(),
            result["error_kind"].clone(),
        ))
    };
    let outcome = match tokio::time::timeout(Duration::from_secs(120), talk).await {
        Ok(outcome) => outcome,
        Err(_) => Err(io::Error::other("plugin timed out")),
    };
    drop(session);
    // SAFETY: killpg takes plain integers; ESRCH once everything has gone is expected.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_cause_class_travels_with_its_result() {
        let base = json!({"status":"failed","output":"denied"});
        let typed = with_kind(base.clone(), &json!("denied"));
        assert_eq!(typed["error_kind"], "denied");
        let untyped = with_kind(base, &Value::Null);
        assert!(untyped.get("error_kind").is_none());
    }
}
