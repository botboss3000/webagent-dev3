/// Encode a content-free tool identity for the run projection.
pub fn marker(name: &str) -> String {
    format!("tool:{name}")
}
