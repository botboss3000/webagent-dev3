//! Provider usage normalization, per-run accumulation, bounded public projection
//! and session totals. Provider-reported accounting is separate from the
//! byte-measured model envelope; the host owns record keys, sealing and caching.
use anyhow::Result;
use serde_json::{json,Value};
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
pub struct Sample{pub at:i64,pub session:String,pub agent:String,pub entry:String,pub model:String,pub usage:Value}

// Civil UTC date from Unix seconds (Howard Hinnant's days-to-civil).
fn stamp(at:i64,minutes:bool)->String{
    let (days,secs)=(at.div_euclid(86400),at.rem_euclid(86400));
    let z=days+719468;let era=z.div_euclid(146097);let doe=z-era*146097;
    let yoe=(doe-doe/1460+doe/36524-doe/146096)/365;let doy=doe-(365*yoe+yoe/4-yoe/100);
    let mp=(5*doy+2)/153;let d=doy-(153*mp+2)/5+1;let m=if mp<10{mp+3}else{mp-9};let y=yoe+era*400+i64::from(m<=2);
    if minutes{format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}",secs/3600,secs%3600/60)}else{format!("{y:04}-{m:02}-{d:02}")}
}
fn tokens(usage:&Value)->u64{usage["input_tokens"].as_u64().unwrap_or(0).saturating_add(usage["output_tokens"].as_u64().unwrap_or(0))}
fn cost(usage:&Value)->f64{usage["known_cost_usd"].as_f64().filter(|v|v.is_finite()&&*v>=0.).unwrap_or(0.)}

/// Bucketed usage series for the model analytics chart. `minute` covers the last
/// hour, `hour` the last two days, `day` every day from the first sample to now.
pub fn analytics(samples:&[Sample],interval:&str,group_by:&str,labels:&std::collections::HashMap<String,String>,now:i64)->Option<Value>{
    let (width,fixed)=match interval{"minute"=>(60,Some(60)),"hour"=>(3600,Some(48)),"day"=>(86400,None),_=>return None};
    if !matches!(group_by,"models"|"sessions"|"agents"|"agent_models"){return None;}
    let last=now.div_euclid(width);
    let first=match fixed{Some(n)=>last-n+1,None=>samples.iter().map(|s|s.at.div_euclid(width)).min().unwrap_or(last).min(last).max(last-3660)};
    let mut days:Vec<Value>=(first..=last).map(|b|json!({"date":stamp(b*width,fixed.is_some()),"models":{}})).collect();
    let (mut totals,mut meta,mut series_labels)=(std::collections::HashMap::<String,u64>::new(),serde_json::Map::new(),serde_json::Map::new());
    for s in samples{
        let bucket=s.at.div_euclid(width);
        if bucket<first||bucket>last{continue;}
        let model=if s.model.is_empty(){"unknown"}else{&s.model};
        let key=match group_by{"models"=>model.to_owned(),"sessions"=>s.session.clone(),"agents"=>s.agent.clone(),_=>format!("{}/{model}",s.agent)};
        if group_by=="agent_models"{meta.insert(key.clone(),json!({"agent_id":s.agent,"agent_label":labels.get(&s.agent).unwrap_or(&s.agent)}));series_labels.insert(key.clone(),json!(model));}
        else if let Some(label)=labels.get(&key){series_labels.insert(key.clone(),json!(label));}
        let cell=&mut days[(bucket-first) as usize]["models"][&key];
        cell["usage_tokens"]=json!(cell["usage_tokens"].as_u64().unwrap_or(0).saturating_add(tokens(&s.usage)));
        cell["cost_usd"]=json!(cell["cost_usd"].as_f64().unwrap_or(0.)+cost(&s.usage));
        *totals.entry(key).or_default()+=tokens(&s.usage);
    }
    let mut models:Vec<(String,u64)>=totals.into_iter().collect();
    models.sort_by(|a,b|b.1.cmp(&a.1).then_with(||a.0.cmp(&b.0)));
    Some(json!({"days":days,"models":models.into_iter().map(|m|m.0).collect::<Vec<_>>(),"group_by":group_by,"interval":interval,"series_labels":series_labels,"series_meta":meta}))
}

/// Lifetime totals for one model-table row. A row is matched by entry id; rows
/// recorded without one count only when `include_unassigned` and the model matches.
pub fn model_totals(samples:&[Sample],entry:&str,model:&str,include_unassigned:bool)->Value{
    let (mut input,mut output,mut usd)=(0u64,0u64,0f64);
    for s in samples.iter().filter(|s|if entry.is_empty(){s.model==model}else{s.entry==entry||(include_unassigned&&s.entry.is_empty()&&s.model==model)}){
        input=input.saturating_add(s.usage["input_tokens"].as_u64().unwrap_or(0));
        output=output.saturating_add(s.usage["output_tokens"].as_u64().unwrap_or(0));
        usd+=cost(&s.usage);
    }
    json!({"total_input_tokens":input,"total_output_tokens":output,"total_cost_usd":usd})
}

