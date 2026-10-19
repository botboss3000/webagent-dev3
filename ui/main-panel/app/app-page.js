'use strict';
import { renderAgentDirectory } from './pages/config/page.js';
import { mountDashboard } from './pages/dashboard/page.js';
import { loadAppPages } from './pages/catalog.js';
import { createPagePreferences } from './pages/preferences.js';
import { mountUsers } from './pages/users/page.js';
import { mountPolicies } from './pages/user-policies/page.js';
import { mountPageAccess } from './pages/page-access/page.js';
import { mountAppSettings } from './pages/settings/page.js';
import { mountFunctions } from './pages/functions/page.js';
import { mountModules } from './pages/modules/page.js';
import { mountAppModels } from './pages/models/page.js';
import { wirePageTabDrag } from '../../shared/page-tabs.js';
import { startView as startFiles, stopView as stopFiles, resetExplorerForRemount } from './pages/files/page.js';

export async function start(mount) {
  const generation=(mount._appGeneration||0)+1;mount._appGeneration=generation;
  mount._appModelsDispose?.();mount._appModelsDispose=null;
  mount._appFilesDispose?.();mount._appFilesDispose=null;
  const active=()=>mount._appGeneration===generation;
  mount.replaceChildren();
  if (!mount._identityListener) {
    mount._identityListener=()=>start(mount).catch(error=>{mount.textContent=error.message;});
    window.addEventListener('v3-identity-changed',mount._identityListener);
  }
  const catalogPages=await loadAppPages();
  if(!active())return;
  const meResponse=await fetch('/api/v3/auth/me',{credentials:'same-origin'});
  const me=meResponse.ok?await meResponse.json():null;
  if(!active())return;
  const visiblePages=catalogPages.filter(page=>!page.admin||me?.is_admin);
  const configPage=visiblePages.find(page=>page.type==='directory');
  const pages=visiblePages.filter(page=>page.type==='app_configuration');
  if (!Array.isArray(pages) || pages.some(page => !/^[a-z][a-z0-9-]*$/.test(page.id) || !page.title)) throw Error('Invalid App catalog');
  mount.innerHTML = '<div class="app-page-framework"><div class="agent-card active agent-card-tabs-only"><div class="agent-card-tabs-wrap"><div class="agent-card-tabs" role="tablist" aria-label="App pages"></div></div></div><div class="agent-detail-panel"><div class="agent-detail-content"><div id="app-config-page" class="agent-detail-body"></div><section id="app-files-page" class="agent-detail-body" hidden><h2>File Manager</h2><p role="status">Checking V3 File Manager…</p></section></div></div></div>';
  for(const page of pages) if(!mount.querySelector('#'+page.panelId)){const panel=document.createElement('section');panel.id=page.panelId;panel.className='agent-detail-body';panel.hidden=true;mount.querySelector('.agent-detail-content').append(panel);}
  // Hidden administration pages must not hold up the shell or conversation.
  // Each selected panel mounts once per identity generation; stale model mounts
  // dispose themselves rather than attaching to a replacement account view.
  const panelMounts=new Map();
  const panelLoaders={dashboard:mountDashboard,modules:mountModules,
    functions:mountFunctions,settings:mountAppSettings,users:mountUsers,
    'user-policies':mountPolicies,'page-access':mountPageAccess,models:mountAppModels};
  function ensurePanel(id) {
    if(!me?.is_admin||!panelLoaders[id]||panelMounts.has(id))return;
    const descriptor=pages.find(page=>page.id===id);
    const panel=mount.querySelector('#'+descriptor.panelId);
    const pending=Promise.resolve().then(()=>active()?panelLoaders[id](panel):null).then(dispose=>{
      if(id==='models'&&typeof dispose==='function'){
        if(active())mount._appModelsDispose=dispose;else dispose();
      }
    }).catch(error=>{
      if(active()){panel.textContent=descriptor.title+' could not load: '+error.message;panelMounts.delete(id);}
    });
    panelMounts.set(id,pending);
  }
  const strip = mount.querySelector('[role=tablist]');
  const all = [configPage, ...pages];
  const status=document.createElement('p');status.setAttribute('role','status');status.className='app-preferences-status';mount.prepend(status);
  const request=async(path,method='GET',body)=>{
    const response=await fetch('/api/v3'+path,{method,credentials:'same-origin',cache:'no-store',headers:{'Content-Type':'application/json',...(method==='GET'?{}:{'x-csrf-token':me?.csrf_token})},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const data=await response.json().catch(()=>({}));
    if(!response.ok)throw Error(data.error||data.message||'Preference request failed ('+response.status+')');
    return data;
  };
  let order=['config'];
  const preferences=createPagePreferences({request,isCurrent:active,onError:message=>{if(active())status.textContent=message;}});
  try{order=await preferences.load();}catch(error){status.textContent='App favorites unavailable: '+error.message;}
  if(!active())return;
  const visibleOrder=ids=>{const result=ids.filter(id=>all.some(p=>p.id===id));if(!result.includes('config'))result.push('config');return result;};
  order=visibleOrder(order);
  let selected = new URL(location.href).searchParams.get('appView') || 'config';
  if (!all.some(p=>p.id===selected)) selected='config';
  const tabs = new Map();
  let filesMounted=false, filesLoading=false, filesPending=false;
  mount._appFilesDispose=()=>{ stopFiles(); resetExplorerForRemount(); window.__v3FilesCsrf=''; window.__v3FilesAdmin=false; };
  async function ensureFiles() {
    if (filesMounted) { await startFiles(); return; }
    if (filesLoading) { filesPending=true; return; }
    filesLoading=true;
    const panel=mount.querySelector('#app-files-page');
    try {
      if (!me?.is_admin) throw Error('File Manager requires administrator access');
      const probe=await fetch('/api/v3/file-manager',{
        method:'POST',credentials:'same-origin',
        headers:{'Content-Type':'application/json','x-csrf-token':me.csrf_token},
        body:JSON.stringify({action:'tree',path:''})
      });
      if (!probe.ok) throw Error('V3 File Manager backend is not ready ('+probe.status+')');
      const response=await fetch('/v3-ui/main-panel/app/pages/files/files.html',{credentials:'same-origin'});
      if (!response.ok) throw Error('File Manager view unavailable');
      const markup=await response.text();
      if (!active() || selected!=='files') return;
      panel.innerHTML=markup;
      window.__v3FilesCsrf=me.csrf_token;
      window.__v3FilesAdmin=true;
      resetExplorerForRemount();
      filesMounted=true;
      await startFiles();
      if (!active() || selected!=='files') stopFiles();
    } catch (error) {
      if (active() && selected==='files') {
        panel.replaceChildren();
        const note=document.createElement('p');
        note.setAttribute('role','status');
        note.textContent=error.message;
        panel.append(note);
      }
      filesMounted=false;
    } finally {
      filesLoading=false;
      if (filesPending) {
        filesPending=false;
        if (active() && selected==='files' && !filesMounted) void ensureFiles();
      }
    }
  }
  function show(id, route = true) {
    selected=id;
    for (const page of all) {
      mount.querySelector('#'+page.panelId).hidden=page.id!==id;
      const tab=tabs.get(page.id);
      tab.hidden=!order.includes(page.id)&&page.id!==id;
      tab.classList.toggle('ephemeral',!order.includes(page.id));
      tab.classList.toggle('active',page.id===id);
      tab.setAttribute('aria-selected',String(page.id===id));
      if (!tab.hidden&&!order.includes(page.id)) strip.append(tab);
    }
    if (route) { const url=new URL(location.href);url.searchParams.set('appView',id);if(url.href!==location.href)history.pushState({},'',url); }
    ensurePanel(id);
    if (id==='files') void ensureFiles(); else if (filesMounted) stopFiles();
  }
  function sync() {
    for (const id of order) strip.append(tabs.get(id));
    const host=mount.querySelector('#app-config-page');
    renderAgentDirectory(host,pages,id=>show(id),order,id=>{
      if(id==='dashboard'){status.textContent='Dashboard favorites need backend work.';return;}
      if(!preferences.ready()){status.textContent='App favorites are unavailable. Reload to retry.';return;}
      order=order.includes(id)?order.filter(item=>item!==id):[id,...order];
      if(selected===id&&!order.includes(id)) selected='config';
      save();
    });
    // The persisted favorite API has a fixed page allowlist. Keep this new
    // frontend-only page navigable while its unsupported save stays disabled.
    const dashboardStar=host.querySelector('[data-favorite-page="dashboard"]');
    if(dashboardStar){dashboardStar.disabled=true;dashboardStar.title='Backend work needed: dashboard favorites';dashboardStar.style.color='var(--danger,#ef4444)';}
    host.querySelector('.agent-config-landing').setAttribute('aria-label','App page directory');
    host.querySelector('.agent-config-settings-group-label').textContent='App pages';
    show(selected,false);
  }
  function save() {
    sync();show(selected);
    void preferences.save(order,all.map(page=>page.id)).then(saved=>{if(active()&&saved){order=visibleOrder(saved);sync();}});
  }
  for (const page of all) {
    const tab=document.createElement('button');tab.type='button';tab.id='app-tab-'+page.id;tab.textContent=page.title;
    tab.setAttribute('role','tab');tab.setAttribute('aria-controls',page.panelId);tab.onclick=()=>show(page.id);
    tabs.set(page.id,tab);strip.append(tab);
    const panel=mount.querySelector('#'+page.panelId);panel.setAttribute('role','tabpanel');panel.setAttribute('aria-labelledby',tab.id);
  }
  sync();
  wirePageTabDrag(strip,{getOrder:()=>order,save,prefix:'app-tab-'});
}
