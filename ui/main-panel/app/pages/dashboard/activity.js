// Existing APIs retain caller/agent scope; no dashboard request broadens access.
const sources={
 agents:['/api/v3/models/agents',['agents_panel'],'agents'],
 sessions:['/api/v3/sessions/stats?limit=100',['sessions_monitor','tokens','tokens_by_agent','cost','tool_usage'],'sessions'],
 accounts:['/api/v3/admin/access/accounts',['user_mgmt'],'accounts'],
 signins:['/api/v3/auth/sessions',['security'],'sessions'],
 status:['/api/v3/status',['active_runs','failures','context'],'runs'],
 health:['/health/ready',['health_board'],null]
};
const numeric=value=>typeof value==='number'&&Number.isFinite(value)&&value>=0;
const text=value=>typeof value==='string'?value:'Unavailable';
const model=(rows,note,state='live',link=null)=>({rows,note,state,link});
/** Read bounded projections independently: one unavailable service cannot hide other tiles. */
export async function readActivity(signal){
 const results=await Promise.allSettled(Object.entries(sources).map(async([key,[path,,list]])=>{
  const response=await fetch(path,{credentials:'same-origin',cache:'no-store',signal});
  if(!response.ok)throw Error(response.status===401?'Sign in required':response.status===403?'Administrator access required':'Unavailable ('+response.status+')');
  const value=await response.json();
  if(!value||typeof value!=='object'||(list&&(!Array.isArray(value[list])||!value[list].every(row=>row&&typeof row==='object'&&!Array.isArray(row)))))throw Error('Invalid dashboard response');
  if(key==='health'&&typeof value.ready!=='boolean')throw Error('Invalid readiness response');
  return value;
 }));
 const tiles=new Map(),data={};
 Object.entries(sources).forEach(([key,[,ids]],index)=>{
  const result=results[index];
  if(result.status==='fulfilled')data[key]=result.value;
  else for(const id of ids)tiles.set(id,model([],result.reason?.message||'Unavailable','unavailable'));
 });
 if(data.agents){
  const agents=data.agents.agents;
  tiles.set('agents_panel',model([['Accessible agents',agents.length],...agents.slice(0,8).map(a=>[text(a.name),text(a.status)])],
   'Agents available to your account'+(agents.length>8?' · showing first 8':''),'live',['Open agents','/app?tab=agents']));
 }
 if(data.accounts){
  const accounts=data.accounts.accounts;
  tiles.set('user_mgmt',model([['Accounts',accounts.length],['Administrators',accounts.filter(a=>a.role==='administrator').length],['Users',accounts.filter(a=>a.role==='user').length],['Guests',accounts.filter(a=>a.role==='guest').length],['Active',accounts.filter(a=>a.status==='active').length]],
   'Installation account summary','live',['Manage users','/app?tab=app&appView=users']));
 }
 if(data.signins)tiles.set('security',model([['Your active sign-ins',data.signins.sessions.length]],
  'Current account only. Installation sign-in history and security events need backend work.','partial'));
 if(data.sessions){
  const sessions=data.sessions.sessions;
  const scope='Your active sessions · '+sessions.length+' records'+(data.sessions.next_offset!=null?' · first page only (100 maximum)':'')+'. Missing provider usage is excluded.';
  tiles.set('sessions_monitor',model([['Sessions in this page',sessions.length],['Messages in this page',sessions.every(s=>numeric(s.message_count))?sessions.reduce((n,s)=>n+s.message_count,0):'Unavailable']],
   scope,'live',['Open sessions','/app?tab=agents&agentScope=sessions&agentView=sessions']));
  const reported=sessions.filter(s=>s.usage&&numeric(s.usage.input_tokens)&&numeric(s.usage.output_tokens)&&numeric(s.usage.reported_calls)&&numeric(s.usage.unreported_calls));
  const sum=key=>reported.reduce((n,s)=>n+(numeric(s.usage[key])?s.usage[key]:0),0);
  const incomplete=reported.length!==sessions.length||sum('unreported_calls')>0||data.sessions.next_offset!=null;
  tiles.set('tokens',model([['Reported input tokens',reported.length?sum('input_tokens'):'Unavailable'],['Reported output tokens',reported.length?sum('output_tokens'):'Unavailable'],['Sessions without usage',sessions.length-reported.length],['Unreported calls',reported.length?sum('unreported_calls'):'Unavailable']],scope,incomplete?'partial':'live'));
  tiles.set('tool_usage',model([['Provider-reported calls',reported.length?sum('reported_calls'):'Unavailable'],['Calls without usage',reported.length?sum('unreported_calls'):'Unavailable']],scope+' Tool invocation statistics need backend work.','partial'));
  const byAgent=new Map();
  for(const s of reported){const key=text(s.agent_id);byAgent.set(key,(byAgent.get(key)||0)+s.usage.input_tokens+s.usage.output_tokens);}
  tiles.set('tokens_by_agent',model([...byAgent].slice(0,12),scope+(byAgent.size>12?' Showing first 12 agents.':''),incomplete?'partial':'live'));
  const known=reported.filter(s=>numeric(s.usage.known_cost_usd)&&numeric(s.usage.unknown_cost_calls)&&s.usage.reported_calls+s.usage.unreported_calls>s.usage.unknown_cost_calls);
  tiles.set('cost',model([['Known cost subtotal (USD)',known.length?'$'+known.reduce((n,s)=>n+s.usage.known_cost_usd,0).toFixed(4):'Unavailable'],['Calls with unknown cost',reported.length?sum('unknown_cost_calls'):'Unavailable']],scope+' Known subtotal only; no inferred subscription cost.','partial'));
 }
 if(data.status){
  const runs=[...new Map(data.status.runs.filter(r=>typeof r.id==='string').map(r=>[r.id,r])).values()];
  const active=runs.filter(r=>['running','queued','waiting_approval','waiting_gate','recovery','paused'].includes(r.status));
  tiles.set('active_runs',model([['Running / queued',active.filter(r=>['running','queued'].includes(r.status)).length],['Waiting / paused / recovery',active.filter(r=>!['running','queued'].includes(r.status)).length]],'Default agent only; retained run snapshot.','partial'));
  const failed=runs.filter(r=>r.status==='failed');
  tiles.set('failures',model([['Failed retained runs',failed.length],...failed.slice(0,5).map(r=>[r.id,text(r.error).slice(0,160)])],'Default agent only · retained recent runs; not installation history.','partial'));
  const run=runs.find(r=>numeric(r.context_controls?.trigger_tokens));
  tiles.set('context',model([['Default agent model',text(data.status.model)],['Compaction trigger (tokens)',run?.context_controls?.trigger_tokens??'Unavailable'],['Tail target (tokens)',run?.context_controls?.tail_tokens??'Unavailable']],
   'Thresholds from a retained default-agent run. Model input/output limits need backend work.','partial'));
 }
 if(data.health)tiles.set('health_board',model([['API readiness',data.health.ready?'Ready':'Not ready']],
  'API readiness only. Database latency and broader service checks need backend work.',data.health.ready?'partial':'unavailable'));
 return tiles;
}
