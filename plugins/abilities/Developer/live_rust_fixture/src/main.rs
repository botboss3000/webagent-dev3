use std::io::{self, Read};
fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let tail = input.split("\"invocation_id\"").nth(1).unwrap();
    let invocation = tail.split('"').nth(1).unwrap();
    assert!(invocation.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    println!("{{\"protocol\":1,\"invocation_id\":\"{}\",\"status\":\"success\",\"output\":{{\"marker\":\"RUST_ALPHA\"}}}}", invocation);
}
