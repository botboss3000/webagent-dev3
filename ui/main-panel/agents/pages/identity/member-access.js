import {createSettingsRow} from './settings-row.js';
import {requestTouchVibration} from '../../../../shared/vibrate.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
const select=(label,options,value)=>{const input=node('select','','ac-input');input.setAttribute('aria-label',label);for(const [id,text]of options){const option=node('option',text);option.value=id;input.append(option);}input.value=value;return input;};
const button=(label,action)=>{const el=node('button',label,'ac-btn');el.type='button';el.style.webkitTapHighlightColor='transparent';el.onclick=event=>{requestTouchVibration(event);action();};return el;};

/** Agent joining, privacy and ladder structure; Abilities owns capability editing. */
export function renderMemberAccess(host,agent,persist,request,{agentId,reload}) {
  const {body}=createSettingsRow(host,{id:'member-access',title:'Member access'});
  const status=node('p','','ac-hint');status.setAttribute('role','status');
  const settings=structuredClone(agent.member_access);
  body.append(node('p','App accounts provide login. This agent controls membership and capabilities.','ac-hint'));
  // V2 access policy as one choice over the two stored switches.
  const modes=[['anonymous','Anonymous','Anyone with the link can chat as a Visitor. Registered users join as Members.',true,true],
    ['registered','Registered','Users need an app account. Registered users join as Members; no visitor access.',false,true],
    ['authorized','Authorized','Users need an app account and an Operator\'s approval. They can request access from the agent.',false,false],
    ['visitors','Visitors only','Anyone can chat as a Visitor. Operators appoint Members.',true,false]];
  const policy=node('div','','members-policy'),choices=node('div','','members-policy-choices');policy.append(node('div','Access policy','members-policy-title'),choices);body.append(policy);
  const current=()=>modes.find(mode=>mode[3]===settings.visitor_access&&mode[4]===settings.app_login_members)[0];
  for(const [id,label,hint,visitor,members] of modes){
    const option=node('label','','members-policy-opt'),input=node('input'),text=node('div','','members-policy-opt-body');
    input.type='radio';input.name='access-policy-'+agentId;input.value=id;input.checked=current()===id;input.disabled=!agent.can_manage;option.classList.toggle('active',input.checked);
    text.append(node('div',label,'members-policy-opt-label'),node('div',hint,'members-policy-opt-hint'));option.append(input,text);choices.append(option);
    input.onchange=async event=>{requestTouchVibration(event);const before={...settings};settings.visitor_access=visitor;settings.app_login_members=members;
      for(const radio of choices.querySelectorAll('input'))radio.disabled=true;
      try{await persist({member_access:settings});status.textContent='Saved';}catch(error){Object.assign(settings,before);status.textContent=error.message;}
      finally{for(const radio of choices.querySelectorAll('input')){radio.checked=radio.value===current();radio.disabled=!agent.can_manage;radio.closest('label').classList.toggle('active',radio.checked);}}};
  }
  body.append(node('p','Operators can always appoint or block individual users on the Members page. Removing an assignment restores the policy\'s automatic access.','ac-hint'));
  const privacyLabel=node('label','Conversation privacy','ac-label');
  const privacy=select('Conversation privacy',[['private','Private'],['operator_reviewable','Operator-reviewable']],settings.conversation_privacy);
  privacy.disabled=!agent.can_manage;privacyLabel.append(privacy);body.append(privacyLabel,node('p','Applies to new conversations. Existing conversations keep their privacy mode. Reviewable conversations disclose that Operators can read and join.','ac-hint'));
  privacy.onchange=async()=>{privacy.disabled=true;const before=settings.conversation_privacy;settings.conversation_privacy=privacy.value;try{await persist({member_access:settings});status.textContent='Saved';}catch(error){settings.conversation_privacy=before;privacy.value=before;status.textContent=error.message;}finally{privacy.disabled=!agent.can_manage;}};
  body.append(node('h4','Agent member levels'),node('p','Highest to lowest. Each level is capped by the levels above it. Configure abilities on the Abilities page. Message limits count each user\'s messages to this agent over the last 24 hours and 30 days.','ac-hint'));
  const levels=agent.capability_levels;
  const save=async(next,reassignments={},confirm_expansion=false)=>{
    status.textContent='Saving…';try{await request('/agents/'+encodeURIComponent(agentId)+'/capability-ladder','PUT',{expected_revision:levels.revision,ladder:next,reassignments,confirm_expansion});await reload();}catch(error){status.textContent=error.message;}
  };
  for(const [index,id]of levels.order.entries()) {
    const level=levels.levels[id],row=node('div','','ac-row');row.style.padding='10px';row.append(node('strong',level.name));
    if(id!=='operator'){
      // Rolling-window message caps per user for this agent; Operators are never limited.
      const limits=node('div','','member-level-limits'),inputs={};
      for(const [key,label] of [['daily_turns','Messages per 24 hours'],['monthly_turns','Messages per 30 days']]){
        const field=node('label',label,'ac-label'),input=node('input','','ac-input');input.type='number';input.min='1';input.step='1';input.placeholder='No limit';
        input.value=level[key]??'';input.disabled=!agent.can_manage;input.setAttribute('aria-label',level.name+' '+label.toLowerCase());field.append(input);limits.append(field);inputs[key]=input;
      }
      if(agent.can_manage)limits.append(button('Save limits',()=>{const next=structuredClone(levels);
        for(const [key,input] of Object.entries(inputs)){const value=input.value.trim();
          if(value&&!(Number.isInteger(Number(value))&&Number(value)>=1)){status.textContent='Limits must be whole numbers of at least 1, or empty for no limit.';return;}
          if(value)next.levels[id][key]=Number(value);else delete next.levels[id][key];}
        void save(next);}));
      row.append(limits);
    }
    if(!['operator','member','visitor'].includes(id)&&agent.can_manage){
      const name=node('input','','ac-input');name.value=level.name;name.maxLength=100;name.setAttribute('aria-label','Level name');row.append(name);
      row.append(button('Rename',()=>{const next=structuredClone(levels);next.levels[id].name=name.value.trim();void save(next);}));
      for(const [label,direction]of [['Move up',-1],['Move down',1]]) {
        const move=button(label,()=>{const next=structuredClone(levels);[next.order[index],next.order[index+direction]]=[next.order[index+direction],next.order[index]];
          if(window.confirm('New order: '+next.order.map(key=>next.levels[key].name).join(' → ')+'. Effective access will follow the new ceilings. Confirm this change?'))void save(next,{},true);});
        move.disabled=index+direction<=0||index+direction>=levels.order.length-1;row.append(move);
      }
      row.append(button('Delete level',()=>{
        const deletion=node('div');const replacement=select('Replacement level',levels.order.filter(key=>key!==id&&key!=='operator').map(key=>[key,levels.levels[key].name]),'member');
        deletion.append(node('p','Reassign existing members to:'),replacement,button('Confirm deletion',()=>{const next=structuredClone(levels);delete next.levels[id];next.order=next.order.filter(key=>key!==id);void save(next,{[id]:replacement.value},true);}),button('Cancel',()=>deletion.remove()));row.append(deletion);
      }));
    }
    body.append(row);
  }
  if(agent.can_manage){
    const form=node('form');const name=node('input','','ac-input');name.placeholder='New member level name';name.required=true;name.maxLength=100;name.setAttribute('aria-label','New member level name');
    const before=select('Insert new level above',levels.order.filter(id=>id!=='operator').map(id=>[id,'Above '+levels.levels[id].name]),'member');
    const add=node('button','Add new member level','ac-btn');add.type='submit';
    form.append(name,before,add);form.onsubmit=event=>{event.preventDefault();const next=structuredClone(levels),id='level_'+crypto.randomUUID().replaceAll('-','');
      next.levels[id]={...structuredClone(next.levels[before.value]),id,name:name.value.trim(),revision:1};next.order.splice(next.order.indexOf(before.value),0,id);void save(next);};body.append(form);
    const link=node('a','Manage members');link.href='/app?tab=agents&agent='+encodeURIComponent(agentId)+'&agentView=members';body.append(link);
  }
  body.append(status);
}
