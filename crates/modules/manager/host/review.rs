//! Actual review control flow lives in the Manager package.
use super::*;
pub(super) async fn judge(app:&App,record:&mut Value)->Result<Verdict> {bridge::runtime(app).judge(record).await}
