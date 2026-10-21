//! Thin static Modules composition; authority and package locking remain the existing owner.
use crate::*;
#[derive(Clone)] pub(crate) struct Modules;
impl webagent_engine::runtime::ports::ModuleBindings<Store> for Modules {
 fn binding_valid<'a>(&'a self,store:&'a Store,run:&'a Value)->impl std::future::Future<Output=bool>+Send+'a {crate::modules::binding_valid_store(store,run)}
 fn binding_lease<'a>(&'a self,store:&'a Store,run:&'a Value)->impl std::future::Future<Output=Result<Option<std::fs::File>>>+Send+'a {crate::modules::binding_lease_store(store,run)}
}
