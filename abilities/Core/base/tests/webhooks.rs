//! Webhook ID, projection and ownership tests spanning several webhook tools.
use super::register_webhook::id_from;
use super::*;
#[test]
fn webhook_ids_are_stable_uuids_and_projection_hides_the_token() {
    let id = id_from(&json!(["admin", "s", "r", "p"]));
    assert_eq!(id, id_from(&json!(["admin", "s", "r", "p"])));
    assert_ne!(id, id_from(&json!(["admin", "s", "r", "q"])));
    let parsed = Uuid::parse_str(&id).unwrap();
    assert_eq!(parsed.get_version_num(), 5);
    let record = json!({"id":id,"revision":2,"artifact":{"name":"n","expires_at":now()+60,"token":"secret","deleted":false}});
    let view = public(&record);
    assert_eq!(view["active"], true);
    assert!(view.get("token").is_none());
    assert_eq!(
        view["setup_url"],
        format!("/api/v3/core/webhooks/{id}/setup")
    );
    assert_eq!(token_digest("t"), digest(&json!("t")));
    assert!(owns("get_webhook_log") && !owns("webhook_cleanup"));
}
