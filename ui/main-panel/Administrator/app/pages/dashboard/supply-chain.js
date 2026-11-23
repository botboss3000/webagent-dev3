// Dependency scan card: the administrator-only /api/v3/admin/supply-chain view of the host
// scanner's newest report. Report-only: "Prepare update" starts a chat on the default agent
// (it has Codebase Admin) with a ready brief, and that agent opens an ordinary broker task that
// goes through the normal build, tests and release. Nothing on this page updates anything.
const SEVERITIES=['CRITICAL','HIGH','MEDIUM','LOW','UNKNOWN'];
const STATES=new Set(['ok','failed','stale','unavailable']);
const numeric=value=>typeof value==='number'&&Number.isFinite(value)&&value>=0;
const text=value=>typeof value==='string'&&value.length<=200;
const when=seconds=>numeric(seconds)?new Date(seconds*1000).toLocaleString():'Unknown';

/** Read and check the card's view; failures come back as {read_error}, never as zero counts. */
export async function readSupplyChain(signal){
 try{
  const response=await fetch('/api/v3/admin/supply-chain',{credentials:'same-origin',cache:'no-store',signal});
  if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===404?'Administrator access required':'Dependency scan unavailable ('+response.status+')');
  const view=await response.json();
  const finding=row=>row&&['package','installed','fixed','id'].every(key=>text(row[key]));
  const target=row=>row&&text(row.name)&&row.by_severity&&typeof row.by_severity==='object'&&row.fixable_by_severity&&typeof row.fixable_by_severity==='object'
   &&Array.isArray(row.fixable_high_critical)&&row.fixable_high_critical.length<=200&&row.fixable_high_critical.every(finding);
  if(!view||view.schema_version!==1||!STATES.has(view.state)||!Array.isArray(view.targets)||view.targets.length>16||!view.targets.every(target))throw Error('Invalid dependency scan response');
  return view;
 }catch(error){if(error.name==='AbortError')throw error;return {read_error:error.message};}
}

/** Unique fixable high/critical findings across targets, with the targets that carry each. */
export function fixable(view){
 const found=new Map();
 for(const target of view?.targets||[])for(const row of target.fixable_high_critical){
  const key=row.package+'\0'+row.installed+'\0'+row.id;
  if(!found.has(key))found.set(key,{...row,targets:[]});
  found.get(key).targets.push(target.name);
 }
 return [...found.values()];
}

/** Tile state, rows and note for the card. */
export function supplyChainModel(view){
 if(!view||view.read_error)return {state:'unavailable',badge:'Unavailable',rows:[],findings:[],note:view?.read_error||'Loading…'};
 const counts=object=>SEVERITIES.filter(key=>numeric(object[key])&&object[key]>0).map(key=>object[key]+' '+key.toLowerCase()).join(' · ')||'none';
 const rows=[['Last scan',when(view.scanned_at)],['Last scanner check',when(view.checked_at??view.scanned_at)],['Release scanned',text(view.release)&&view.release?view.release.slice(0,8):'Unknown']];
 for(const target of view.targets)rows.push([target.name+' findings',counts(target.by_severity)],[target.name+' fixable',counts(target.fixable_by_severity)]);
 const ui=view.ui_libraries;
 rows.push(['UI library versions',!ui?'Not checked':ui.missing?'Version record missing':ui.mismatch?.length?ui.mismatch.length+' mismatch'+(ui.mismatch.length===1?'':'es')+': '+ui.mismatch.map(m=>m.library).join(', '):'Match the record'+(ui.recorded?' ('+Object.entries(ui.recorded).map(([k,v])=>k+' '+v).join(', ')+')':'')]);
 const findings=fixable(view);
 const badge={ok:'Live',failed:'Scan failed',stale:'Scan is older than '+(view.stale_days||'?')+' days or the scanner stopped',unavailable:'Unavailable'}[view.state];
 const note=view.state==='failed'?(view.reason==='missing'?'No scan report found.':view.reason==='malformed'||view.reason==='too_large'||view.reason==='refused'?'The scan report was refused as unreadable.':'The last scan failed'+(text(view.error)&&view.error?': '+view.error:'.')+' Earlier results shown.')
  :view.state==='unavailable'?'The build broker could not be reached.':
  findings.length?findings.length+' high or critical finding'+(findings.length===1?' has':'s have')+' a fix available. Unfixable findings are counted only.':'No high or critical finding with a fix available.';
 return {state:view.state==='ok'&&!(ui&&(ui.missing||ui.mismatch?.length))?'live':view.state==='ok'?'partial':view.state==='unavailable'?'unavailable':'error',badge,rows,findings,note};
}

