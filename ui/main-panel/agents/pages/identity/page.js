import {requestTouchVibration} from '../../../../shared/vibrate.js';
import {createSettingsRow, setRegisterOpen} from './settings-row.js';
import {renderMainChat} from './main-chat.js';
import {renderMemberAccess} from './member-access.js';

'use strict';
import { renderSharedChat } from './shared-chat.js';
import { renderEmbedSettings } from './embed.js';
import { renderVisibility } from './visibility.js';
import { renderClosing } from './closing.js';
import { renderVoiceOutput } from './voice.js';
import { renderManager } from './manager.js';
import { renderCompaction } from './compaction.js';
import { renderToolOutputPolicy } from './tool-output-policy.js';

const STYLE_HREF = '/ui/main-panel/agents/pages/identity/identity-register.css';
const CHEV_LEFT = '<svg class="ir-chev-back" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 18-6-6 6-6"/></svg>';

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
  'working-message-promotion': '<path d="m3 11 18-6v14L3 13z"/><path d="M7 12v4.5a2.5 2.5 0 0 0 5 0V13"/>',
  'final-response': '<circle cx="12" cy="12" r="9"/><path d="m8.5 12.5 2.5 2.5 4.5-5"/>',
  'voice-output': '<path d="M11 5 6.5 9H3v6h3.5L11 19z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.5 5.5a9 9 0 0 1 0 13"/>',
  fallback: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.6 1.6 0 0 0-1-1.5 1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.6 1.6 0 0 0 1.5-1 1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1z"/>',
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
  'working-message-promotion': 'How working messages become standalone bubbles.',
  'final-response': 'Completion flags and notification guidance.',
  'voice-output': 'Choose the voice that reads replies aloud.',
};

function ensureStyles() {
  if (document.querySelector('link[data-ir-style]')) return;
  const link = document.createElement('link');
  link.rel = 'stylesheet';
  link.href = STYLE_HREF;
  link.dataset.irStyle = '';
  document.head.append(link);
}

function iconFor(id) {
  return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + (ICONS[id] || ICONS.fallback) + '</svg>';
}

/* Settings register: a list of categories that opens one category at a time. */
function buildRegister(host, stage) {
  const shell = document.createElement('div');
  shell.className = 'ir-shell';
  const stack = document.createElement('div');
  stack.className = 'ir-stack';
  const menu = document.createElement('div');
  menu.className = 'ir-menu is-intro';
  menu.setAttribute('aria-label', 'Identity settings');
  const detail = document.createElement('div');
  detail.className = 'ir-detail';
  detail.hidden = true;
  const head = document.createElement('div');
  head.className = 'ir-detail-head';
  const back = document.createElement('button');
  back.type = 'button';
  back.className = 'ir-back';
  back.innerHTML = CHEV_LEFT + '<span class="ir-back-title"></span>';
  const slot = document.createElement('span');
  slot.className = 'ir-toggle-slot';
  head.append(back, slot);
  const detailBody = document.createElement('div');
  detailBody.className = 'ir-detail-body';
  detail.append(head, detailBody);
  stack.append(menu, detail);
  shell.append(stack);
  host.append(shell);

  const pages = new Map();
  const entries = [];
  for (const section of [...stage.children]) {
    const id = section.dataset.identitySection
      || (section.classList.contains('main-chat-settings') ? 'main-chat' : '');
    if (!id) continue;
    const body = section.querySelector('.ir-section-body')
      || section.querySelector('.ac-ability-body')
      || section;
    const toggleWrap = section.querySelector('.conn-toggle-wrap');
    const toggle = toggleWrap?.querySelector('input.conn-toggle') || null;
    const title = section.dataset.irTitle || section.querySelector('.ac-ability-name')?.textContent?.trim() || 'Settings';
    section.querySelector('.ir-section-head')?.remove();
    section.querySelector('.conn-card-header')?.remove();
    body.hidden = false;
    body.style.display = 'block';
    body.classList.add('ir-page-body');
    entries.push({id, title, section, body, toggleWrap, toggle});
  }
  stage.remove();
  // Sections stay in the document so a section that reloads itself still sees itself connected.
  const anchors = document.createElement('div');
  anchors.className = 'ir-anchors';
  anchors.append(...entries.map(entry => entry.section));
  shell.append(anchors);

  let currentId = null;
  entries.forEach((entry, index) => {
    pages.set(entry.id, entry);
    const row = document.createElement('button');
    row.type = 'button';
    row.className = 'ir-row';
    row.dataset.irFor = entry.id;
    row.style.setProperty('--i', String(index));
    row.innerHTML = '<span class="ir-tile">' + iconFor(entry.id) + '</span>'
      + '<span class="ir-text"><span class="ir-title"></span><span class="ir-hint"></span></span>'
      + '<span class="ir-state"></span>';
    row.querySelector('.ir-title').textContent = entry.title;
    const hint = HINTS[entry.id] || '';
    const hintEl = row.querySelector('.ir-hint');
    hintEl.textContent = hint;
    if (!hint) hintEl.remove();
    entry.state = row.querySelector('.ir-state');
    if (entry.toggle) {
      entry.state.textContent = entry.toggle.checked ? 'On' : 'Off';
      entry.toggle.addEventListener('change', () => {
        entry.state.textContent = entry.toggle.checked ? 'On' : 'Off';
      });
    } else {
      entry.state.remove();
    }
    row.addEventListener('click', event => { requestTouchVibration(event); open(entry.id); });
    menu.append(row);
  });

  let animating = false;

  function open(id) {
    const entry = pages.get(id);
    if (!entry || animating) return;
    // A section that asks to open while its own panel is showing only needs the focus kept.
    if (currentId === id && !detail.hidden) return;
    currentId = id;
    back.querySelector('.ir-back-title').textContent = entry.title;
    detailBody.replaceChildren(entry.body);
    slot.replaceChildren(entry.toggleWrap || '');
    animating = true;
    menu.classList.add('is-out-left');
    detail.hidden = false;
    detail.classList.add('is-in-right');
    void detail.offsetWidth;
    requestAnimationFrame(() => detail.classList.remove('is-in-right'));
    setTimeout(() => {
      menu.hidden = true;
      menu.classList.remove('is-out-left');
      animating = false;
      back.focus({preventScroll: true});
    }, 220);
  }

  function close() {
    if (animating || detail.hidden) return;
    animating = true;
    detail.classList.add('is-out-right');
    menu.hidden = false;
    menu.classList.add('is-in-left');
    void menu.offsetWidth;
    requestAnimationFrame(() => menu.classList.remove('is-in-left'));
    setTimeout(() => {
      detail.hidden = true;
      detail.classList.remove('is-out-right');
      slot.replaceChildren();
      animating = false;
      menu.querySelector('[data-ir-for="' + currentId + '"]')?.focus({preventScroll: true});
    }, 220);
  }

  setRegisterOpen(open);
  back.addEventListener('click', event => { requestTouchVibration(event); close(); });
  detail.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); close(); }
  });

  const pin = () => {
    if (menu.hidden) return;
    stack.style.minHeight = menu.offsetHeight + 'px';
  };
  requestAnimationFrame(pin);
  if (window.ResizeObserver) new ResizeObserver(pin).observe(menu);
  window.addEventListener('resize', pin);
  setTimeout(() => menu.classList.remove('is-intro'), entries.length * 45 + 500);
}

