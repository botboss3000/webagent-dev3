//! Version-one isolated tool packages; engine admission remains authoritative.
use crate::*;
use std::path::{Path as FsPath, Component};
#[derive(Clone,Debug,serde::Serialize)]
pub(crate) struct Package {#[serde(skip_serializing)] pub root:PathBuf,pub manifest:Value,pub schemas:Vec<Value>,pub generation:String,#[serde(skip_serializing)] pub legacy_generation:String}
/// Read a bounded regular plugin input; reject symlinks and unexpected file types.
fn read(path:&FsPath,limit:u64)->Result<Vec<u8>> {
    let meta=std::fs::symlink_metadata(path)?;
    if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>limit {anyhow::bail!("Unsafe plugin file");}
    Ok(std::fs::read(path)?)
}
/// Resolve a manifest path inside its package without allowing absolute paths or traversal.
fn relative(root:&FsPath,value:&Value)->Result<PathBuf>{
    let name=value.as_str().ok_or_else(||anyhow!("Missing plugin path"))?;
    if name.is_empty()||FsPath::new(name).components().any(|p|!matches!(p,Component::Normal(_))){anyhow::bail!("Invalid plugin path");}
    let path=root.join(name);
    if !path.canonicalize()?.starts_with(root.canonicalize()?){anyhow::bail!("Plugin path escaped package");}
    Ok(path)
}
/// Hash declared locked Rust inputs and the build contract to locate an independently cached artifact.
fn build_key(root:&FsPath,manifest:&Value,legacy:bool)->Result<String>{
    let inputs=manifest["build"]["inputs"].as_array().ok_or_else(||anyhow!("Missing build inputs"))?;
    if inputs.len()>1024||manifest["build"]["dependencies"]!=json!([])||manifest["build"]["flags"]!=json!([]){anyhow::bail!("Unsupported build contract");}
    let entry=manifest["entrypoint"].as_str().ok_or_else(||anyhow!("Missing entrypoint"))?;
    if !access::valid_id(entry){anyhow::bail!("Invalid artifact name");}
    let mut files=std::collections::BTreeMap::new();
    for name in inputs {
        let path=relative(root,name)?;let name=name.as_str().unwrap();
        if files.insert(name.to_owned(),hex::encode(Sha256::digest(read(&path,16*1024*1024)?))).is_some(){anyhow::bail!("Duplicate build input");}
    }
    if !files.contains_key("Cargo.toml")||!files.contains_key("Cargo.lock"){anyhow::bail!("Locked standalone crate required");}
    let mut build=manifest["build"].clone();
    if !legacy{build.as_object_mut().unwrap().remove("assets");}
    Ok(digest(&json!({"build":build,"entrypoint":entry,"inputs":files})))
}
/// Fingerprint declared runtime helpers/assets without changing the executable build-cache key.
fn runtime_input_hashes(root:&FsPath,manifest:&Value)->Result<std::collections::BTreeMap<String,String>>{
    let mut files=std::collections::BTreeMap::new();
    if let Some(build)=manifest.get("build"){
        if !build.is_object(){anyhow::bail!("Invalid package build declaration");}
        for key in ["inputs","assets"]{
            if let Some(values)=build.get(key){
                let values=values.as_array().ok_or_else(||anyhow!("Invalid runtime input list"))?;
                for value in values{
                    let path=relative(root,value)?;
                    files.insert(value.as_str().unwrap().to_owned(),hex::encode(Sha256::digest(read(&path,16*1024*1024)?)));
                    if files.len()>1024{anyhow::bail!("Runtime input bound exceeded");}
                }
            }
        }
    }
    Ok(files)
}
impl Package {
    /// Validate protocol, ownership, schema subset, and executable generation before catalog installation.
    pub(crate) fn load(root:&FsPath,id:&str,tools:&[String])->Result<Option<Self>> {
        Self::load_for_scheme(root,id,tools,false)
    }
    /// Preserve original artifacts for legacy durable bindings; new keys exclude runtime assets.
    fn load_for_scheme(root:&FsPath,id:&str,tools:&[String],legacy:bool)->Result<Option<Self>> {
        let path=root.join("plugin.json");
        if !path.exists(){return Ok(None);}
        let bytes=read(&path,64*1024)?;
        let manifest:Value=serde_json::from_slice(&bytes)?;
        // Existing native package adapters retain their own contract.
        if matches!(id,"codebase_admin"|"diagnostics") && manifest.get("dispatch").is_none(){
            if id=="diagnostics" {
                let expected=json!({"version":1,"id":"diagnostics","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py",
                    "tools":tools,"build":{"inputs":["runner.py","adapter.py"],"dependencies":[],"toolchain":"image-python3","flags":["-I","-B"],
                    "assets":["diagnostics.json","diagnostics.skill.md","tools","skills"]}});
                if manifest!=expected {anyhow::bail!("Unsupported native Diagnostics contract");}
                for name in ["runner.py","adapter.py"] {read(&root.join(name),2*1024*1024)?;}
            }
            return Ok(None);
        }
        if manifest["dispatch"]!="isolated"||manifest["protocol"].as_u64()!=Some(1)
            ||manifest["interface"].as_u64()!=Some(1)||manifest["id"]!=id
            ||manifest["tools"]!=json!(tools)||!matches!(manifest["runtime"].as_str(),Some("python3"|"executable"))
            ||manifest["effect"]!="write" {anyhow::bail!("Unsupported isolated plugin contract");}
        let executable=if manifest["build"]["toolchain"]=="rust-1.90" {
            let build_key=build_key(root,&manifest,legacy)?;
            PathBuf::from("/opt/v3/assets/artifacts").join(build_key).join(manifest["entrypoint"].as_str().ok_or_else(||anyhow!("Missing executable"))?)
        } else {relative(root,&manifest["entrypoint"])?};
        let code=read(&executable,32*1024*1024)?;
        if manifest["runtime"]=="executable" {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&executable)?.permissions().mode()&0o111==0 {anyhow::bail!("Plugin is not executable");}
        }
        let schema_bytes=read(&relative(root,&manifest["schemas"])?,256*1024)?;
        let schemas:Vec<Value>=serde_json::from_slice(&schema_bytes)?;
        let mut names=std::collections::BTreeSet::new();
        let native=crate::tools::native_schemas().into_iter().chain(crate::mcp::all_schemas()).collect::<Vec<_>>();
        for schema in &schemas {
            let name=schema["name"].as_str().ok_or_else(||anyhow!("Missing schema name"))?;
            if !tools.iter().any(|t|t==name)||!names.insert(name)
                ||native.iter().any(|s|s["name"]==name)||crate::abilities::is_discovery(name)
                ||matches!(name,"publish_main_message"|"set_closer_status"|"codebase_admin_protocol")
                ||["administrator_","v3_","ability_plugin","mcp_"].iter().any(|prefix|name.starts_with(prefix))
                ||schema["type"]!="function"||schema["description"].as_str().is_none_or(|s|s.is_empty()||s.len()>8192)
                ||schema["parameters"]["type"]!="object" {anyhow::bail!("Invalid plugin schema owner");}
            validate_schema(&schema["parameters"],0)?;
        }
        if names.len()!=tools.len()||tools.is_empty(){anyhow::bail!("Incomplete plugin schemas");}
        let hashes=json!([hex::encode(Sha256::digest(bytes)),hex::encode(Sha256::digest(code)),hex::encode(Sha256::digest(schema_bytes))]);
        let legacy_generation=digest(&hashes);
        let generation=digest(&json!([hashes,runtime_input_hashes(root,&manifest)?]));
        Ok(Some(Self{root:root.to_owned(),manifest,schemas,generation,legacy_generation}))
    }
    /// Check ownership against the package's validated tool schemas.
    pub(crate) fn owns(&self,name:&str)->bool {self.schemas.iter().any(|s|s["name"]==name)}
    /// Persist the admitted implementation root, generation, and schema for one model-call tool.
    pub(crate) fn binding(&self,name:&str)->Option<Value> {
        self.owns(name).then(||json!({"root":self.root,"generation":self.generation,"generation_scheme":2,
            "schema":self.schemas.iter().find(|s|s["name"]==name),"id":self.manifest["id"]}))
    }
}
/// Validate the bounded closed schema subset supported by isolated protocol 1.
fn validate_schema(schema:&Value,depth:usize)->Result<()> {
    if depth>24||!schema.is_object(){anyhow::bail!("Invalid plugin parameter schema");}
    let allowed=["type","description","properties","required","additionalProperties","items","enum","minimum","maximum","minLength","maxLength","minItems","maxItems"];
    if schema.as_object().unwrap().keys().any(|k|!allowed.contains(&k.as_str())){anyhow::bail!("Unsupported plugin schema keyword");}
    if !matches!(schema["type"].as_str(),Some("object"|"array"|"string"|"integer"|"number"|"boolean"|"null")){anyhow::bail!("Unsupported plugin schema type");}
    if let Some(values)=schema.get("enum"){if values.as_array().is_none_or(|v|v.is_empty()||v.len()>128){anyhow::bail!("Invalid enum");}}
    for key in ["minimum","maximum"]{if schema.get(key).is_some_and(|v|!v.is_number()){anyhow::bail!("Invalid numeric bound");}}
    for key in ["minLength","maxLength","minItems","maxItems"]{if schema.get(key).is_some_and(|v|v.as_u64().is_none()){anyhow::bail!("Invalid length bound");}}
    if schema["type"]=="object" {
        let props=schema["properties"].as_object().ok_or_else(||anyhow!("Object properties required"))?;
        if props.len()>128||schema["additionalProperties"]!=false {anyhow::bail!("Closed plugin object required");}
        for (name,s) in props {if !access::valid_id(name){anyhow::bail!("Invalid parameter name");}validate_schema(s,depth+1)?;}
        if let Some(required)=schema.get("required"){let list=required.as_array().ok_or_else(||anyhow!("Invalid required list"))?;
            let mut seen=std::collections::BTreeSet::new();
            for name in list {let name=name.as_str().ok_or_else(||anyhow!("Invalid required key"))?;if !props.contains_key(name)||!seen.insert(name){anyhow::bail!("Invalid required key");}}}
    } else if schema["type"]=="array" {validate_schema(&schema["items"],depth+1)?;}
    Ok(())
}
/// Check invocation arguments against the admitted schema before any worker effect.
fn accepts(schema:&Value,value:&Value,depth:usize)->bool {
    if depth>24 {return false;}
    if schema.get("enum").is_some_and(|e|!e.as_array().unwrap().contains(value)){return false;}
    let in_range=|actual:u64,min:&str,max:&str|schema[min].as_u64().is_none_or(|v|actual>=v)&&schema[max].as_u64().is_none_or(|v|actual<=v);
    match schema["type"].as_str(){
        Some("object")=>value.as_object().is_some_and(|v|{
            let props=schema["properties"].as_object().unwrap();
            v.iter().all(|(k,v)|props.get(k).is_some_and(|s|accepts(s,v,depth+1)))
                &&schema["required"].as_array().is_none_or(|keys|keys.iter().all(|k|v.contains_key(k.as_str().unwrap())))
        }),
        Some("array")=>value.as_array().is_some_and(|v|in_range(v.len()as u64,"minItems","maxItems")&&v.iter().all(|v|accepts(&schema["items"],v,depth+1))),
        Some("string")=>value.as_str().is_some_and(|v|in_range(v.chars().count()as u64,"minLength","maxLength")),
        Some("number"|"integer")=>value.as_f64().is_some_and(|v|v.is_finite()&&(schema["type"]!="integer"||value.is_i64()||value.is_u64())&&schema["minimum"].as_f64().is_none_or(|m|v>=m)&&schema["maximum"].as_f64().is_none_or(|m|v<=m)),
        Some("boolean")=>value.is_boolean(),Some("null")=>value.is_null(),_=>false
    }
}
/// Expose generic schemas from the current validated publication for installation policy APIs.
pub(crate) fn current_schemas()->Vec<Value> {
    static EMPTY:std::sync::OnceLock<Arc<abilities::Catalog>>=std::sync::OnceLock::new();
    let catalog=crate::live_assets::catalog(EMPTY.get_or_init(||Arc::new(abilities::Catalog::empty())));
    catalog.all_schemas().into_iter().filter(|s|catalog.plugin_binding(s["name"].as_str().unwrap_or("")).is_some()).collect()
}
/// Recheck current installation, retain the admitted implementation, and send a sealed binding to the worker.
pub(crate) async fn execute(app:&App,sid:&str,name:&str,args:&Value,invocation:&str,binding:&Value,cancel:CancellationToken)->Result<Value> {
    let root=PathBuf::from(binding["root"].as_str().ok_or_else(||anyhow!("Missing plugin binding"))?);
    let id=binding["id"].as_str().ok_or_else(||anyhow!("Missing plugin identity"))?;
    // Check current installation independently from the pinned implementation.
    let current=crate::live_assets::catalog(&app.abilities);
    if current.plugin_binding(name).is_none_or(|b|b["id"]!=id){return Ok(json!({"status":"denied","output":"Plugin removed or ownership changed"}));}
    let manifest:Value=serde_json::from_slice(&read(&root.join(format!("{id}.json")),64*1024)?)?;
    let tools:Vec<String>=serde_json::from_value(manifest["tools"].clone())?;
    let scheme=match binding.get("generation_scheme"){
        None=>1,Some(value)=>value.as_u64().filter(|v|matches!(v,1|2)).ok_or_else(||anyhow!("Unsupported binding generation scheme"))?
    };
    let package=Package::load_for_scheme(&root,id,&tools,scheme==1)?.ok_or_else(||anyhow!("Plugin binding unavailable"))?;
    let expected=if scheme==1{&package.legacy_generation}else{&package.generation};
    if expected!=binding["generation"].as_str().unwrap_or("")||!package.owns(name)||!accepts(&binding["schema"]["parameters"],args,0){
        return Ok(json!({"status":"denied","output":"Plugin binding or arguments invalid"}));
    }
    let trusted=json!({"ability_plugin":{"root":root,"generation":expected,"generation_scheme":scheme,"id":id,"name":name,"invocation_id":invocation}});
    crate::tools::execute_trusted("ability_plugin_invoke",args,sid,&app.tool_socket,Some(trusted),cancel).await
}
#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn closed_schema_rejects_unknown_and_wrong_types(){
  let s=json!({"type":"object","properties":{"text":{"type":"string","maxLength":3}},"required":["text"],"additionalProperties":false});
  validate_schema(&s,0).unwrap();assert!(accepts(&s,&json!({"text":"yes"}),0));
  for v in [json!({}),json!({"text":true}),json!({"text":"long"}),json!({"text":"yes","extra":1})]{assert!(!accepts(&s,&v,0));}
  assert!(validate_schema(&json!({"type":"object","properties":{},"additionalProperties":true}),0).is_err());
 }
}

