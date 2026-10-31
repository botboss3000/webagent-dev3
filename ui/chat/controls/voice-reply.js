// @code-index {"version":1,"feature":"chat-voice-reply","aliases":["voice reply","read replies aloud","agent voice output"],"purpose":"Below-composer on/off control that adds a seekable voice player under each new agent reply, spoken in the agent's Kokoro voice. Config: ui/chat/chat.json -> */active_footer/below_pill rows/controls.voice_reply places it; the agent's Identity voice_output decides whether it shows.","span_lines":90,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common/active_footer/below_pill/controls/voice_reply"],"profile_overrides":true}],"opportunities":[]}}
import {setVoiceReplyControl} from './chat-config.js';
import {textForSpeech} from './composer-media.js';
import {createVoicePlayer} from './voice-player.js';
import {requestTouchVibration} from '../../shared/vibrate.js';

const STORAGE_KEY = 'webagent_voice_reply';

/**
 * Mounts the voice reply toggle. It shows only when the session's agent has voice output on.
 * While the user has it on, each assistant reply published live gets a voice player under its
 * text; the text is never held back for audio. The server streams a plan line, then one MP3
 * chunk per line, so the player shows progress and can play from the first sentence; playback
 * starts only when the user presses Play. panel.js
 * drives open/published/destroy; Identity announces setting changes with `v3:voice-output-changed`.
 */
export function mountVoiceReply({client, note}) {
  let session = '', generation = 0, available = false, abort = null, current = null;
  const players = [];
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'chat-voice-reply-btn';
  button.dataset.headerControl = 'voice_reply';
  button.style.webkitTapHighlightColor = 'transparent';
  const wanted = () => { try { return localStorage.getItem(STORAGE_KEY) === 'true'; } catch { return false; } };
  function paint() {
    const on = wanted();
    button.style.display = available ? '' : 'none';
    button.setAttribute('aria-pressed', String(on));
    button.title = on ? 'Voice reply on: new replies get a voice player' : 'Voice reply off: tap to hear new replies';
    button.setAttribute('aria-label', button.title);
    button.classList.toggle('active', on);
    button.innerHTML = '<i data-lucide="' + (on ? 'volume-2' : 'volume-x') + '" style="width:16px;height:16px;"></i>';
    window.lucide?.createIcons();
  }
  // Stop the streaming reply; an unfinished player keeps what it has and says it stopped.
  function stop() {
    abort?.abort();
    abort = null;
    current?.pause();
  }
  async function speak(text, host) {
    stop();
    const sid = session, epoch = generation, controller = new AbortController();
    const player = createVoicePlayer();
    abort = controller;
    current = player;
    players.push(player);
    host.append(player.element);
    const stale = () => sid !== session || epoch !== generation || abort !== controller;
    try {
      const me = await (await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'})).json();
      const response = await fetch('/api/v3/sessions/' + encodeURIComponent(sid) + '/voice/reply', {
        method: 'POST', credentials: 'same-origin', signal: controller.signal,
        headers: {'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token || ''},
        body: JSON.stringify({text})
      });
      if (!response.ok) throw Error((await response.json().catch(() => ({}))).error || 'Voice reply is unavailable');
      const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
      let buffer = '';
      for (;;) {
        const {value, done} = await reader.read();
        if (stale()) return;
        if (done) break;
        buffer += value;
        let end;
        while ((end = buffer.indexOf('\n')) >= 0) {
          const line = buffer.slice(0, end).trim();
          buffer = buffer.slice(end + 1);
          if (!line) continue;
          const data = JSON.parse(line);
          if (data.error) throw Error(data.error);
          if (data.plan) { player.plan(data.plan.chunks); continue; }
          await player.add(Uint8Array.from(atob(data.audio), c => c.charCodeAt(0)).buffer);
        }
      }
      player.finish();
    } catch (error) {
      if (error.name === 'AbortError') { player.fail('Voice reply stopped'); return; }
      player.fail('Voice unavailable');
      if (!stale()) note(error.message);
    } finally {
      if (abort === controller) abort = null;
    }
  }
  async function load() {
    const sid = session, epoch = generation;
    available = false; paint();
    if (!sid) return;
    try {
      const capabilities = await client.voiceCapabilities(sid);
      if (sid === session && epoch === generation) available = capabilities.voice_output?.enabled === true;
    } catch { /* Without capabilities the control stays hidden. */ }
    if (!available) stop();
    paint();
  }
  button.addEventListener('click', event => {
    requestTouchVibration(event);
    const on = !wanted();
    try { localStorage.setItem(STORAGE_KEY, String(on)); } catch { /* Private mode: this tap still applies. */ }
    if (!on) stop();
    paint();
  });
  const reload = () => { void load(); };
  window.addEventListener('v3:voice-output-changed', reload);
  setVoiceReplyControl(button);
  paint();
  // Drop decoded audio held by players of the previous conversation.
  const releaseAll = () => { stop(); for (const player of players.splice(0)) player.release(); current = null; };
  return {
    /** Selects a session: stops speech, frees old players and checks its agent's voice output. */
    open(id) { releaseAll(); ++generation; session = id || ''; void load(); },
    /** Adds a voice player under a live (not recovered) assistant reply while the control is shown and on. */
    published(item, recovered = false) {
      if (recovered || !available || !wanted() || item.role !== 'assistant' || typeof item.content !== 'string') return;
      const text = textForSpeech(item.content);
      const row = [...document.querySelectorAll('#chat-messages-inner [data-public-id]')].find(node => node.dataset.publicId === item.id);
      const host = row?.matches('.chat-bubble') ? row : row?.querySelector('.chat-bubble') || row;
      if (text && host) void speak(text, host);
    },
    destroy() {
      releaseAll(); ++generation;
      window.removeEventListener('v3:voice-output-changed', reload);
      setVoiceReplyControl(null); button.remove();
    }
  };
}
