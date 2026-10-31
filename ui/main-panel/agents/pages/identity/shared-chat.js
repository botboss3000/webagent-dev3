import {createSettingsRow} from './settings-row.js';
// Shared-chat policy uses Identity's serialized, revision-fenced persistence.
export function renderSharedChat(host,agent,persist) {
  const {body:section}=createSettingsRow(host,{id:'shared-conversations',title:'Shared conversations'});
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Participant sharing applies immediately. Operator review is controlled by the privacy mode captured when each conversation was created. Joining is announced.';section.append(hint);
  const form=document.createElement('form');section.append(form);
  const fields={};
  for(const [key,label,defaultValue] of [['shareable','Participants may invite others',false],['help','Enable Request help',false],['initial_active','Start new chats with the agent active',true]]){
    const wrap=document.createElement('label');wrap.className='ac-label';wrap.style.display='block';
    const input=document.createElement('input');input.type='checkbox';input.checked=agent.shared_chat?.[key]??defaultValue;input.disabled=!agent.can_manage;input.dataset.sharedChatField=key;fields[key]=input;
    wrap.append(input,document.createTextNode(' '+label));form.append(wrap);
  }
  const status=document.createElement('p');status.className='ac-hint';status.setAttribute('role','status');
  const save=document.createElement('button');save.type='submit';save.className='ac-btn';save.textContent='Save shared-chat policy';save.disabled=!agent.can_manage;form.append(save,status);
  form.onsubmit=async event=>{event.preventDefault();if(!agent.can_manage)return;save.disabled=true;status.textContent='Saving…';try{const saved=await persist({shared_chat:Object.fromEntries(Object.entries(fields).map(([key,input])=>[key,input.checked]))});agent=saved;for(const [key,input] of Object.entries(fields))input.checked=saved.shared_chat[key];status.textContent='Saved'}catch(error){status.textContent='Save failed: '+error.message}finally{save.disabled=!agent.can_manage}};
}
