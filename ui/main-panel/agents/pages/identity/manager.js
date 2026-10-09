'use strict';
// Expansion and enablement are independent; prompt saves retain the saved enabled state.
export function renderManager(content, agent, persist) {
  const settings=agent.manager_incoming_message;
  if(!settings)return;
  const section=document.createElement('section');section.className='agent-message-visibility-settings agent-closing-message-settings';
  const list=document.createElement('div');list.className='ac-list agent-closing-message-list';section.append(list);
  const card=document.createElement('div');card.className='ac-row agent-message-visibility-route';
  const row=document.createElement('div');row.className='ac-ability-row agent-closing-message-row';
  row.tabIndex=0;row.setAttribute('role','button');row.setAttribute('aria-expanded','false');row.setAttribute('aria-label','Incoming-message review configuration');
  const label=document.createElement('div');label.className='ac-ability-label';
  const title=document.createElement('div');title.className='ac-ability-name';title.textContent='Review incoming messages';
  const description=document.createElement('div');description.className='ac-ability-desc';description.textContent='Expand to edit the manager’s acceptance prompt. Enable review separately.';
  label.append(title,description);
  const toggle=document.createElement('button');toggle.type='button';toggle.className='ac-btn agent-manager-switch';toggle.setAttribute('role','switch');toggle.setAttribute('aria-label','Enable incoming-message review');toggle.disabled=!agent.can_manage;
  row.append(label,toggle);card.append(row);
  const body=document.createElement('div');body.className='ac-ability-body';body.hidden=true;card.append(body);list.append(card);
  const expand=()=>{body.hidden=!body.hidden;card.classList.toggle('expanded',!body.hidden);row.setAttribute('aria-expanded',String(!body.hidden));};
  row.addEventListener('click',event=>{if(!event.target.closest('[role="switch"]'))expand();});
  row.addEventListener('keydown',event=>{if(event.target===row&&(event.key==='Enter'||event.key===' ')){event.preventDefault();expand();}});
  const criteriaLabel=document.createElement('label');criteriaLabel.className='agent-closing-message-guidance';criteriaLabel.textContent='Acceptance prompt';
  const criteria=document.createElement('textarea');criteria.className='ac-input';criteria.rows=5;criteria.maxLength=8192;criteria.value=settings.criteria||'';criteria.disabled=!agent.can_manage;criteria.placeholder='Accept questions about clothing, sizes, availability and orders. Reject unrelated requests.';
  criteriaLabel.append(criteria);
  const note=document.createElement('p');note.className='ac-hint';note.textContent='Save the prompt with review on or off. Uses this agent’s assigned System model when enabled.';
  const save=document.createElement('button');save.type='button';save.className='ac-btn ac-btn-ghost';save.textContent='Save prompt';save.disabled=!agent.can_manage;
  const status=document.createElement('span');status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  body.append(criteriaLabel,note,save,status);content.append(section);
  let saved={...settings},busy=false;
  const paint=()=>{toggle.setAttribute('aria-checked',String(saved.enabled));toggle.textContent=saved.enabled?'On':'Off';};
  paint();
  async function commit(enabled) {
    if(busy)return;
    if(enabled&&!criteria.value.trim()){body.hidden=false;card.classList.add('expanded');row.setAttribute('aria-expanded','true');status.textContent='Enter an acceptance prompt before enabling review.';criteria.focus();return;}
    busy=true;toggle.disabled=true;criteria.disabled=true;save.disabled=true;status.textContent='Saving…';
    try {
      const result=await persist({manager_incoming_message:{enabled,criteria:criteria.value}});
      saved={...result.manager_incoming_message};criteria.value=saved.criteria;paint();status.textContent='Saved';
    } catch(error){body.hidden=false;card.classList.add('expanded');row.setAttribute('aria-expanded','true');paint();status.textContent='Save failed: '+error.message;}
    finally{busy=false;toggle.disabled=!agent.can_manage;criteria.disabled=!agent.can_manage;save.disabled=!agent.can_manage;}
  }
  toggle.addEventListener('click',event=>{event.stopPropagation();void commit(!saved.enabled);});
  save.addEventListener('click',()=>void commit(saved.enabled));
}
