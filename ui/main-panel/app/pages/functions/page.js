'use strict';
import { runtimeHelp, renderRuntimeReference } from './runtime-help.js';
export function mountFunctions(host) {
 const fields=[
 ['max_active_runs','Maximum concurrent runs',0,16],
 ['max_unresolved_runs','Maximum unresolved runs per account',1,200],
 ['poll_interval_ms','Queue polling interval (milliseconds)',10,60000],
 ['recovery_interval_ms','Recovery interval (milliseconds)',1000,3600000],
 ['max_parallel_calls','Parallel tool calls per batch',1,4]];
 host.innerHTML='<h2>Functions</h2><details class="app-runtime"><summary><span>Concurrency limit</span><strong data-limit>Loading…</strong></summary><form></form></details><p role="status">Loading configuration…</p>';
 const form=host.querySelector('form'),status=host.querySelector('[role=status]');
 for(const [key,label,min,max] of fields){
 if(key==='max_unresolved_runs'){const h=document.createElement('h3');h.textContent='More configuration';form.append(h);}
 const lab=document.createElement('label');lab.textContent=label;
 const input=document.createElement('input');input.name=key;input.type='number';input.min=min;input.max=max;input.step='1';input.required=key!=='max_active_runs';lab.append(input);form.append(lab);
 const help=runtimeHelp['scheduler.'+key]||runtimeHelp['tools.'+key];
 const description=document.createElement('p');description.id='runtime-help-'+key;description.textContent=help.purpose+' Used by: '+help.used+'. Takes effect: '+help.effect+'.';input.setAttribute('aria-describedby',description.id);form.append(description);
 if(key==='max_active_runs')input.placeholder='Unlimited';
 }
 const note=document.createElement('p');note.textContent='Save updates the configuration. Restart the loop for concurrency, polling or recovery changes. Queue-limit changes also require an API restart. Tool batch changes apply on the next call. Existing runs are retained.';form.append(note);
 const reference=document.createElement('details');reference.className='app-runtime-reference';reference.innerHTML='<summary>All configuration settings</summary><div></div>';host.append(reference);
 const button=document.createElement('button');button.type='submit';button.className='members-btn';button.textContent='Save configuration';button.disabled=true;form.append(button);
 let digest='',busy=false;
 async function request(method='GET',settings){
 const auth=await fetch('/api/v3/auth/me',{credentials:'same-origin',cache:'no-store'});
 if(!auth.ok)throw Error('Sign in required');
 const me=await auth.json();if(!me.is_admin)throw Error('Administrator access required');
 const response=await fetch('/api/v3/admin/loop-runtime',{method,credentials:'same-origin',cache:'no-store',
 headers:{'Content-Type':'application/json',...(method==='GET'?{}:{'x-csrf-token':me.csrf_token})},
 ...(settings?{body:JSON.stringify({expected_digest:digest,settings})}:{})});
 const data=await response.json().catch(()=>({}));
 if(!response.ok)throw Error(data.error||data.message||'Request failed ('+response.status+')');
 return data;
 }
 function render(data){
 digest=data.digest;renderRuntimeReference(reference.querySelector('div'),data.config);
 for(const input of form.querySelectorAll('input'))input.value=String((input.name==='max_parallel_calls'?data.config.tools:data.config.scheduler)[input.name]);
 const value=data.config.scheduler.max_active_runs;host.querySelector('[data-limit]').textContent=value===0?'Unlimited':String(value);
 }
 request().then(data=>{if(!host.isConnected)return;render(data);button.disabled=false;status.textContent='Saved settings. Scheduler changes require a loop restart.';}).catch(error=>{if(host.isConnected)status.textContent=error.message;});
 form.onsubmit=async event=>{
 event.preventDefault();if(busy||!digest)return;busy=true;button.disabled=true;
 const settings={};for(const input of form.querySelectorAll('input'))settings[input.name]=Number(input.value);
 status.textContent='Saving…';
 try{const data=await request('PUT',settings);if(!host.isConnected)return;render(data);status.textContent='Saved. Restart the loop to apply scheduler settings.';}
 catch(error){if(host.isConnected)status.textContent=error.message;}
 finally{busy=false;if(host.isConnected)button.disabled=false;}
 };
}
