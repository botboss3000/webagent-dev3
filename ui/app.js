import {createBackgroundIssues} from './shared/background-issues.js';
// Port of V2 private-access shell; authentication remains exclusively V3.
const privateResponse=await fetch('/v3-ui/private-access/private-access-page/private-access-page.html');
if(!privateResponse.ok)throw Error('Private access page failed to load');
document.getElementById('app-restricted-overlay').innerHTML=await privateResponse.text();
const privateHost=document.querySelector('[data-private-access-form]');
const privateReveal=document.querySelector('[data-private-access-reveal]');
privateReveal.onclick=()=>{privateHost.hidden=false;privateReveal.hidden=true;privateReveal.setAttribute('aria-expanded','true');document.getElementById('login-username').focus({preventScroll:true});};
function privateVisible(visible){document.body.classList.toggle('app-restricted',visible);document.getElementById('app-restricted-overlay').setAttribute('aria-hidden',String(!visible));privateHost.hidden=true;privateReveal.hidden=false;privateReveal.setAttribute('aria-expanded','false');}
import { initHeaderCarousel } from './shared/header-carousel.js';
initHeaderCarousel();
import { wirePageTabDrag } from './shared/page-tabs.js';
import { purgeAllBrowserData } from './browser-data-reset.js';
import { createFloatingChat,createChatLauncher } from './chat/widget/window.js';
import { mountNativeWidget } from './chat/widget/native.js';
import { authorizeEmbedPopup } from './embed/authorize.js';
import { mountChatResize } from './shared/chat-resize.js';
import { initChatConfig } from './chat/controls/chat-config.js';
import { mountChatPanel } from './chat/core/panel.js';

