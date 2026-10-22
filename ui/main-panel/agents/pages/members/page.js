import {requestTouchVibration} from '../../../../shared/vibrate.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
/** Explicit assignments are distinct from automatic Member access and blocks. */
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
    host.append(node('p','Removing an assignment restores automatic Member or Visitor access. Block from agent overrides both. Counts are scoped to this agent.','ac-hint'));
    const sections=new Map();for(const label of ['Operators','Members and visitors','Blocked']){const section=node('section');section.append(node('h3',label));host.append(section);sections.set(label,section);}
    const append=member=>{
      const blocked=member.relationship==='revoked',owner=member.user_id===data.owner_id,operator=member.agent_operator===true;
      const section=sections.get(blocked?'Blocked':operator?'Operators':'Members and visitors');
      const row=node('div','','ac-row');row.style.padding='12px';row.append(node('strong',member.username||member.user_id),node('p',owner?'Owner · Operator':member.access_origin==='app_login'?'Automatic app-login membership':member.access_origin==='visitor'?'Visitor access':member.access_origin==='none'?'No current agent access':blocked?'Blocked':'Explicit assignment','ac-hint'));
      const select=choices(member.effective_level_id||member.capability_level_id);select.disabled=owner||blocked;row.append(select);
      select.onchange=()=>{if((select.value==='operator'||operator)&&!window.confirm('Change Operator appointment for '+(member.username||member.user_id)+'?')){select.value=member.capability_level_id;return;}
        void update({user_id:member.user_id,level_id:select.value,blocked:false,expected_revision:member.revision});};
      const action=(label,level_id,blocked)=>{const button=node('button',label,'ac-btn');button.type='button';button.disabled=owner;button.style.webkitTapHighlightColor='transparent';button.onclick=event=>{requestTouchVibration(event);if(window.confirm(label+' for '+(member.username||member.user_id)+'?'))void update({user_id:member.user_id,level_id,blocked,expected_revision:member.revision});};row.append(button);};
      if(blocked)action('Unblock',null,false);else{action('Remove assignment',null,false);action('Block from agent',null,true);}
      row.append(node('p','Sessions: '+(member.session_count??'Unavailable')+' · Messages: '+(member.message_count??'Unavailable'),'ac-hint'));section.append(row);
    };
    for(const member of data.members||[])append(member);
    if(data.next_offset!=null){const more=node('button','Load more members','ac-btn');more.type='button';let offset=data.next_offset;more.onclick=async()=>{more.disabled=true;try{const next=await request(path+'?offset='+offset);if(host._membersGeneration!==generation)return;for(const member of next.members||[])append(member);offset=next.next_offset;more.hidden=offset==null;}catch(error){statusText(error.message);}finally{more.disabled=false;}};host.append(more);}
    statusText('Members configuration ready.');
  }catch(error){status.textContent='Members unavailable: '+error.message;}
}
