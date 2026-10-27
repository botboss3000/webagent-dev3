import {requestTouchVibration} from '../../../../shared/vibrate.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
const requestId=()=>globalThis.crypto?.randomUUID?.()||'create-'+Date.now()+'-'+Math.random().toString(16).slice(2);
/** One request ID per draft: a retried submit is idempotent, a changed draft gets a new ID. */
export function renderCreateAgent(host,request,{onCreated}) {
  host.replaceChildren(node('h2','Create agent'),node('p','You own the new agent. Choose its model, abilities and members next.','ac-hint'));
  const form=node('form','','ac-model-table'),name=node('input','','ac-input'),description=node('textarea','','ac-input'),submit=node('button','Create agent','ac-btn ac-btn-primary');
  const status=node('p','','ac-hint');status.setAttribute('role','status');
  name.id='create-agent-name';name.required=true;name.maxLength=200;name.autocomplete='off';name.placeholder='Research assistant';
  description.id='create-agent-description';description.maxLength=2000;description.rows=3;description.placeholder='What this agent is for (optional)';
  const field=(input,label,hint)=>{const row=node('div','','ac-ability-row ac-model-cfg-row ac-row-static'),text=node('label','','ac-ability-label');text.htmlFor=input.id;text.append(node('span',label,'ac-ability-name'),node('span',hint,'ac-ability-desc'));row.append(text,input);return row;};
  submit.type='submit';submit.style.webkitTapHighlightColor='transparent';submit.addEventListener('click',requestTouchVibration);
  form.append(field(name,'Name','Shown on the agent card and in chat.'),field(description,'Description','Helps members pick the right agent.'),submit);
  host.append(form,status);
  let id=requestId(),draft='';
  form.onsubmit=async event=>{
    event.preventDefault();
    const body={client_request_id:id,name:name.value.trim(),description:description.value.trim()||null};
    if(!body.name){status.textContent='Enter a name for the agent.';name.focus();return;}
    const current=JSON.stringify([body.name,body.description]);if(draft&&draft!==current)body.client_request_id=id=requestId();draft=current;
    submit.disabled=true;status.textContent='Creating…';
    try{
      const created=await request('/agents','POST',body);
      id=requestId();draft='';form.reset();status.textContent='Created '+created.name+'.';
      await onCreated(created);
    }catch(error){status.textContent='Agent not created: '+error.message;}
    finally{submit.disabled=false;}
  };
  name.focus({preventScroll:true});
}
