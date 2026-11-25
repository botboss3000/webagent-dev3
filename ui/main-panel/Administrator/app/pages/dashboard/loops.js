// Agent loops: the administrator-only /api/v3/admin/system/loops view, shown on the Dashboard
// and the Builds page: how many loops run and, for each, its state, heartbeat age, runs in
// progress and accounts assigned. Read-only; a failed read is "unavailable", never "0 loops".
const STATES=new Set(['serving','starting','stale','missing']);
const count=value=>Number.isInteger(value)&&value>=0;

/** Read and check the view; failures come back as {read_error}. */
export async function readLoops(signal){
 try{
  const response=await fetch('/api/v3/admin/system/loops',{credentials:'same-origin',cache:'no-store',signal});
  if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===404?'Administrator access required':'Loop state unavailable ('+response.status+')');
  const view=await response.json();
  const member=row=>row&&count(row.index)&&STATES.has(row.state)&&count(row.runs_in_progress)&&count(row.accounts)
   &&(row.heartbeat_age_seconds===null||count(row.heartbeat_age_seconds));
  if(!view||view.schema_version!==1||!count(view.loops)||view.loops>16||!Array.isArray(view.members)||view.members.length!==view.loops||!view.members.every(member))throw Error('Invalid loop state response');
  return view;
 }catch(error){if(error.name==='AbortError')throw error;return {read_error:error.message};}
}

/** Tile state, badge, one row per loop and a note. */
export function loopsModel(view){
 if(!view||view.read_error)return {state:'unavailable',badge:'Unavailable',rows:[],note:view?.read_error||'Loading…'};
 if(!view.loops)return {state:'error',badge:'No loops running',rows:[],note:'No agent loop has reported to the database.'};
 const bad=view.members.filter(m=>m.state!=='serving');
 const rows=view.members.map(m=>{
  const age=m.heartbeat_age_seconds===null?'no heartbeat':'heartbeat '+m.heartbeat_age_seconds+' s ago';
  return ['Loop '+m.index,m.state+' · '+age+' · '+m.runs_in_progress+' run'+(m.runs_in_progress===1?'':'s')+' · '+m.accounts+' account'+(m.accounts===1?'':'s')];
 });
 return {state:bad.length?'error':'live',
  badge:view.loops+(view.loops===1?' loop':' loops')+(bad.length?' · '+bad.length+' not serving':''),
  rows,
  note:view.runs_in_progress+' run'+(view.runs_in_progress===1?'':'s')+' in progress across '+view.accounts_total+' accounts. The administrator account is always on loop 0.'};
}

/** Paint the Dashboard tile into its parts. */
export function paintLoops(view,{tile,badge,body,needs},node){
 const model=loopsModel(view);
 tile.dataset.state=model.state;badge.textContent=model.badge;
 body.replaceChildren();
 for(const [label,value] of model.rows){const row=node('div',undefined,'dashboard-field');row.append(node('span',String(label)),node('span',String(value)));body.append(row);}
 needs.textContent=model.note;
}

/** Paint the Builds page strip: a heading and one wrapping line per loop. */
export function paintLoopsStrip(view,strip){
 const model=loopsModel(view);
 strip.dataset.tone=model.state==='live'?'success':model.state==='error'?'danger':'';
 strip.replaceChildren();
 const head=document.createElement('strong');head.textContent='Agent loops · '+model.badge;strip.append(head);
 for(const [label,value] of model.rows){const row=document.createElement('div');row.textContent=label+': '+value;strip.append(row);}
}
