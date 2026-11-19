import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigForm,createConfigList,createLongTextRow,createToggleRow} from '../../../../shared/config-rows/config-rows.js';
// Expansion and enablement are independent; prompt saves retain the saved enabled state.
export function renderManager(content, agent, persist) {
  const settings=agent.manager_incoming_message;
  if(!settings)return;
  const {body,toggle,setExpanded}=createSubpage(content,{id:'review-incoming-messages',title:'Review incoming messages',toggleLabel:'Enable incoming-message review'});
  toggle.disabled=!agent.can_manage;
  const note=document.createElement('p');note.className='ac-hint';note.textContent='Reviews visitors’ messages; the agent’s owner and members skip review unless Also review staff is checked. Save the prompt with review on or off. Uses this agent’s assigned System model when enabled.';
  // The prompt and staff switch save together with Save prompt; the header switch saves them with the new state.
  const form=createConfigForm(body,{saveLabel:'Save prompt',onSave:()=>commit(saved.enabled)});
  const list=createConfigList(body,{label:'Incoming-message review settings'});
  const criteria=createLongTextRow(list,{title:'Acceptance prompt',value:settings.criteria||'',maxLength:8192,minHeight:100,disabled:!agent.can_manage,
    placeholder:'Accept questions about clothing, sizes, availability and orders. Reject unrelated requests.',key:'criteria',form});
  const staff=createToggleRow(list,{title:'Also review staff (owner and members)',value:!!settings.review_staff,disabled:!agent.can_manage,key:'review_staff',form});
  const status=document.createElement('span');status.setAttribute('role','status');status.setAttribute('aria-live','polite');
  body.append(note);form.attach();body.append(status);
  let saved={...settings},busy=false;
  const paint=()=>{toggle.checked=!!saved.enabled;};
  paint();
  const lock=off=>{toggle.disabled=off;criteria.setDisabled(off);staff.setDisabled(off);};
  async function commit(enabled) {
    if(busy)throw Error('A save is already running');
    if(enabled&&!criteria.getValue().trim()){paint();setExpanded(true);status.textContent='Enter an acceptance prompt before enabling review.';criteria.editor.focus();throw Error('Enter an acceptance prompt first');}
    busy=true;lock(true);status.textContent='Saving…';
    try {
      const result=await persist({manager_incoming_message:{enabled,criteria:criteria.getValue(),review_staff:staff.getValue()}});
      saved={...result.manager_incoming_message};criteria.setValue(saved.criteria);staff.setValue(!!saved.review_staff);paint();status.textContent='Saved';
      form.set('criteria',saved.criteria,true);form.set('review_staff',!!saved.review_staff,true);
    } catch(error){setExpanded(true);paint();status.textContent='Save failed: '+error.message;throw error;}
    finally{busy=false;lock(!agent.can_manage);}
  }
  toggle.addEventListener('click',event=>{event.stopPropagation();commit(!saved.enabled).catch(()=>{});});
}
