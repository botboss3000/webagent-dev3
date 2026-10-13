import { el, request } from '../users/support.js';
export async function mountPolicies(host){
  const status=el('p','members-loading','Loading policies…');status.setAttribute('role','status');host.replaceChildren(status);
  try{
    const [policy,tiers]=await Promise.all([request('/admin/access/registration'),request('/admin/access/tiers')]);host.replaceChildren(status);status.textContent='';
    const access=el('section','members-policy');access.append(el('div','members-policy-title','Access policy'));const choices=el('div','members-policy-choices');access.append(choices);host.append(access);
    for(const [value,label,hint] of [['admin_approval','Private','New users can register, but an administrator must approve access.'],['public_registered','Open Registration','Anyone may create an account and sign in.']]){
      const opt=el('label','members-policy-opt'+(policy.access_mode===value?' active':'')),input=el('input');input.type='radio';input.name='v3-registration-policy';input.value=value;input.checked=policy.access_mode===value;
      const body=el('div','members-policy-opt-body');body.append(el('div','members-policy-opt-label',label),el('div','members-policy-opt-hint',hint));opt.append(input,body);choices.append(opt);
      input.onchange=async()=>{for(const n of choices.querySelectorAll('input'))n.disabled=true;try{await request('/admin/access/registration','PUT',{access_mode:value,expected_revision:policy.revision});await mountPolicies(host);}catch(e){status.textContent=e.message;for(const n of choices.querySelectorAll('input')){n.disabled=false;n.checked=n.value===policy.access_mode;}}};
    }
    const section=el('section','members-policy');section.append(el('div','members-policy-title','Experience tiers'));host.append(section);
    for(const tier of tiers.tiers){const details=el('details','ac-row'),summary=el('summary','ac-ability-row',tier.id),form=el('form','ac-ability-body');details.append(summary,form);section.append(details);
      const fields={};for(const [key,label,value] of [['allowed_models','Allowed models',tier.allowed_models.join(', ')],['model_policy','Catalog / BYO policy (JSON; null retains legacy rules)',JSON.stringify(tier.model_policy??null)],['tools','Tool permissions (JSON)',JSON.stringify(tier.tools)],['max_sessions','Maximum sessions',tier.max_sessions],['max_active_runs','Maximum active runs',tier.max_active_runs]]){const lab=el('label','',label),input=el('input','ac-input');input.value=String(value);input.required=true;if(key.startsWith('max_')){input.type='number';input.min='0';input.max=key==='max_sessions'?'10000':'200';}input.disabled=tier.id==='operator';lab.append(input);form.append(lab);fields[key]=input;}
      if(tier.id!=='operator'){const save=el('button','members-btn','Save tier');save.type='submit';form.append(save);form.onsubmit=async ev=>{ev.preventDefault();save.disabled=true;try{const next={...tier,allowed_models:fields.allowed_models.value.split(',').map(s=>s.trim()).filter(Boolean),tools:JSON.parse(fields.tools.value),model_policy:JSON.parse(fields.model_policy.value),max_sessions:Number(fields.max_sessions.value),max_active_runs:Number(fields.max_active_runs.value)};await request('/admin/access/tiers/'+encodeURIComponent(tier.id),'PUT',{expected_revision:tier.revision,tier:next});await mountPolicies(host);}catch(e){status.textContent=e.message;save.disabled=false;}};}
    }
  }catch(e){status.textContent='Policies unavailable: '+e.message;}
}
