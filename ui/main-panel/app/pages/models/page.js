'use strict';
import { mountModelTable } from '../../../../shared/models/model-table.js';
import { initModelAnalytics } from '../../../../shared/models/model-analytics.js';
import { usageFetch, usageQuery } from '../../../../shared/models/v3-adapter.js';
import { showNotification } from '../../../../chat/core/notification-panel.js';
const MISSING='NEED BACKEND CONTRACT';
const unavailable=async()=>new Response(JSON.stringify({error:MISSING}),{status:501,headers:{'Content-Type':'application/json'}});

// Actual reference template + renderer. Draft staging never changes publication or agent roles.
export async function mountAppModels(host) {
  let providers={};
  const controller=new AbortController();let disposed=false,table,analytics,observer;
  let scopeGeneration=0,roster,staged=[],dirty=false,busy=false,ready=false;
  const signal=controller.signal;
  const live=()=>!disposed;
  async function request(path,method='GET',body) {
    const auth=await fetch('/api/v3/auth/me',{credentials:'same-origin',cache:'no-store',signal});
    if(!auth.ok)throw Error('Sign in required');const me=await auth.json();
    if(!me.is_admin)throw Error('Administrator access required');
    const response=await fetch('/api/v3'+path,{method,credentials:'same-origin',cache:'no-store',signal,
      headers:{'Content-Type':'application/json',...(method==='GET'?{}:{'x-csrf-token':me.csrf_token})},
      ...(body===undefined?{}:{body:JSON.stringify(body)})});
    const data=await response.json().catch(()=>({}));
    if(!response.ok)throw Error(data.error||data.message||'Request failed ('+response.status+')');
    if(!live())throw Error('Catalog page closed');return data;
  }
  const templateResponse=await fetch(new URL('./page.html',import.meta.url),{cache:'no-store',signal});
  if(!templateResponse.ok)throw Error('Model Catalog template unavailable');
  const template=document.createElement('template');template.innerHTML=await templateResponse.text();
  if(!live())return()=>{};
  host.replaceChildren(template.content.querySelector('template').content.cloneNode(true));
  host.classList.add('v3-reference-models');host.querySelector('#settings-page-models').hidden=false;
  for(const href of ['/v3-ui/shared/models/reference.css','/v3-ui/shared/models/pending.css','/v3-ui/main-panel/app/pages/models/models.css']) {
    const link=document.createElement('link');link.rel='stylesheet';link.href=href;host.prepend(link);
  }
  const q=selector=>host.querySelector(selector);
  const scope=q('#ac-model-scope'),status=q('#ac-model-scope-status'),result=q('#ac-model-lifecycle-result');
  const save=q('#ac-model-save-draft'),publish=q('#ac-model-publish');
  const create=document.createElement('button');create.type='button';create.className='ac-btn';create.textContent='New catalog';
  q('.model-scope-toolbar').append(create);
  q('#ac-model-scope-hint').textContent='Base is administrator-only. Publish exposes metadata only; tier bindings and agent adoption are separate explicit actions.';
  create.onclick=()=>action(async()=>{
    if(dirty&&!confirm('Discard unsaved draft changes?'))return;
    const name=prompt('Name for the new app catalog');if(!name?.trim())return;
    const id='catalog_'+crypto.randomUUID().replaceAll('-','');
    await request('/admin/models/rosters/'+id,'PUT',{expected_revision:0,name:name.trim(),entries:{}});
    const option=document.createElement('option');option.value=id;option.textContent=name.trim();scope.append(option);scope.value=id;await load();
  });
  const path=()=>'/admin/models/rosters/'+encodeURIComponent(scope.value);
  function update() {
    if(!live())return;
    status.textContent=(roster?.retired?'Retired — ':'')+'Draft '+(roster?.revision||0)+(dirty?' · unsaved changes':'')+' · Published '+(roster?.published_revision??'none');
    save.disabled=!ready||busy||!!roster?.retired;publish.disabled=busy||dirty||!roster?.revision||!!roster?.retired;
    scope.disabled=busy;create.disabled=!ready||busy;
    for(const button of host.querySelectorAll('[data-model-lifecycle]'))button.disabled=busy||!roster?.revision||(button.dataset.modelLifecycle==='retire'&&(scope.value==='base'||roster?.retired));
  }
  function display(data) {result.hidden=false;result.replaceChildren();const pre=document.createElement('pre');pre.textContent=JSON.stringify(data,null,2);result.append(pre);}
  function error(e) {if(live()&&e.name!=='AbortError'){ready=false;update();status.textContent=e.message;display({error:e.message});}}
  function rows(entries) {return Object.values(entries).map(e=>({entry_id:e.id,provider:e.transport,model:e.upstream_model,display_name:e.display_name,
    api_key:'',decision_capable:e.capabilities.includes('decision'),text_capable:e.capabilities.includes('text'),image_capable:e.capabilities.includes('image_in'),image_out_capable:e.capabilities.includes('image_out'),voice_capable:e.capabilities.includes('voice'),video_capable:e.capabilities.includes('video'),catalog_origin:'platform'}));}
  const adapter={
    loadConfig:async()=>({provider:'deepseek',roster:structuredClone(staged),role_assignments:{}}),
    // Provider files on the server name the platform providers; the subscription is per-account only.
    loadPresets:async()=>{
      const {providers:files}=await request('/models/providers');
      providers=Object.fromEntries(files.filter(f=>f.provider!=='openai_subscription').map(f=>[f.provider,{name:f.name,base_url:f.base_url,validated:f.validated}]));
      return providers;
    },
    saveRoster:async({providers:next})=>{
      if(busy||roster?.retired||!live())throw Error('Catalog is not editable');
      for(const row of next){
        if(!providers[row.provider])throw Error('Unsupported platform provider');
        if(!row.entry_id&&providers[row.provider].validated===false)throw Error(providers[row.provider].name+' is not yet validated');
        if(typeof row.model!=='string'||row.model.length<1||row.model.length>200||!/^[!-~]+$/.test(row.model))throw Error('Invalid upstream model ID');
        if(row.alias_group_id||row.reference_cost_input!=null||row.reference_cost_output!=null)throw Error(MISSING+': alias groups and pricing');
        row.entry_id||='model_'+crypto.randomUUID().replaceAll('-','');
      }
      staged=structuredClone(next);dirty=true;update();return{providers:next};
    },
    fetchModels:async({provider,api_key})=>request('/models/discovery','POST',{provider,api_key}),
    fetchModelInfo:async()=>({}),fetchModelUsage:async args=>request('/admin/usage/models/totals?'+usageQuery(args)).catch(()=>null),
    loadDirectives:async()=>({custom:{}}),
    saveDirectives:async()=>{throw Error(MISSING+': role directives');},
    saveDefaultEntryId:async()=>{throw Error('Defaults belong to user accounts, not app publication');},
    subscriptionRequest:unavailable,
    listCredentials:async()=>{throw Error(MISSING+': named credential library');}
  };
  function markPending() {
    for(const element of host.querySelectorAll('.ac-model-meta-defaultline,.ac-model-meta-aliases,.ac-saved-credentials .ac-ability-name')) {
      if(element.querySelector('.v3-contract-pending'))continue;const label=document.createElement('span');label.className='v3-contract-pending';label.textContent=MISSING;element.append(label);
    }
    for(const control of host.querySelectorAll('.ac-model-meta-defaultline input,.ac-model-alias-add'))control.disabled=true;
  }
  const extras=q('#ac-model-adv-extras');extras.classList.remove('ac-kv-hidden');
  for(const control of extras.querySelectorAll('input,select,button')) {
    control.disabled=true;const label=document.createElement('span');label.className='v3-contract-pending';label.textContent=MISSING;control.after(label);
  }
  analytics=initModelAnalytics(q('#ac-model-analytics'),{fetchFn:usageFetch('/admin/usage/models'),apiPath:p=>p,scope:'global',description:'Application model usage across all users.',
    onHistoryCleared:model=>{table?.reload();showNotification({key:'usage-reset',title:'Usage history cleared for '+model,message:'Earlier usage is hidden from the all-users graph and totals.',actions:[{label:'Undo',kind:'approve',run:async()=>{
      await request('/admin/usage/models/undo','POST');await Promise.all([table?.reload(),analytics.reload()]);
    }}]});}});
  async function load() {
    const generation=++scopeGeneration;table?.destroy();table=null;dirty=false;
    status.textContent='Loading…';result.hidden=true;
    const {rosters}=await request('/admin/models/rosters');ready=true;
    if(!live()||generation!==scopeGeneration)return;
    const selected=scope.value||'base';scope.replaceChildren();
    const all=[{id:'base',name:'Base — Admin only'},...rosters.filter(r=>r.id!=='base')];
    for(const r of all){const option=document.createElement('option');option.value=r.id;option.textContent=r.name;scope.append(option);}
    scope.value=selected;if(!scope.value)scope.value='base';
    roster=rosters.find(r=>r.id===scope.value)||{id:'base',name:'Base — Admin only',revision:0,draft:{},published_revision:null,retired:false};
    staged=rows(roster.draft);update();
    table=mountModelTable(q('#ac-model-host'),{adapter,catalogOnly:true,manualModelEntry:true,usageScope:'global',fetchFn:unavailable,
      onRoster:r=>analytics?.setModelLabels(r),onModelFocus:m=>analytics?.setModelFocus(m),advancedExtra:body=>body.append(extras)});
    markPending();
  }
  async function action(work) {if(busy)return;busy=true;update();try{await work();}catch(e){error(e);}finally{busy=false;update();}}
  save.onclick=()=>action(async()=>{
    const entries=Object.fromEntries(staged.map(row=>[row.entry_id,{id:row.entry_id,display_name:row.display_name||row.model,upstream_model:row.model,transport:row.provider,
      capabilities:[...(row.decision_capable?['decision']:[]),...(row.text_capable?['text']:[]),...(row.image_capable?['image_in']:[]),...(row.image_out_capable?['image_out']:[]),...(row.voice_capable?['voice']:[]),...(row.video_capable?['video']:[])]}]));
    roster=(await request(path(),'PUT',{expected_revision:roster.revision,name:roster.name,entries})).roster;
    // Metadata first; every key write uses the newly returned fence. Keys never enter history.
    for(const row of staged)if(row.api_key){
      const saved=await request(path()+'/entries/'+encodeURIComponent(row.entry_id)+'/credential','PUT',{expected_revision:roster.revision,api_key:row.api_key});
      roster.revision=saved.revision;row.api_key='';
    }
    dirty=false;display({saved:true,revision:roster.revision,published_revision:roster.published_revision});await table.reload();
  });
  publish.onclick=()=>action(async()=>{roster=(await request(path()+'/publish','POST',{expected_revision:roster.revision})).roster;display({published_revision:roster.published_revision});});
  for(const button of host.querySelectorAll('[data-model-lifecycle]'))button.onclick=()=>action(async()=>{
    const operation=button.dataset.modelLifecycle;
    if(operation==='retire'&&!confirm('Retire this catalog? Further dispatch will be denied.'))return;
    const data=await request(path()+'/'+operation,operation==='history'?'GET':'POST',operation==='retire'?{expected_revision:roster.revision}:undefined);
    if(data.roster)roster=data.roster;display(data);
    if(operation==='history')for(const snapshot of data.history||[]) {
      const button=document.createElement('button');button.className='ac-btn';button.textContent='Rollback to revision '+snapshot.revision;
      button.onclick=()=>action(async()=>{if(!confirm('Publish this historical metadata as a new revision? Credentials will not be restored.'))return;roster=(await request(path()+'/rollback','POST',{expected_revision:roster.revision,revision:snapshot.revision})).roster;display({published_revision:roster.published_revision});});result.append(button);
    }
  });
  scope.onchange=()=>{if(dirty&&!confirm('Discard unsaved draft changes?')){scope.value=roster.id;return;}load().catch(error);};
  observer=new MutationObserver(markPending);observer.observe(q('#ac-model-host'),{subtree:true,childList:true});
  const dispose=()=>{if(disposed)return;disposed=true;++scopeGeneration;controller.abort();observer?.disconnect();table?.destroy();analytics?.destroy?.();staged=[];host.replaceChildren();};
  update();try{await load();}catch(e){error(e);}
  return dispose;
}
