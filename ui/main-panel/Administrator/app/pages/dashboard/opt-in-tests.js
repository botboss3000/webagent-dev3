// Opt-in tests card: the administrator-only /api/v3/admin/opt-in-tests view of the broker's
// nightly run of the listed opt-in test groups (tests too load-sensitive for every release's
// suite, such as the two-process agent-loop tests). Report-only; the failure output is in the
// job on the Builds page.
const STATES=new Set(['passed','failed','skipped','stale','missing','unreadable','unavailable']);
const GROUP_STATES=new Set(['passed','failed','skipped','error']);
const numeric=value=>typeof value==='number'&&Number.isFinite(value)&&value>=0;
const name=value=>typeof value==='string'&&/^[A-Za-z0-9_:]{1,160}$/.test(value);
const when=seconds=>numeric(seconds)?new Date(seconds*1000).toLocaleString():'Never';

/** Read and check the card's view; failures come back as {read_error}, never as a pass. */
export async function readOptInTests(signal){
 try{
  const response=await fetch('/api/v3/admin/opt-in-tests',{credentials:'same-origin',cache:'no-store',signal});
  if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===404?'Administrator access required':'Opt-in tests unavailable ('+response.status+')');
  const view=await response.json();
  const group=row=>row&&name(row.group)&&GROUP_STATES.has(row.status)&&numeric(row.tests)&&Array.isArray(row.failed_tests)&&row.failed_tests.length<=50&&row.failed_tests.every(name)
   &&(row.job_id===null||(typeof row.job_id==='string'&&/^[0-9a-f-]{36}$/.test(row.job_id)));
  if(!view||view.schema_version!==1||!STATES.has(view.state)||!Array.isArray(view.groups)||view.groups.length>16||!view.groups.every(group))throw Error('Invalid opt-in tests response');
  return view;
 }catch(error){if(error.name==='AbortError')throw error;return {read_error:error.message};}
}

/** Tile state, badge, rows and note for the card. */
export function optInTestsModel(view){
 if(!view||view.read_error)return {state:'unavailable',badge:'Unavailable',rows:[],note:view?.read_error||'Loading…'};
 const badge={passed:'Passed',failed:'Failed',skipped:'Skipped (builds were running)',stale:'No run in the last 2 days',missing:'Not run yet',unreadable:'Report unreadable',unavailable:'Unavailable'}[view.state];
 const rows=[['Last nightly run',when(view.checked_at)]];
 for(const g of view.groups)rows.push([g.group,g.status+' · '+g.tests+' test'+(g.tests===1?'':'s')+(g.job_id?' · job '+g.job_id.slice(0,8):'')]);
 const failed=view.groups.flatMap(g=>g.failed_tests.map(t=>g.group+': '+t));
 const note=view.state==='failed'?(failed.length?'Failed: '+failed.slice(0,5).join(', ')+(failed.length>5?' and '+(failed.length-5)+' more':'')+'.':'A group failed before naming a test.')+' The failure output is in the job on the Builds page.'
  :view.state==='unavailable'?'The build broker could not be reached.'
  :view.state==='missing'?'The nightly timer has not written a result yet.'
  :view.state==='unreadable'?'The nightly summary was refused as unreadable.'
  :view.state==='stale'?'The nightly timer has not reported for over 2 days.'
  :view.state==='skipped'?'Release builds ran all night, so the run was skipped.':'All listed opt-in test groups passed.';
 return {state:view.state==='passed'?'live':view.state==='skipped'||view.state==='stale'||view.state==='missing'?'partial':view.state==='unavailable'?'unavailable':'error',badge,rows,note};
}

/** Paint the card into its tile parts. */
export function paintOptInTests(view,{tile,badge,body,needs},node){
 const model=optInTestsModel(view);
 tile.dataset.state=model.state;badge.textContent=model.badge;
 body.replaceChildren();
 for(const [label,value] of model.rows){const row=node('div',undefined,'dashboard-field');row.append(node('span',String(label)),node('span',String(value)));body.append(row);}
 needs.textContent=model.note;
}
