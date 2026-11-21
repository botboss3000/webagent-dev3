import {createSubpage, createSubpageList} from '../../../../shared/subpage-list/subpage-list.js';
import {renderMainChat} from './main-chat.js';
import {renderMemberAccess} from './member-access.js';
import { renderSharedChat } from './shared-chat.js';
import { renderEmbedSettings } from './embed.js';
import { renderClosing } from './closing.js';
import { renderVoiceOutput } from './voice.js';
import { renderManager } from './manager.js';
import { renderCompaction } from './compaction.js';
import { renderToolOutputPolicy } from './tool-output-policy.js';
import { renderChatCustomization } from './chat-customization.js';
import { createConfigList, createLongTextRow, createSelectRow, createTextRow } from '../../../../shared/config-rows/config-rows.js';

const ICONS = {
  profile: '<circle cx="12" cy="8" r="4"/><path d="M4.5 20.5a7.5 7.5 0 0 1 15 0"/>',
  'member-access': '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0"/><path d="M17 5.2a3.5 3.5 0 0 1 0 6.6"/><path d="M18.5 20a6.5 6.5 0 0 0-2.2-4.9"/>',
  'website-chat-widget': '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3a14 14 0 0 1 0 18a14 14 0 0 1 0-18"/>',
  'shared-conversations': '<path d="M20 14a2 2 0 0 1-2 2H8l-4 4V5a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2z"/>',
  'main-chat': '<path d="M21 11.5a8.4 8.4 0 0 1-8.5 8.4 8.9 8.9 0 0 1-3.8-.9L3 21l1.9-5.6a8.3 8.3 0 0 1-.9-3.9A8.4 8.4 0 0 1 12.5 3 8.4 8.4 0 0 1 21 11.5z"/>',
  'context-compaction': '<path d="m12 2 9 5-9 5-9-5z"/><path d="m3 12 9 5 9-5"/><path d="m3 17 9 5 9-5"/>',
  'duplicate-tool-results': '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M6 15H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v1"/>',
  'lifetime-budget-retention': '<ellipse cx="12" cy="5.5" rx="8" ry="3"/><path d="M4 5.5v13c0 1.7 3.6 3 8 3s8-1.3 8-3v-13"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>',
  'chat-modes': '<path d="M4 7h16"/><circle cx="9" cy="7" r="2.5"/><path d="M4 17h16"/><circle cx="15" cy="17" r="2.5"/>',
  'review-incoming-messages': '<path d="M22 12h-6l-2 3h-4l-2-3H2"/><path d="M5.4 5.1 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.4-6.9A2 2 0 0 0 16.8 4H7.2a2 2 0 0 0-1.8 1.1z"/>',
  'final-response': '<circle cx="12" cy="12" r="9"/><path d="m8.5 12.5 2.5 2.5 4.5-5"/>',
  'voice-output': '<path d="M11 5 6.5 9H3v6h3.5L11 19z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.5 5.5a9 9 0 0 1 0 13"/>',
  'chat-customization': '<path d="M4 7h16"/><circle cx="9" cy="7" r="2.5"/><path d="M4 17h16"/><circle cx="15" cy="17" r="2.5"/>',
};

const HINTS = {
  profile: 'Name, description and how the agent introduces itself.',
  'member-access': 'Who can join, conversation privacy and member levels.',
  'website-chat-widget': 'Embed this agent on your own website.',
  'shared-conversations': 'Inviting others and letting people ask for help.',
  'main-chat': 'Coordination settings and Main Notes skill overrides.',
  'context-compaction': 'Summarize older history before it fills the context.',
  'duplicate-tool-results': 'Reference repeated tool results instead of sending them again.',
  'lifetime-budget-retention': 'Experimental lifetime and size budgets for tool results.',
  'chat-modes': 'Default mode and the instructions each mode adds.',
  'review-incoming-messages': 'Screen visitors’ messages before the agent answers.',
  'final-response': 'Promote all messages, completion flag and statement.',
  'voice-output': 'Choose the voice that reads replies aloud, and dictation cleanup.',
  'chat-customization': 'How this agent\u2019s chats look and behave, per surface, by request or row.',
};

// List order, one-line description and icon tile for each Identity category.
const CATEGORIES = Object.keys(HINTS).map(id => ({id, hint: HINTS[id], icon: ICONS[id]}));

