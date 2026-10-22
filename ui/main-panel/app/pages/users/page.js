import { el, request, button, openAccountManager } from './support.js';
export async function mountUsers(host){
  const status=el('p','members-loading','Loading users…');status.setAttribute('role','status');host.replaceChildren(status);
  try{
    const [data,tiers,me]=await Promise.all([request('/admin/access/accounts'),request('/admin/access/tiers'),fetch('/api/v3/auth/me').then(r=>r.json())]);
    host.replaceChildren();host.append(button('Refresh',()=>mountUsers(host)),status);status.textContent='';
    for(const [title,filter] of [['Admins',a=>a.role==='administrator'],['Users',a=>a.role==='user'],['Anonymous',a=>a.role==='guest']]){
      const section=el('section','members-section');section.append(el('div','members-policy-title',title));
      const rows=data.accounts.filter(filter);if(!rows.length){section.append(el('p','members-empty','No '+title.toLowerCase()+' yet.'));host.append(section);continue;}
      const wrap=el('div','members-table-wrap'),table=el('table','members-table');
      const head=el('thead'),tr=el('tr');for(const label of ['User','Experience','Sessions','Messages','Last login','Status','Actions'])tr.append(el('th','',label));head.append(tr);table.append(head);const tbody=el('tbody');table.append(tbody);
      for(const account of rows){
        const row=el('tr'),name=el('td');name.append(el('div','members-user-name',account.profile?.display_name||account.username),el('div','members-user-sub',account.username));row.append(name);
        const tierCell=el('td'),select=el('select','ac-input ac-config-sel');select.setAttribute('aria-label','Tier for '+account.username);
        for(const tier of tiers.tiers){const option=el('option','',tier.name||tier.id);option.value=tier.id;select.append(option);}select.value=account.tier_id;select.disabled=account.user_id==='admin'||account.role==='guest';tierCell.append(select);row.append(tierCell);
        row.append(el('td','members-num',String(account.session_count??'—')),el('td','members-num',String(account.message_count??'—')),el('td','',account.profile?.last_login_at?new Date(account.profile.last_login_at*1000).toLocaleString():'—'),el('td','members-status',account.status));
        const actions=el('td','members-actions');row.append(actions);
        async function save(patch,control){control.disabled=true;status.textContent='Saving…';try{await request('/admin/access/accounts/'+encodeURIComponent(account.user_id),'PUT',{expected_revision:account.access_revision,status:account.status,role:account.role,tier_id:account.tier_id,...patch});await mountUsers(host);}catch(e){status.textContent=e.message;control.disabled=false;select.value=account.tier_id;}}
        select.onchange=()=>{const appoint=select.value==='operator'&&account.role!=='administrator';if(appoint&&!window.confirm('Appoint '+account.username+' as an App Administrator?')){select.value=account.tier_id;return;}save({tier_id:select.value,...(appoint?{role:'administrator'}:{})},select);};
        if(account.user_id!==me.user_id&&account.user_id!=='admin'){
          const access=button(account.status==='active'?'Restrict':'Authorize',()=>save({status:account.status==='active'?'suspended':'active'},access));actions.append(access);
          if(account.role!=='guest'){const role=button(account.role==='administrator'?'Remove admin':'Make admin',()=>{if(window.confirm('Change App Administrator appointment for '+account.username+'?'))save({role:account.role==='administrator'?'user':'administrator',tier_id:account.role==='administrator'?'basic':'operator'},role);});actions.append(role);}
        }
        const manage=button('Profile & sign-ins',()=>openAccountManager(account,host,status));actions.append(manage);
        tbody.append(row);
      }
      wrap.append(table);section.append(wrap);host.append(section);
    }
  }catch(e){status.textContent='Users unavailable: '+e.message;}
}
