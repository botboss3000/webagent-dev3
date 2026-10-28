//! Usage report computation over samples the host has already authorized. It
//! reads nothing itself; the host collects runs, resets and labels.
use serde_json::{json,Value};
use std::collections::HashMap;
use webagent_usage::{Sample,UsageReports};

pub struct Reports;

impl UsageReports for Reports {
    async fn analytics(&self,samples:&[Sample],interval:&str,group_by:&str,labels:&HashMap<String,String>,now:i64)->anyhow::Result<Option<Value>>{
        Ok(analytics(samples,interval,group_by,labels,now))
    }
    async fn model_totals(&self,samples:&[Sample],entry:&str,model:&str,include_unassigned:bool)->anyhow::Result<Value>{
        Ok(model_totals(samples,entry,model,include_unassigned))
    }
    async fn apply_resets(&self,samples:&[Sample],resets:&Value)->anyhow::Result<Vec<Sample>>{
        Ok(samples.iter().cloned().filter_map(|mut sample|apply_resets(&mut sample,resets).then_some(sample)).collect())
    }
    async fn mark_reset(&self,resets:&Value,kind:&str,model:&str,now:i64)->anyhow::Result<Option<Value>>{
        let mut resets=resets.clone();
        let changed=if kind=="undo"{undo_reset(&mut resets)}else{mark_reset(&mut resets,kind,model,now)};
        Ok(changed.then_some(resets))
    }
}

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
pub fn analytics(samples:&[Sample],interval:&str,group_by:&str,labels:&HashMap<String,String>,now:i64)->Option<Value>{
    let (width,fixed)=match interval{"minute"=>(60,Some(60)),"hour"=>(3600,Some(48)),"day"=>(86400,None),_=>return None};
    if !matches!(group_by,"models"|"sessions"|"agents"|"agent_models"){return None;}
    let last=now.div_euclid(width);
    let first=match fixed{Some(n)=>last-n+1,None=>samples.iter().map(|s|s.at.div_euclid(width)).min().unwrap_or(last).min(last).max(last-3660)};
    let mut days:Vec<Value>=(first..=last).map(|b|json!({"date":stamp(b*width,fixed.is_some()),"models":{}})).collect();
    let (mut totals,mut meta,mut series_labels)=(HashMap::<String,u64>::new(),serde_json::Map::new(),serde_json::Map::new());
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
/// The markers before the latest reset are kept so it can be undone once.
pub fn mark_reset(resets:&mut Value,kind:&str,model:&str,now:i64)->bool{
    let fields:&[&str]=match kind{"model" if !model.is_empty()=>&[],"all"=>&["input","output","cost"],"input"=>&["input"],"output"=>&["output"],"cost"=>&["cost"],_=>return false};
    let previous=json!({"models":resets["models"].clone(),"fields":resets["fields"].clone()});
    if kind=="model"{resets["models"][model]=json!(now);}
    for field in fields{resets["fields"][*field]=json!(now);}
    resets["undo"]=previous;
    true
}
/// Restore the markers from before the latest reset; false when there is nothing to undo.
pub fn undo_reset(resets:&mut Value)->bool{
    let Some(previous)=resets.as_object_mut().and_then(|r|r.remove("undo")).filter(Value::is_object) else{return false};
    resets["models"]=previous["models"].clone();resets["fields"]=previous["fields"].clone();
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
    fn sample(at:i64,agent:&str,entry:&str,model:&str,input:u64,output:u64,usd:f64)->Sample{
        Sample{at,session:"s1".into(),agent:agent.into(),entry:entry.into(),model:model.into(),usage:json!({"input_tokens":input,"output_tokens":output,"known_cost_usd":usd})}
    }
    #[test]fn analytics_buckets_and_totals_match_samples(){
        let day=86400;let now=20_000*day+3600;
        let samples=[sample(now-2*day,"a","e1","m1",10,5,0.5),sample(now,"a","e1","m1",3,1,0.),sample(now,"b","","m2",7,0,0.25),sample(now-40*day,"a","e1","m1",1,1,0.)];
        let labels=HashMap::from([("b".to_string(),"Agent B".to_string())]);
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
        // Undo restores the markers from before the latest reset, once.
        assert!(undo_reset(&mut resets));assert_eq!(resets["fields"]["input"],Value::Null);assert_eq!(resets["fields"]["output"],200);
        let mut late=sample(250,"a","","m2",3,4,0.5);apply_resets(&mut late,&resets);assert_eq!(late.usage["input_tokens"],3);
        assert!(!undo_reset(&mut resets));
        let mut cleared=Value::Null;assert!(mark_reset(&mut cleared,"model","m1",100)&&undo_reset(&mut cleared));
        assert!(apply_resets(&mut sample(50,"a","e1","m1",1,1,0.),&cleared));
    }
    #[tokio::test]async fn interface_matches_the_functions(){
        let samples=vec![sample(100,"a","e1","m1",5,5,1.),sample(200,"a","","m2",3,4,0.5)];
        let resets=Reports.mark_reset(&Value::Null,"model","m1",150).await.unwrap().unwrap();
        assert_eq!(Reports.apply_resets(&samples,&resets).await.unwrap(),vec![samples[1].clone()]);
        assert!(Reports.mark_reset(&resets,"week","",1).await.unwrap().is_none());
        assert!(Reports.mark_reset(&Value::Null,"undo","",1).await.unwrap().is_none());
        assert_eq!(Reports.model_totals(&samples,"e1","m1",false).await.unwrap(),model_totals(&samples,"e1","m1",false));
        let labels=HashMap::new();
        assert_eq!(Reports.analytics(&samples,"day","models",&labels,300).await.unwrap(),analytics(&samples,"day","models",&labels,300));
    }
}
