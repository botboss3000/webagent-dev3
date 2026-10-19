import {createSettingsRow} from './settings-row.js';
'use strict';
// Working-message publication belongs to one agent-wide mode; chat-mode prompts
// continue to provide call-scoped message formatting.
export function renderVisibility(content, agent, persist) {
  const settings = agent.message_visibility;
  if (!settings) return;
  const {body}=createSettingsRow(content,{id:'working-message-promotion',title:'Working-message promotion'});
  const hint=document.createElement('p');hint.className='ac-hint';hint.textContent='Choose how nonfinal assistant messages become standalone chat bubbles.';body.append(hint);
  const modeWrap = document.createElement('label'); modeWrap.className = 'agent-promotion-mode-wrap';
  const mode = document.createElement('select'); mode.className = 'ac-input agent-promotion-mode';
  mode.setAttribute('aria-label','Working-message promotion');
  for (const [value,title] of [['none','No promotion'],['manual','Selected through tool'],['auto','Auto promote all']]) {
    const option=document.createElement('option'); option.value=value; option.textContent=title; mode.append(option);
  }
  mode.value=settings.mode; mode.disabled=!agent.can_manage;
  const modeStatus=document.createElement('span'); modeStatus.className='agent-promotion-mode-status';
  modeStatus.setAttribute('role','status'); modeStatus.setAttribute('aria-live','polite'); modeWrap.append(mode,modeStatus);
  body.append(modeWrap);
  const note=document.createElement('p'); note.className='ac-hint'; body.append(note);
  const fields=document.createElement('div'); body.append(fields);
  const field=(title,rows)=>{const label=document.createElement('label');label.className='agent-closing-message-guidance';const span=document.createElement('span');span.textContent=title;const input=document.createElement('textarea');input.className='ac-input';input.rows=rows;label.append(span,input);fields.append(label);return input;};
  const fixed=field('Fixed promotion protocol',5); fixed.readOnly=true; fixed.value=settings.fixed_protocol||'';
  const guidance=field('Additional promotion guidance',2); guidance.value=settings.guidance||''; guidance.disabled=!agent.can_manage; guidance.placeholder='Optional guidance for selecting a standalone working message.';
  const save=document.createElement('button');save.type='button';save.className='ac-btn ac-btn-ghost';save.textContent='Save guidance';save.disabled=!agent.can_manage;fields.append(save);
  const status=document.createElement('span');status.setAttribute('role','status');status.setAttribute('aria-live','polite');fields.append(status);
  const applyMode=()=>{fields.hidden=mode.value!=='manual';note.textContent=mode.value==='auto'
    ? 'Every nonfinal assistant message is promoted automatically. Promotion protocol and guidance are inactive.'
    : mode.value==='none' ? 'Nonfinal assistant messages stay in private activity. Promotion protocol and guidance are inactive.'
      : 'The agent chooses standalone working messages with the promotion tool.';};
  applyMode();
  guidance.addEventListener('input',()=>{save.textContent='Save guidance';save.classList.remove('is-saved');status.textContent='';});
  let savedMode=settings.mode, savedGuidance=settings.guidance||'';
  async function commit(nextMode,nextGuidance) {
    mode.disabled=true;guidance.disabled=true;save.disabled=true;status.textContent='';modeStatus.textContent='Saving…';save.textContent='Saving…';save.classList.remove('is-saved');
    try {
      const result=await persist({message_visibility:{mode:nextMode,guidance:nextGuidance}});
      savedMode=result.message_visibility.mode;savedGuidance=result.message_visibility.guidance;
      modeStatus.textContent='Saved';
      mode.value=savedMode;guidance.value=savedGuidance;applyMode();save.textContent='Saved';save.classList.add('is-saved');
    } catch(error) {
      mode.value=savedMode;guidance.value=savedGuidance;applyMode();save.textContent='Save guidance';modeStatus.textContent='Save failed';status.textContent='Save failed: '+error.message;
    } finally {
      mode.disabled=!agent.can_manage;guidance.disabled=!agent.can_manage;save.disabled=!agent.can_manage;
    }
  }
  mode.addEventListener('change',()=>{applyMode();commit(mode.value,guidance.value);});
  save.addEventListener('click',()=>commit(mode.value,guidance.value));
}
