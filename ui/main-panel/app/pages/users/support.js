import { mountUsers } from './page.js';
"use strict";
// Adapted from V2 users/users.js: members roster sections, table and access radios.
// Billing and browser-cache controls deliberately excluded; all requests are V3.
export const el=(tag,cls='',text='')=>{const n=document.createElement(tag);n.className=cls;n.textContent=text;return n;};
export async function request(path,method='GET',body){
  const auth=await fetch('/api/v3/auth/me',{credentials:'same-origin',cache:'no-store'});
  if(!auth.ok)throw Error('Sign in required');const me=await auth.json();
  if(!me.is_admin)throw Error('Administrator access required');
  const r=await fetch('/api/v3'+path,{method,credentials:'same-origin',cache:'no-store',headers:{'Content-Type':'application/json',...(method==='GET'?{}:{'x-csrf-token':me.csrf_token})},...(body===undefined?{}:{body:JSON.stringify(body)})});
  const data=await r.json().catch(()=>({}));if(!r.ok)throw Error(data.error||data.message||'Request failed ('+r.status+')');return data;
}
export function button(text,action){const b=el('button','members-btn',text);b.type='button';b.onclick=action;return b;}
export async function openAccountManager(account,host,status){
  host.querySelector('[data-account-manager]')?.remove();
  const panel=el('section','members-policy');panel.dataset.accountManager=account.user_id;panel.append(el('div','members-policy-title','Manage '+account.username));
  const form=el('form','v3-user-form'),username=el('input','ac-input'),display=el('input','ac-input');username.value=account.username;display.value=account.profile?.display_name||account.username;
  for(const [title,input] of [['Username',username],['Display name',display]]){const label=el('label','',title);input.required=true;input.maxLength=128;label.append(input);form.append(label);}
  const save=el('button','members-btn','Save profile');save.type='submit';form.append(save);panel.append(form,button('Close',()=>panel.remove()));host.append(panel);
  form.onsubmit=async event=>{event.preventDefault();save.disabled=true;try{await request('/admin/access/accounts/'+encodeURIComponent(account.user_id)+'/profile','PUT',{username:username.value,display_name:display.value});await mountUsers(host);}catch(error){status.textContent=error.message;save.disabled=false;}};
  const sessions=el('div','members-loading','Loading sign-ins…');panel.append(sessions);
  try{const data=await request('/admin/access/accounts/'+encodeURIComponent(account.user_id)+'/sessions');sessions.replaceChildren();if(!data.sessions.length)sessions.textContent='No active sign-ins.';
    for(const session of data.sessions){const row=el('div','ac-ability-row');row.append(el('span','ac-ability-name',session.user_agent||'Browser session'));const revoke=button('Revoke',async()=>{revoke.disabled=true;try{await request('/admin/access/accounts/'+encodeURIComponent(account.user_id)+'/sessions/'+encodeURIComponent(session.id),'DELETE');row.remove();}catch(error){status.textContent=error.message;revoke.disabled=false;}});row.append(revoke);sessions.append(row);}
  }catch(error){sessions.textContent=error.message;}
  username.focus();
}
