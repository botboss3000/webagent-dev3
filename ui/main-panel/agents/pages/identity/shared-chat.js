// Shared-chat policy uses Identity's serialized, revision-fenced persistence.
export function renderSharedChat(host,agent,persist,request,{agentId}) {
  const section=document.createElement('details'),summary=document.createElement('summary');summary.textContent='Shared conversations';section.append(summary);host.append(section);
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Policies apply immediately to all chats. Joined users see the whole transcript. The initial agent state applies only to new chats. Viewing is silent and read-only; joining is announced.';section.append(hint);
  const form=document.createElement('form');section.append(form);
  const fields={};
  for(const [key,label,defaultValue] of [['shareable','Participants may invite others',false],['admin_view','Allow administrative viewing',false],['admin_join','Allow administrators to join',false],['help','Enable Request help',false],['initial_active','Start new chats with the agent active',true]]){
    const wrap=document.createElement('label');wrap.className='ac-label';wrap.style.display='block';
    const input=document.createElement('input');input.type='checkbox';input.checked=agent.shared_chat?.[key]??defaultValue;input.disabled=!agent.can_manage;input.dataset.sharedChatField=key;fields[key]=input;
    wrap.append(input,document.createTextNode(' '+label));form.append(wrap);
  }
  const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');
  const save=document.createElement('button');save.type='submit';save.className='ac-btn';save.textContent='Save shared-chat policy';save.disabled=!agent.can_manage;form.append(save,status);
  form.onsubmit=async event=>{event.preventDefault();if(!agent.can_manage)return;save.disabled=true;status.textContent='Saving…';try{const saved=await persist({shared_chat:Object.fromEntries(Object.entries(fields).map(([key,input])=>[key,input.checked]))});agent=saved;for(const [key,input] of Object.entries(fields))input.checked=saved.shared_chat[key];status.textContent='Saved'}catch(error){status.textContent='Save failed: '+error.message}finally{save.disabled=!agent.can_manage}};
  if(!agent.can_manage)return;
  const refresh=document.createElement('button');refresh.type='button';refresh.className='ac-btn';refresh.textContent='Review shared sessions';
  const list=document.createElement('div');section.append(refresh,list);
  let generation=0;
  refresh.onclick=async()=>{const epoch=++generation;refresh.disabled=true;try{const data=await request('/agents/'+encodeURIComponent(agentId)+'/shared-sessions');if(epoch!==generation||!host.isConnected)return;list.replaceChildren();for(const item of data.sessions||[]){const row=document.createElement('p'),title=document.createElement('span');title.textContent=item.title||item.id;row.append(title);if(item.can_observe){const link=document.createElement('a');link.href='/app?session_id='+encodeURIComponent(item.id);link.textContent=' Observe';row.append(link);}if(item.can_join){const join=document.createElement('button');join.type='button';join.className='ac-btn';join.textContent='Join';join.onclick=async()=>{join.disabled=true;try{await request('/sessions/'+encodeURIComponent(item.id)+'/join','POST',{client_request_id:crypto.randomUUID()});location.href='/app?session_id='+encodeURIComponent(item.id)}catch(error){status.textContent='Join failed: '+error.message;join.disabled=false}};row.append(join);}list.append(row);}if(!data.sessions?.length)list.textContent='No sessions available for administrative viewing or joining.';}catch(error){status.textContent='Session review unavailable: '+error.message}finally{refresh.disabled=false}};
}