// Adapted V2 Identity fields; V3 transport, tenant permissions and revision fencing.
export async function renderIdentity(host, request, {agentId, onSaved = () => {}}) {
  const path = '/models/agents/' + encodeURIComponent(agentId) + '/identity';
  host.replaceChildren();
  let agent;
  try { agent = await request(path); }
  catch (error) {
    const message = document.createElement('p'); message.className = 'ac-hint';
    message.setAttribute('role', 'alert'); message.textContent = 'Identity unavailable: ' + error.message;
    host.append(message); return;
  }
  onSaved(agent);
  // A save from another tab, device or agent moves the revision this page holds. Each save sends
  // only its own fields, so on that conflict refresh the revision once and send it again.
  const put = async (patch) => {
    try { return await request(path,'PUT',{revision:agent.revision,...patch}) }
    catch(error) {
      if(error.status !== 409) throw error;
      agent.revision = (await request(path)).revision;
      return request(path,'PUT',{revision:agent.revision,...patch});
    }
  };
  // Sections build in the register's off-screen staging element, which stays in the document so
  // asynchronous sections still see themselves as connected.
  const register = createSubpageList(host, {label: 'Identity settings', categories: CATEGORIES});
  const stage = register.stage;
  // Serialize saves so concurrent fields share the latest revision.
  let queue = Promise.resolve();
  const persist = (patch) => {
    const task = queue.then(async()=>{ const saved=await put(patch); agent=saved; onSaved(saved); return saved; });
    queue=task.catch(()=>{}); return task;
  };
  // An unchanged value (compared once earlier saves land) sends nothing.
  const saveField = (field, value) => {
    const task = queue.then(async()=>{ if(value!==agent[field]){ agent=await put({[field]:value}); onSaved(agent); } return agent; });
    queue=task.catch(()=>{}); return task;
  };
  const profile = createSubpage(stage, {id:'profile', title:'Profile'});
  const content = profile.body;
  const card = document.createElement('div');
  card.className = 'identity-profile';
  card.innerHTML = '<span class="identity-avatar" aria-hidden="true"></span><span class="identity-profile-text"><span class="identity-profile-name"></span><span class="identity-profile-sub"></span></span>';
  card.querySelector('.identity-avatar').textContent = (agent.name || 'Agent').trim().split(/\s+/).map(word => word[0]).join('').slice(0, 2).toUpperCase();
  const profileName = card.querySelector('.identity-profile-name');
  profileName.textContent = agent.name || 'Unnamed agent';
  card.querySelector('.identity-profile-sub').textContent = 'Agent · ' + agentId;
  content.append(card);
  const profileList = createConfigList(content, {label:'Profile fields'});
  const nameRow = createTextRow(profileList, {title:'Name', value:agent.name||'', placeholder:'Name this agent…', disabled:!agent.can_manage,
    onSave: async value => {
      const name = value.trim();
      if(!name) throw Error('Name cannot be blank');
      await saveField('name', name);
      if(nameRow.getValue().trim() === name) nameRow.setValue(agent.name);
      profileName.textContent = agent.name || 'Unnamed agent';
    }});
  nameRow.control.dataset.field = 'name';
  const descriptionRow = createLongTextRow(profileList, {title:'Description', value:agent.description||'', placeholder:'Describe what this agent is for…',
    minHeight:80, disabled:!agent.can_manage, onSave: value => saveField('description', value)});
  descriptionRow.editor.area.dataset.field = 'description';
  renderMemberAccess(stage,agent,persist,request,{agentId,reload:()=>renderIdentity(host,request,{agentId,onSaved})});
  if(agent.can_manage)await renderEmbedSettings(stage,request,{agentId});
  renderSharedChat(stage, agent, persist);
  await renderMainChat(stage, request, {agentId,canManage:agent.can_manage});
  renderCompaction(stage, agent, persist, request);
  renderToolOutputPolicy(stage, agent, persist);
  const {body:modeBody}=createSubpage(stage,{id:'chat-modes',title:'Chat modes'});
  const hint = document.createElement('p'); hint.className = 'ac-hint';
  hint.textContent = 'Default mode applies to new conversations. Read-only modes cannot execute mutating tools. Each mode adds its own instruction before model calls; existing permissions and approvals still apply.'; modeBody.append(hint);
  let modes = structuredClone(agent.execution_modes);
  const choices = () => modes.filter(m=>!m._deleted&&!m._new).map(m=>[m.id,m.label]);
  const notice = document.createElement('p'); notice.className = 'ac-hint'; notice.setAttribute('role','status');
  const defaultRow = createSelectRow(createConfigList(modeBody,{label:'Default chat mode'}), {title:'Default mode', options:choices(), value:agent.default_execution_mode,
    disabled:!agent.can_manage, onSave: async value => { notice.textContent=''; await persist({default_execution_mode:value}); notice.textContent='Default mode saved'; }});
  const select = defaultRow.control; select.dataset.field = 'default_execution_mode';
  // Saved mode edits can rename or remove modes, so the choices are rebuilt from the saved list.
  const fill = () => { select.replaceChildren(...choices().map(([id,label])=>{ const option=document.createElement('option'); option.value=id; option.textContent=label; return option; })); defaultRow.setValue(agent.default_execution_mode); };
  modeBody.append(notice);
  const editor=document.createElement('div'); editor.style.cssText='display:flex;flex-direction:column;gap:8px;padding:10px 0';modeBody.append(editor);
  const renderModes=()=>{
    editor.replaceChildren();
    modes.forEach(mode=>{
      const card=document.createElement('details');card.dataset.modeId=mode.id;card.style.cssText='border:var(--border-width) solid var(--border);border-radius:8px;padding:8px 10px;background:var(--bg-1)';
      const summary=document.createElement('summary');summary.style.cssText='cursor:pointer;display:flex;gap:8px;font-size:12px;font-weight:650';
      const nameText=document.createElement('span');summary.append(nameText);card.append(summary);
      const fields=document.createElement('div');fields.style.cssText='display:flex;flex-direction:column;gap:8px;margin-top:10px';card.append(fields);
      const save=document.createElement('button');save.type='button';save.className='ac-btn ac-btn-ghost';save.textContent='SAVE CHANGES';save.disabled=!mode._new;save.dataset.modeCardSave='';
      const status=document.createElement('span');status.setAttribute('role','status');
      const updateSummary=()=>{nameText.textContent=(mode.label||mode.id)+(mode._deleted?' (will be removed)':'');};updateSummary();
      // The card saves every mode together with its own button, so its rows only collect edits.
      const list=createConfigList(fields,{label:(mode.label||mode.id)+' settings'}),rows=[];
      for(const [key,title] of [['label','Name'],['description','Footer description'],['prompt','Prompt injection'],['turn_label','Per-turn label'],['turn_guidance','Per-turn guidance']]){
        const long=key==='prompt'||key==='turn_guidance';
        const options={title,value:mode[key]||'',disabled:!agent.can_manage};
        if(key==='turn_label')options.maxLength=60;
        if(key==='turn_guidance')Object.assign(options,{maxLength:4000,hint:'Sent with every turn in this mode, after the per-turn label. Leave both empty to send nothing.',
          tip:'The per-turn label and this text are sent to the model with every turn while the session is in this mode, after the conversation history, and never saved in it. Use it for guidance the agent must follow on every turn, on top of the mode. Through a bridge (Claude Code, OpenCode), they are added with each user message, and again at the next step when they change.'});
        const row=long?createLongTextRow(list,options):createTextRow(list,options);
        (long?row.editor.area:row.control).dataset.modeField=key;
        row.control.addEventListener('input',()=>{mode[key]=row.getValue();save.disabled=false;status.textContent='Unsaved changes';updateSummary();});
        rows.push(row);
      }
      if(!mode.builtin&&agent.can_manage){const remove=document.createElement('button');remove.type='button';remove.className='ac-btn ac-btn-ghost';remove.textContent='Remove mode';remove.addEventListener('click',()=>{mode._deleted=true;rows.forEach(row=>row.setDisabled(true));save.disabled=false;remove.disabled=true;updateSummary();});fields.append(remove);}
      if(agent.can_manage){fields.append(save,status);save.addEventListener('click',async()=>{
        save.disabled=true;status.textContent='Saving…';
        const payload=modes.filter(m=>!m._deleted).map(({_deleted,_new,permission_policy,...m})=>m);
        try {await persist({execution_modes:payload});modes=structuredClone(agent.execution_modes);fill();renderModes();notice.textContent='Mode configurations saved';}
        catch(error){status.textContent='Save failed: '+error.message;save.disabled=false;}
      });}
      editor.append(card);
    });
    if(agent.can_manage){const add=document.createElement('button');add.type='button';add.className='ac-btn ac-btn-ghost';add.textContent='Add mode';add.addEventListener('click',()=>{let n=1;while(modes.some(m=>m.id==='custom-'+n))n++;modes.push({id:'custom-'+n,label:'New mode',description:'',prompt:'',turn_label:'Harness guidance:',turn_guidance:'',builtin:false,_new:true});renderModes();});editor.append(add);}
  };renderModes();
  renderManager(stage, agent, persist);

  renderClosing(stage, agent, persist);
  renderVoiceOutput(stage, agent, persist);
  renderChatCustomization(stage, request, {agentId, canManage: agent.can_manage});

  register.present();
}
