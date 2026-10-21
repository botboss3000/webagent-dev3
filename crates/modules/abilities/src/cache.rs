//! One caller-owned cache of complete snapshots; selected invalid sources never fall back.
use crate::{Catalog,Host};
use anyhow::Result;
use std::{collections::HashMap,path::PathBuf,sync::{Arc,Mutex}};
type Key=(PathBuf,bool,Option<String>);
pub struct CatalogCache<H:Host> { values:Mutex<HashMap<Key,Arc<Catalog<H>>>> }
impl<H:Host> Default for CatalogCache<H> {fn default()->Self {Self{values:Mutex::new(HashMap::new())}}}
impl<H:Host> CatalogCache<H> {
 pub fn current(&self,fallback:&Arc<Catalog<H>>)->Result<Arc<Catalog<H>>> {
  let selected=H::selected_source()?;
  // An empty policy-metadata fallback is never interpreted as the working directory.
  if selected.is_none()&&fallback.snapshot_root().as_os_str().is_empty(){return Ok(fallback.clone());}
  let optimization=H::optimization_declaration()?;
  let generation=optimization.as_ref().map(|(generation,_)|generation.clone());
  let (root,published)=match selected {
   Some(source)=>(source.join("plugins/abilities"),true),
   None=>{
    if fallback.optimization_generation()==&generation{return Ok(fallback.clone());}
    (fallback.snapshot_root().to_owned(),fallback.published())
   },
  };
  let key=(root.clone(),published,generation);
  let mut values=self.values.lock().unwrap_or_else(|e|e.into_inner());
  if let Some(value)=values.get(&key){return Ok(value.clone());}
  let mut value=Catalog::load_mode_with_optimization(&root,!published,published,optimization)?;
  if published {value.freeze();}
  let value=Arc::new(value);
  if values.len()>=8 {values.clear();}
  values.insert(key,value.clone());Ok(value)
 }
}
