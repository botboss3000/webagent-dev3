import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigForm,createConfigList,createToggleRow} from '../../../../shared/config-rows/config-rows.js';
// Shared-chat policy uses Identity's serialized, revision-fenced persistence.
export function renderSharedChat(host,agent,persist) {
  const {body:section}=createSubpage(host,{id:'shared-conversations',title:'Shared conversations'});
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Participant sharing applies immediately. Operator review is controlled by the privacy mode captured when each conversation was created. Joining is announced.';section.append(hint);
  const list=createConfigList(section,{label:'Shared-chat policy'}),rows={};
  // The three switches save together with one explicit Save.
  const form=createConfigForm(section,{saveLabel:'Save shared-chat policy',onSave:async values=>{
    if(!agent.can_manage)throw Error('Only an agent manager can change this policy');
    const saved=await persist({shared_chat:values});agent=saved;
    for(const [key,row] of Object.entries(rows)){row.setValue(saved.shared_chat[key]);form.set(key,row.getValue(),true);}
  }});
  for(const [key,title,defaultValue] of [['shareable','Participants may invite others',false],['help','Enable Request help',false],['initial_active','Start new chats with the agent active',true]]){
    const row=createToggleRow(list,{title,value:agent.shared_chat?.[key]??defaultValue,disabled:!agent.can_manage,key,form});
    row.control.dataset.sharedChatField=key;rows[key]=row;
  }
  form.attach();
}
