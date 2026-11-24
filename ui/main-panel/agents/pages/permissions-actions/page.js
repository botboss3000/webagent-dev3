import {requestTouchVibration} from '../../../../shared/vibrate/vibrate.js';
import {mountActions} from '../../../../shared/actions/actions-list.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
const when=ms=>ms?new Date(ms).toLocaleString([], {dateStyle:'short',timeStyle:'short'}):'';
/**
 * Permissions & Actions: the caller's own saved approvals for this agent (from "Remember this
 * approval" on approval cards) and their recorded actions. Text-only DOM: tool arguments are
 * untrusted. Every change carries the revision the page was drawn from.
 */
export async function renderPermissionsActions(host,request,{agentId}) {
  const generation=(host._permissionsGeneration||0)+1;host._permissionsGeneration=generation;
  const current=()=>host._permissionsGeneration===generation;
  const rerender=()=>renderPermissionsActions(host,request,{agentId});
  host.replaceChildren(node('h2','Permissions & Actions'),node('p','Your saved approvals for this agent, the actions it recorded for you and how often it used each tool. Only you see these.','ac-hint'));
  const approvals=section('Saved approvals');host.append(approvals.box);
  const actions=section('Actions');host.append(actions.box);
  actions.box.append(node('p','Your recorded actions with this agent: publishing, pushing and other tool calls that must not happen twice. Mark an uncertain one done or failed after checking what really happened.','ac-hint'));
  const actionsBox=node('div');actions.box.append(actionsBox);
  void mountActions(actionsBox,request,'/actions?agent='+encodeURIComponent(agentId),{showSession:true,resolvePath:id=>'/actions/'+encodeURIComponent(id)+'/resolve'}).then(list=>{if(current())actions.count.textContent=String(list.length);});
  const use=section('Tool use');host.append(use.box);
  void renderToolUse(use,request,agentId,current);
  await renderApprovals(approvals,request,agentId,current,rerender);
}
/** Per-tool counters, added to as each of your runs with this agent ends. */
async function renderToolUse(part,request,agentId,current){
  const status=node('p','Loading…','ac-hint');status.setAttribute('role','status');part.box.append(status);
  let data;
  try{data=await request('/agents/'+encodeURIComponent(agentId)+'/tool-use');}catch(error){if(current())status.textContent='Tool use unavailable: '+error.message;return;}
  if(!current())return;
  const tools=data.tools||[];
  part.count.textContent=String(tools.length);
  status.textContent='Counted as each of your runs ends: calls the agent made, approvals it asked you for, and calls a saved approval let through.';
  if(!tools.length){part.box.append(node('div','No tool use counted yet.','members-empty'));return;}
  const list=node('ul','','permissions-tool-use');
  for(const tool of tools){
    const item=node('li');
    item.append(node('span',tool.tool,'artifacts-script-name'),node('span',[tool.calls+(tool.calls===1?' call':' calls'),tool.asked?'asked '+tool.asked:'',tool.saved?'saved approval '+tool.saved:'',tool.last_used_at?'last '+new Date(tool.last_used_at*1000).toLocaleString([], {dateStyle:'short',timeStyle:'short'}):''].filter(Boolean).join(' · '),'artifacts-meta'));
    list.append(item);
  }
  part.box.append(list);
}
async function renderApprovals(part,request,agentId,current,rerender){
  const status=node('p','Loading…','ac-hint');status.setAttribute('role','status');part.box.append(status);
  const path='/agents/'+encodeURIComponent(agentId)+'/saved-approvals';
  let data;
  try{data=await request(path);}catch(error){if(current())status.textContent='Saved approvals unavailable: '+error.message;return;}
  if(!current())return;
  const entries=data.entries||[];
  part.count.textContent=String(entries.length);
  status.textContent=!data.enabled?'Saved approvals are switched off for this app: these are kept but not used.':'Approved by you with "Remember this approval". A matching call runs without asking; a lapsed one asks again until you approve it again here.';
  if(!entries.length){part.box.append(node('div','No saved approvals yet. Tick "Remember this approval" on an approval card to add one.','members-empty'));return;}
  const send=(id,body)=>request(path+'/'+encodeURIComponent(id),'POST',{expected_revision:data.revision,...body});
  const act=(label,tone,run)=>{const button=node('button',label,'members-btn '+tone);button.type='button';button.style.webkitTapHighlightColor='transparent';
    button.onclick=async event=>{requestTouchVibration(event);button.disabled=true;try{if(await run()===null){button.disabled=false;return;}await rerender();}catch(error){status.textContent=error.message;button.disabled=false;}};return button;};
  for(const entry of [...entries].sort((a,b)=>(b.created_ms||0)-(a.created_ms||0))){
    const card=node('details','','artifacts-script'),summary=node('summary');
    const scope=(entry.match==='any'?'Any values':'These values')+' · '+(entry.reach==='agent'?'This agent, all my chats':'One chat');
    summary.append(node('span',entry.tool,'artifacts-script-name'),node('span',(entry.lapsed?'Lapsed · ':'')+scope,'artifacts-script-description'));card.append(summary);
    if(entry.lapsed){card.open=true;card.append(node('p','The tool\'s permission changed since you saved this, so it is not used. Approve it again to use it under the permission it has now.','ac-hint'));}
    card.append(node('p',[entry.reach==='chat'&&entry.session_id?'Chat '+String(entry.session_id).slice(0,8):'','Saved '+when(entry.created_ms),entry.via==='page'?'approved again here':''].filter(Boolean).join(' · '),'artifacts-meta'));
    if(entry.match==='values'){
      card.append(node('h4','Values'),node('pre',JSON.stringify(entry.fixed??{},null,2),'artifacts-args'));
      if(Array.isArray(entry.free)&&entry.free.length)card.append(node('p','Free: '+entry.free.join(', '),'artifacts-meta'));
    }
    // Change match and reach together; "these values" is offered only while the entry still holds its values.
    const match=picker('Match',[['values','These values',entry.match==='values'],['any','Any values',true]],entry.match);
    const reach=picker('Where',[['chat','This chat',true],['agent','This agent, all my chats',data.agent_wide||entry.reach==='agent']],entry.reach);
    card.append(match.box,reach.box);
    const buttons=node('div','','artifacts-actions');
    buttons.append(act('Save change','authorize',()=>{if(match.value()===entry.match&&reach.value()===entry.reach)return null;return send(entry.id,{action:'change',match:match.value(),reach:reach.value()});}));
    if(entry.lapsed)buttons.append(act('Approve again','authorize',()=>{if(!window.confirm('Approve "'+entry.tool+'" again under its permission now? Matching calls will run without asking.'))return null;return send(entry.id,{action:'reapprove'});}));
    buttons.append(act('Revoke','restrict',()=>{if(!window.confirm('Revoke this saved approval for "'+entry.tool+'"? Matching calls will ask again.'))return null;return send(entry.id,{action:'revoke'});}));
    card.append(buttons);part.box.append(card);
  }
}
/** A labelled select; options are [value, label, enabled]. */
function picker(label,options,value){
  const box=node('label','','permissions-picker'),select=node('select','','ac-input');
  box.append(node('span',label,'permissions-picker-label'),select);
  for(const [optionValue,text,enabled] of options){const option=node('option',text);option.value=optionValue;option.disabled=!enabled;select.append(option);}
  select.value=value;
  return {box,value:()=>select.value};
}
function section(title){const box=node('section','','members-section artifacts-section'),header=node('div','','members-section-header'),count=node('span','','members-section-count');header.append(node('span',title,'members-section-title'),count);box.append(header);return {box,count};}