// v2 dom-utils imports v1 auth and uploads; v3 uses text nodes and its own transport.
import { renderIdentity } from './main-panel/agents/pages/identity/page.js';
import {renderMembers} from './main-panel/agents/pages/members/page.js';
import { renderAbilities } from './main-panel/agents/pages/abilities/page.js';
import { loadAgentPages } from './main-panel/agents/pages/catalog.js';
import { renderAgentDirectory } from './main-panel/agents/pages/config/page.js';
import { mountSelectionTable } from './main-panel/agents/pages/model/page.js';
import { mountSessionsPage } from './main-panel/agents/pages/sessions/page.js';
import { mountAvailableModels } from './main-panel/account/available-models.js';
let availableModelsDispose;
let selectionTable;
let agentSelectionQueue=Promise.resolve();
let sessionsPage=null,sessionsGeneration=0;
// Accept the original bookmark scope, then normalize it to the Sessions route.
let sessionsScope=['sessions','all'].includes(new URL(location.href).searchParams.get('agentScope'));
let agentPages=[];
let configPage;
let sessionsDescriptor;
const $=id=>document.getElementById(id);
let agentId=new URL(location.href).searchParams.get('agent')||'shared_default';
let agentRoster=[];
let explicitAssignment=false;
const initialTab=new URL(location.href).searchParams.get('tab');
let agentModel='';
async function mountAgents(){
  const response=await fetch('/ui/main-panel/agents/agents.html',{credentials:'same-origin'});
  if(!response.ok)throw Error('Agents page unavailable');
  const template=document.createElement('template');
  template.innerHTML=await response.text();
  const grid=template.content.querySelector('#agents-grid');
  if(!grid)throw Error('Agents page is incomplete');
  grid.classList.remove('agents-phantom');
  grid.classList.add('carousel');
  grid.querySelector('.agents-phantom-detail')?.remove();
  const squares=grid.querySelector('.agents-squares');
  squares.replaceChildren();
  const detail=document.createElement('div');
  detail.className='agent-card active agent-card-tabs-only';
  detail.innerHTML=`<div class="agent-card-tabs-wrap"><div class="agent-card-tabs" role="tablist" aria-label="Agent configuration pages"></div></div>
    <div class="agent-detail-panel"><div class="agent-detail-content"><div class="agent-detail-body" id="model-page">
    <h2>Model</h2><div class="ac-model-table">
      <div class="ac-ability-row ac-model-cfg-row ac-row-static"><span class="ac-ability-label"><span class="ac-ability-name">Provider</span><span class="ac-ability-desc">Use a ChatGPT subscription for this agent.</span></span><span class="ac-model-cfg-control">ChatGPT Subscription</span></div>
      <div class="ac-ability-row ac-model-cfg-row ac-row-static"><span class="ac-ability-label"><span class="ac-ability-name">Connection</span></span><span class="ac-model-cfg-control"><span id="connection" class="ac-hint"></span><button id="connect" type="button" class="ac-btn ac-btn-primary ac-btn-sm">Click to Issue verification code</button><button id="disconnect" type="button" class="ac-btn ac-btn-sm">Disconnect</button><span id="device" class="ac-subscription-auth"></span></span></div>
      <form id="model-form" class="ac-ability-row ac-model-cfg-row"><label for="model" class="ac-ability-label"><span class="ac-ability-name">Model</span><span class="ac-ability-desc">Model ID used for new chat turns.</span></label><span class="ac-model-cfg-control"><input id="model" class="ac-input ac-input-sm" placeholder="gpt-6-luna" required pattern="[A-Za-z0-9._-]+"><button class="ac-btn ac-btn-primary ac-btn-sm">Save model</button></span></form>
    </div><details id="settings"><summary>Tool policies</summary><form id="policy-form"><div id="rules"></div><button type="submit">Save policies</button></form></details>
    </div><div id="identity-page" class="agent-detail-body" hidden></div><div id="abilities-page" hidden></div><div id="members-page" class="agent-detail-body" hidden></div><div id="config-page" class="agent-detail-body" hidden></div></div></div>`;
  const pages=await loadAgentPages();
  configPage=pages.find(page=>page.type==='directory');
  sessionsDescriptor=pages.find(page=>page.type==='sessions');
  agentPages=pages.filter(page=>page.type==='agent_configuration');
  const tabs=detail.querySelector('.agent-card-tabs');
  for(const page of [configPage,...agentPages]){
    const tab=document.createElement('button');
    tab.id='agent-tab-'+page.id;tab.type='button';tab.textContent=page.title;
    tab.setAttribute('role','tab');tab.setAttribute('aria-controls',page.panelId);
    tab.setAttribute('aria-selected','false');
    tabs.append(tab);
    const body=detail.querySelector('#'+page.panelId);
    if(!body)throw Error('Agent page panel unavailable: '+page.id);
    body.setAttribute('role','tabpanel');body.setAttribute('aria-labelledby',tab.id);
  }
  const selectionHost=document.createElement('div');selectionHost.id='selection-model-table';detail.querySelector('#model-page h2').after(selectionHost);
  detail.querySelector('#model-page h2').textContent=agentPages.find(page=>page.id==='model').title;
  const panel=detail.querySelector('.agent-detail-panel');
  panel.remove();
  const region=document.createElement('div');
  region.id='agents-detail-region';
  region.className='open';
  const detailRow=document.createElement('div');
  detailRow.className='agent-row';
  detailRow.dataset.agentId=agentId;
  detailRow.append(detail,panel);
  region.append(detailRow);
  grid.append(region);
  const common=document.createElement('div');common.id='sessions-region';common.className='open';common.hidden=true;
  common.innerHTML='<div class="agent-row expanded agent-row-sessions"><div class="agent-card active agent-card-tabs-only"><div class="agent-card-tabs-wrap"><div class="agent-card-tabs" role="tablist" aria-label="Sessions pages"><button id="sessions-page-tab" type="button" role="tab" class="active" aria-selected="true" aria-controls="sessions-section">Sessions</button></div></div></div><div class="agent-detail-panel"><div class="agent-detail-content"><section id="sessions-section" role="tabpanel" aria-labelledby="sessions-page-tab"></section></div></div></div>';
  common.querySelector('#sessions-page-tab').textContent=sessionsDescriptor.title;
  common.querySelector('#sessions-page-tab').setAttribute('aria-controls',sessionsDescriptor.panelId);
  grid.append(common);
  const css=document.createElement('link');css.rel='stylesheet';css.href='/v3-ui/main-panel/agents/pages/sessions/sessions.css';document.head.append(css);
  $('agents-grid').replaceWith(grid);
  const route=new URL(location.href);
  if(activeMainPage==='agents'){
    route.searchParams.set('tab','agents');
    route.searchParams.set('agent',agentId);
    history.replaceState({},'',route);
  }

}
// Caller-scoped discovery; configuration selection never rebinds an open chat.
function renderAgentCards(){
  const squares=document.querySelector('#agents-grid .agents-squares');
  squares.replaceChildren();
  const common=document.createElement('div');common.className='agent-row';
  common.innerHTML='<button type="button" class="agent-card agent-square" aria-label="Sessions"><div class="agent-card-top"><div class="agent-card-icon-wrap color-purple">'+ '<i data-lucide="messages-square"></i>'+'</div><div class="agent-card-meta"><div class="agent-card-name-row"><span class="agent-card-name">Sessions</span></div></div></div></button>';
  common.firstElementChild.setAttribute('aria-label',sessionsDescriptor.title);
  common.querySelector('.agent-card-name').textContent=sessionsDescriptor.title;
  common.firstElementChild.classList.toggle('activated',sessionsScope);common.firstElementChild.setAttribute('aria-pressed',String(sessionsScope));
  common.firstElementChild.onclick=guard(()=>showSessions());squares.append(common);
  for(const agent of agentRoster){
    const row=document.createElement('div');row.className='agent-row';row.dataset.agentId=agent.id;
    row.innerHTML='<div class="agent-card agent-square activated"><div class="agent-card-top"><div class="agent-card-icon-wrap color-purple"><svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="4"/><circle cx="12" cy="12" r="3"/></svg></div><div class="agent-card-meta"><div class="agent-card-name-row"><span class="agent-card-name">WebAgent</span><span class="agent-status-dot"></span></div></div></div></div>';
    const card=row.firstElementChild;
    card.classList.toggle('activated',!sessionsScope&&agent.id===agentId);
    card.setAttribute('role','button');card.tabIndex=0;
    card.setAttribute('aria-label','Configure '+(agent.name||agent.id));
    card.setAttribute('aria-pressed',String(!sessionsScope&&agent.id===agentId));
    card.querySelector('.agent-card-name').textContent=agent.name||agent.id;
    card.querySelector('.agent-status-dot')?.remove();
    if(agent.description)card.title=agent.description;
    const select=guard(()=>{
      // Serialize remounts so rapid card clicks cannot retain another agent's handlers.
      agentSelectionQueue=agentSelectionQueue.catch(()=>{}).then(()=>selectAgent(agent.id));
      return agentSelectionQueue;
    });
    card.onclick=select;
    card.onkeydown=event=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();select();}};
    squares.append(row);
  }
  if(!agentRoster.length){const empty=document.createElement('p');empty.textContent='No agents are available to this account.';squares.append(empty);}
  document.querySelector('#agents-detail-region .agent-row').dataset.agentId=agentId;
}
async function loadAgentRoster(){
  const data=await api('/models/agents');
  if(!Array.isArray(data.agents))throw Error('Agent list unavailable');
  agentRoster=data.agents;
  if(agentRoster.length&&!agentRoster.some(agent=>agent.id===agentId)){
    agentId=(agentRoster.find(agent=>agent.id==='shared_default')||agentRoster[0]).id;
    const route=new URL(location.href);route.searchParams.set('agent',agentId);history.replaceState({},'',route);
    note('Requested agent is unavailable. Showing an available agent.');
  }
  renderAgentCards();
}
let mainPages=[];
// Lucide names for the mobile sidebar; pages without an entry use a plain square.
const mobileNavIcons={app:'layout-grid',agents:'bot',account:'user-cog',builds:'hammer','session-metrics':'chart-line'};
let activeMainPage='';
const protectedPageStarts=new Map();
const protectedAsset=path=>typeof path==='string'&&path.startsWith('/api/v3/modules/');
function startProtectedPage(page){
  if(!accountMe||!page||!protectedAsset(page.entry)||protectedPageStarts.has(page.id))return;
  const mount=$('tab-'+page.id);
  // Optional package assets require authentication. A rejected import must not
  // abort the shell before sign-in, or hide unrelated pages after sign-in.
  const pending=(async()=>{
    for(const href of page.css||[]){
      if(!protectedAsset(href))continue;
      const link=document.createElement('link');link.rel='stylesheet';link.href=href;
      document.head.insertBefore(link,document.querySelector('link[href^="/v3-ui/app.css"]'));
    }
    if(protectedAsset(page.html)){
      const response=await fetch(page.html,{credentials:'same-origin'});
      if(!response.ok)throw Error(page.label+' page unavailable');
      mount.innerHTML=await response.text();
    }
    const module=await import(page.entry);
    if(typeof module.start!=='function')throw Error(page.label+' page has no start function');
    await module.start(mount);
  })().catch(error=>{
    mount.textContent=page.label+' could not load: '+error.message;
    protectedPageStarts.delete(page.id);
  });
  protectedPageStarts.set(page.id,pending);
}
async function loadMainPages(){
  const response=await fetch('/api/v3/pages/catalog',{credentials:'same-origin',cache:'no-store'});
  if(!response.ok)throw Error('Main page catalog unavailable');
  const catalog=await response.json();
  if(!Array.isArray(catalog.main)||!catalog.main.length)throw Error('No main pages configured');
  mainPages=catalog.main;
  activeMainPage=mainPages.some(page=>page.id===initialTab)?initialTab:mainPages[0].id;
  for(const page of mainPages){
    for(const href of page.css||[]){
      if(protectedAsset(href))continue;
      const link=document.createElement('link');
      link.rel='stylesheet';link.href=href;
      document.head.insertBefore(link,document.querySelector('link[href^="/v3-ui/app.css"]'));
    }
    const tab=document.createElement('button');
    tab.id='tab-button-'+page.id;tab.type='button';tab.className='main-tab';
    tab.dataset.value=page.id;tab.setAttribute('role','tab');tab.textContent=page.label;
    tab.onclick=()=>showMainPage(page.id);
    $('main-tabs').append(tab);
    const option=document.createElement('option');
    option.value=page.id;option.textContent=page.label;
    $('main-tab-select').append(option);
    const mobile=document.createElement('button');
    mobile.id='mobile-nav-'+page.id;mobile.type='button';mobile.className='mobile-nav-item';
    const icon=document.createElement('i');icon.className='lucide-icon';icon.dataset.lucide=mobileNavIcons[page.id]||'square';
    const label=document.createElement('span');label.textContent=page.label;
    mobile.append(icon,label);mobile.onclick=()=>showMainPage(page.id);
    $('mobile-nav-items').append(mobile);
    window.lucide?.createIcons();
    const mount=document.createElement('div');
    mount.id='tab-'+page.id;mount.className='tab-content';mount.hidden=page.id!==activeMainPage;
    $('main-panel').append(mount);
    if(page.html&&!protectedAsset(page.html)){
      const partial=await fetch(page.html,{credentials:'same-origin'});
      if(!partial.ok)throw Error(page.label+' page unavailable');
      mount.innerHTML=await partial.text();
    }
    if(page.id==='agents')await mountAgents();
    if(page.entry&&!protectedAsset(page.entry)){
      const module=await import(page.entry);
      if(typeof module.start!=='function')throw Error(page.label+' page has no start function');
      await module.start(mount);
    }
  }
  updateMainPageState();
}
function updateMainPageState(){
  for(const page of mainPages){
    const selected=page.id===activeMainPage;
    $('tab-'+page.id).hidden=!selected;
    $('tab-'+page.id).classList.toggle('active',selected);
    $('tab-button-'+page.id).classList.toggle('active',selected);
    $('tab-button-'+page.id).setAttribute('aria-selected',String(selected));
  }
  $('main-tab-select').value=activeMainPage;
}
let csrf='',state={},poll;
const ids=new Map();
const keyId=key=>{if(!ids.has(key))ids.set(key,crypto.randomUUID());return ids.get(key);};
let noticeSource='',actionNotice='',backgroundNotices=[];
const paintNotices=()=>{$('notice').textContent=[actionNotice,...backgroundNotices].filter(Boolean).join(' · ');};
const issues=createBackgroundIssues({request:(...args)=>api(...args),render:messages=>{backgroundNotices=messages;paintNotices();}});
const note=(text,source='')=>{
  const visibleText=(text==='Sign in required'||text==='Guest access is not enabled')?'':text;
  actionNotice=visibleText;paintNotices();
  noticeSource=visibleText?source:'';
};
const clearNoticeFrom=source=>{if(noticeSource===source)note('');};
async function api(path,method='GET',body){const response=await fetch('/api/v3'+path,{method,credentials:'same-origin',headers:{'Content-Type':'application/json',...(method==='GET'?{}:{'x-csrf-token':csrf})},...(body===undefined?{}:{body:JSON.stringify(body)})});const data=await response.json().catch(()=>({}));if(!response.ok){if(response.status===401&&path!=='/auth/login')out();const error=Error(data.error||data.message||'Request failed ('+response.status+')');error.status=response.status;throw error;}return data;}
const guard=fn=>async event=>{event?.preventDefault();try{await fn(event);note('');}catch(error){note(error.message);}};
const chat = await mountChatPanel({request:api, guard, note, clearNoticeFrom, issues, keyId, forgetKey:key=>ids.delete(key), refresh, getState:()=>state, getAgentId:()=>agentId, getIdentity:()=>accountMe});
await loadMainPages();

