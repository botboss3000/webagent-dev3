import {requestTouchVibration} from '../../../../shared/vibrate/vibrate.js';
import {createMainNotes} from '../../../../chat/elements/main-notes/index.js';
import {mountActions} from '../../../../shared/actions/actions-list.js';
const node=(tag,text='',cls='')=>{const el=document.createElement(tag);el.textContent=text;el.className=cls;return el;};
/** Artifacts: what an agent keeps across conversations for this user — Main Notes, saved scripts and UI elements. Text-only DOM; script and element content is untrusted. */
export async function renderArtifacts(host,request,{agentId}) {
  const generation=(host._artifactsGeneration||0)+1;host._artifactsGeneration=generation;
  host._artifactsNotes?.destroy();host._artifactsNotes=null;
  host.replaceChildren(node('h2','Artifacts'),node('p','What this agent keeps across conversations: your Main Notes, your saved scripts and your UI elements. Agents propose saved scripts, and nothing is saved until you approve it here. UI elements follow the mode of each chat: in Ask mode you approve each save in the chat.','ac-hint'));
  const notes=section('Main Notes');host.append(notes.box);
  const controller=createMainNotes({request,agentId});controller.element.open=true;notes.box.append(controller.element);host._artifactsNotes=controller;
  const elements=section('UI elements');host.append(elements.box);
  void renderElements(elements,request,agentId,()=>host._artifactsGeneration===generation,()=>renderArtifacts(host,request,{agentId}));
  const actions=section('Actions');host.append(actions.box);
  actions.box.append(node('p','Your recorded actions with this agent: publishing, pushing and other tool calls that must not happen twice. Mark an uncertain one done or failed after checking what really happened.','ac-hint'));
  const actionsBox=node('div');actions.box.append(actionsBox);
  void mountActions(actionsBox,request,'/actions?agent='+encodeURIComponent(agentId),{showSession:true,resolvePath:id=>'/actions/'+encodeURIComponent(id)+'/resolve'}).then(list=>{if(host._artifactsGeneration===generation)actions.count.textContent=String(list.length);});
  const scripts=section('Saved scripts');host.append(scripts.box);
  const status=node('p','Loading…','ac-hint');status.setAttribute('role','status');scripts.box.append(status);
  const path='/agents/'+encodeURIComponent(agentId)+'/saved-scripts';
  let data;
  try{data=await request(path);}catch(error){if(host._artifactsGeneration===generation)status.textContent='Saved scripts unavailable: '+error.message;return;}
  if(host._artifactsGeneration!==generation)return;
  const rerender=()=>renderArtifacts(host,request,{agentId});
  // Each action carries the revision this view was drawn from, so a stale page cannot overwrite a newer decision.
  const act=(label,tone,run)=>{const button=node('button',label,'members-btn '+tone);button.type='button';button.style.webkitTapHighlightColor='transparent';
    button.onclick=async event=>{requestTouchVibration(event);button.disabled=true;try{if(await run()===null){button.disabled=false;return;}await rerender();}catch(error){status.textContent=error.message;button.disabled=false;}};return button;};
  const proposals=Object.values(data.proposals||{}).sort((a,b)=>(a.created_at||0)-(b.created_at||0));
  const saved=Object.entries(data.scripts||{}).sort(([a],[b])=>a.localeCompare(b));
  status.textContent=saved.length+' of '+data.max_saved+' saved';
  scripts.count.textContent=String(saved.length);
  if(proposals.length){
    scripts.box.append(node('h3','Awaiting your approval','artifacts-subhead'));
    for(const proposal of proposals){
      const replaces=data.scripts?.[proposal.script.name];
      const card=scriptCard(proposal.script,replaces?'Replaces your saved script of the same name':'New saved script');card.open=true;
      card.append(node('p','Source conversation: '+proposal.origin_session_id,'artifacts-meta'),node('p','Approving saves exactly these steps. They still meet the agent\'s current permissions each time the script runs.','ac-hint'));
      const actions=node('div','','artifacts-actions');
      actions.append(act('Approve','authorize',()=>request(path+'/proposals/'+encodeURIComponent(proposal.id),'POST',{expected_revision:data.revision,approve:true})),
        act('Reject','restrict',()=>request(path+'/proposals/'+encodeURIComponent(proposal.id),'POST',{expected_revision:data.revision,approve:false})));
      card.append(actions);scripts.box.append(card);
    }
  }
  if(!saved.length)scripts.box.append(node('div','No saved scripts yet. When an agent finds a sequence worth reusing, it proposes it here.','members-empty'));
  for(const [name,entry] of saved){
    const card=scriptCard(entry.script,entry.saved_at?'Saved '+new Date(entry.saved_at*1000).toLocaleString():'');
    const actions=node('div','','artifacts-actions');
    actions.append(act('Delete','restrict',async()=>{if(!window.confirm('Delete the saved script "'+name+'"? Agents can no longer run it. This cannot be undone.'))return null;return request(path+'/scripts/'+encodeURIComponent(name),'DELETE',{expected_revision:data.revision});}));
    card.append(actions);scripts.box.append(card);
  }
}
/** UI elements: the agent saves them with sandbox_save (approved in the chat in Ask mode); this lists and deletes them. */
async function renderElements(part,request,agentId,current,rerender){
  const status=node('p','Loading…','ac-hint');status.setAttribute('role','status');part.box.append(status);
  const path='/agents/'+encodeURIComponent(agentId)+'/ui-elements';
  let data;
  try{data=await request(path);}catch(error){if(current())status.textContent='UI elements unavailable: '+error.message;return;}
  if(!current())return;
  const act=(label,tone,run)=>{const button=node('button',label,'members-btn '+tone);button.type='button';button.style.webkitTapHighlightColor='transparent';
    button.onclick=async event=>{requestTouchVibration(event);button.disabled=true;try{if(await run()===null){button.disabled=false;return;}await rerender();}catch(error){status.textContent=error.message;button.disabled=false;}};return button;};
  const mine=data.mine||[],shared=data.shared||[];
  status.textContent=mine.length+' of '+data.max_elements+' saved'+(shared.length?' · '+shared.length+' shared':'');
  part.count.textContent=String(mine.length+shared.length);
  const card=(element,note)=>{const box=node('details','','artifacts-script'),summary=node('summary');
    summary.append(node('span',element.title||element.name,'artifacts-script-name'),node('span',element.description||'','artifacts-script-description'));box.append(summary);
    box.append(node('p',[element.name,element.revision?'revision '+element.revision:'',note].filter(Boolean).join(' · '),'artifacts-meta'));return box;};
  if(!mine.length&&!shared.length)part.box.append(node('div','No UI elements yet. When an agent builds a page worth reusing, it saves it here; in Ask mode you approve that in the chat.','members-empty'));
  const list=(items,scope,note,canDelete)=>{for(const element of items){
    const box=card(element,element.saved_at?note+' '+new Date(element.saved_at*1000).toLocaleString():note);
    if(canDelete){const actions=node('div','','artifacts-actions');
      actions.append(act('Delete','restrict',async()=>{if(!window.confirm('Delete the UI element "'+element.name+'"? Cards that show it stop working. This cannot be undone.'))return null;return request(path+'/'+scope+'/'+encodeURIComponent(element.name),'DELETE');}));box.append(actions);}
    part.box.append(box);}};
  list(mine,'mine','Saved',true);
  list(shared,'shared','Shared with every user of this agent ·',data.can_manage===true);
}
function section(title){const box=node('section','','members-section artifacts-section'),header=node('div','','members-section-header'),count=node('span','','members-section-count');header.append(node('span',title,'members-section-title'),count);box.append(header);return {box,count};}
/** One script: name and description in the summary; params and each step's tool and arguments inside. */
function scriptCard(script,note){
  const card=node('details','','artifacts-script'),summary=node('summary');
  summary.append(node('span',script.name,'artifacts-script-name'),node('span',script.description,'artifacts-script-description'));card.append(summary);
  if(note)card.append(node('p',note,'artifacts-meta'));
  const params=(script.params||[]).map(p=>typeof p==='string'?{name:p}:p);
  if(params.length){card.append(node('h4','Params'));const list=node('ul','','artifacts-params');for(const p of params){const item=node('li');item.append(node('code',p.name));if(p.description)item.append(document.createTextNode(' — '+p.description));list.append(item);}card.append(list);}
  card.append(node('h4','Steps ('+script.steps.length+')'));const steps=node('ol','','artifacts-steps');
  for(const step of script.steps){const item=node('li');item.append(node('code',step.tool),node('pre',JSON.stringify(step.arguments??{},null,2),'artifacts-args'));steps.append(item);}
  card.append(steps);return card;
}
