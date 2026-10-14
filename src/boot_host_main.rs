//! Trusted host launch authority; shared pre-exec confinement precedes replaceable code.
#[path = "engine_sandbox.rs"]
mod engine_sandbox;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::os::unix::{fs::{MetadataExt, PermissionsExt}, process::CommandExt};
use std::path::PathBuf;

/// Admit one fixed content-addressed root-owned artifact; stdin/stdout remain private pipes.
fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.len() != 2 { bail!("Boot launcher arguments"); }
    let program = PathBuf::from(&arguments[1]);
    let name = program.file_name().and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("artifact-"))
        .ok_or_else(|| anyhow::anyhow!("Boot artifact name"))?;
    if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !program.is_absolute() || program.canonicalize()? != program {
        bail!("Boot artifact path");
    }
    let info = std::fs::symlink_metadata(&program)?;
    let parent = std::fs::symlink_metadata(program.parent().unwrap())?;
    if !info.is_file() || info.uid() != 0 || info.nlink() != 1
        || info.permissions().mode() & 0o022 != 0 || info.len() > 32 * 1024 * 1024
        || !parent.is_dir() || parent.uid() != 0 || parent.permissions().mode() & 0o022 != 0 {
        bail!("Boot artifact ownership");
    }
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&program)?));
    if digest != name { bail!("Boot artifact hash"); }
    // Preparation is parent-only; apply performs raw syscalls inside pre_exec.
    let sandbox = engine_sandbox::prepare(&program)?;
    let mut command = std::process::Command::new(&program);
    unsafe { command.pre_exec(move || sandbox.apply()); }
    let status = command.env_clear().current_dir("/")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::null()).status()?;
    if !status.success() { bail!("Boot implementation failed"); }
    Ok(())
}