function disposeSessionsPage(){++sessionsGeneration;sessionsPage?.destroy();sessionsPage=null;$('tab-agents').classList.remove('sessions-view');}
async function showSessions(routeMode='push'){
  if(activeMainPage!=='agents')return;
  sessionsScope=true;$('tab-agents').classList.add('sessions-view');$('agents-detail-region').hidden=true;$('sessions-region').hidden=false;renderAgentCards();
  const route=new URL(location.href);route.searchParams.set('tab','agents');route.searchParams.set('agentScope','sessions');route.searchParams.set('agentView','sessions');
  if(route.href!==location.href)history[routeMode==='replace'?'replaceState':'pushState']({},'',route);
  if(sessionsPage||activeMainPage!=='agents')return;
  const generation=++sessionsGeneration;
  const mounted=await mountSessionsPage($('sessions-section'),{request:api,userId:accountMe?.user_id||accountMe?.user,roster:agentRoster,getSessionId:()=>chat.sessionId,isCurrent:()=>generation===sessionsGeneration&&sessionsScope&&activeMainPage==='agents',
    open:async id=>{await chat.open(id);if(mobileLayout.matches)showMobileView('chat');},
    changed:()=>{window.dispatchEvent(new Event('v3:sessions-changed'));void chat.sessions().catch(()=>{});}});
  if(generation===sessionsGeneration)sessionsPage=mounted;else mounted?.destroy();
}
// Agent pages resolve tenant authority independently; members see effective read-only abilities.
async function showAgentView(view,routeMode='push'){
  if(sessionsScope){await showSessions(routeMode);return;}
  disposeSessionsPage();$('sessions-region').hidden=true;
  const pages=[configPage,...agentPages];
  const canManage=agentRoster.find(agent=>agent.id===agentId)?.can_manage===true;
  const selected=!canManage&&!['model','abilities'].includes(view)?'abilities':pages.some(page=>page.id===view)?view:'config';
  for(const page of pages){
    $(page.panelId).hidden=page.id!==selected;
    const tab=$('agent-tab-'+page.id);
    tab.setAttribute('aria-selected',String(page.id===selected));
    tab.classList.toggle('active',page.id===selected);
    tab.hidden=!agentTabOrder.includes(page.id)&&page.id!==selected;
    tab.classList.toggle('ephemeral',!agentTabOrder.includes(page.id)&&page.id===selected);
    if(!tab.hidden&&!agentTabOrder.includes(page.id))tab.parentElement.append(tab);
  }
  const route=new URL(location.href);
  route.searchParams.set('agentView',selected);
  if(route.href!==location.href)history[routeMode==='replace'?'replaceState':'pushState']({},'',route);
  if(selected==='identity')await renderIdentity($('identity-page'),api,{agentId,onSaved:agent=>{const saved=agentRoster.find(item=>item.id===agentId);if(saved)saved.name=agent.name;renderAgentCards();}});
  if(selected==='abilities')await renderAbilities($('abilities-page'),api,{agentId});
  if(selected==='members')await renderMembers($('members-page'),api,{agentId});
}
async function selectAgent(id){
  if(!sessionsScope&&id===agentId){$('agents-detail-region').scrollIntoView({block:'start',behavior:'smooth'});return;}
  sessionsScope=false;
  disposeSessionsPage();
  $('sessions-region').hidden=true;
  $('agents-detail-region').hidden=false;
  agentId=id;
  const route=new URL(location.href);
  route.searchParams.set('tab','agents');
  route.searchParams.set('agent',id);
  route.searchParams.delete('agentScope');
  if(route.searchParams.get('agentView')==='sessions')route.searchParams.set('agentView','config');
  history.pushState({},'',route);
  renderAgentCards();
  for(const page of [configPage,...agentPages])$(page.panelId).hidden=true;
  // Rebind configuration handlers without remounting or reopening the conversation.
  await Promise.all([
    mountAgentModels(),
    showAgentView(route.searchParams.get('agentView')||'config','replace')
  ]);
}
for(const page of [configPage,...agentPages]){
  $('agent-tab-'+page.id).onclick=guard(()=>showAgentView(page.id));
}
// Like V2, the favorite list is also the tab order; Config cannot be removed.
let agentTabOrder=['config'];
let agentTabsRevision=0;
let agentTabsReady=false;
let agentTabsGeneration=0;
let agentTabsQueue=Promise.resolve();
const validAgentTabs=new Set(['config',...agentPages.map(page=>page.id)]);
function applyAgentTabs(snapshot){
  agentTabOrder=[...new Set(snapshot.page_ids||[])].filter(id=>validAgentTabs.has(id));
  if(!agentTabOrder.includes('config'))agentTabOrder.push('config');
  agentTabsRevision=snapshot.revision;
  syncAgentTabs();
}
async function loadAgentTabs(){
  agentTabsGeneration++;
  agentTabsReady=false;
  const snapshot=await api('/user/agent-page-favorites');
  applyAgentTabs(snapshot);
  agentTabsReady=true;
}
function syncAgentTabs(){
  const strip=document.querySelector('#agents-detail-region .agent-card-tabs');
  for(const id of agentTabOrder)strip.append($('agent-tab-'+id));
  for(const page of [configPage,...agentPages])$('agent-tab-'+page.id).hidden=!agentTabOrder.includes(page.id);
  renderAgentDirectory($('config-page'),agentPages,view=>guard(()=>showAgentView(view))(),agentTabOrder,id=>{
    if(!agentTabsReady){note('Agent page preferences are unavailable. Reload to retry.');return;}
    if(agentTabOrder.includes(id))agentTabOrder=agentTabOrder.filter(item=>item!==id);
    else agentTabOrder.unshift(id);
    saveAgentTabs();
  });
}
function saveAgentTabs(){
  syncAgentTabs();
  const pages=agentTabOrder.slice(),user=accountMe?.user_id||accountMe?.user,generation=agentTabsGeneration;
  const request=async()=>{
    if(generation!==agentTabsGeneration||!agentTabsReady||user!==(accountMe?.user_id||accountMe?.user))return;
    try{
      const saved=await api('/user/agent-page-favorites','PUT',{page_ids:pages,expected_revision:agentTabsRevision});
      agentTabsRevision=saved.revision;
    }catch(error){
      agentTabsReady=false;
      note('Agent page favorites were not saved: '+error.message);
      try{await loadAgentTabs();}catch{note('Agent page favorites could not be loaded. Reload to retry.');}
    }
  };
  agentTabsQueue=agentTabsQueue.then(request,request);
}

