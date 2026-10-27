 'use strict';
import { mountModelTable } from '../../../../shared/models/model-table.js';
import { initModelAnalytics } from '../../../../shared/models/model-analytics.js';
import { createModelAdapter, usageFetch } from '../../../../shared/models/v3-adapter.js';
import { mountModelLimits } from './model-limits.js';
import { showNotification } from '../../../../chat/core/notification-panel.js';

// Actual v2_ref Models mount: analytics sibling + shared renderer; only the adapter is v3-specific.
export async function mountSelectionTable(host,{request,agentId,onAssignment}) {
  const bridge=createModelAdapter(request,agentId,onAssignment);await bridge.ready();
  host.replaceChildren();
  const css=document.createElement('link');css.rel='stylesheet';css.href='/v3-ui/shared/models/reference.css';
  const pendingCss=document.createElement('link');pendingCss.rel='stylesheet';pendingCss.href='/v3-ui/shared/models/pending.css';
  const group=document.createElement('div');group.className='v3-reference-models';
  const analyticsHost=document.createElement('div');analyticsHost.className='agent-model-analytics-host';
  const tableHost=document.createElement('div');tableHost.style.marginTop='8px';group.append(analyticsHost,tableHost);host.append(css,pendingCss,group);
  // Clearing and resetting usage only records markers, so the latest one can be undone.
  const usageCleared=title=>{table.reload();showNotification({key:'usage-reset',title,message:'Earlier usage is hidden from the graph and the model table.',actions:[{label:'Undo',kind:'approve',run:async()=>{
    await request('/usage/models/undo?'+new URLSearchParams({agent_id:agentId}),'POST');await Promise.all([table.reload(),analytics.reload()]);
  }}]});};
  const analytics=initModelAnalytics(analyticsHost,{fetchFn:usageFetch('/usage/models'),apiPath:p=>p,scope:'user',agentId,description:'Compare the daily usage of every model used with this agent.',onHistoryCleared:model=>usageCleared('Usage history cleared for '+model)});
  let limits;
  const table=mountModelTable(tableHost,{adapter:bridge.adapter,agentLocalRoster:true,userCatalog:true,inlineCatalogAdd:true,subscriptionScope:'agent',directivePresentation:'inline',directivesEditable:false,manualModelEntry:true,
    onLimitEdit:(id,field)=>limits?.edit(id,field),modelBudget:id=>limits?.budget(id),
    fetchFn:bridge.blockedFetch,onRoster:p=>analytics?.setModelLabels(p),onModelFocus:m=>analytics?.setModelFocus(m),onModelChange:()=>{analytics?.reload();limits?.reload();},
    advancedExtra:body=>{
      const row=document.createElement('div');row.className='ac-row';row.dataset.ownUsage='';row.innerHTML='<div class="ac-ability-row"><span class="ac-ability-label"><span class="ac-ability-name">Reset Usage Counters</span><span class="ac-ability-desc">Clear your own usage with this agent: the tokens and cost in the table and graph above. Other users\' usage is unchanged.</span></span></div><div class="ac-ability-body"><select class="ac-input"><option value="all">All (input + output + cost)</option><option value="input">Input Tokens only</option><option value="output">Output Tokens only</option><option value="cost">Cost only</option></select> <button type="button" class="ac-btn ac-btn-ghost" style="color:var(--danger)">Reset</button> <span class="ac-ability-desc" role="status"></span></div>';
      row.firstElementChild.onclick=()=>row.classList.toggle('expanded');
      const [select,button,status]=row.querySelectorAll('select,button,[role=status]');
      button.onclick=async()=>{
        if(!confirm('Reset '+select.selectedOptions[0].textContent+' usage for this agent?'))return;
        button.disabled=true;status.textContent='Resetting…';
        try{await request('/usage/models/reset?'+new URLSearchParams({agent_id:agentId,scope:select.value}),'POST');status.textContent='';await analytics.reload();usageCleared('Usage counters reset: '+select.selectedOptions[0].textContent);}
        catch(e){status.textContent=e.message||'Reset failed';}
        finally{button.disabled=false;}
      };
      body.append(row);
    }});
  const limitsHost=document.createElement("div");group.append(limitsHost);
  limits=await mountModelLimits(limitsHost,bridge.adapter,()=>table.reload());
  await table.reload();
  let stopped=false;
  function markPending(el){if(el)el.classList.add('v3-pending-feature');}
  function pending(){
    if(stopped)return;
    for(const el of group.querySelectorAll('.ac-model-meta-aliases,.ac-directives-section-title,.ac-directive-hint'))markPending(el);
    for(const el of group.querySelectorAll('.ac-model-alias-add')){el.disabled=true;markPending(el);}
    for(const el of group.querySelectorAll('.ac-model-meta-editable')){
      if(el.closest('.ac-model-meta-nameline'))continue;
      const line=el.closest('.ac-model-meta-line');markPending(line);el.setAttribute('aria-disabled','true');el.tabIndex=-1;el.style.pointerEvents='none';
    }
    // Saved subscription credentials remain unported; API keys use the agent vault.
    for(const row of tableHost.querySelectorAll('.ac-model-cfg-row')){
      const unported=row.querySelector('.ac-ability-name')?.textContent==='Connection';
      for(const btn of row.querySelectorAll('button'))if(['Load','Save'].includes(btn.textContent)){btn.hidden=unported;btn.style.display=unported?'none':'';}
    }
    for(const row of group.querySelectorAll('.ac-directive-row'))markPending(row.querySelector('.ac-ability-name')||row);
    if(!bridge.state()?.can_manage){for(const el of tableHost.querySelectorAll('.ac-model-meta-editable,.ac-saved-drag-handle')){el.style.pointerEvents='none';el.tabIndex=-1;el.setAttribute('aria-disabled','true');}}
    // Resetting your own usage needs no roster authority.
    if(!bridge.state()?.can_manage){for(const el of tableHost.querySelectorAll('input,select,textarea,button'))if(!el.closest('[data-own-usage]'))el.disabled=true;}
  }
  // Rebuilt rows and expanded details keep their pending color after rerender.
  const observer=new MutationObserver(pending);observer.observe(group,{childList:true,subtree:true});pending();
  return {reload:async()=>{await table.reload();await limits.reload();},getState:()=>table.getState(),destroy:()=>{stopped=true;observer.disconnect();limits.destroy();bridge.destroy();table.destroy();analytics?.destroy?.();host.replaceChildren();}};
}
