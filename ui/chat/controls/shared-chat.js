import {setSharedChatControls} from './chat-config.js';
/** Session controls share one state and never infer permission from button placement. */
export function mountSharedChat({root,client,guard,note,ensureSession,onState=()=>{}}){
  let session='',generation=0,state=null,busy=false,loading=null,destroyed=false;
  const nodes={};
  for(const [name,label] of [['share','Share'],['request_help','Request help'],['agent_active','Agent active']]){
    const node=document.createElement('button');node.type='button';node.className='ac-btn chat-shared-control';node.dataset.headerControl=name;node.textContent=label;node.disabled=true;nodes[name]=node;
  }
  const panel=document.createElement('section');panel.className='v3-shared-chat-panel';panel.hidden=true;panel.setAttribute('role','dialog');panel.setAttribute('aria-label','Share conversation');root.append(panel);
  const title=document.createElement('strong');title.textContent='Share conversation';
  const expiry=document.createElement('select');expiry.setAttribute('aria-label','Invitation expiration');
  for(const [seconds,label] of [[3600,'1 hour'],[86400,'24 hours'],[604800,'7 days']]){const option=document.createElement('option');option.value=seconds;option.textContent=label;expiry.append(option);}expiry.value='86400';
  const issue=document.createElement('button');issue.type='button';issue.className='ac-btn';issue.textContent='Create invitation';
  const result=document.createElement('input');result.readOnly=true;result.hidden=true;result.setAttribute('aria-label','Single-use invitation link');
  const copy=document.createElement('button');copy.type='button';copy.className='ac-btn';copy.textContent='Copy link';copy.hidden=true;copy.onclick=guard(async()=>{await navigator.clipboard.writeText(result.value);note('Invitation link copied');});
  const list=document.createElement('div');
  const close=document.createElement('button');close.type='button';close.className='ac-btn';close.textContent='Close';close.onclick=()=>{panel.hidden=true;nodes.share.setAttribute('aria-expanded','false');};
  panel.append(title,expiry,issue,result,copy,list,close);
  const status=document.createElement('p');status.className='ac-hint v3-shared-chat-status';status.setAttribute('role','status');root.querySelector('#chat-input-area').prepend(status);
  nodes.share.setAttribute('aria-haspopup','dialog');nodes.share.setAttribute('aria-expanded','false');
  function paint(){
    const access=state?.access||{},policy=state?.policy||{};
    nodes.share.disabled=busy||!state||!access.can_invite||!policy.shareable;
    nodes.request_help.disabled=busy||!state||!access.can_send||!policy.help;
    const pending=state?.help?.status==='pending';nodes.request_help.textContent=pending?'Cancel help request':'Request help';
    nodes.agent_active.disabled=busy||!state||!access.can_control;nodes.agent_active.textContent=state?.participation?.active===false?'Agent inactive':'Agent active';nodes.agent_active.setAttribute('aria-pressed',String(state?.participation?.active!==false));
    status.textContent=state ? [access.observer?'Administrative observation · read only':'',policy.admin_view?'Administrative review enabled':'',pending?'Waiting for user to join':'',state.participation?.active===false?'Agent inactive · human messages are not sent to the agent':''].filter(Boolean).join(' · ') : '';
    issue.disabled=nodes.share.disabled;
    list.replaceChildren();for(const invite of state?.invitations||[]){const row=document.createElement('p');const expired=invite.expires_at*1000<=Date.now();row.textContent=(invite.revoked?'Revoked':invite.redeemed?'Redeemed':expired?'Expired':'Available')+' invitation · expires '+new Date(invite.expires_at*1000).toLocaleString();const revoke=document.createElement('button');revoke.type='button';revoke.className='ac-btn';revoke.textContent='Revoke';revoke.disabled=nodes.share.disabled||invite.revoked||invite.redeemed||expired;revoke.onclick=guard(()=>mutate(()=>client.revokeInvite(session,invite.id,{client_request_id:crypto.randomUUID()})));row.append(revoke);list.append(row);}
  }
  async function load(){if(!session||loading?.generation===generation||destroyed)return;const id=session,epoch=generation,request={generation};loading=request;try{const data=await client.sharing(id);if(session!==id||generation!==epoch||destroyed)return;state=data;onState(data);paint();}finally{if(loading===request)loading=null;}}
  async function ready(){if(!session)await ensureSession();await load();if(!state)throw Error('Shared conversation controls are unavailable');}
  async function mutate(action){if(busy)return;const id=session,epoch=generation;busy=true;paint();try{await action();if(session===id&&generation===epoch)await load();}finally{busy=false;paint();}}
  nodes.share.onclick=guard(async()=>{await ready();if(nodes.share.disabled)return;panel.hidden=!panel.hidden;nodes.share.setAttribute('aria-expanded',String(!panel.hidden));});
  issue.onclick=guard(async()=>{await ready();const id=session,epoch=generation;await mutate(async()=>{const data=await client.invite(id,{client_request_id:crypto.randomUUID(),expires_in:Number(expiry.value)});if(id!==session||epoch!==generation)return;const url=new URL('/app',location.origin);url.searchParams.set('chat_invite',data.token);result.value=url.href;result.hidden=false;copy.hidden=false;});});
  nodes.agent_active.onclick=guard(async()=>{await ready();if(nodes.agent_active.disabled)return;await mutate(()=>client.participation(session,{client_request_id:crypto.randomUUID(),expected_revision:state.participation.revision,active:!state.participation.active}));});
  nodes.request_help.onclick=guard(async()=>{await ready();if(nodes.request_help.disabled)return;await mutate(()=>state.help?.status==='pending'?client.cancelHelp(session,{client_request_id:crypto.randomUUID()}):client.help(session,{client_request_id:crypto.randomUUID()}));});
  setSharedChatControls(nodes);paint();
  const timer=setInterval(()=>{if(session&&document.visibilityState!=='hidden')load().catch(()=>{});},3000);
  function disconnect(){++generation;session='';state=null;panel.hidden=true;result.value='';result.hidden=true;copy.hidden=true;onState(null);paint();}
  return {async open(id){disconnect();session=id;if(id)await load();},refresh:load,disconnect,destroy(){destroyed=true;clearInterval(timer);disconnect();setSharedChatControls({});panel.remove();status.remove();},get state(){return state;}};
}
