//! Tests that span session's read_attachment and search_this_session tools.
use super::read_attachment::AttachmentArgs;
use super::search_this_session::{search_args, search_rows, SearchArgs};
use crate::text_search;
use serde_json::json;
#[test]
fn search_ranks_public_rows_and_validates_arguments() {
    let rows = [
        json!({"seq":1,"id":"m1","run_id":"r","role":"tool","created_at":1,"content":"gardening secret"}),
        json!({"seq":2,"id":"m2","run_id":"r","role":"user","created_at":2,"content":"Planning the garden beds"}),
        json!({"seq":3,"id":"m3","run_id":"r","role":"assistant","created_at":3,"content":"Gardening tips: water early"}),
    ];
    let args = SearchArgs {
        query: "gardens".into(),
        limit: None,
    };
    let terms = search_args(&args).unwrap();
    let limits = text_search::Limits::from_settings(&json!({"search_results_per_session":1}));
    let hits = search_rows(&rows, &terms, limits);
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit["role"] != "tool"));
    assert!(hits[0]["excerpt"].as_str().unwrap().contains("arden"));
    for bad in [
        json!({"query":""}),
        json!({"query":"the"}),
        json!({"query":"x","limit":0}),
    ] {
        let parsed: SearchArgs = serde_json::from_value(bad).unwrap();
        assert!(search_args(&parsed).is_err());
    }
    assert!(
        serde_json::from_value::<SearchArgs>(json!({"query":"x","session_id":"foreign"})).is_err()
    );
    assert!(serde_json::from_value::<AttachmentArgs>(
        json!({"attachment_id":"x","user_id":"foreign"})
    )
    .is_err());
}
