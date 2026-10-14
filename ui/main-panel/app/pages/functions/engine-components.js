'use strict';
// @code-index {"version":1,"feature":"engine-component-control-feedback","aliases":["engine component buttons"],"purpose":"Engine component buttons delegate feedback to the App page owner. Config: ui/vibrate.json -> enabled, duration_ms. Revision-fenced component authorization remains in the owning request handler.","span_lines":65,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
// Engine controls withdraw functionality; package editing/publication stays broker-owned.
export function mountEngineComponents(host) {
 const section=document.createElement('section');section.className='app-engine-components';
 // Own button styling here so controls remain readable without another page's CSS.
 const styles=document.createElement('style');styles.textContent=`
 .app-engine-components .app-engine-button { color:var(--fg-1,#eee);background:var(--bg-2,#282828);border:1px solid var(--border-strong,var(--border,#777));border-radius:8px;padding:8px 12px;min-height:44px;font:inherit;cursor:pointer; }
 .app-engine-components .app-engine-button:hover:not(:disabled) { background:var(--bg-elev,var(--bg-2,#282828)); }
 .app-engine-components .app-engine-button:focus-visible { outline:2px solid var(--fg-1,#eee);outline-offset:2px; }
 .app-engine-components .app-engine-button:disabled { opacity:.55;cursor:default; }
 `;section.append(styles);
 const heading=document.createElement('h3');heading.textContent='Engine components';section.append(heading);
 const note=document.createElement('p');note.textContent='Disable a required component to pause affected work. Restoring it resumes only work paused for that component. Install, replace, or remove packages through the build broker.';section.append(note);
 const list=document.createElement('div'),status=document.createElement('p');status.setAttribute('role','status');section.append(list,status);
 const refresh=document.createElement('button');refresh.type='button';refresh.className='app-engine-button';refresh.textContent='Refresh components';refresh.style.webkitTapHighlightColor='transparent';section.append(refresh);host.append(section);
 let snapshot=null,busy=false;
 // App's page owner supplies touch feedback: ui/vibrate.json -> enabled, duration_ms.
 /** Authenticate administrator reads and revision-fenced writes through the owning API. */
 async function request(method='GET',component,body) {
  const response=await fetch('/api/v3/auth/me',{credentials:'same-origin',cache:'no-store'});
  const me=await response.json();if(!response.ok||!me.is_admin)throw Error('Administrator access required');
  const result=await fetch('/api/v3/admin/engine-components'+(component?'/'+encodeURIComponent(component):''),{
   method,credentials:'same-origin',cache:'no-store',headers:{'Content-Type':'application/json',...(method==='GET'?{}:{'x-csrf-token':me.csrf_token})},
   ...(body?{body:JSON.stringify(body)}:{})});
  const data=await result.json().catch(()=>({}));if(!result.ok)throw Error(data.error||data.message||'Component request failed');return data;
 }
 /** Present host-validated availability; rendering never grants or invokes component authority. */
 function render(data) {
  snapshot=data;list.replaceChildren();
  for(const component of data.components) {
   const row=document.createElement('div'),label=document.createElement('p');label.textContent=component.id+' · '+component.group+' · '+(component.mode||'not installed')+' · '+(component.available?'Available':'Unavailable');
   row.append(label);
   if(component.implementation_version){const version=document.createElement('p');version.textContent='Implementation '+component.implementation_version+' · interface '+component.interface_version;row.append(version);}
   if(component.runtime_requirements?.length){const dependencies=document.createElement('p');dependencies.textContent='Requires: '+component.runtime_requirements.join(', ');row.append(dependencies);}
   const button=document.createElement('button');button.type='button';button.className='app-engine-button';button.style.webkitTapHighlightColor='transparent';
   button.textContent=component.enabled?'Disable component':'Enable component';button.disabled=component.installed===false&&!component.enabled;
   button.onclick=async()=>{
    if(busy||button.disabled)return;busy=true;button.disabled=true;refresh.disabled=true;status.textContent='Updating component…';
    try{const next=await request('PUT',component.id,{expected_revision:snapshot.revision,expected_catalog_revision:snapshot.catalog_revision,enabled:!component.enabled});
     if(!host.isConnected)return;render(next);status.textContent=component.enabled?'Component disabled. Affected work pauses at its next boundary.':'Component enabled. Eligible paused work resumes on the next scheduler tick.';}
    catch(error){if(host.isConnected)status.textContent=error.message;}
    finally{busy=false;if(host.isConnected){refresh.disabled=false;button.disabled=false;}}
   };row.append(button);list.append(row);
  }
  const summary=document.createElement('p');summary.textContent=data.affected_work+' run(s) paused for engine components.';list.append(summary);
  const adoption=document.createElement('p');adoption.textContent='Catalog adoption: API '+(data.adoption.api?.revision===data.catalog_revision?'current':'pending')+', loop '+(data.adoption.loop?.revision===data.catalog_revision?'current':'pending')+'.';list.append(adoption);
 }
 /** Refresh an attached surface without overlapping requests or creating background polling. */
 async function load() {
  if(busy)return;busy=true;refresh.disabled=true;status.textContent='Loading engine components…';
  try{const data=await request();if(host.isConnected){render(data);status.textContent='Component status loaded.';}}
  catch(error){if(host.isConnected)status.textContent=error.message;}
  finally{busy=false;if(host.isConnected)refresh.disabled=false;}
 }
 refresh.onclick=()=>{if(!busy)load();};load();
}
