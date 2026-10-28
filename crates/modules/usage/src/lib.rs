//! Provider usage normalization, per-run accumulation, bounded public projection
//! and session totals. Provider-reported accounting is separate from the
//! byte-measured model envelope; the host owns record keys, sealing and caching.
//! Report computation lives in `webagent-usage-reports` behind `UsageReports`.
use anyhow::Result;
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use std::collections::HashMap;
use webagent_module_service::module_interface;
use sqlx::{Sqlite,Transaction};
use std::future::Future;

const COUNTERS:[&str;7]=["input_tokens","output_tokens","cached_input_tokens","reported_calls","unreported_calls","unknown_cost_calls","unknown_cache_calls"];

fn reported(response:&Value)->Option<Value>{
    let usage=&response["usage"];
    let input=usage["input_tokens"].as_u64().or_else(||usage["prompt_tokens"].as_u64())?;
    let output=usage["output_tokens"].as_u64().or_else(||usage["completion_tokens"].as_u64())?;
    if input>1_000_000_000 || output>1_000_000_000{return None;}
    let cached=usage["input_tokens_details"]["cached_tokens"].as_u64()
        .or_else(||usage["prompt_tokens_details"]["cached_tokens"].as_u64())
        .or_else(||usage["prompt_cache_hit_tokens"].as_u64()).or_else(||usage["cached_input_tokens"].as_u64()).filter(|v|*v<=input);
    let cost=usage["cost"].as_f64().or_else(||usage["cost_usd"].as_f64()).filter(|v|v.is_finite() && *v>=0. && *v<=1_000_000.);
    Some(json!({"input_tokens":input,"output_tokens":output,"cached_input_tokens":cached,"cost_usd":cost,"source":"provider"}))
}
pub fn record(run:&mut Value,response:&Value){
    let sample=reported(response);
    let mut usage=if run["usage"].is_object(){run["usage"].clone()}else{json!({"input_tokens":0,"output_tokens":0,"cached_input_tokens":0,"reported_calls":0,"unreported_calls":0,"known_cost_usd":0.0,"unknown_cost_calls":0,"unknown_cache_calls":0})};
    if let Some(sample)=&sample{
        for field in ["input_tokens","output_tokens","cached_input_tokens"]{
            usage[field]=json!(usage[field].as_u64().unwrap_or(0).saturating_add(sample[field].as_u64().unwrap_or(0)));
        }
        if sample["cached_input_tokens"].is_null(){usage["unknown_cache_calls"]=json!(usage["unknown_cache_calls"].as_u64().unwrap_or(0)+1);}
        usage["reported_calls"]=json!(usage["reported_calls"].as_u64().unwrap_or(0)+1);
        if let Some(cost)=sample["cost_usd"].as_f64(){
            usage["known_cost_usd"]=json!(usage["known_cost_usd"].as_f64().unwrap_or(0.)+cost);
        }else{usage["unknown_cost_calls"]=json!(usage["unknown_cost_calls"].as_u64().unwrap_or(0)+1);}
    }else{
        usage["unknown_cache_calls"]=json!(usage["unknown_cache_calls"].as_u64().unwrap_or(0)+1);
        usage["unreported_calls"]=json!(usage["unreported_calls"].as_u64().unwrap_or(0)+1);
        usage["unknown_cost_calls"]=json!(usage["unknown_cost_calls"].as_u64().unwrap_or(0)+1);
    }
    usage["latest"]=sample.unwrap_or(Value::Null);
    run["usage"]=usage;
}
// A fixed numeric projection cannot disclose provider extras or request context.
pub fn public(value:&Value)->Value{
    if !value.is_object(){return Value::Null;}
    let mut result=serde_json::Map::new();
    for field in COUNTERS{
        result.insert(field.into(),json!(value[field].as_u64().unwrap_or(0)));
    }
    result.insert("known_cost_usd".into(),json!(value["known_cost_usd"].as_f64().filter(|v|v.is_finite()&&*v>=0.)));
    if let Some(sample)=reported(&json!({"usage":value["latest"]})){result.insert("latest".into(),sample);}
    Value::Object(result)
}

