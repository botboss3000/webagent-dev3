//! Provider identities, validated selections and model budgets without transport or storage authority.
pub mod provider;
pub mod limits;

pub fn valid_model(model:&str)->bool {
    !model.is_empty() && model.len()<=128 && model.bytes().all(|b|
        b.is_ascii_alphanumeric() || matches!(b,b'-'|b'.'|b'_'|b'/'|b':'))
}