#[cfg(test)]
mod catalog_tests {
 use super::*;
 fn package(root:&FsPath)->PathBuf{
  let path=root.join("Custom/fixture_ability");std::fs::create_dir_all(path.join("skills")).unwrap();
  std::fs::write(path.join("fixture_ability.json"),serde_json::to_vec(&json!({"display_name":"Fixture","kind":"ability","description":"New drop-in ability","readiness":"ready","tools":["fixture_echo"]})).unwrap()).unwrap();
  std::fs::write(path.join("fixture_ability.skill.md"),"First guidance").unwrap();
  std::fs::write(path.join("runner.py"),"print('fixture')").unwrap();
  std::fs::write(path.join("plugin.json"),serde_json::to_vec(&json!({"id":"fixture_ability","dispatch":"isolated","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py","schemas":"schemas.json","effect":"write","tools":["fixture_echo"]})).unwrap()).unwrap();
  std::fs::write(path.join("schemas.json"),serde_json::to_vec(&json!([{"type":"function","name":"fixture_echo","description":"Echo supplied text","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}}])).unwrap()).unwrap();path
 }
 #[test]fn new_tools_and_guidance_are_loaded_without_native_registration(){
  let dir=tempfile::tempdir().unwrap();let path=package(dir.path());
  let catalog=abilities::Catalog::load_snapshot(dir.path()).unwrap();
  assert!(catalog.known("fixture_ability"));
  assert!(catalog.all_schemas().iter().any(|s|s["name"]=="fixture_echo"));
  assert_eq!(catalog.configuration_skill("fixture_ability").unwrap(),"First guidance");
  let old_version=catalog.version().to_owned();
  std::fs::write(path.join("fixture_ability.skill.md"),"Updated guidance").unwrap();
  let next=abilities::Catalog::load_snapshot(dir.path()).unwrap();
  assert_ne!(old_version,next.version());
  assert_eq!(next.configuration_skill("fixture_ability").unwrap(),"Updated guidance");
  let before=next.plugin_binding("fixture_echo").unwrap();
  std::fs::write(path.join("runner.py"),"print('next')").unwrap();
  assert_ne!(before["generation"],abilities::Catalog::load_snapshot(dir.path()).unwrap().plugin_binding("fixture_echo").unwrap()["generation"]);
 }
 #[test]fn helper_and_runtime_asset_updates_change_generation_while_legacy_bindings_remain_verifiable(){
  let dir=tempfile::tempdir().unwrap();let path=package(dir.path());
  let p=path.join("plugin.json");let mut manifest:Value=serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
  manifest["build"]=json!({"inputs":["runner.py","helper.py"],"assets":["fixture_ability.skill.md"]});
  std::fs::write(path.join("helper.py"),"VALUE = 1").unwrap();std::fs::write(&p,serde_json::to_vec(&manifest).unwrap()).unwrap();
  let before=Package::load(&path,"fixture_ability",&["fixture_echo".into()]).unwrap().unwrap();
  assert_eq!(before.binding("fixture_echo").unwrap()["generation_scheme"],2);
  std::fs::write(path.join("helper.py"),"VALUE = 2").unwrap();
  let next=Package::load(&path,"fixture_ability",&["fixture_echo".into()]).unwrap().unwrap();
  assert_ne!(before.generation,next.generation);assert_eq!(before.legacy_generation,next.legacy_generation);
  std::fs::write(path.join("fixture_ability.skill.md"),"Changed asset").unwrap();
  let asset=Package::load(&path,"fixture_ability",&["fixture_echo".into()]).unwrap().unwrap();
  assert_ne!(next.generation,asset.generation);
 }
 #[test]fn pending_packages_withdraw_bindings_and_cannot_declare_trusted_core_kind(){
  let dir=tempfile::tempdir().unwrap();let path=package(dir.path());let declaration=path.join("fixture_ability.json");
  let mut value:Value=serde_json::from_slice(&std::fs::read(&declaration).unwrap()).unwrap();
  value["readiness"]=json!("pending");std::fs::write(&declaration,serde_json::to_vec(&value).unwrap()).unwrap();
  assert!(abilities::Catalog::load_snapshot(dir.path()).unwrap().plugin_binding("fixture_echo").is_none());
  value["readiness"]=json!("ready");value["kind"]=json!("virtual_core");std::fs::write(&declaration,serde_json::to_vec(&value).unwrap()).unwrap();
  assert!(abilities::Catalog::load_snapshot(dir.path()).is_err());
 }
 #[test]fn runtime_asset_declarations_do_not_change_the_compile_cache_key(){
  let dir=tempfile::tempdir().unwrap();
  for name in ["Cargo.toml","Cargo.lock","main.rs"]{std::fs::write(dir.path().join(name),"fixed input").unwrap();}
  let mut manifest=json!({"entrypoint":"fixture","build":{"toolchain":"rust-1.90","inputs":["Cargo.toml","Cargo.lock","main.rs"],"dependencies":[],"flags":[],"assets":[]}});
  let before=build_key(dir.path(),&manifest,false).unwrap();let legacy=build_key(dir.path(),&manifest,true).unwrap();
  manifest["build"]["assets"]=json!(["new-guidance.md"]);
  assert_eq!(before,build_key(dir.path(),&manifest,false).unwrap());
  assert_ne!(legacy,build_key(dir.path(),&manifest,true).unwrap());
 }
 #[test]fn incompatible_protocol_and_core_schema_collision_are_rejected(){
  let dir=tempfile::tempdir().unwrap();let path=package(dir.path());
  let p=path.join("plugin.json");let bytes=std::fs::read(&p).unwrap();let mut value:Value=serde_json::from_slice(&bytes).unwrap();
  value["protocol"]=json!(true);std::fs::write(&p,serde_json::to_vec(&value).unwrap()).unwrap();
  assert!(abilities::Catalog::load_snapshot(dir.path()).is_err());
  value["protocol"]=json!(1);value["tools"]=json!(["file_read"]);std::fs::write(&p,serde_json::to_vec(&value).unwrap()).unwrap();
  assert!(Package::load(&path,"fixture_ability",&["file_read".into()]).is_err());
 }
}

