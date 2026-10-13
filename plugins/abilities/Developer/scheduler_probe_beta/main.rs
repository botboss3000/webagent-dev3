#![allow(long_running_const_eval)]
use std::io::Read;
// Bounded compile-time work makes independent compiler overlap observable.
const fn work() -> u64 { let mut x=1u64;let mut i=0;while i<1000000 {x=x.wrapping_mul(1664525).wrapping_add(1013904223);i+=1;}x }
const VALUE:u64=work();
fn main() {
 let mut data=String::new();
 std::io::stdin().take(1048576).read_to_string(&mut data).unwrap();
 let id=data.split("\"invocation_id\":\"").nth(1).and_then(|v|v.split('"').next()).unwrap_or("");
 std::hint::black_box(VALUE);
 println!("{{\"protocol\":1,\"invocation_id\":\"{}\",\"status\":\"success\",\"output\":\"scheduler parallel probe\"}}",id);
}
