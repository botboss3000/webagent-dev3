// Only authenticated snapshot metadata can request one registered navigation; tool text cannot.
export function appViewRequest(snapshot, identity, dispatch, now = Date.now()) {
  const request=snapshot?.request;
  if(!request||request.actor_id!==identity?.user_id)return false;
  // Navigation requests expire; a credential form stays until saved, dismissed or replaced.
  if(request.destination!=='credential'&&(!Number.isFinite(request.expires_at)||request.expires_at*1000<now))return false;
  if(!['chat','app:files','agents:config','agents:model','agents:abilities','app:settings','automation','builds','registration','credential','handoff'].includes(request.destination))return false;
  if(typeof request.agent_id!=='string'||!/^[A-Za-z0-9_-]{1,128}$/.test(request.agent_id))return false;
  if(request.destination==='handoff'&&(typeof request.target_session_id!=='string'||!/^[-a-f0-9]{36}$/i.test(request.target_session_id)))return false;
  if(request.destination==='credential'){
    if(typeof request.credential_name!=='string'||!/^[-A-Za-z0-9_]{1,64}$/.test(request.credential_name)||typeof request.credential_origin!=='string'||!/^https:\/\//.test(request.credential_origin)||typeof request.request_id!=='string'||!/^[a-f0-9]{64}$/.test(request.request_id)||!Number.isSafeInteger(request.expected_revision)||request.expected_revision<0)return false;
  }
  // Optional display fields: an invalid label or non-HTTPS link is dropped, never rendered.
  const label=typeof request.credential_label==='string'&&request.credential_label.length<=80?request.credential_label:'';
  let createUrl='';try{const url=new URL(request.credential_url);if(url.protocol==='https:'&&!url.username&&!url.password)createUrl=url.href;}catch{}
  dispatch({destination:request.destination,target_session_id:request.target_session_id,agent_id:request.agent_id,name:request.credential_name,origin:request.credential_origin,requestId:request.request_id,revision:request.expected_revision,label,createUrl});
  return true;
}

export function appViewUrl(destination,agent,current,target) {
  const route=new URL(current);
  if(destination==='handoff'){if(typeof target!=='string'||!/^[-a-f0-9]{36}$/i.test(target))return null;route.search='';route.searchParams.set('session_id',target);return route.href;}
  const views={'app:files':['app','appView','files'],'app:settings':['app','appView','settings'],
    'agents:config':['agents','agentView','config'],'agents:model':['agents','agentView','model'],
    'agents:abilities':['agents','agentView','abilities'],'automation':['automation'],'builds':['builds']};
  const view=views[destination];if(!view)return null;
  route.searchParams.set('tab',view[0]);
  if(view[1])route.searchParams.set(view[1],view[2]);
  if(view[0]==='agents')route.searchParams.set('agent',agent);
  return route.href;
}

// Polls and reconnects must not reopen a dismissed form or erase a credential being typed.
// A pending credential form is session state rather than a one-shot navigation, so it bypasses the revision dispatcher.
export function credentialRequest(snapshot,identity,now=Date.now()){
  let detail=null;
  if(snapshot?.request?.destination==='credential')appViewRequest(snapshot,identity,value=>{detail=value;},now);
  return detail;
}

export function createAppViewDispatcher(){
  const revisions=new Map();
  return (snapshot,identity,dispatch,now=Date.now())=>{
    const revision=snapshot?.revision,source=snapshot?.request?.session_id;
    if(!Number.isSafeInteger(revision)||revision<1||typeof source!=='string')return false;
    const key=identity?.user_id+'/'+source;
    if(revision<=(revisions.get(key)||0))return false;
    if(!appViewRequest(snapshot,identity,dispatch,now))return false;
    revisions.set(key,revision);
    if(revisions.size>64)revisions.delete(revisions.keys().next().value);
    return true;
  };
}