/// Record a reset at `now`: `model` clears that model's history; `all`, `input`,
/// `output` or `cost` zero those counters for earlier runs. Run records stay intact.
pub fn mark_reset(resets:&mut Value,kind:&str,model:&str,now:i64)->bool{
    let fields:&[&str]=match kind{"model" if !model.is_empty()=>{resets["models"][model]=json!(now);return true}"all"=>&["input","output","cost"],"input"=>&["input"],"output"=>&["output"],"cost"=>&["cost"],_=>return false};
    for field in fields{resets["fields"][*field]=json!(now);}
    true
}
/// Apply recorded resets to one sample; false when its model's history was cleared.
pub fn apply_resets(sample:&mut Sample,resets:&Value)->bool{
    if resets["models"][sample.model.as_str()].as_i64().is_some_and(|at|sample.at<=at){return false;}
    for (field,key,zero) in [("input","input_tokens",json!(0)),("output","output_tokens",json!(0)),("cost","known_cost_usd",json!(0.0))]{
        if resets["fields"][field].as_i64().is_some_and(|at|sample.at<=at){sample.usage[key]=zero;}
    }
    true
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
    fn sample(at:i64,agent:&str,entry:&str,model:&str,input:u64,output:u64,usd:f64)->Sample{
        Sample{at,session:"s1".into(),agent:agent.into(),entry:entry.into(),model:model.into(),usage:json!({"input_tokens":input,"output_tokens":output,"known_cost_usd":usd})}
    }
    #[test]fn analytics_buckets_and_totals_match_samples(){
        let day=86400;let now=20_000*day+3600;
        let samples=[sample(now-2*day,"a","e1","m1",10,5,0.5),sample(now,"a","e1","m1",3,1,0.),sample(now,"b","","m2",7,0,0.25),sample(now-40*day,"a","e1","m1",1,1,0.)];
        let labels=std::collections::HashMap::from([("b".to_string(),"Agent B".to_string())]);
        let out=analytics(&samples,"day","models",&labels,now).unwrap();
        let days=out["days"].as_array().unwrap();
        assert_eq!(days.len(),41);assert_eq!(days[40]["date"],stamp(now,false));
        assert_eq!(days[40]["models"]["m1"]["usage_tokens"],4);assert_eq!(days[38]["models"]["m1"]["cost_usd"],0.5);
        assert_eq!(out["models"],json!(["m1","m2"]));
        let agents=analytics(&samples,"day","agents",&labels,now).unwrap();
        assert_eq!(agents["series_labels"]["b"],"Agent B");
        let nested=analytics(&samples,"day","agent_models",&labels,now).unwrap();
        assert_eq!(nested["series_meta"]["b/m2"]["agent_label"],"Agent B");assert_eq!(nested["series_labels"]["b/m2"],"m2");
        let hours=analytics(&samples,"hour","sessions",&labels,now).unwrap();
        assert_eq!(hours["days"].as_array().unwrap().len(),48);assert_eq!(hours["models"],json!(["s1"]));
        assert!(analytics(&samples,"week","models",&labels,now).is_none());
        assert_eq!(stamp(0,true),"1970-01-01T00:00");assert_eq!(stamp(951_782_400,false),"2000-02-29");
        assert_eq!(model_totals(&samples,"e1","m1",false),json!({"total_input_tokens":14,"total_output_tokens":7,"total_cost_usd":0.5}));
        assert_eq!(model_totals(&samples,"e2","m2",true)["total_input_tokens"],7);
        assert_eq!(model_totals(&samples,"e2","m2",false)["total_input_tokens"],0);
    }
    #[test]fn resets_clear_models_and_zero_counters_before_their_time(){
        let mut resets=Value::Null;
        assert!(mark_reset(&mut resets,"model","m1",100)&&mark_reset(&mut resets,"output","",200)&&!mark_reset(&mut resets,"model","",1)&&!mark_reset(&mut resets,"week","",1));
        let mut old=sample(100,"a","e1","m1",5,5,1.);let mut kept=sample(101,"a","e1","m1",5,5,1.);let mut other=sample(150,"a","","m2",3,4,0.5);
        assert!(!apply_resets(&mut old,&resets));assert!(apply_resets(&mut kept,&resets)&&apply_resets(&mut other,&resets));
        assert_eq!((kept.usage["input_tokens"].as_u64(),kept.usage["output_tokens"].as_u64(),kept.usage["known_cost_usd"].as_f64()),(Some(5),Some(0),Some(1.)));
        assert!(mark_reset(&mut resets,"all","",300));let mut late=sample(250,"a","","m2",3,4,0.5);apply_resets(&mut late,&resets);
        assert_eq!(model_totals(&[late],"","m2",false),json!({"total_input_tokens":0,"total_output_tokens":0,"total_cost_usd":0.0}));
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
