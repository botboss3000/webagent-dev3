//! Static calls to the existing foreign domain owners. No App, Store, pools, or copied authority.
use anyhow::Result;
use serde_json::Value;
use std::path::{Path,PathBuf};
pub trait Host:Clone+Send+Sync+'static {
 fn selected_source()->Result<Option<PathBuf>>;
 fn native_schema_snapshot_at(source:Option<&Path>)->Result<Vec<Value>>;
 fn native_schemas_for_catalog(root:&Path,publication:bool)->Result<Vec<Value>>;
 fn native_schemas_without_administrator()->Vec<Value>;
 fn native_schemas()->Vec<Value>;
 fn administrator_supported(name:&str)->bool;
 fn module_known(name:&str)->bool;
 fn module_available(name:&str)->bool;
 fn optimization_folder()->PathBuf;
 fn optimization_declaration()->Result<Option<(String,Value)>>;
 fn optimization_available()->bool;
 fn optimization_generation()->Option<String>;
 fn optimization_guidance()->Option<String>;
 fn mcp_folder()->PathBuf;
 fn mcp_available()->bool;
 fn mcp_schemas()->Vec<Value>;
 fn mcp_guidance()->&'static str;
}
