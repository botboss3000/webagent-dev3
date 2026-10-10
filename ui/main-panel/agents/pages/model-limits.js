'use strict';
// Agent-local budgets use the roster revision fence and are pinned by new runs.
export async function mountModelLimits(host,adapter,onSaved=()=>{}) {
  const section=document.createElement('section');section.className='ac-row';
  section.style.marginTop='16px';
  const title=document.createElement('h3');title.textContent='Model input and output limits';
  const hint=document.createElement('p');hint.textContent='Budgets apply to new chat, summary, naming and review calls. Select a model to change its budgets, or click a limit in its expanded model row.';
  const select=document.createElement('select');select.className='ac-input';select.setAttribute('aria-label','Model for token limits');
  const info=document.createElement('button');info.type='button';info.className='ac-btn ac-btn-ghost';info.textContent='ⓘ Provider limits and how budgets work';
  const dialog=document.createElement('dialog');dialog.setAttribute('aria-label','Provider limits and budget usage');
  dialog.style.cssText='margin:auto;position:fixed;inset:0;width:min(560px,90vw);max-width:90vw;max-height:85vh;overflow:auto;background:var(--bg,#161616);color:inherit;border:1px solid var(--border,#666);border-radius:12px;padding:20px';
  const dialogTitle=document.createElement('h3');dialogTitle.textContent='Provider limits and budget usage';
  const provider=document.createElement('p'),usage=document.createElement('p'),scope=document.createElement('p');
  for(const paragraph of [provider,usage,scope])paragraph.style.cssText='margin:12px 0;line-height:1.5';
  usage.textContent='Input budget controls estimated space for the complete model request, including instructions, history and tools. Automatic compaction starts at the earlier of the agent’s trigger or the effective System model input budget. Summary chunks must also fit that budget. Planning estimates four request bytes per token; actual provider token counts can differ.';
  scope.textContent='Maximum output tokens is sent as a cap on generated output. Input plus output must fit this connection’s context window. These agent-local settings are pinned for new runs and apply to chat, summaries, naming and review calls. Defaults reserve 8,192 tokens for output.';
  const close=document.createElement('button');close.type='button';close.className='ac-btn ac-btn-primary';close.textContent='Close';
  close.onclick=()=>dialog.close();dialog.append(dialogTitle,provider,usage,scope,close);section.append(dialog);
  info.onclick=()=>{if(!dialog.open)dialog.showModal();};
  dialog.addEventListener('click',event=>{if(event.target===dialog){const r=dialog.getBoundingClientRect();if(event.clientX<r.left||event.clientX>r.right||event.clientY<r.top||event.clientY>r.bottom)dialog.close();}});
  const input=document.createElement('input'),output=document.createElement('input');
  for(const field of [input,output]){field.type='number';field.required=true;field.min='1';field.step='1';field.className='ac-input';}
  input.setAttribute('aria-label','Input token budget');output.setAttribute('aria-label','Maximum output tokens');
  const inputLabel=document.createElement('label');inputLabel.textContent='Input token budget ';inputLabel.append(input);
  const outputLabel=document.createElement('label');outputLabel.textContent='Maximum output tokens ';outputLabel.append(output);
  const save=document.createElement('button');save.className='ac-btn ac-btn-primary';save.textContent='Save limits';
  const reset=document.createElement('button');reset.className='ac-btn ac-btn-ghost';reset.textContent='Use defaults';
  const status=document.createElement('p');status.setAttribute('role','status');
  section.append(title,hint,select,info,inputLabel,outputLabel,save,reset,status);host.append(section);
  let data,stopped=false,busy=false;
  function selected(){return data?.models.find(m=>m.id===select.value);}
  function show(){
    const m=selected();if(!m)return;
    provider.textContent=m.context_tokens?((m.transport==='openai_subscription'?'Subscription context: ':'API context: ')+m.context_tokens.toLocaleString()+' tokens · published API max output: '+m.max_output_tokens.toLocaleString()+' tokens. '+(m.transport==='openai_subscription'?'Public API context is '+m.published_context_tokens.toLocaleString()+' tokens. ':'')+'Input plus output must fit this connection’s context window.'):'Provider limits are not known. Enter limits supported by this model.';
    input.value=m.limits?.input_tokens??(m.context_tokens?m.context_tokens-m.default_output_tokens:'');
    output.value=m.limits?.output_tokens??m.default_output_tokens??'';
    input.max=m.context_tokens??10000000;output.max=m.max_output_tokens??1000000;
    for(const field of [select,input,output,save,reset])field.disabled=busy||!data.can_manage;
  }
  async function reload(){
    const previous=select.value;data=await adapter.loadModelLimits();if(stopped)return;
    select.replaceChildren(...data.models.map(m=>{const option=document.createElement('option');option.value=m.id;option.textContent=m.name+' ('+m.model+')';return option;}));
    if(data.models.some(m=>m.id===previous))select.value=previous;
    section.hidden=!data.models.length;show();
  }
  async function submit(defaults){
    const m=selected();if(!m||busy)return;
    if(!defaults&&(!input.reportValidity()||!output.reportValidity()))return;
    const limits=defaults?null:{input_tokens:Number(input.value),output_tokens:Number(output.value)};
    if(limits&&m.context_tokens&&limits.input_tokens+limits.output_tokens>m.context_tokens){status.textContent='Input plus output exceeds this connection’s context window.';return;}
    busy=true;for(const field of [select,input,output,save,reset])field.disabled=true;status.textContent='Saving…';
    try{await adapter.saveModelLimits(m.id,limits);status.textContent=defaults?'Default budgets restored for new runs.':'Limits saved for new runs.';await reload();await onSaved();}
    catch(error){status.textContent=error.message||'Could not save limits';}
    finally{busy=false;show();}
  }
  select.onchange=()=>{status.textContent='';show();};
  save.onclick=()=>submit(false);reset.onclick=()=>submit(true);
  await reload();return {reload,budget:id=>{const m=data?.models.find(m=>m.id===id);return m?{input:m.limits?.input_tokens??(m.context_tokens?m.context_tokens-m.default_output_tokens:null),output:m.limits?.output_tokens??m.default_output_tokens}:null;},edit:(id,field='input')=>{if(!data?.models.some(m=>m.id===id))return;select.value=id;show();section.scrollIntoView({block:'nearest'});(field==='output'?output:input).focus();},destroy:()=>{stopped=true;host.replaceChildren();}};
}
