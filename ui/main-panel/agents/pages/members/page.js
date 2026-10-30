import {requestTouchVibration} from '../../../../shared/vibrate.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
/** Explicit assignments are distinct from automatic Member access and blocks. One V2-style table lists everyone. */
export async function renderMembers(host,request,{agentId}) {
  const generation=(host._membersGeneration||0)+1;host._membersGeneration=generation;
  const path='/agents/'+encodeURIComponent(agentId)+'/members';host.replaceChildren(node('h2','Members'));
  const status=node('p','Loading…','ac-hint');status.setAttribute('role','status');host.append(status);
  try {
    const data=await request(path);if(host._membersGeneration!==generation)return;
    const levels=data.capability_levels,statusText=message=>{status.textContent=message;};
    const choices=(current)=>{const select=node('select','','ac-input');select.setAttribute('aria-label','Agent member level');
      for(const id of levels.order){const option=node('option',levels.levels[id].name);option.value=id;select.append(option);}select.value=current;return select;};
    const update=async(body)=>{try{await request(path,'PUT',body);await renderMembers(host,request,{agentId});}catch(error){statusText(error.message);}};
    const form=node('form'),username=node('input','','ac-input'),level=choices('member'),add=node('button','Add member','ac-btn');
    username.placeholder='Exact app username';username.setAttribute('aria-label','Exact app username');username.required=true;add.type='submit';form.append(username,level,add);host.append(form);
    form.onsubmit=async event=>{event.preventDefault();if(level.value==='operator'&&!window.confirm('Appoint this user as an Agent Operator? Operators can configure this agent and appoint other Operators.'))return;add.disabled=true;
      try{const result=await request(path+'?username='+encodeURIComponent(username.value.trim()));await update({user_id:result.user.user_id,level_id:level.value,blocked:false,expected_revision:0});}catch(error){statusText(error.message);}finally{add.disabled=false;}};
    host.append(node('p','Removing an assignment restores automatic Member or Visitor access. Block from agent overrides both. Counts are scoped to this agent.','members-notice'));
    const section=node('div','','members-section'),header=node('div','','members-section-header'),count=node('span','0','members-section-count');
    header.append(node('span','All members','members-section-title'),count);section.append(header);host.append(section);
    const wrap=node('div','','members-table-wrap'),table=node('table','','members-table'),tbody=node('tbody');
    const head=node('tr');for(const [label,cls] of [['User'],['Level'],['Access'],['Sessions','members-num'],['Messages','members-num'],['']])head.append(node('th',label,cls||''));
    table.append(node('thead'),tbody);table.tHead.append(head);wrap.append(table);section.append(wrap);
    const empty=node('div','No members have been assigned to this agent yet.','members-empty');section.append(empty);
    const blockedOf=member=>member.relationship==='revoked';
    // Owner, Operators, other access, then Blocked: the order the former sections used.
    const rank=member=>blockedOf(member)?3:member.user_id===data.owner_id?0:member.agent_operator===true?1:2;
    const access=member=>{const blocked=blockedOf(member),owner=member.user_id===data.owner_id;
      if(blocked)return ['Blocked','blocked'];if(owner)return ['Owner · Operator','ok'];if(member.agent_operator===true)return ['Operator','ok'];
      return member.access_origin==='app_login'?['App login','ok']:member.access_origin==='visitor'?['Visitor','pending']:member.access_origin==='none'?['No access','pending']:['Assigned','ok'];};
    const append=member=>{
      const blocked=blockedOf(member),owner=member.user_id===data.owner_id,operator=member.agent_operator===true,name=member.username||member.user_id;
      const row=node('tr'),user=node('td'),levelCell=node('td'),accessCell=node('td'),actions=node('td','','members-actions');
      user.append(node('div',name,'members-user-name'));if(member.username&&member.username!==member.user_id)user.append(node('div',member.user_id,'members-user-sub'));
      const select=choices(member.effective_level_id||member.capability_level_id);select.disabled=owner||blocked;select.style.width='auto';levelCell.append(select);
      select.onchange=()=>{if((select.value==='operator'||operator)&&!window.confirm('Change Operator appointment for '+name+'?')){select.value=member.capability_level_id;return;}
        void update({user_id:member.user_id,level_id:select.value,blocked:false,expected_revision:member.revision});};
      const [label,tone]=access(member);accessCell.append(node('span',label,'members-status '+tone));
      const action=(text,level_id,block,tone)=>{const button=node('button',text,'members-btn '+tone);button.type='button';button.disabled=owner;button.style.webkitTapHighlightColor='transparent';button.onclick=event=>{requestTouchVibration(event);if(window.confirm(text+' for '+name+'?'))void update({user_id:member.user_id,level_id,blocked:block,expected_revision:member.revision});};actions.append(button);};
      if(blocked)action('Unblock',null,false,'authorize');else{action('Remove',null,false,'');action('Block',null,true,'restrict');}
      row.append(user,levelCell,accessCell,node('td',String(member.session_count??'—'),'members-num'),node('td',String(member.message_count??'—'),'members-num'),actions);tbody.append(row);
    };
    const show=(members,more)=>{for(const member of [...members].sort((a,b)=>rank(a)-rank(b)))append(member);count.textContent=tbody.rows.length+(more?'+':'');empty.hidden=tbody.rows.length>0;wrap.hidden=!tbody.rows.length;};
    show(data.members||[],data.next_offset!=null);
    if(data.next_offset!=null){const more=node('button','Load more members','ac-btn');more.type='button';let offset=data.next_offset;more.onclick=async()=>{more.disabled=true;try{const next=await request(path+'?offset='+offset);if(host._membersGeneration!==generation)return;offset=next.next_offset;show(next.members||[],offset!=null);more.hidden=offset==null;}catch(error){statusText(error.message);}finally{more.disabled=false;}};host.append(more);}
    statusText('Members configuration ready.');
  }catch(error){status.textContent='Members unavailable: '+error.message;}
}
