//! Model catalogs, selection, provider IO and runtime; foreign authority is supplied by its owner.
pub mod provider;
pub mod limits;
pub mod providers;

pub fn valid_model(model:&str)->bool {
    !model.is_empty() && model.len()<=128 && model.bytes().all(|b|
        b.is_ascii_alphanumeric() || matches!(b,b'-'|b'.'|b'_'|b'/'|b':'))
}

pub mod storage;
#[path="selection.rs"] pub mod model_selection;
pub mod account_catalog;
pub mod platform_catalog;
pub mod transport;

pub mod runtime;
pub mod api;

#[cfg(test)] mod model_io_tests;

mod agent_rosters;
