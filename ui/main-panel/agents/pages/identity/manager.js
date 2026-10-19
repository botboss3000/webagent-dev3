import {createSettingsRow} from './settings-row.js';
'use strict';
// Expansion and enablement are independent; prompt saves retain the saved enabled state.
export function renderManager(content, agent, persist) {
  const settings=agent.manager_incoming_message;
  if(!settings)return;
  const {body,toggle,setExpanded}=createSettingsRow(content,{id:'review-incoming-messages',title:'Review incoming messages',toggleLabel:'Enable incoming-message review'});
  toggle.disabled=!agent.can_manage;
  const criteriaLabel=document.createElement('label');criteriaLabel.className='agent-closing-message-guidance';criteriaLabel.textContent='Acceptance prompt';
  const criteria=document.createElement('textarea');criteria.className='ac-input';criteria.rows=5;criteria.maxLength=8192;criteria.value=settings.criteria||'';criteria.disabled=!agent.can_manage;criteria.placeholder='Accept questions about clothing, sizes, availability and orders. Reject unrelated requests.';
  criteriaLabel.append(criteria);
  const note=document.createElement('p');note.className='ac-hint';note.textContent='Save the prompt with review on or off. Uses this agent’s assigned System model when enabled.';
  const save=document.createElement('button');save.type='button';save.className='ac-btn ac-btn-ghost';save.textContent='Save prompt';save.disabled=!agent.can_manage;
  const status=document.createElement('span');status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  body.append(criteriaLabel,note,save,status);
  let saved={...settings},busy=false;
  const paint=()=>{toggle.checked=!!saved.enabled;};
  paint();
  async function commit(enabled) {
    if(busy)return;
    if(enabled&&!criteria.value.trim()){paint();setExpanded(true);status.textContent='Enter an acceptance prompt before enabling review.';criteria.focus();return;}
    busy=true;toggle.disabled=true;criteria.disabled=true;save.disabled=true;status.textContent='Saving…';
    try {
      const result=await persist({manager_incoming_message:{enabled,criteria:criteria.value}});
      saved={...result.manager_incoming_message};criteria.value=saved.criteria;paint();status.textContent='Saved';
    } catch(error){setExpanded(true);paint();status.textContent='Save failed: '+error.message;}
    finally{busy=false;toggle.disabled=!agent.can_manage;criteria.disabled=!agent.can_manage;save.disabled=!agent.can_manage;}
  }
  toggle.addEventListener('click',event=>{event.stopPropagation();void commit(!saved.enabled);});
  save.addEventListener('click',()=>void commit(saved.enabled));
}
