import {el,request,button} from '../users/support.js';
const title=tier=>tier.name||tier.id;
const select=(label,options,value)=>{const input=el('select','ac-input');input.setAttribute('aria-label',label);for(const [id,text]of options){const item=el('option','',text);item.value=id;input.append(item);}input.value=value;return input;};
/** App-wide account ceilings; agent Operators configure their independent member ladders. */
export async function mountPolicies(host){
  const status=el('p','members-loading','Loading policies…');status.setAttribute('role','status');host.replaceChildren(status);
  try{
    const [policy,data,catalog]=await Promise.all([request('/admin/access/registration'),request('/admin/access/tiers'),request('/abilities')]);
    host.replaceChildren(status);status.textContent='';
    const access=el('section','members-policy');access.append(el('div','members-policy-title','App registration'));host.append(access);
    for(const [value,label]of [['admin_approval','Private — administrator approval'],['public_registered','Open registration']]){
      const wrap=el('label','members-policy-opt'),input=el('input');input.type='radio';input.name='app-registration';input.value=value;input.checked=policy.access_mode===value;wrap.append(input,document.createTextNode(label));access.append(wrap);
      input.onchange=async()=>{input.disabled=true;try{await request('/admin/access/registration','PUT',{access_mode:value,expected_revision:policy.revision});await mountPolicies(host);}catch(error){status.textContent=error.message;input.disabled=false;}};
    }
    const levels=new Map(data.tiers.map(tier=>[tier.id,tier]));const order=data.order;
    const names=Object.fromEntries(data.tiers.map(tier=>[tier.id,title(tier)]));
    const saveOrder=async(next,nextNames=names)=>{try{await request('/admin/access/tiers/order','PUT',{expected_revision:data.revision,order:next,names:nextNames,confirm_expansion:true});await mountPolicies(host);}catch(error){status.textContent=error.message;}};
    host.append(el('h3','','App account levels'),el('p','ac-hint','Highest to lowest. Each level is capped by the levels above it. App administration does not grant access to someone else’s agent.'));
    const permissionOptions=[['deny','Deny'],['ask','Ask'],['auto','Auto']];
    const abilities=catalog.abilities||[];
    const tools=[...new Set(abilities.flatMap(ability=>(ability.tools||[]).map(tool=>tool.name)))].sort();
    for(const [index,id]of order.entries()){
      const tier=levels.get(id),next=structuredClone(tier);delete next.name;
      const section=el('details','ac-row');section.append(el('summary','ac-ability-row',title(tier)));const form=el('form','ac-ability-body');section.append(form);host.append(section);
      if(!['operator','basic','anonymous'].includes(id)){
        const name=el('input','ac-input');name.value=title(tier);name.maxLength=100;name.setAttribute('aria-label','Account level name');
        form.append(name,button('Rename',()=>saveOrder(order,{...names,[id]:name.value.trim()})));
        for(const [label,step]of [['Move up',-1],['Move down',1]]){
          const move=button(label,()=>{const reordered=[...order];[reordered[index],reordered[index+step]]=[reordered[index+step],reordered[index]];
            if(window.confirm('New account order: '+reordered.map(key=>names[key]).join(' → ')+'. Existing accounts will use the new ceilings. Confirm?'))void saveOrder(reordered);});
          move.disabled=index+step<=0||index+step>=order.length-1;form.append(move);
        }
      }
      form.append(el('p','ac-hint','These are maximum account permissions across all agents. Lower agent levels may restrict them further.'));
      for(const [mapKey,heading,entries]of [['ability_permissions','Abilities',[['*','All abilities (including future additions)'],...abilities.map(a=>[a.id,a.name])]],['tools','Tools',[['*','All tools (including future additions)'],...tools.map(name=>[name,name])]]]){
        const details=el('details');details.append(el('summary','',heading));form.append(details);next[mapKey]||={};
        for(const [key,label]of entries){const row=el('label','ac-label',label);const input=select(label,permissionOptions,next[mapKey][key]||next[mapKey]['*']||'deny');input.onchange=()=>{next[mapKey][key]=input.value;};row.append(input);details.append(row);}
      }
      for(const [key,label]of [['max_sessions','Maximum sessions'],['max_active_runs','Maximum active runs']]){const wrap=el('label','ac-label',label),input=el('input','ac-input');input.type='number';input.min='0';input.max=key==='max_sessions'?'10000':'200';input.required=true;input.value=String(next[key]);input.oninput=()=>{next[key]=Number(input.value);};wrap.append(input);form.append(wrap);}
      const models=el('label','ac-label','Allowed BYO model IDs (comma separated)'),modelInput=el('input','ac-input');modelInput.value=next.allowed_models.join(', ');modelInput.oninput=()=>{next.allowed_models=modelInput.value.split(',').map(v=>v.trim()).filter(Boolean);};models.append(modelInput);form.append(models);
      const modelPolicy=el('label','ac-label','Published model catalog / BYO policy (JSON)'),policyInput=el('textarea','ac-input');policyInput.rows=3;policyInput.value=JSON.stringify(next.model_policy??null);modelPolicy.append(policyInput);form.append(modelPolicy);
      const save=el('button','members-btn','Save account level');save.type='submit';form.append(save);form.onsubmit=async event=>{event.preventDefault();save.disabled=true;try{next.model_policy=JSON.parse(policyInput.value);await request('/admin/access/tiers/'+encodeURIComponent(id),'PUT',{expected_revision:tier.revision,tier:next});await mountPolicies(host);}catch(error){status.textContent=error.message;save.disabled=false;}};
    }
    const create=el('form','members-policy'),name=el('input','ac-input'),before=select('Insert account level above',order.filter(id=>id!=='operator').map(id=>[id,'Above '+names[id]]),'basic'),add=el('button','members-btn','Create account level');
    name.placeholder='New account level name, e.g. Pro';name.required=true;name.maxLength=100;name.setAttribute('aria-label','New account level name');add.type='submit';create.append(name,before,add);host.append(create);
    create.onsubmit=async event=>{event.preventDefault();add.disabled=true;try{const id=name.value.trim().toLowerCase().replace(/[^a-z0-9]+/g,'_').replace(/^_|_$/g,'');if(!id||levels.has(id)||['admin','base','anonymous'].includes(id))throw Error('Choose a distinct account level name');
      const next=structuredClone(levels.get(before.value));delete next.name;next.id=id;next.revision=1;
      await request('/admin/access/tiers/'+encodeURIComponent(id),'PUT',{expected_revision:0,tier:next});
      const fresh=await request('/admin/access/tiers'),nextOrder=fresh.order.filter(key=>key!==id);nextOrder.splice(nextOrder.indexOf(before.value),0,id);
      await request('/admin/access/tiers/order','PUT',{expected_revision:fresh.revision,order:nextOrder,names:{...Object.fromEntries(fresh.tiers.map(tier=>[tier.id,title(tier)])),[id]:name.value.trim()},confirm_expansion:true});await mountPolicies(host);
    }catch(error){status.textContent=error.message;add.disabled=false;}};
  }catch(error){status.textContent='Policies unavailable: '+error.message;}
}
