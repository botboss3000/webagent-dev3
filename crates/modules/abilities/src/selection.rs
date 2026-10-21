use serde_json::{json,Value};
/// Encode a content-free tool identity for the run projection.
pub fn marker(name:&str)->String {format!("tool:{name}")}
/// Merge successful native selection receipts; host fences and the ledger own persistence.
pub fn apply_selection(run:&mut Value,result:&Value) {
    if !matches!(run["pending"]["name"].as_str(),Some("search_tools"|"load_tool"))||result["status"]!="success"||result["output"]["activated"]!=true {return;}
    let Some(items)=result["output"]["selected_tools"].as_array() else {return;};
    if items.len()>32||items.iter().any(|v|v.as_str().is_none()) {return;}
    let mut loaded=run["loaded_abilities"].as_array().cloned().unwrap_or_default();
    let mut selected=run["selected_tools"].as_array().cloned().unwrap_or_default();
    for item in items {
        let tool_marker=json!(marker(item.as_str().unwrap()));
        if !loaded.contains(&tool_marker) {loaded.push(tool_marker);}
        if !selected.contains(item) {selected.push(item.clone());}
    }
    // This run mirrors content-free conversation selectors. The host commits the
    // conversation ledger after the native fence; these markers grant no permission.
    run["loaded_abilities"]=json!(loaded);run["selected_tools"]=json!(selected);
}
