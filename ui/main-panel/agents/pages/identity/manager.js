'use strict';
// One agent-owned admission gate. The shared Identity queue fences every save.
export function renderManager(content, agent, persist) {
  const settings=agent.manager_incoming_message;
  if(!settings)return;
  const section=document.createElement('section');section.className='agent-closing-message-settings';
  const card=document.createElement('details');card.className='ac-row';
  const row=document.createElement('summary');row.className='ac-ability-row';
  const label=document.createElement('div');label.className='ac-ability-label';
  const title=document.createElement('div');title.className='ac-ability-name';title.textContent='Review incoming messages';
  const description=document.createElement('div');description.className='ac-ability-desc';description.textContent='The session manager checks whether each message fits this agent’s purpose before the agent starts.';
  label.append(title,description);
  const toggle=document.createElement('input');toggle.type='checkbox';toggle.setAttribute('aria-label','Review incoming messages');toggle.checked=settings.enabled;toggle.disabled=!agent.can_manage;
  row.append(label,toggle);card.append(row);
  const body=document.createElement('div');body.className='ac-ability-body';
  const criteriaLabel=document.createElement('label');criteriaLabel.className='agent-closing-message-guidance';criteriaLabel.textContent='Acceptance criteria';
  const criteria=document.createElement('textarea');criteria.className='ac-input';criteria.rows=5;criteria.maxLength=8192;criteria.value=settings.criteria||'';criteria.disabled=!agent.can_manage;criteria.placeholder='Accept questions about clothing, sizes, availability and orders. Reject unrelated requests.';
  criteriaLabel.append(criteria);
  const note=document.createElement('p');note.className='ac-hint';note.textContent='Uses this agent’s assigned System model. Rejected messages remain visible but do not reach the agent. Review errors do not allow a message through.';
  const save=document.createElement('button');save.type='button';save.className='ac-btn ac-btn-ghost';save.textContent='Save criteria';save.disabled=!agent.can_manage;
  const status=document.createElement('span');status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  body.append(criteriaLabel,note,save,status);card.append(body);section.append(card);content.append(section);
  let saved={...settings},busy=false;
  async function commit() {
    if(busy)return;
    if(toggle.checked&&!criteria.value.trim()){card.open=true;status.textContent='Enter acceptance criteria before enabling review.';toggle.checked=saved.enabled;criteria.focus();return;}
    busy=true;toggle.disabled=true;criteria.disabled=true;save.disabled=true;status.textContent='Saving…';
    try {
      const result=await persist({manager_incoming_message:{enabled:toggle.checked,criteria:criteria.value}});
      saved={...result.manager_incoming_message};toggle.checked=saved.enabled;criteria.value=saved.criteria;status.textContent='Saved';
    } catch(error){toggle.checked=saved.enabled;status.textContent='Save failed: '+error.message;}
    finally{busy=false;toggle.disabled=!agent.can_manage;criteria.disabled=!agent.can_manage;save.disabled=!agent.can_manage;}
  }
  toggle.addEventListener('click',event=>event.stopPropagation());
  toggle.addEventListener('change',()=>{card.open=true;void commit();});
  save.addEventListener('click',()=>void commit());
}