/// Sealed session-total records inside the caller's transaction.
pub trait SessionRecords:Sync {
 /// Public usage this run last contributed, or null before its first save.
 fn previous_run_usage(&self,tx:&mut Transaction<'_,Sqlite>,run:&Value)->impl Future<Output=Result<Value>>+Send;
 fn read_total(&self,tx:&mut Transaction<'_,Sqlite>,session_id:&str)->impl Future<Output=Result<Option<Value>>>+Send;
 fn write_total(&self,tx:&mut Transaction<'_,Sqlite>,session_id:&str,total:&Value)->impl Future<Output=Result<()>>+Send;
 fn invalidate_total(&self,session_id:&str);
}

/// Replace this run's previous contribution to its session total, so re-saving
/// a run never double counts. Returns the run's public usage.
pub async fn stage_session_total<'a,'c:'a,R:SessionRecords>(records:&'a R,tx:&'a mut Transaction<'c,Sqlite>,run:&'a Value)->Result<Value>{
    let previous=records.previous_run_usage(tx,run).await?;
    let new_usage=public(&run["usage"]);
    if new_usage.is_object() && previous!=new_usage {
        let sid=run["session_id"].as_str().unwrap();
        let mut total=records.read_total(tx,sid).await?.unwrap_or(json!({}));
        for field in COUNTERS{
            total[field]=json!(total[field].as_u64().unwrap_or(0).saturating_sub(previous[field].as_u64().unwrap_or(0)).saturating_add(new_usage[field].as_u64().unwrap_or(0)));
        }
        total["known_cost_usd"]=json!((total["known_cost_usd"].as_f64().unwrap_or(0.)-previous["known_cost_usd"].as_f64().unwrap_or(0.)+new_usage["known_cost_usd"].as_f64().unwrap_or(0.)).max(0.));
        records.write_total(tx,sid,&total).await?;
        // Aggregates are read from the database; they must not use the doc cache.
        records.invalidate_total(sid);
    }
    Ok(new_usage)
}

/// One run's public usage, attributed to its last update and its session's agent.
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct Sample{pub at:i64,pub session:String,pub agent:String,pub entry:String,pub model:String,pub usage:Value}

/// Usage reports for the Models pages. `webagent-usage-reports` implements it:
/// linked into stable builds, its own service process (`usage-reports`) in dev builds.
#[module_interface(version = 1)]
pub trait UsageReports {
    /// Bucketed usage series for the model analytics chart, or None for an invalid interval or grouping.
    async fn analytics(&self,samples:&[Sample],interval:&str,group_by:&str,labels:&HashMap<String,String>,now:i64)->anyhow::Result<Option<Value>>;
    /// Lifetime totals for one model-table row.
    async fn model_totals(&self,samples:&[Sample],entry:&str,model:&str,include_unassigned:bool)->anyhow::Result<Value>;
    /// The samples left after `resets`, with reset counters zeroed.
    async fn apply_resets(&self,samples:&[Sample],resets:&Value)->anyhow::Result<Vec<Sample>>;
    /// The markers after recording reset `kind` (`undo` restores the previous ones),
    /// or None when the reset is invalid or there is nothing to undo.
    async fn mark_reset(&self,resets:&Value,kind:&str,model:&str,now:i64)->anyhow::Result<Option<Value>>;
}

#[cfg(test)]
mod tests{
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::atomic::{AtomicUsize,Ordering};
    #[test]fn usage_is_reported_bounded_and_private(){
        let mut run=json!({});
        record(&mut run,&json!({"usage":{"prompt_tokens":20,"completion_tokens":5,"prompt_cache_hit_tokens":4,"cost":0.12,"private":"secret"}}));
        record(&mut run,&json!({"usage":{"input_tokens":10,"output_tokens":2}}));
        record(&mut run,&json!({}));
        let out=public(&run["usage"]);
        assert_eq!(out["input_tokens"],30);assert_eq!(out["output_tokens"],7);
        assert_eq!(out["cached_input_tokens"],4);assert_eq!(out["reported_calls"],2);
        assert_eq!(out["unreported_calls"],1);assert_eq!(out["unknown_cost_calls"],2);
        assert!(!out.to_string().contains("secret"));
        assert!(reported(&json!({"usage":{"input_tokens":-1,"output_tokens":4}})).is_none());
    }
    struct Docs{invalidated:AtomicUsize}
    async fn doc(tx:&mut Transaction<'_,Sqlite>,key:&str)->Result<Option<Value>>{
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(key).fetch_optional(&mut **tx).await?;
        Ok(raw.map(|raw|serde_json::from_slice(&raw)).transpose()?)
    }
    async fn put(tx:&mut Transaction<'_,Sqlite>,key:&str,value:&Value)->Result<()>{
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(serde_json::to_vec(value)?).execute(&mut **tx).await?;Ok(())
    }
    impl SessionRecords for Docs {
        async fn previous_run_usage(&self,tx:&mut Transaction<'_,Sqlite>,run:&Value)->Result<Value>{
            Ok(doc(tx,&format!("run_view:{}",run["id"].as_str().unwrap())).await?.map_or(Value::Null,|view|view["usage"].clone()))
        }
        async fn read_total(&self,tx:&mut Transaction<'_,Sqlite>,sid:&str)->Result<Option<Value>>{doc(tx,&format!("session_usage:{sid}")).await}
        async fn write_total(&self,tx:&mut Transaction<'_,Sqlite>,sid:&str,total:&Value)->Result<()>{put(tx,&format!("session_usage:{sid}"),total).await}
        fn invalidate_total(&self,_:&str){self.invalidated.fetch_add(1,Ordering::SeqCst);}
    }
    /// Stage like the run repository: total and run view commit in one transaction.
    async fn save(pool:&sqlx::SqlitePool,docs:&Docs,run:&Value,commit:bool)->Value{
        let mut tx=pool.begin().await.unwrap();
        let usage=stage_session_total(docs,&mut tx,run).await.unwrap();
        put(&mut tx,&format!("run_view:{}",run["id"].as_str().unwrap()),&json!({"usage":usage})).await.unwrap();
        if commit{tx.commit().await.unwrap();}else{tx.rollback().await.unwrap();}
        usage
    }
    async fn total(pool:&sqlx::SqlitePool)->Option<Value>{
        doc(&mut pool.begin().await.unwrap(),"session_usage:s").await.unwrap()
    }
    #[tokio::test]async fn session_total_replaces_each_runs_contribution(){
        let pool=SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL)").execute(&pool).await.unwrap();
        let docs=Docs{invalidated:AtomicUsize::new(0)};
        let mut one=json!({"id":"r1","session_id":"s"});
        record(&mut one,&json!({"usage":{"input_tokens":20,"output_tokens":5,"cost":0.5,"private":"secret"}}));
        save(&pool,&docs,&one,true).await;
        save(&pool,&docs,&one,true).await;
        assert_eq!(docs.invalidated.load(Ordering::SeqCst),1,"an unchanged re-save must not touch the total");
        let mut two=json!({"id":"r2","session_id":"s"});
        record(&mut two,&json!({"usage":{"input_tokens":7,"output_tokens":1}}));
        save(&pool,&docs,&two,true).await;
        record(&mut one,&json!({"usage":{"input_tokens":3,"output_tokens":2}}));
        save(&pool,&docs,&one,true).await;
        let t=total(&pool).await.unwrap();
        assert_eq!((t["input_tokens"].as_u64(),t["output_tokens"].as_u64(),t["reported_calls"].as_u64()),(Some(30),Some(8),Some(3)));
        assert_eq!(t["known_cost_usd"],0.5);assert!(!t.to_string().contains("secret"));
        // A rolled-back save leaves the committed total untouched.
        record(&mut two,&json!({"usage":{"input_tokens":100,"output_tokens":100}}));
        save(&pool,&docs,&two,false).await;
        assert_eq!(total(&pool).await.unwrap()["input_tokens"],30);
        assert!(stage_session_total(&docs,&mut pool.begin().await.unwrap(),&json!({"id":"r3","session_id":"s"})).await.unwrap().is_null());
    }
}
