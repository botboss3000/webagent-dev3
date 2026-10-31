import {requestTouchVibration} from '../../../../shared/vibrate.js';
import {_fmtTime} from '../sessions/js/adapter.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
/** Explicit assignments are distinct from automatic Member access and blocks. One V2-style table lists everyone. */
export async function renderMembers(host,request,{agentId}) {
  const generation=(host._membersGeneration||0)+1;host._membersGeneration=generation;
  const path='/agents/'+encodeURIComponent(agentId)+'/members';host.replaceChildren(node('h2','Members'));
  const status=node('p','Loading…','ac-hint');status.setAttribute('role','status');host.append(status);
  const skeleton=node('div','','members-skeleton');skeleton.setAttribute('aria-hidden','true');
  skeleton.innerHTML=['64px','78%','66%','84%','70%'].map((width,index)=>'<div class="mem-sk-row"><span class="mem-sk sk-shimmer'+(index?'':' mem-sk-head')+'" style="width:'+width+'"></span></div>').join('');host.append(skeleton);
  try {
    const data=await request(path);skeleton.remove();if(host._membersGeneration!==generation)return;
    const levels=data.capability_levels,statusText=message=>{status.textContent=message;};
    const choices=(current)=>{const select=node('select','','ac-input');select.setAttribute('aria-label','Agent member level');
      for(const id of levels.order){const option=node('option',levels.levels[id].name);option.value=id;select.append(option);}select.value=current;return select;};
    const update=async(body)=>{try{await request(path,'PUT',body);await renderMembers(host,request,{agentId});}catch(error){statusText(error.message);}};
    const form=node('form'),username=node('input','','ac-input'),level=choices('member'),add=node('button','Add member','ac-btn');
    username.placeholder='Exact app username';username.setAttribute('aria-label','Exact app username');username.required=true;add.type='submit';form.append(username,level,add);host.append(form);
    form.onsubmit=async event=>{event.preventDefault();if(level.value==='operator'&&!window.confirm('Appoint this user as an Agent Operator? Operators can configure this agent and appoint other Operators.'))return;add.disabled=true;
      try{const result=await request(path+'?username='+encodeURIComponent(username.value.trim()));await update({user_id:result.user.user_id,level_id:level.value,blocked:false,expected_revision:0});}catch(error){statusText(error.message);}finally{add.disabled=false;}};
    host.append(node('p','Removing an assignment restores automatic Member or Visitor access. Block from agent overrides both. Counts are scoped to this agent.','members-notice'));
    // Pending help requests (agent request_human or the chat's Request help), one row each above the members table.
    const help=node('div','','members-section'),helpHeader=node('div','','members-section-header'),helpCount=node('span','0','members-section-count');
    helpHeader.append(node('span','Help requests','members-section-title'),helpCount);help.append(helpHeader);host.append(help);
    const helpWrap=node('div','','members-table-wrap'),helpTable=node('table','','members-table'),helpBody=node('tbody'),helpHead=node('tr');
    for(const label of ['Conversation','Requested','Status',''])helpHead.append(node('th',label));
    helpTable.append(node('thead'),helpBody);helpTable.tHead.append(helpHead);helpWrap.append(helpTable);help.append(helpWrap);
    const helpEmpty=node('div','No pending help requests.','members-empty');help.append(helpEmpty);
    try{
      const shared=await request('/agents/'+encodeURIComponent(agentId)+'/shared-sessions');if(host._membersGeneration!==generation)return;
      for(const item of (shared.sessions||[]).filter(session=>session.help_pending)){
        const sid=encodeURIComponent(item.id),row=node('tr'),conversation=node('td'),status=node('td'),actions=node('td','','members-actions');
        const link=node('a',item.title||item.id);link.href='/app?session_id='+sid;conversation.append(link);
        status.append(node('span','Needs a human','members-status pending'));
        if(item.can_join){const join=node('button','Join','members-btn authorize');join.type='button';join.style.webkitTapHighlightColor='transparent';
          join.onclick=async event=>{requestTouchVibration(event);join.disabled=true;try{await request('/sessions/'+sid+'/join','POST',{client_request_id:crypto.randomUUID()});location.href='/app?session_id='+sid;}catch(error){statusText('Join failed: '+error.message);join.disabled=false;}};actions.append(join);}
        row.append(conversation,node('td',item.help_requested_at?new Date(item.help_requested_at*1000).toLocaleString():'—'),status,actions);helpBody.append(row);
      }
    }catch(error){helpEmpty.textContent='Help requests unavailable: '+error.message;}
    helpCount.textContent=String(helpBody.rows.length);helpWrap.hidden=!helpBody.rows.length;helpEmpty.hidden=helpBody.rows.length>0;
    // Pending access requests (Authorized policy): approve as Member or decline.
    const requests=data.access_requests||[];
    if(requests.length){
      const box=node('div','','members-section'),boxHeader=node('div','','members-section-header'),boxWrap=node('div','','members-table-wrap'),boxTable=node('table','','members-table'),boxBody=node('tbody'),boxHead=node('tr');
      boxHeader.append(node('span','Access requests','members-section-title'),node('span',String(requests.length),'members-section-count'));
      for(const label of ['User','Requested',''])boxHead.append(node('th',label));boxTable.append(node('thead'),boxBody);boxTable.tHead.append(boxHead);boxWrap.append(boxTable);box.append(boxHeader,boxWrap);host.append(box);
      for(const item of requests){
        const name=item.username||item.user_id,shown=item.display_name||name,row=node('tr'),user=node('td'),actions=node('td','','members-actions');
        user.append(node('div',shown,'members-user-name'));if(name!==shown)user.append(node('div',name,'members-user-sub'));
        const act=(text,tone,run)=>{const button=node('button',text,'members-btn '+tone);button.type='button';button.style.webkitTapHighlightColor='transparent';button.onclick=async event=>{requestTouchVibration(event);button.disabled=true;await run();button.disabled=false;};actions.append(button);};
        act('Authorize','authorize',()=>update({user_id:item.user_id,level_id:'member',blocked:false,expected_revision:item.revision}));
        act('Deny & block','restrict',async()=>{if(!window.confirm('Decline and block '+name+' from this agent? They cannot request again until unblocked.'))return;await update({user_id:item.user_id,level_id:null,blocked:true,expected_revision:item.revision});});
        act('Deny','restrict',async()=>{if(!window.confirm('Decline the access request from '+name+'?'))return;try{await request('/agents/'+encodeURIComponent(agentId)+'/access-requests/'+encodeURIComponent(item.user_id),'DELETE');await renderMembers(host,request,{agentId});}catch(error){statusText(error.message);}});
        row.append(user,node('td',new Date(item.requested_at*1000).toLocaleString()),actions);boxBody.append(row);
      }
    }
    // Read-only V2-style level summary; levels are edited on the Identity page.
    const levelPanel=node('details','','members-levels'),levelSummary=node('summary','Member levels '),levelBody=node('div','','members-levels-body');
    levelSummary.append(node('span',String(levels.order.length),'members-levels-count'));levelPanel.append(levelSummary,levelBody);host.append(levelPanel);
    const loaded=[];
    const renderLevels=()=>{const counts=new Map();for(const member of loaded)if(member.relationship!=='revoked'&&member.access_origin!=='none'){const id=member.effective_level_id||member.capability_level_id;counts.set(id,(counts.get(id)||0)+1);}
      levelBody.replaceChildren(...levels.order.map(id=>{const card=node('article','','members-level-card'),meta=node('div','','members-level-meta'),total=counts.get(id)||0;
        meta.append(node('span',total+' member'+(total===1?'':'s')));card.append(node('div',levels.levels[id].name,'members-level-name'),meta);return card;}),
        node('p','Edit levels and their permissions on the Identity page.','ac-hint'));};
    const section=node('div','','members-section'),header=node('div','','members-section-header'),count=node('span','0','members-section-count');
    // Removed assignments and disabled accounts stay hidden unless asked for.
    let showInactive=false;const inactiveToggle=node('button','','members-btn');inactiveToggle.type='button';inactiveToggle.style.webkitTapHighlightColor='transparent';inactiveToggle.style.marginLeft='auto';
    header.append(node('span','All members','members-section-title'),count,inactiveToggle);section.append(header);host.append(section);
    const wrap=node('div','','members-table-wrap'),table=node('table','','members-table'),tbody=node('tbody');
    const head=node('tr');for(const [label,cls] of [['User'],['Level'],['Access'],['Channel'],['Sessions','members-num'],['Messages','members-num'],['Last login'],['']])head.append(node('th',label,cls||''));
    table.append(node('thead'),tbody);table.tHead.append(head);wrap.append(table);section.append(wrap);
    const empty=node('div','No members have been assigned to this agent yet.','members-empty');section.append(empty);
    const blockedOf=member=>member.relationship==='revoked';
    // Owner, Operators, other access, then Blocked: the order the former sections used.
    const rank=member=>blockedOf(member)?3:member.user_id===data.owner_id?0:member.agent_operator===true?1:2;
    const access=member=>{const blocked=blockedOf(member),owner=member.user_id===data.owner_id;
      if(blocked)return ['Blocked','blocked'];if(owner)return ['Owner · Operator','ok'];if(member.agent_operator===true)return ['Operator','ok'];
      return member.access_origin==='app_login'?['App login','ok']:member.access_origin==='visitor'?['Visitor','pending']:member.access_origin==='none'?['No access','pending']:['Assigned','ok'];};
    const append=member=>{
      const blocked=blockedOf(member),owner=member.user_id===data.owner_id,operator=member.agent_operator===true,name=member.username||member.user_id,shown=member.display_name||name;
      const row=node('tr');row.dataset.inactive=String(!blocked&&!owner&&(member.access_origin==='none'||['disabled','suspended'].includes(member.account_status)));row.hidden=row.dataset.inactive==='true'&&!showInactive;
      const user=node('td'),levelCell=node('td'),accessCell=node('td'),actions=node('td','','members-actions');
      user.append(node('div',shown,'members-user-name'));if(name!==shown)user.append(node('div',name,'members-user-sub'));
      const select=choices(member.effective_level_id||member.capability_level_id);select.disabled=owner||blocked;select.style.width='auto';levelCell.append(select);
      select.onchange=()=>{if((select.value==='operator'||operator)&&!window.confirm('Change Operator appointment for '+name+'?')){select.value=member.capability_level_id;return;}
        void update({user_id:member.user_id,level_id:select.value,blocked:false,expected_revision:member.revision});};
      const [label,tone]=access(member);accessCell.append(node('span',label,'members-status '+tone));
      const action=(text,level_id,block,tone)=>{const button=node('button',text,'members-btn '+tone);button.type='button';button.disabled=owner;button.style.webkitTapHighlightColor='transparent';button.onclick=event=>{requestTouchVibration(event);if(window.confirm(text+' for '+name+'?'))void update({user_id:member.user_id,level_id,blocked:block,expected_revision:member.revision});};actions.append(button);};
      if(blocked)action('Unblock',null,false,'authorize');else{action('Remove',null,false,'');action('Block',null,true,'restrict');}
      row.append(user,levelCell,accessCell,node('td',(member.source_channel||'web').replace(/_/g,' ')),node('td',String(member.session_count??'—'),'members-num'),node('td',String(member.message_count??'—'),'members-num'),node('td',_fmtTime(member.last_login_at)),actions);tbody.append(row);
    };
    const show=(members,more)=>{loaded.push(...members);renderLevels();for(const member of [...members].sort((a,b)=>rank(a)-rank(b)))append(member);paintInactive(more);};
    const paintInactive=more=>{const rows=[...tbody.rows],inactive=rows.filter(row=>row.dataset.inactive==='true').length,shown=rows.filter(row=>!row.hidden).length;
      count.textContent=(rows.length-inactive)+(more?'+':'');inactiveToggle.hidden=!inactive;inactiveToggle.textContent=showInactive?'Hide inactive':'Show inactive ('+inactive+')';
      empty.hidden=shown>0;wrap.hidden=!shown;};
    inactiveToggle.onclick=event=>{requestTouchVibration(event);showInactive=!showInactive;for(const row of tbody.rows)row.hidden=row.dataset.inactive==='true'&&!showInactive;paintInactive(count.textContent.endsWith('+'));};
    show(data.members||[],data.next_offset!=null);
    if(data.next_offset!=null){const more=node('button','Load more members','ac-btn');more.type='button';let offset=data.next_offset;more.onclick=async()=>{more.disabled=true;try{const next=await request(path+'?offset='+offset);if(host._membersGeneration!==generation)return;offset=next.next_offset;show(next.members||[],offset!=null);more.hidden=offset==null;}catch(error){statusText(error.message);}finally{more.disabled=false;}};host.append(more);}
    statusText('Members configuration ready.');
  }catch(error){skeleton.remove();status.textContent='Members unavailable: '+error.message;}
}
