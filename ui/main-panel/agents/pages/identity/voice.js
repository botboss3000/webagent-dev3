import {createSettingsRow} from './settings-row.js';
'use strict';
// Agent voice output: the toggle shows the chat's voice reply control for this agent, and the
// chosen Kokoro voice speaks its replies. The server validates voices against its catalog.
export function renderVoiceOutput(content, agent, persist) {
  if (!agent.voice_output || !Array.isArray(agent.voice_catalog)) return;
  const {body, toggle} = createSettingsRow(content, {id:'voice-output', title:'Voice output', toggleLabel:'Enable voice output'});
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = 'When enabled, chats with this agent show a voice reply control below the composer. Users who turn it on hear each new reply in the voice chosen here. Play a sample to compare voices.';
  const status = document.createElement('p');
  status.className = 'ac-hint';
  status.setAttribute('role', 'status');
  status.setAttribute('aria-live', 'polite');
  const list = document.createElement('div');
  list.className = 'agent-voice-list';
  list.setAttribute('role', 'radiogroup');
  list.setAttribute('aria-label', 'Voice');
  list.style.cssText = 'display:flex;flex-direction:column;gap:4px;margin:8px 0';
  body.append(hint, list, status);

  const player = new Audio();
  let playing = null;
  const resetPlay = () => { if (playing) { playing.textContent = 'Play sample'; playing.setAttribute('aria-pressed', 'false'); } playing = null; };
  player.onended = resetPlay;
  player.onerror = () => { if (playing) status.textContent = 'Sample unavailable'; resetPlay(); };
  player.onplaying = () => { if (playing) playing.textContent = 'Stop'; };

  let saved = {...agent.voice_output};
  let busy = false;
  const radios = [];
  let group = '';
  for (const voice of agent.voice_catalog) {
    if (voice.group !== group) {
      group = voice.group;
      const heading = document.createElement('div');
      heading.className = 'ac-label';
      heading.style.cssText = 'margin-top:8px';
      heading.textContent = group;
      list.append(heading);
    }
    const row = document.createElement('div');
    row.style.cssText = 'display:flex;align-items:center;gap:8px;min-height:32px';
    const label = document.createElement('label');
    label.style.cssText = 'display:flex;align-items:center;gap:8px;flex:1;min-width:0;cursor:pointer;-webkit-tap-highlight-color:transparent';
    const radio = document.createElement('input');
    radio.type = 'radio';
    radio.name = 'identity-voice-' + agent.id;
    radio.value = voice.id;
    radio.dataset.voice = voice.id;
    radio.addEventListener('change', () => { if (radio.checked) void commit({voice: voice.id}); });
    radios.push(radio);
    label.append(radio, document.createTextNode(voice.name));
    const sample = document.createElement('button');
    sample.type = 'button';
    sample.className = 'ac-btn ac-btn-ghost';
    sample.style.webkitTapHighlightColor = 'transparent';
    sample.textContent = 'Play sample';
    sample.dataset.voiceSample = voice.id;
    sample.setAttribute('aria-pressed', 'false');
    sample.setAttribute('aria-label', 'Play ' + voice.name + ' sample');
    sample.addEventListener('click', () => {
      const same = playing === sample;
      player.pause();
      resetPlay();
      if (same) return;
      playing = sample;
      sample.textContent = 'Loading…';
      sample.setAttribute('aria-pressed', 'true');
      status.textContent = '';
      player.src = '/api/v3/voice/samples/' + encodeURIComponent(voice.id);
      player.play().catch(() => { status.textContent = 'Sample unavailable'; resetPlay(); });
    });
    row.append(label, sample);
    list.append(row);
  }

  const paint = () => {
    toggle.checked = saved.enabled === true;
    toggle.disabled = !agent.can_manage || busy;
    for (const radio of radios) {
      radio.checked = radio.value === saved.voice;
      radio.disabled = !agent.can_manage || busy;
    }
  };
  async function commit(change) {
    if (busy) return;
    busy = true;
    paint();
    status.textContent = 'Saving…';
    try {
      const result = await persist({voice_output: {...saved, ...change}});
      saved = {...result.voice_output};
      status.textContent = 'Saved';
      window.dispatchEvent(new Event('v3:voice-output-changed'));
    } catch (error) {
      status.textContent = 'Save failed: ' + error.message;
    } finally {
      busy = false;
      paint();
    }
  }
  paint();
  toggle.addEventListener('click', event => { event.stopPropagation(); void commit({enabled: saved.enabled !== true}); });
}
