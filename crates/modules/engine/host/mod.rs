//! Statically linked root composition of the Engine and existing foreign owners.
use crate::*;
pub(crate) mod policy;
mod native;
mod modules;
pub(crate) type Engine=webagent_engine::runtime::Runtime<Store,loop_runtime::NativeSchemas,abilities::Catalog,tool_execution::NativeActions,shared_chat::EngineChat,modules::Modules,policy::NativePolicy>;
impl App {
 pub(crate) fn engine(&self)->Engine {
  use webagent_engine::runtime::{Runtime,Handles};
  Runtime::new(Handles{runtime:self.runtime.clone(),runtime_path:self.runtime_path.clone(),compaction_config_path:self.compaction_config_path.clone(),control_dir:self.control_dir.clone(),gate:self.gate.clone(),active:self.active.clone(),slots:self.slots.clone(),ready:self.ready.clone(),stop:self.stop.clone()},self.store.clone(),self.abilities.clone(),self.provider.clone(),self.refresh.clone(),self.login.clone(),self.chat_actor.clone(),self.chat_access.clone(),tool_execution::NativeActions,shared_chat::EngineChat,modules::Modules,policy::NativePolicy::new(self),self.tool_socket.clone(),self.build_socket.clone())
 }
}