syncAgentTabs();
wirePageTabDrag(document.querySelector('#agents-detail-region .agent-card-tabs'), { getOrder: () => agentTabOrder, save: saveAgentTabs, prefix: 'agent-tab-' });
async function loadGuestPolicy(){const approval=document.querySelector('.private-access-approval');approval.hidden=true;try{const policy=await api('/auth/access-mode');$('guest-login').hidden=!policy.guest_access;approval.hidden=policy.access_mode!=='admin_approval';}catch{$('guest-login').hidden=true;}}
$('guest-login').onclick=async()=>{const btn=$('guest-login');btn.disabled=true;try{const result=await api('/auth/guest-login','POST',{});csrf=result.csrf_token;await boot();}catch(error){$('registration-status').textContent=error.message;}finally{btn.disabled=false;}};
// Authentication loss is a state transition, not a refresh event. Late 401s
// must not reset an open sign-in form or recursively refresh identity consumers.
function out(){if(document.body.classList.contains('app-restricted'))return;issues.stop();disposeSessionsPage();widget?.hide();accountMe=null;agentTabsGeneration++;agentTabsReady=false;agentTabOrder=['config'];syncAgentTabs();availableModelsDispose?.();availableModelsDispose=null;window.dispatchEvent(new Event('v3-identity-changed'));loadGuestPolicy();csrf='';chat.disconnect();clearInterval(poll);closeMobileNav();closeAccountMenu();$('app').hidden=true;privateVisible(true);window.__boot?.reveal();}
async function refresh(){if(accountMe&&!accountMe.is_admin){state=await api('/user/status');$('shutoff').hidden=true;chat.runs(state.runs||[]);return;}state=await api('/status');agentModel=state.model||agentModel;$('shutoff').querySelector('span').textContent=state.shutoff?'Execution shut off — enable':'Shut off execution';$('connection').textContent=state.provider?.connected?'Connected':'Connection required';$('connect').hidden=!!state.provider?.connected;$('disconnect').hidden=!state.provider?.connected;if(document.activeElement!==$('model'))$('model').value=agentModel||state.model||'';$('device').replaceChildren();const login=state.provider?.login;if(login?.verification_url){const link=document.createElement('a');link.href=login.verification_url;link.target='_blank';link.rel='noopener noreferrer';link.textContent='Open ChatGPT authorization';const code=document.createElement('p');code.textContent='Code: '+login.user_code+' — '+(login.status||'Waiting');$('device').append(link,code);}if(!$('rules').children.length)for(const [tool,rule]of Object.entries(state.policy?.rules||{})){const label=document.createElement('label');label.textContent=tool;const select=document.createElement('select');select.name=tool;for(const value of ['allow','deny','require_approval']){const option=document.createElement('option');option.value=value;option.textContent=value.replaceAll('_',' ');select.append(option);}select.value=rule;label.append(select);$('rules').append(label);}chat.runs(state.runs||[]);}
// Explicit role administration replaces the legacy default-model form for every agent.
// app-settings ui.mobile_mode.full_height toggles the mobile edge-to-edge shell (app.css).
function applyMobileFullHeight(enabled){
  document.body.classList.toggle('mobile-full-height',enabled===true);
}
async function boot(){
  const me=await api('/auth/me');
  csrf=me.csrf_token;
  await initChatConfig().catch(() => {});
  if(await authorizeEmbedPopup({request:api,me}))return;
  widget?.configure();
  // Boot defaults are applied once per sign-in, never by live refresh over a user's drag.
  const bootUi=await api('/auth/ui-config');
  const panel=bootUi.app_ui?.chat_panel||{width:45,unit:'%'};
  applyMobileFullHeight(bootUi.app_ui?.mobile_mode?.full_height===true);
  desktopChatVisible=panel.width!==0;
  if(!desktopChatVisible){mobileView='main';widget.hide();}
  chatResize.configure(panel);syncLayout();
  renderAccount(me);issues.start();
  window.dispatchEvent(new Event('v3-identity-changed'));
  privateVisible(false);
  $('app').hidden=false;
  window.__boot?.shellReady();
  startProtectedPage(mainPages.find(page=>page.id===activeMainPage));
  await agentTabsQueue;
  await loadAgentTabs();
  await loadAgentRoster();
  const inviteToken=new URL(location.href).searchParams.get('chat_invite');
  if(inviteToken){
    try{const redeemed=await api('/chat-invitations/redeem','POST',{token:inviteToken,client_request_id:keyId('invite:'+inviteToken)});const route=new URL(location.href);route.searchParams.delete('chat_invite');route.searchParams.set('session_id',redeemed.session_id);history.replaceState({},'',route);ids.delete('invite:'+inviteToken);}
    catch(error){note('Invitation could not be redeemed: '+error.message);}
  }
  if(!agentRoster.length){$('agents-detail-region').hidden=true;await chat.sessions();const requested=new URL(location.href).searchParams.get('session_id');if(requested){try{await chat.open(requested,'replace');}catch(error){note('Conversation unavailable: '+error.message);}}if(sessionsScope)await showSessions('replace');window.__boot?.appReady();return;}
  $('agents-detail-region').hidden=false;
  await refresh();
  await mountAgentModels();
  await showAgentView(new URL(location.href).searchParams.get('agentView')||(me.is_admin?'config':'model'),'replace');
  const items=await chat.sessions();
  const requested=new URL(location.href).searchParams.get('session_id');
  const selected=items.find(item=>item.id===requested)||(requested?{id:requested}:items[0]);
  if(selected){try{await chat.open(selected.id,'replace');}catch(error){note('Conversation unavailable: '+error.message);}}
  else if(requested){const route=new URL(location.href);route.searchParams.delete('session_id');history.replaceState({},'',route);}
  if(requested&&requested!==selected?.id)note('Requested conversation is unavailable. Showing your latest conversation.');
  if(initialTab==='account')showAccountPanel('replace');
  window.__boot?.appReady();
  clearInterval(poll);
  poll=setInterval(()=>refresh().then(()=>issues.resolve('status_poll')).catch(error=>issues.fail('status_poll',error)),3000);
}
async function mountAgentModels(){
  selectionTable?.destroy();
  selectionTable=null;
  $('selection-model-table').replaceChildren();
  $('model-page').querySelector('.ac-model-table').hidden=true;
  $('settings').hidden=true;
  const mountModels=()=>mountSelectionTable($('selection-model-table'),{
    request:api,agentId,
    onAgent:async id=>{
      agentId=id;
      const route=new URL(location.href);
      route.searchParams.set('agent',id);
      if(route.href!==location.href)history.pushState({},'',route);
      await showAgentView(route.searchParams.get('agentView')||'model','replace');
    },
    onAssignment:async(id,assignment)=>{
      explicitAssignment=!!assignment?.roles?.standard;
      $('model-page').querySelector('.ac-model-table').hidden=true;
      $('settings').hidden=true;
      renderAgentCards();
    }
  });
  try{
    selectionTable=await mountModels();
  }catch(error){
    if(error.status===401)throw error;
    if(agentId!=='shared_default'&&[403,404].includes(error.status)){
      agentId='shared_default';
      const route=new URL(location.href);
      route.searchParams.set('agent',agentId);
      history.replaceState({},'',route);
      renderAgentCards();
      try{selectionTable=await mountModels();}
      catch(fallbackError){error=fallbackError;selectionTable=null;}
      if(selectionTable)note('Requested agent is unavailable. Showing WebAgent.');
    }
    if(!selectionTable){
      const warning=document.createElement('p');
      warning.className='ac-hint';
      warning.setAttribute('role','status');
      warning.textContent=error.status===404?'Model role management requires a backend update.':'Model assignment management unavailable: '+error.message;
      $('selection-model-table').replaceChildren(warning);
    }
  }
}
// A rejected sign-in is a credential error, not expiry of an authenticated session.
// @code-index {"version":1,"feature":"password-error-feedback","aliases":["incorrect password","sign-in error"],"purpose":"Keep credential forms open and report errors below their password fields; error feedback is an accessibility invariant.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Credential rejection must remain visible and cannot be hidden by a preference."}}
/** Set an inline password error and its accessible invalid/description state. */
function passwordError(fieldId,errorId,message=''){
  const field=$(fieldId);
  let error=$(errorId);
  // An older cached form partial can coexist with the newly published script.
  if(!error){
    error=document.createElement('p');error.id=errorId;error.className='password-error';
    error.setAttribute('role','status');error.setAttribute('aria-live','polite');field.after(error);
  }
  const descriptions=new Set((field.getAttribute('aria-describedby')||'').split(/\s+/).filter(Boolean));
  descriptions.add(errorId);field.setAttribute('aria-describedby',[...descriptions].join(' '));
  error.textContent=message;error.hidden=!message;
  if(message)field.setAttribute('aria-invalid','true');else field.removeAttribute('aria-invalid');
}
for(const [field,error]of [['login-password','login-password-error'],['account-current-password','account-current-password-error']]){
  $(field).addEventListener('input',()=>passwordError(field,error));
}
$('login-username').addEventListener('input',()=>passwordError('login-password','login-password-error'));
// Registration mirrors V2's mode switch; V3 retains independent cookie authentication.
let registerMode=false;
$('register-toggle').onclick=()=>{
  registerMode=!registerMode;
  passwordError('login-password','login-password-error');
  $('registration-status').classList.remove('is-error');
  $('login-title').textContent=registerMode?'Create account':'Sign in';
  $('login-submit').textContent=registerMode?'Create account':'Sign in';
  $('register-toggle').textContent=registerMode?'Back to sign in':'Create an account';
  $('register-name-row').hidden=!registerMode;
  $('register-name').disabled=!registerMode;
  $('register-name').required=registerMode;
  $('login-password').autocomplete=registerMode?'new-password':'current-password';
  $('login-password').minLength=registerMode?8:0;
  $('registration-status').textContent='';
};
$('login-form').onsubmit=async event=>{
  event.preventDefault();
  const submit=$('login-submit');submit.disabled=true;
  passwordError('login-password','login-password-error');
  $('registration-status').classList.remove('is-error');
  $('registration-status').textContent=registerMode?'Creating account…':'Signing in…';
  try{
    const data=new FormData($('login-form'));
    const body={username:String(data.get('username')).trim(),password:data.get('password')};
    if(registerMode)body.display_name=String(data.get('display_name')).trim();
    const result=await api(registerMode?'/auth/register':'/auth/login','POST',body);
    if(result.pending_approval){
      $('login-password').value='';
      $('registration-status').textContent='Account created and waiting for administrator approval.';
      return;
    }
    csrf=result.csrf_token;$('login-form').reset();await boot();
    $('registration-status').textContent='';
  }catch(error){
    if(!registerMode&&error.status===401){
      $('registration-status').textContent='';
      passwordError('login-password','login-password-error','Incorrect username or password. Please try again.');
    }else{
      $('registration-status').classList.add('is-error');
      $('registration-status').textContent=error.message;
    }
  }
  finally{submit.disabled=false;}
};
const accountMenu=$('user-dropdown-menu');
// V2's body-level portal keeps the panel out of the header carousel's clipping context.
document.body.append(accountMenu);
function positionAccountMenu(){
  if(accountMenu.hidden)return;
  const anchor=$('top-user-id').getBoundingClientRect(),height=accountMenu.offsetHeight;
  // In the mobile sidebar footer the trigger sits low, so open upward when the menu does not fit below.
  const below=Math.round(anchor.bottom+6),top=below+height>innerHeight-8?Math.round(anchor.top-height-6):below;
  accountMenu.style.marginTop='0';
  accountMenu.style.top=Math.max(8,Math.min(top,innerHeight-height-8))+'px';
  accountMenu.style.left=Math.max(8,Math.min(Math.round(anchor.left),innerWidth-accountMenu.offsetWidth-8))+'px';
}
function closeAccountMenu(){accountMenu.hidden=true;$('top-user-id').setAttribute('aria-expanded','false');}
$('top-user-id').onclick=event=>{event.stopPropagation();accountMenu.hidden=!accountMenu.hidden;$('top-user-id').setAttribute('aria-expanded',String(!accountMenu.hidden));positionAccountMenu();};
const closeOutsideAccountMenu=event=>{if(!accountMenu.hidden&&!event.target.closest('#user-dropdown-menu, #top-user-id'))closeAccountMenu();};
document.addEventListener('pointerdown',closeOutsideAccountMenu,true);
document.addEventListener('click',closeOutsideAccountMenu,true);
window.addEventListener('resize',positionAccountMenu);
document.querySelector('#main-tabs').addEventListener('scroll',positionAccountMenu);
document.addEventListener?.('keydown',event=>{if(event.key==='Escape')closeAccountMenu();});
let accountMe=null;
function renderAccount(me){
  accountMe=me;
  // Main-page visibility follows the authenticated identity, including on sign-in changes.
  for(const page of mainPages.filter(page=>page.id==='builds')){
    $('tab-button-'+page.id).hidden=!me.is_admin;
    $('mobile-nav-'+page.id).hidden=!me.is_admin;
    const option=$('main-tab-select').querySelector('option[value="'+page.id+'"]');
    if(option) option.disabled=!me.is_admin;
  }
  if(!me.is_admin&&activeMainPage==='builds') showMainPage('agents','replace');
  const guest=me.role==='guest';
  for(const id of ['account-profile-form','account-password-form','account-session-policy-form'])$(id).hidden=guest;
  $('shutoff').hidden=!me.is_admin;
  $('header-refresh-browser').hidden=!me.is_admin;
  // Agent-scoped Models has its own permission checks and is also useful to members.
  document.querySelector('#agents-grid .agent-card-tabs-wrap').hidden=false;
  $('account-name').textContent=me.display_name||me.user||'Account';
  $('account-username').textContent=me.user||'';
  $('account-identity').textContent=me.user||'';
  $('account-login-name').value=me.user||'';
  $('account-display-name').value=me.display_name||me.user||'';
  $('top-user-id').title=me.user||'Manage account';
}
function showAccountPanel(routeMode='push'){showMainPage('account',routeMode);}
function showMainPage(id,routeMode='push'){
  if(!mainPages.some(page=>page.id===id)||id==='builds'&&!accountMe?.is_admin)return;
  closeMobileNav();
  activeMainPage=id;
  updateMainPageState();
  startProtectedPage(mainPages.find(page=>page.id===id));
  if(id!=='agents')disposeSessionsPage();else if(sessionsScope&&accountMe)void showSessions('replace').catch(error=>note(error.message));
  if(id==='account')loadAccountControls();
  const route=new URL(location.href);
  route.searchParams.set('tab',id);
  if(id==='agents')route.searchParams.set('agent',agentId);
  if(route.href!==location.href)history[routeMode==='replace'?'replaceState':'pushState']({},'',route);
  if(mobileLayout.matches)showMobileView('main');
  updateMobileNavigation();
}
$('manage-account').onclick=()=>{closeAccountMenu();showAccountPanel();};
$('account-profile-form').onsubmit=async event=>{
  event.preventDefault();
  const status=$('account-profile-status');
  const display_name=$('account-display-name').value.trim();
  const username=$('account-login-name').value.trim();
  if(!display_name||!username){status.textContent='Enter a username and display name.';return;}
  try{
    const profile=await api('/auth/profile','PUT',{username,display_name});
    renderAccount({...accountMe,...profile});
    status.textContent='Saved';
  }catch(error){status.textContent=error.message;}
};
async function loadAccountControls(){
  availableModelsDispose?.();
  availableModelsDispose=mountAvailableModels($('account-available-models'),api);
  try{
    const [policy,listing]=await Promise.all([api('/auth/session-policy'),api('/auth/sessions')]);
    $('account-session-days').value=String(policy.days);
    const list=$('account-sessions');
    list.replaceChildren();
    for(const entry of listing.sessions){
      const row=document.createElement('div');row.className='account-signin-row';
      const details=document.createElement('div');details.className='account-signin-details';
      const name=document.createElement('strong');name.textContent=entry.current?'This browser':(entry.user_agent||'Unknown browser');
      const dates=document.createElement('span');
      const when=entry.created_at?new Date(entry.created_at*1000).toLocaleString():'Earlier sign-in';
      dates.textContent=when+' · expires '+new Date(entry.expires_at*1000).toLocaleString();
      details.append(name,dates);
      const revoke=document.createElement('button');revoke.type='button';
      revoke.textContent=entry.current?'Sign out':'Revoke';revoke.onclick=async()=>{
        await api('/auth/sessions/'+encodeURIComponent(entry.id),'DELETE');
        if(entry.current){out();note('Signed out.');}else await loadAccountControls();
      };
      revoke.className='account-btn';row.append(details,revoke);list.append(row);
    }
    if(!listing.sessions.length)list.textContent='No active sign-ins.';
    $('account-sessions-status').textContent='';
  }catch(error){$('account-sessions-status').textContent=error.message;}
}
$('account-sessions-refresh').onclick=loadAccountControls;
$('account-session-policy-form').onsubmit=async event=>{
  event.preventDefault();
  const status=$('account-session-policy-status');
  try{
    await api('/auth/session-policy','PUT',{days:Number($('account-session-days').value)});
    status.textContent='Saved. Applies to new sign-ins.';
  }catch(error){status.textContent=error.message;}
};
$('account-password-form').onsubmit=async event=>{
  event.preventDefault();
  const status=$('account-password-status');
  passwordError('account-current-password','account-current-password-error');
  status.classList.remove('is-error');status.textContent='';
  const current_password=$('account-current-password').value;
  const new_password=$('account-new-password').value;
  if(new_password!==$('account-confirm-password').value){
    status.classList.add('is-error');
    status.textContent='New passwords do not match.';
    return;
  }
  try{
    await api('/auth/change-password','POST',{current_password,new_password});
    $('account-password-form').reset();
    out();
    note('Password changed. Sign in again.');
  }catch(error){
    if(error.status===403&&error.message==='Current password is incorrect'){
      passwordError('account-current-password','account-current-password-error','Current password is incorrect. Please try again.');
    }else{status.classList.add('is-error');status.textContent=error.message;}
  }
};
const mobileLayout=matchMedia('(max-width:800px)');
let mobileView=initialTab==='account'?'main':new URL(location.href).searchParams.has('session_id')?'chat':'main';
let desktopChatVisible=true;
const widget=mountNativeWidget({chat,createFloatingChat,createChatLauncher,onChange:()=>syncLayout(),onDock:()=>{desktopChatVisible=true;syncLayout();},note});
const chatResize=mountChatResize({enabled:()=>!mobileLayout.matches&&!widget.floating&&desktopChatVisible});
function updateChatControl(visible){$('chat-toggle-btn').classList.toggle('active',visible);$('chat-toggle-btn').setAttribute('aria-expanded',String(visible));}
function showMobileView(view){mobileView=view;$('app').dataset.mobileView=view;updateChatControl(view==='chat');updateMainPageState();}
function syncLayout(){
  document.body.classList.toggle('mobile-mode',mobileLayout.matches);
  $('mobile-nav-toggle').hidden=!mobileLayout.matches;
  $('mobile-back-main-btn').hidden=!mobileLayout.matches;
  if(!mobileLayout.matches)closeMobileNav();
  // The desktop tab strip is hidden on mobile, so the account control lives in the sidebar footer there.
  const account=document.querySelector('.user-dropdown');
  if(mobileLayout.matches)$('mobile-nav-footer').append(account);
  else if(account.parentElement!==$('main-tabs'))$('main-tabs').insertBefore(account,$('main-tabs').querySelector('.main-tab'));
  positionAccountMenu();
  updateMobileNavigation();
  if(widget.floating){if(mobileLayout.matches)$('app').dataset.mobileView='main';else delete $('app').dataset.mobileView;updateMainPageState();$('chat-panel').hidden=false;$('chat-resize-handle').hidden=true;updateChatControl(widget.isOpen);return;}
  if(mobileLayout.matches){$('chat-panel').hidden=false;$('chat-resize-handle').hidden=false;showMobileView(mobileView);}
  else{delete $('app').dataset.mobileView;$('chat-panel').hidden=!desktopChatVisible;$('chat-resize-handle').hidden=!desktopChatVisible;updateChatControl(desktopChatVisible);updateMainPageState();}
}
const mobileNav=$('mobile-nav-menu');
function closeMobileNav(){mobileNav.hidden=true;$('mobile-nav-toggle').setAttribute('aria-expanded','false');}
function updateMobileNavigation(){
  $('mobile-back-main-btn').textContent=mainPages.find(page=>page.id===activeMainPage)?.label||'';
  for(const page of mainPages)$('mobile-nav-'+page.id).classList.toggle('active',page.id===activeMainPage);
}
$('mobile-nav-toggle').onclick=event=>{event.stopPropagation();closeAccountMenu();mobileNav.hidden=!mobileNav.hidden;$('mobile-nav-toggle').setAttribute('aria-expanded',String(!mobileNav.hidden));};
$('mobile-back-main-btn').onclick=()=>showMainPage(activeMainPage);
// The account menu is a body portal opened from the sidebar footer; using it keeps the sidebar open.
document.addEventListener('click',event=>{if(!mobileNav.hidden&&!event.target.closest('#mobile-nav-menu, #mobile-nav-toggle, #user-dropdown-menu'))closeMobileNav();});
document.addEventListener('keydown',event=>{if(event.key==='Escape')closeMobileNav();});
mobileLayout.addEventListener('change',syncLayout);syncLayout();
window.addEventListener('popstate',()=>location.reload());
$('chat-float-btn').onclick=()=>{widget.float();syncLayout();};
$('chat-toggle-btn').onclick=()=>{if(widget.floating){widget.open();syncLayout();return;}if(mobileLayout.matches){showMobileView('chat');}else{desktopChatVisible=!desktopChatVisible;syncLayout();}};