#[cfg(test)]
mod diagnostics_native_contract_tests {
    use super::*;
    #[test]
    /// Browser credentials remain behind native dispatch; malformed or incomplete contracts cannot install.
    fn diagnostics_retains_native_authority_and_rejects_incompatible_packages() {
        let root=tempfile::tempdir().unwrap();
        let tools=vec!["read_diagnostics".to_owned(),"login_as_current_user".to_owned(),"dev_browser_url".to_owned(),
            "dev_browser_action".to_owned(),"dev_browser_record".to_owned()];
        let manifest=json!({"version":1,"id":"diagnostics","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py",
            "tools":tools,"build":{"inputs":["runner.py","adapter.py"],"dependencies":[],"toolchain":"image-python3","flags":["-I","-B"],
            "assets":["diagnostics.json","diagnostics.skill.md","tools","skills"]}});
        for name in ["runner.py","adapter.py"] {std::fs::write(root.path().join(name),"# native fixture\n").unwrap();}
        std::fs::write(root.path().join("plugin.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(Package::load(root.path(),"diagnostics",&tools).unwrap().is_none());
        let mut invalid=manifest.clone();invalid["protocol"]=json!(true);
        std::fs::write(root.path().join("plugin.json"),serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(Package::load(root.path(),"diagnostics",&tools).is_err());
        std::fs::write(root.path().join("plugin.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::remove_file(root.path().join("adapter.py")).unwrap();
        assert!(Package::load(root.path(),"diagnostics",&tools).is_err());
    }
}