// Adapted V2 Identity fields; V3 transport, tenant permissions and revision fencing.
export async function renderIdentity(host, request, {agentId, onSaved = () => {}}) {
  const path = '/models/agents/' + encodeURIComponent(agentId) + '/identity';
  ensureStyles();
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
  // Sections build in an off-screen staging element; the register presents their content.
  // It stays in the document so asynchronous sections see themselves as connected.
  const stage = document.createElement('div');
  stage.className = 'ir-stage';
  host.append(stage);
  // Serialize fields so concurrent name/description saves share the latest revision.
  let queue = Promise.resolve();
  const profile = createSettingsRow(stage, {id:'profile', title:'Profile'});
  const content = profile.body;
  const card = document.createElement('div');
  card.className = 'ir-profile';
  card.innerHTML = '<span class="ir-avatar" aria-hidden="true"></span><span class="ir-profile-text"><span class="ir-profile-name"></span><span class="ir-profile-sub"></span></span>';
  card.querySelector('.ir-avatar').textContent = (agent.name || 'Agent').trim().split(/\s+/).map(word => word[0]).join('').slice(0, 2).toUpperCase();
  const profileName = card.querySelector('.ir-profile-name');
  profileName.textContent = agent.name || 'Unnamed agent';
  card.querySelector('.ir-profile-sub').textContent = 'Agent · ' + agentId;
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
  renderCompaction(stage, agent, persist);
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
      for(const [key,label] of [['label','Name'],['description','Footer description'],['prompt','Prompt injection']]){
        const wrap=document.createElement('label');wrap.className='ac-label';wrap.textContent=label;
        const input=document.createElement(key==='prompt'?'textarea':'input');input.className='ac-input';input.dataset.modeField=key;
        if(key==='prompt')input.rows=6;input.value=mode[key]||'';input.disabled=!agent.can_manage;
        input.addEventListener('input',()=>{mode[key]=input.value;save.disabled=false;status.textContent='Unsaved changes';updateSummary();});wrap.append(input);fields.append(wrap);
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
    if(agent.can_manage){const add=document.createElement('button');add.type='button';add.className='ac-btn ac-btn-ghost';add.textContent='Add mode';add.addEventListener('click',()=>{let n=1;while(modes.some(m=>m.id==='custom-'+n))n++;modes.push({id:'custom-'+n,label:'New mode',description:'',prompt:'',builtin:false,_new:true});renderModes();});editor.append(add);}
  };renderModes();
  renderManager(stage, agent, persist);
  renderVisibility(stage, agent, persist);

  renderClosing(stage, agent, persist);
  renderVoiceOutput(stage, agent, persist);

  buildRegister(host, stage);
}
