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
  const toggle=(title,key)=>{
    body.append(node('h4',title));const label=node('label','','ac-label'),input=node('input');input.type='checkbox';input.checked=settings[key];input.disabled=!agent.can_manage;
    label.append(input,document.createTextNode(' '+(key==='visitor_access'?'Allow visitor access':'Accept app registered users as members')));body.append(label);
    input.onchange=async()=>{input.disabled=true;const before=settings[key];settings[key]=input.checked;try{await persist({member_access:settings});status.textContent='Saved';}catch(error){settings[key]=before;input.checked=before;status.textContent=error.message;}finally{input.disabled=!agent.can_manage;}};
  };
  toggle('Visitor','visitor_access');toggle('Member','app_login_members');
  body.append(node('p','When automatic membership is off, Operators appoint members. Removing an assignment restores automatic access; blocking denies all access.','ac-hint'));
  const privacyLabel=node('label','Conversation privacy','ac-label');
  const privacy=select('Conversation privacy',[['private','Private'],['operator_reviewable','Operator-reviewable']],settings.conversation_privacy);
  privacy.disabled=!agent.can_manage;privacyLabel.append(privacy);body.append(privacyLabel,node('p','Applies to new conversations. Existing conversations keep their privacy mode. Reviewable conversations disclose that Operators can read and join.','ac-hint'));
  privacy.onchange=async()=>{privacy.disabled=true;const before=settings.conversation_privacy;settings.conversation_privacy=privacy.value;try{await persist({member_access:settings});status.textContent='Saved';}catch(error){settings.conversation_privacy=before;privacy.value=before;status.textContent=error.message;}finally{privacy.disabled=!agent.can_manage;}};
  body.append(node('h4','Agent member levels'),node('p','Highest to lowest. Each level is capped by the levels above it. Configure abilities on the Abilities page.','ac-hint'));
  const levels=agent.capability_levels;
  const save=async(next,reassignments={},confirm_expansion=false)=>{
    status.textContent='Saving…';try{await request('/agents/'+encodeURIComponent(agentId)+'/capability-ladder','PUT',{expected_revision:levels.revision,ladder:next,reassignments,confirm_expansion});await reload();}catch(error){status.textContent=error.message;}
  };
  for(const [index,id]of levels.order.entries()) {
    const level=levels.levels[id],row=node('div','','ac-row');row.style.padding='10px';row.append(node('strong',level.name));
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
