import {requestTouchVibration} from '../../../../shared/vibrate/vibrate.js';
import {createConfigList,createTextRow,createLongTextRow} from '../../../../shared/config-rows/config-rows.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
const requestId=()=>globalThis.crypto?.randomUUID?.()||'create-'+Date.now()+'-'+Math.random().toString(16).slice(2);
/** One request ID per draft: a retried submit is idempotent, a changed draft gets a new ID. */
export function renderCreateAgent(host,request,{onCreated}) {
  host.replaceChildren(node('h2','Create agent'),node('p','You own the new agent. Choose its model, abilities and members next.','ac-hint'));
  const form=node('form','','ac-model-table'),submit=node('button','Create agent','ac-btn ac-btn-primary');
  // The shared rows only collect the draft here: the agent is created by the submit below.
  const list=createConfigList(form,{label:'New agent'});
  const name=createTextRow(list,{title:'Name',hint:'Shown on the agent card and in chat.',placeholder:'Research assistant',maxLength:200}).control;
  const description=createLongTextRow(list,{title:'Description',hint:'Helps members pick the right agent.',placeholder:'What this agent is for (optional)',maxLength:2000,minHeight:72});
  const status=node('p','','ac-hint');status.setAttribute('role','status');
  name.id='create-agent-name';name.required=true;name.autocomplete='off';
  submit.type='submit';submit.style.webkitTapHighlightColor='transparent';submit.addEventListener('click',requestTouchVibration);
  form.append(submit);
  host.append(form,status);
  let id=requestId(),draft='';
  form.onsubmit=async event=>{
    event.preventDefault();
    const body={client_request_id:id,name:name.value.trim(),description:description.getValue().trim()||null};
    if(!body.name){status.textContent='Enter a name for the agent.';name.focus();return;}
    const current=JSON.stringify([body.name,body.description]);if(draft&&draft!==current)body.client_request_id=id=requestId();draft=current;
    submit.disabled=true;status.textContent='Creating…';
    try{
      const created=await request('/models/agents','POST',body);
      id=requestId();draft='';form.reset();description.setValue('');status.textContent='Created '+created.name+'.';
      await onCreated(created);
    }catch(error){status.textContent='Agent not created: '+error.message;}
    finally{submit.disabled=false;}
  };
  name.focus({preventScroll:true});
}
