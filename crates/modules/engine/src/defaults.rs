//! Runtime external installation defaults with the existing explicit test-fixture boundary.
use anyhow::Result;
use serde_json::Value;
use std::path::{Path,PathBuf};
pub fn fallback_directory()->PathBuf {
 #[cfg(any(test,feature="test-support"))] {PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join("config")}
 #[cfg(not(any(test,feature="test-support")))] {PathBuf::from("/usr/local/share/webagent/defaults")}
}
pub fn published_root()->Option<PathBuf> {
 let ui=std::env::var_os("V3_UI_ROOT")?;
 webagent_configuration::prompts::published_root_at(Path::new(&ui)).ok()
}
pub fn prompt_defaults_at(source:Option<&Path>)->Result<Vec<u8>> {
 webagent_configuration::prompts::prompt_defaults_at(source,&fallback_directory())
}
pub fn retention_defaults()->Result<Vec<u8>> {
 webagent_configuration::prompts::retention_defaults_at(None,&fallback_directory())
}
pub fn resolved_prompts_at(root:&Path,source:Option<&Path>)->Result<Value> {
 webagent_configuration::prompts::resolved_prompts_at(root,source,&fallback_directory())
}
pub fn resolved_prompts(root:&Path)->Result<Value>{resolved_prompts_at(root,None)}

pub fn prompt_defaults()->Result<Vec<u8>> {prompt_defaults_at(None)}
