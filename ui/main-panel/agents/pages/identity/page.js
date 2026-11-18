import {createSettingsRow, createSettingsRegister} from '../../../../shared/settings-register/settings-register.js';
import {renderMainChat} from './main-chat.js';
import {renderMemberAccess} from './member-access.js';

'use strict';
import { renderSharedChat } from './shared-chat.js';
import { renderEmbedSettings } from './embed.js';
import { renderClosing } from './closing.js';
import { renderVoiceOutput } from './voice.js';
import { renderManager } from './manager.js';
import { renderCompaction } from './compaction.js';
import { renderToolOutputPolicy } from './tool-output-policy.js';
import { tipBadge } from '../../../../shared/models/field-tip.js';

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
  'voice-output': 'Choose the voice that reads replies aloud.',
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
  const register = createSettingsRegister(host, {label: 'Identity settings', categories: CATEGORIES});
  const stage = register.stage;
  // Serialize fields so concurrent name/description saves share the latest revision.
  let queue = Promise.resolve();
  const profile = createSettingsRow(stage, {id:'profile', title:'Profile'});
  const content = profile.body;
  const card = document.createElement('div');
  card.className = 'identity-profile';
  card.innerHTML = '<span class="identity-avatar" aria-hidden="true"></span><span class="identity-profile-text"><span class="identity-profile-name"></span><span class="identity-profile-sub"></span></span>';
  card.querySelector('.identity-avatar').textContent = (agent.name || 'Agent').trim().split(/\s+/).map(word => word[0]).join('').slice(0, 2).toUpperCase();
  const profileName = card.querySelector('.identity-profile-name');
  profileName.textContent = agent.name || 'Unnamed agent';
  card.querySelector('.identity-profile-sub').textContent = 'Agent · ' + agentId;
  content.append(card);
  for (const [field, label, placeholder] of [['name','Name','Name this agent…'],['description','Description','Describe what this agent is for…']]) {
    const wrap = document.createElement('div'); wrap.className = 'ac-cfg-field';
    const labelEl = document.createElement('label'); labelEl.className = 'ac-label'; labelEl.textContent = label;
    const indicator = document.createElement('span'); indicator.className = 'ac-cfg-ind';
    indicator.setAttribute('role','status'); indicator.setAttribute('aria-live','polite'); labelEl.append(indicator);
    const input = document.createElement(field === 'description' ? 'textarea' : 'input');
    input.id = 'identity-' + field; labelEl.htmlFor = input.id;
    if(field === 'description') input.rows = 3; else input.type = 'text';
    input.className = 'ac-input'; input.dataset.field = field;
    input.value = agent[field] || ''; input.placeholder = placeholder; input.readOnly = !agent.can_manage;
    wrap.append(labelEl,input); content.append(wrap);
    let timer;
    const status = (text) => { indicator.textContent = text === 'Saved' ? '✓' : text === 'Saving…' ? '…' : '!'; indicator.title = text; indicator.setAttribute('aria-label',text); };
    const save = () => {
      clearTimeout(timer);
      const value = field === 'name' ? input.value.trim() : input.value;
      if(field === 'name' && !value) { input.value = agent.name; status('Name cannot be blank'); return; }
      queue = queue.then(async () => {
        if(value === agent[field]) return;
        status('Saving…');
        try {
          agent = await put({[field]:value});
          if(input.value.trim() === value && field === 'name') input.value = agent.name;
          if(field === 'name') profileName.textContent = agent.name || 'Unnamed agent';
          status('Saved'); onSaved(agent);
        } catch(error) { status('Save failed: ' + error.message); }
      });
    };
    if(!input.readOnly) {
      input.addEventListener('input',()=>{clearTimeout(timer); timer=setTimeout(save,700);});
      input.addEventListener('blur',save);
    }
  }
  const persist = (patch) => {
    const task = queue.then(async()=>{ const saved=await put(patch); agent=saved; onSaved(saved); return saved; });
    queue=task.catch(()=>{}); return task;
  };
  renderMemberAccess(stage,agent,persist,request,{agentId,reload:()=>renderIdentity(host,request,{agentId,onSaved})});
  if(agent.can_manage)await renderEmbedSettings(stage,request,{agentId});
  renderSharedChat(stage, agent, persist);
  await renderMainChat(stage, request, {agentId,canManage:agent.can_manage});
  renderCompaction(stage, agent, persist, request);
  renderToolOutputPolicy(stage, agent, persist);
  const {body:modeBody}=createSettingsRow(stage,{id:'chat-modes',title:'Chat modes'});
  const hint = document.createElement('p'); hint.className = 'ac-hint';
  hint.textContent = 'Default mode applies to new conversations. Read-only modes cannot execute mutating tools. Each mode adds its own instruction before model calls; existing permissions and approvals still apply.'; modeBody.append(hint);
  let modes = structuredClone(agent.execution_modes);
  const defaultLabel = document.createElement('label'); defaultLabel.className = 'ac-label'; defaultLabel.textContent = 'Default mode';
  const select = document.createElement('select'); select.className = 'ac-input'; select.dataset.field = 'default_execution_mode'; select.disabled = !agent.can_manage;
  const notice = document.createElement('p'); notice.className = 'ac-hint'; notice.setAttribute('role','status');
  const fill = () => { select.replaceChildren(); modes.filter(m=>!m._deleted&&!m._new).forEach(m=>{ const option=document.createElement('option'); option.value=m.id; option.textContent=m.label; select.append(option); }); select.value=agent.default_execution_mode; };
  fill(); defaultLabel.append(select); modeBody.append(defaultLabel,notice);
  select.addEventListener('change',async()=>{select.disabled=true;try {await persist({default_execution_mode:select.value});notice.textContent='Default mode saved';} catch(error){notice.textContent='Save failed: '+error.message;} finally {fill();select.disabled=!agent.can_manage;} });
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
      for(const [key,label] of [['label','Name'],['description','Footer description'],['prompt','Prompt injection'],['turn_label','Per-turn label'],['turn_guidance','Per-turn guidance']]){
        const wrap=document.createElement('label');wrap.className='ac-label';wrap.textContent=label;
        const multiline=key==='prompt'||key==='turn_guidance';
        const input=document.createElement(multiline?'textarea':'input');input.className='ac-input';input.dataset.modeField=key;
        if(key==='prompt')input.rows=6;if(key==='turn_guidance'){input.rows=6;input.maxLength=4000;}if(key==='turn_label')input.maxLength=60;
        input.value=mode[key]||'';input.disabled=!agent.can_manage;
        if(key==='turn_guidance'){
          wrap.append(' ',tipBadge('The per-turn label and this text are sent to the model with every turn while the session is in this mode, after the conversation history, and never saved in it. Use it for guidance the agent must follow on every turn, on top of the mode. Through a bridge (Claude Code, OpenCode), they are added with each user message, and again at the next step when they change.'));
        }
        input.addEventListener('input',()=>{mode[key]=input.value;save.disabled=false;status.textContent='Unsaved changes';updateSummary();});wrap.append(input);
        if(key==='turn_guidance'){const note=document.createElement('span');note.className='ac-hint';note.textContent='Sent with every turn in this mode, after the per-turn label. Leave both empty to send nothing.';wrap.append(note);}
        fields.append(wrap);
      }
      if(!mode.builtin&&agent.can_manage){const remove=document.createElement('button');remove.type='button';remove.className='ac-btn ac-btn-ghost';remove.textContent='Remove mode';remove.addEventListener('click',()=>{mode._deleted=true;fields.querySelectorAll('input,textarea,select').forEach(el=>el.disabled=true);save.disabled=false;remove.disabled=true;updateSummary();});fields.append(remove);}
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

  register.present();
}
