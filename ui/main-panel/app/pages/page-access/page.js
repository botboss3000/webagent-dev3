import { el, request } from '../users/support.js';
// V2 users/policies/page-access.js: audience rows separated into page scopes.
export async function mountPageAccess(host){
  const status=el('p','members-loading','Loading page audiences…');status.setAttribute('role','status');host.replaceChildren(status);
  try{
    const [policy,catalog,app]=await Promise.all([request('/admin/access/page-policy'),fetch('/api/v3/pages/catalog').then(r=>r.json()),fetch(new URL('../catalog.json',import.meta.url)).then(r=>r.json())]);
    host.replaceChildren(status);status.textContent='';
    for(const [scope,title,pages] of [['main','Main panel pages',catalog.main],['app','App pages',app.pages]]){
      const section=el('section','members-policy');section.append(el('div','members-policy-title',title));const list=el('div','ac-list');section.append(list);host.append(section);
      for(const page of pages){const row=el('label','ac-ability-row'),name=el('span','ac-ability-name',page.label||page.title),select=el('select','ac-input ac-config-sel');select.setAttribute('aria-label','Audience for '+(page.label||page.title));
        for(const [value,text] of [['everyone','Everyone'],['registered','Registered users'],['admin','Admins only']]){const option=el('option','',text);option.value=value;select.append(option);}
        const locked=scope==='app'&&['settings','users','user-policies','page-access','models'].includes(page.id);select.value=locked?'admin':policy.pages?.[scope]?.[page.id]||'everyone';select.disabled=locked;row.append(name,select);list.append(row);
        select.onchange=async()=>{const previous=policy.pages?.[scope]?.[page.id]||'everyone';select.disabled=true;status.textContent='Saving…';const next=structuredClone(policy.pages);next[scope]??={};next[scope][page.id]=select.value;try{await request('/admin/access/page-policy','PUT',{expected_revision:policy.revision,pages:next});await mountPageAccess(host);}catch(error){status.textContent=error.message;select.value=previous;select.disabled=false;}};
      }
    }
  }catch(error){status.textContent='Page audiences unavailable: '+error.message;}
}