/** The brief handed to the in-app agent; it proposes version bumps, it applies nothing itself. */
export function prepareBrief(view){
 const lines=fixable(view).map(f=>'- '+f.package+' '+f.installed+' → '+f.fixed+' ('+f.id+'; in '+f.targets.join(', ')+')');
 return ['The dependency scan found high or critical findings that have a fix available. Prepare an update for them.',
  'Scan: '+when(view.scanned_at)+', release '+(view.release||'unknown')+'.',
  'Fixable findings (package, installed → fixed version, identifier, where):',...lines,'',
  'Please: open an ordinary broker task (build_work begin, broker_id new) and propose the smallest version bumps that',
  'clear these: Cargo.lock crate updates for cargo findings, the container base image or its Debian packages for',
  'api/loop/gate findings (first check the Blocker board and the Builds page for an open base-image task and work with',
  'it instead of duplicating it), and ui/vendor plus ui/vendor/package-lock.json for UI libraries. Run the normal',
  'checks and release it like any change, then report the commit and which findings remain. Do not change the scanner.'].join('\n');
}

/** Start the chat with the brief and open it. */
export async function prepareUpdate(view,{fetchFn=fetch,assign=url=>location.assign(url)}={}){
 const auth=await fetchFn('/api/v3/auth/me',{credentials:'same-origin',cache:'no-store'});
 if(!auth.ok)throw Error('Sign in required');
 const me=await auth.json();
 const post=async(path,body)=>{
  const response=await fetchFn('/api/v3'+path,{method:'POST',credentials:'same-origin',cache:'no-store',headers:{'Content-Type':'application/json','x-csrf-token':me.csrf_token},body:JSON.stringify(body)});
  const data=await response.json().catch(()=>({}));
  if(!response.ok)throw Error(data.error||'Request failed ('+response.status+')');
  return data;
 };
 const session=await post('/sessions',{agent_id:'shared_default',client_request_id:crypto.randomUUID()});
 const id=session.id||session.session_id||session.session?.id;
 if(typeof id!=='string'||!id)throw Error('Chat was not created');
 await post('/sessions/'+encodeURIComponent(id)+'/messages',{text:prepareBrief(view),attachment_ids:[],client_message_id:crypto.randomUUID()});
 assign('/app?session_id='+encodeURIComponent(id));
}

/** Paint the card into its tile parts; the button stays disabled with nothing to fix. */
export function paintSupplyChain(view,{tile,badge,body,needs},node){
 const model=supplyChainModel(view);
 tile.dataset.state=model.state;badge.textContent=model.badge;
 body.replaceChildren();
 for(const [label,value] of model.rows){const row=node('div',undefined,'dashboard-field');row.append(node('span',String(label)),node('span',String(value)));body.append(row);}
 if(model.findings.length){
  const list=node('details',undefined,'dashboard-findings');
  list.append(node('summary','Fixable high/critical findings ('+model.findings.length+')'));
  const items=node('ul');
  for(const f of model.findings.slice(0,50))items.append(node('li',f.package+' '+f.installed+' → '+f.fixed+' · '+f.id+' · '+f.targets.join(', ')));
  if(model.findings.length>50)items.append(node('li','and '+(model.findings.length-50)+' more'));
  list.append(items);body.append(list);
 }
 const actions=node('div',undefined,'dashboard-tile-actions');
 const button=node('button','Prepare update');button.type='button';
 button.disabled=!model.findings.length||view?.state==='unavailable';
 button.title='Opens a chat with the default agent and a brief proposing the version bumps; nothing is changed until that work is built, tested and released.';
 const status=node('span');status.setAttribute('role','status');
 button.onclick=async()=>{button.disabled=true;status.textContent='Starting a chat…';
  try{await prepareUpdate(view);}catch(error){status.textContent=error.message;button.disabled=false;}};
 actions.append(button,status);body.append(actions);
 needs.textContent=model.note;
}