let browserResetInProgress=false;
async function hardRefresh(){
  if(browserResetInProgress)return;
  browserResetInProgress=true;
  $('header-refresh-browser').disabled=true;
  try { await purgeAllBrowserData(); } finally {
    const url=new URL(location.href);
    url.searchParams.set('_',String(Date.now()));
    location.replace(url);
  }
}
$('header-refresh-browser').onclick=()=>{void hardRefresh();};
function setupHeaderPullRefresh(){
  const indicator=$('ptr-indicator'),blue=$('ptr-blue'),red=$('ptr-red'),hint=$('ptr-hint');
  const fullDash=98,normalAt=.5,hardAt=.85;
  let pullZone=50,configLoading=false;
  // Keep the last valid preference on network/config errors; never interrupt a drag.
  async function refreshPullZone(){
    if(configLoading||document.hidden||document.body.classList.contains('app-restricted'))return;
    configLoading=true;
    try{
      const response=await fetch('/api/v3/auth/ui-config',{credentials:'same-origin',cache:'no-store'});
      if(!response.ok)return;
      const appUi=(await response.json()).app_ui;
      const value=appUi?.pull_to_refresh?.start_zone_px;
      if(typeof value==='number'&&Number.isFinite(value)&&value>=0)pullZone=value;
      applyMobileFullHeight(appUi?.mobile_mode?.full_height===true);
    }catch{}finally{configLoading=false;}
  }
  setInterval(refreshPullZone,5000);
  document.addEventListener('visibilitychange',refreshPullZone);
  window.addEventListener('v3-identity-changed',refreshPullZone);
  void refreshPullZone();
  let dragging=false,startY=0,stage=0;
  function end(){
    if(!dragging)return;
    const action=stage;
    dragging=false;startY=0;stage=0;
    if(!action){indicator.hidden=true;hint.textContent='';hint.classList.remove('show');return;}
    indicator.classList.add('loading');
    hint.textContent=action===2?'CLEARING BROWSER DATA':'REFRESHING';hint.classList.add('show');
    if(action===1)setTimeout(()=>location.reload(),120);
    if(action===2)void hardRefresh();
  }
  document.addEventListener('touchstart',event=>{
    if(!mobileLayout.matches||window.scrollY>5||indicator.classList.contains('loading')||!event.touches.length)return;
    const touch=event.touches[0];
    // Only the viewport top band can arm refresh; content pulls remain scrolling.
    if(touch.clientY<0||touch.clientY>pullZone)return;
    dragging=true;startY=touch.clientY;stage=0;
    indicator.hidden=false;indicator.style.height='0px';indicator.style.opacity='1';
    blue.setAttribute('stroke-dashoffset',String(fullDash));
    red.style.display='none';red.setAttribute('stroke-dashoffset',String(fullDash));
    hint.textContent='';hint.classList.remove('show');
  },{passive:true});
  document.addEventListener('touchmove',event=>{
    if(!dragging||!event.touches.length)return;
    // A second finger makes this a chat pinch-zoom, never a refresh.
    if(event.touches.length>1){dragging=false;stage=0;indicator.hidden=true;hint.classList.remove('show');return;}
    const raw=event.touches[0].clientY-startY;
    if(raw<=0){indicator.hidden=true;dragging=false;return;}
    const height=window.innerHeight,normal=height*normalAt,hard=height*hardAt;
    indicator.style.height=Math.min(raw*.4,height*.85)+'px';
    if(raw>=hard){
      stage=2;blue.setAttribute('stroke-dashoffset','0');red.style.display='';
      red.setAttribute('stroke-dashoffset',String(fullDash*(1-Math.min((raw-normal)/(hard-normal),1))));
      hint.textContent='HARD REFRESH';hint.classList.add('show');
    }else if(raw>=normal){
      stage=1;blue.setAttribute('stroke-dashoffset','0');red.style.display='none';
      hint.textContent='RELEASE TO REFRESH';hint.classList.add('show');
    }else{
      stage=0;blue.setAttribute('stroke-dashoffset',String(fullDash*(1-raw/normal)));
      red.style.display='none';hint.textContent='PULL TO REFRESH';hint.classList.add('show');
    }
    if(event.cancelable)event.preventDefault();
  },{passive:false});
  document.addEventListener('touchend',end,{passive:true});
  document.addEventListener('touchcancel',()=>{dragging=false;indicator.hidden=true;},{passive:true});
}
setupHeaderPullRefresh();
$('logout').onclick=guard(async()=>{await api('/auth/logout','POST',{});out();});
const systemTheme=matchMedia('(prefers-color-scheme: light)');
let themeChoice='system';
try{const saved=localStorage.getItem('v3-theme');if(['light','dark','system'].includes(saved))themeChoice=saved;}catch{}
// The account menu and the mobile sidebar footer each have a theme switch; both stay in sync.
const themeOptions=document.querySelectorAll('#user-dropdown-menu .theme-option, #mobile-nav-footer .theme-option');
function applyAccountTheme(){
  const light=themeChoice==='light'||themeChoice==='system'&&systemTheme.matches;
  document.body.classList.toggle('light-mode',light);
  // Mirrors --bg-0 (design-system.css) so OS status/nav bars match the app theme.
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content',light?'#ffffff':'#000000');
  for(const option of themeOptions){
    const active=option.dataset.theme===themeChoice;
    option.classList.toggle('active',active);option.setAttribute('aria-pressed',String(active));
  }
}
// Cross-fade the whole page into the new theme; unsupported browsers and reduced motion switch instantly.
function fadeAccountTheme(){
  if(!document.startViewTransition||matchMedia('(prefers-reduced-motion: reduce)').matches){applyAccountTheme();return;}
  document.startViewTransition(applyAccountTheme);
}
for(const option of themeOptions)option.onclick=()=>{
  themeChoice=option.dataset.theme;
  try{localStorage.setItem('v3-theme',themeChoice);}catch{}
  fadeAccountTheme();
};
systemTheme.addEventListener('change',()=>{if(themeChoice==='system')fadeAccountTheme();});
applyAccountTheme();
$('shutoff').onclick=guard(async()=>{await api('/control','PUT',{shutoff:!state.shutoff,client_request_id:crypto.randomUUID()});await refresh();});
$('connect').onclick=guard(async()=>{await api('/provider/login','POST',{});await refresh();$('device').scrollIntoView({block:'nearest'});});
$('disconnect').onclick=guard(async()=>{await api('/provider/logout','POST',{});await refresh();});
$('model-form').onsubmit=guard(async()=>{agentModel=(await api('/agents/'+agentId+'/model','PUT',{model:$('model').value.trim()})).model;await refresh();});
$('policy-form').onsubmit=guard(async()=>{const rules=Object.fromEntries([...$('rules').querySelectorAll('select')].map(node=>[node.name,node.value]));await api('/policy','PUT',{rules});await refresh();});
api('/auth/setup-status').then(status=>{if(!status.initialized){location.replace('/setup.html');return;}return boot();}).catch(error=>{if(error.status===401)out();note(error.message);window.__boot?.reveal();});

// App-shell ownership includes the header account portal; chat shares its gesture registry.
import './shared/header-feedback.js';
