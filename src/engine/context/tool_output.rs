//! Root compatibility for Engine-owned lifetime and duplicate result context.
pub use webagent_engine::context::retention::*;
use crate::*;
#[cfg(test)] use std::fs;
#[cfg(test)] use webagent_engine::context::retention::fixture_record_dedup_origin as record_dedup_origin;
pub async fn dedup_prepare(app: &crate::App, run: &mut Value, preview_bytes: usize,
    cancel: &tokio_util::sync::CancellationToken) -> Result<Vec<Value>> {app.engine().dedup_prepare(run,preview_bytes,cancel).await}
pub async fn pause_dedup(app:&crate::App,run:&mut Value,error:&anyhow::Error)->Result<()> {app.engine().pause_dedup(run,error).await}
pub async fn dedup_summary_source(app: &crate::App, run: &Value, source: &[Value]) -> Result<Vec<Value>> {app.engine().dedup_summary_source(run,source).await}
pub async fn dedup_config_fingerprint(app:&crate::App,agent:&str,sid:&str)->Result<String> {app.engine().dedup_config_fingerprint(agent,sid).await}
#[cfg(test)] mod dedup_tests {
    use super::*;
    const TEMPLATE:&str="Call {current_call_id} equals {reference_call_id} from {tool}.";
    #[test] fn duplicate_prompt_has_no_fallback_and_reloads_the_actual_live_file() {
        let root=tempfile::tempdir().unwrap();assert!(dedup_template(root.path()).is_err());
        fs::create_dir(root.path().join("config")).unwrap();let path=root.path().join("config/prompts.json");
        fs::write(&path,"{}").unwrap();assert!(dedup_template(root.path()).is_err());
        fs::write(&path,json!({"tool_result_deduplication":{"placeholder":TEMPLATE}}).to_string()).unwrap();
        assert_eq!(dedup_template(root.path()).unwrap(),TEMPLATE);
        let changed="Changed {current_call_id} equals {reference_call_id}";
        fs::write(&path,json!({"tool_result_deduplication":{"placeholder":changed}}).to_string()).unwrap();
        assert_eq!(dedup_template(root.path()).unwrap(),changed);
        for value in [json!(null),json!(false),json!(""),json!("{unknown}")] {
            fs::write(&path,json!({"tool_result_deduplication":{"placeholder":value}}).to_string()).unwrap();
            assert!(dedup_template(root.path()).is_err());
        }
    }
    #[tokio::test] async fn lifetime_policy_is_disabled_without_discarding_saved_configuration() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let path=app.store.root.join("config/tool-output-retention.json");
        let saved=json!({"enabled":true,"scope":"selected","tools":["file_read"],"lifetime_calls":17});
        fs::write(&path,saved.to_string()).unwrap();
        let policy=effective(&app.store,"shared_default").await.unwrap();
        assert!(!policy.enabled);assert_eq!(policy.lifetime_calls,17);
        assert_eq!(serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),saved);
    }
    #[test] fn allowlist_is_explicit_and_does_not_grant_permission() {
        assert!(dedup_included(&json!({}),"file_read"));
        assert!(!dedup_included(&json!({}),"file_write"));
        assert!(!dedup_included(&json!({"tool_result_deduplication":{"file_read":false}}),"file_read"));
        assert!(dedup_included(&json!({"tool_result_deduplication":{"file_write":true}}),"file_write"));
    }

    async fn journal(app:&crate::App,run:&mut Value,id:&str,path:&str,body:&Value) {
        let call=json!({"type":"function_call","call_id":id,"name":"file_read","arguments":json!({"path":path}).to_string()});
        run["input"].as_array_mut().unwrap().push(call.clone());
        app.store.save_run(run,"model_output",&json!({"output":[call.clone()]})).await.unwrap();
        record_dedup_origin(run,&call,body).unwrap();
        run["input"].as_array_mut().unwrap().push(json!({"type":"function_call_output","call_id":id,"output":body.to_string()}));
        app.store.save_run(run,"tool_result",&json!({})).await.unwrap();
    }
    /// Native control pending state has no name; retain exact identity and recover old paused runs safely.
    #[tokio::test]
    async fn native_control_provenance_uses_recorded_calls_and_recovers_null_names() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let cancel=tokio_util::sync::CancellationToken::new();
        dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        for (turn,name) in ["set_closer_status","publish_main_message"].into_iter().enumerate() {
            run["turns"]=json!(turn+1);
            let id=format!("native-{name}");
            let call=json!({"type":"function_call","call_id":id,"name":name,"arguments":"{}"});
            run["input"].as_array_mut().unwrap().push(call.clone());
            app.store.save_run(&mut run,"model_output",&json!({"output":[call]})).await.unwrap();
            run["pending"]=json!({"call_id":id});
            let body=json!({"status":"ok","output":"Native control recorded"});
            record(&mut run,&body).unwrap();
            assert_eq!(run["tool_result_projection"]["origins"][&id]["tool"],name);
            run["input"].as_array_mut().unwrap().push(json!({"type":"function_call_output","call_id":id,"output":body.to_string()}));
            app.store.save_run(&mut run,"tool_result",&json!({})).await.unwrap();
            let projected=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
            assert_eq!(projected.last().unwrap()["output"],body.to_string());
            assert_eq!(app.store.tool_result(&sid,&rid,&id).await.unwrap().unwrap()["output"],body.to_string());
            // Simulate the already-deployed defect in a persisted run, then resume it.
            run["tool_result_projection"]["origins"][&id]["tool"]=Value::Null;
            app.save(&mut run).await.unwrap();run=app.run(&rid).await.unwrap();
            let recovered=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
            assert_eq!(recovered,projected);
            assert_eq!(run["tool_result_projection"]["origins"][&id]["tool"],name);
            let valid=run.clone();
            run["tool_result_projection"]["origins"][&id]["tool"]=json!("file_write");
            assert!(dedup_prepare(&app,&mut run,8192,&cancel).await.is_err());
            run=valid.clone();
            run["tool_result_projection"]["origins"][&id]["tool"]=Value::Null;
            run["tool_result_projection"]["origins"][&id]["sha256"]=json!("0".repeat(64));
            assert!(dedup_prepare(&app,&mut run,8192,&cancel).await.is_err());
            run=valid.clone();
            run["tool_result_projection"]["origins"][&id]["tool"]=Value::Null;
            run["input"].as_array_mut().unwrap().iter_mut().find(|item|
                item["type"]=="function_call" && item["call_id"]==id).unwrap()["name"]=json!("file_write");
            assert!(dedup_prepare(&app,&mut run,8192,&cancel).await.is_err());
            run=valid;
        }
    }

    #[tokio::test]
    async fn duplicate_projection_preserves_journal_prefix_restart_and_compaction_primary() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let cancel=tokio_util::sync::CancellationToken::new();
        dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        let body=json!({"status":"success","content":"public fixture ".repeat(1200)});
        journal(&app,&mut run,"first","a.rs",&body).await;
        let first=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        assert_eq!(first.last().unwrap()["output"],body.to_string());
        journal(&app,&mut run,"second","a.rs",&body).await;
        let second=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        assert_eq!(&second[..first.len()],first.as_slice());
        let placeholder:Value=serde_json::from_str(second.last().unwrap()["output"].as_str().unwrap()).unwrap();
        assert_eq!(placeholder["reference_call_id"],"first");
        assert_eq!(app.store.tool_result(&sid,&rid,"second").await.unwrap().unwrap()["output"],body.to_string());
        let mut restarted=app.run(&rid).await.unwrap();
        assert_eq!(dedup_prepare(&app,&mut restarted,8192,&cancel).await.unwrap(),second);
        restarted["input"].as_array_mut().unwrap().retain(|item|item["call_id"]!="first");
        restarted["tool_result_projection"]["rebase"]=json!(true);
        app.save(&mut restarted).await.unwrap();
        let compacted=dedup_prepare(&app,&mut restarted,8192,&cancel).await.unwrap();
        assert_eq!(compacted.last().unwrap()["output"],body.to_string());
        assert_eq!(app.store.tool_result_origin(&sid,"first").await.unwrap().unwrap().1["output"],body.to_string());
        journal(&app,&mut restarted,"third","a.rs",&body).await;
        let third=dedup_prepare(&app,&mut restarted,8192,&cancel).await.unwrap();
        let placeholder:Value=serde_json::from_str(third.last().unwrap()["output"].as_str().unwrap()).unwrap();
        assert_eq!(placeholder["reference_call_id"],"second");
        let before=third.clone();journal(&app,&mut restarted,"other","b.rs",&json!({"status":"success","content":"other"})).await;
        let other=dedup_prepare(&app,&mut restarted,8192,&cancel).await.unwrap();assert_eq!(&other[..before.len()],before.as_slice());
    }


    #[tokio::test]
    async fn migration_preview_stays_stable_and_new_duplicates_reference_only_a_full_body() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let cancel=tokio_util::sync::CancellationToken::new();
        let body=json!({"status":"success","content":"public original content ".repeat(1000)});
        journal(&app,&mut run,"seeded-preview","a.rs",&body).await;
        let seeded=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        assert_ne!(seeded.last().unwrap()["output"],body.to_string());
        journal(&app,&mut run,"first-full","a.rs",&body).await;
        let full=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        assert_eq!(&full[..seeded.len()],seeded.as_slice());
        assert_eq!(full.last().unwrap()["output"],body.to_string());
        journal(&app,&mut run,"later-placeholder","a.rs",&body).await;
        let duplicate=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        let receipt:Value=serde_json::from_str(duplicate.last().unwrap()["output"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["reference_call_id"],"first-full");
        let reference=duplicate.iter().find(|item|item["type"]=="function_call_output"&&item["call_id"]=="first-full").unwrap();
        assert_eq!(reference["output"],body.to_string());
    }

    #[tokio::test]
    async fn projection_pause_retains_input_and_only_marks_component_unavailability_for_restore() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let input=run["input"].clone();
        pause_dedup(&app,&mut run,&anyhow::anyhow!("prompts.json: placeholder is required")).await.unwrap();
        assert_eq!(run["status"],"paused");assert_eq!(run["input"],input);
        assert!(run["error"].as_str().unwrap().contains("prompts.json"));
        assert!(!crate::engine_components::retained_pause(&run));
        let mut controls=crate::engine_components::controls(&app.store.root).unwrap();
        controls.disabled.push("context-assembly".into());
        crate::engine_components::save_controls(&app.store.root,&controls).unwrap();
        pause_dedup(&app,&mut run,&anyhow::anyhow!("component withdrawn during projection")).await.unwrap();
        assert_eq!(run["input"],input);assert!(crate::engine_components::retained_pause(&run));
        assert_eq!(run["component_pause"]["component"],"context-assembly");
    }

    #[tokio::test]
    async fn parallel_result_completion_uses_recorded_call_order() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let cancel=tokio_util::sync::CancellationToken::new();
        dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        let body=json!({"status":"success","content":"same public output ".repeat(500)});
        journal(&app,&mut run,"first-parallel","a.rs",&body).await;
        journal(&app,&mut run,"second-parallel","a.rs",&body).await;
        let all=run["input"].as_array().unwrap().clone();
        let mut input:Vec<Value>=all.iter().filter(|item|item["type"]!="function_call_output").cloned().collect();
        input.extend(all.iter().rev().filter(|item|item["type"]=="function_call_output").cloned());
        run["input"]=json!(input);app.save(&mut run).await.unwrap();
        let projected=dedup_prepare(&app,&mut run,8192,&cancel).await.unwrap();
        let second=projected.iter().find(|item|item["type"]=="function_call_output"&&item["call_id"]=="second-parallel").unwrap();
        let receipt:Value=serde_json::from_str(second["output"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["reference_call_id"],"first-parallel");
        let first=projected.iter().find(|item|item["type"]=="function_call_output"&&item["call_id"]=="first-parallel").unwrap();
        assert_eq!(first["output"],body.to_string());
    }

}
