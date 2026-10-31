// @code-index {"version":1,"feature":"chat-voice-reply","aliases":["voice reply","read replies aloud","agent voice output"],"purpose":"Below-composer on/off control that speaks each new agent reply in the agent's Kokoro voice. Config: ui/chat/chat.json -> chat_common.active_footer.below_pill rows/controls.voice_reply places it; the agent's Identity voice_output decides whether it shows.","span_lines":90,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common/active_footer/below_pill/controls/voice_reply"],"profile_overrides":true}],"opportunities":[]}}
import {setVoiceReplyControl} from './chat-config.js';
import {textForSpeech} from './composer-media.js';
import {requestTouchVibration} from '../../shared/vibrate.js';

const STORAGE_KEY = 'webagent_voice_reply';
// An empty WAV played during the tap so mobile browsers allow later, untouched playback.
const UNLOCK = 'data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQAAAAA=';

/**
 * Mounts the voice reply toggle. It shows only when the session's agent has voice output on;
 * while the user has it on, each assistant reply published live is spoken. The server streams
 * one MP3 chunk per NDJSON line, so playback starts after the first sentence. panel.js drives
 * open/published/destroy; Identity announces setting changes with `v3:voice-output-changed`.
 */
export function mountVoiceReply({client, note}) {
  let session = '', generation = 0, available = false, abort = null, current = '';
  const queue = [];
  const player = new Audio();
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
    button.title = on ? 'Voice reply on: new replies are read aloud' : 'Voice reply off: tap to hear new replies';
    button.setAttribute('aria-label', button.title);
    button.classList.toggle('active', on);
    button.innerHTML = '<i data-lucide="' + (on ? 'volume-2' : 'volume-x') + '" style="width:16px;height:16px;"></i>';
    window.lucide?.createIcons();
  }
  // Plays queued chunks in order; each blob URL is released once it has played or failed.
  function playNext() {
    if (current || !queue.length) return;
    current = queue.shift();
    player.src = current;
    player.play().catch(finished);
  }
  function finished() { if (current) URL.revokeObjectURL(current); current = ''; playNext(); }
  player.onended = finished;
  player.onerror = finished;
  function stop() {
    abort?.abort(); abort = null;
    for (const url of queue.splice(0)) URL.revokeObjectURL(url);
    player.pause();
    if (current) URL.revokeObjectURL(current);
    current = '';
  }
  async function speak(text) {
    stop();
    const sid = session, epoch = generation, controller = new AbortController();
    abort = controller;
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
        if (done || stale()) return;
        buffer += value;
        let end;
        while ((end = buffer.indexOf('\n')) >= 0) {
          const line = buffer.slice(0, end).trim();
          buffer = buffer.slice(end + 1);
          if (!line) continue;
          const data = JSON.parse(line);
          if (data.error) throw Error(data.error);
          const bytes = Uint8Array.from(atob(data.audio), c => c.charCodeAt(0));
          queue.push(URL.createObjectURL(new Blob([bytes], {type: 'audio/mpeg'})));
          playNext();
        }
      }
    } catch (error) {
      if (error.name !== 'AbortError' && !stale()) note(error.message);
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
    if (on) { player.src = UNLOCK; player.play().catch(() => {}); } else stop();
    paint();
  });
  const reload = () => { void load(); };
  window.addEventListener('v3:voice-output-changed', reload);
  setVoiceReplyControl(button);
  paint();
  return {
    /** Selects a session: stops speech and checks its agent's voice output. */
    open(id) { stop(); ++generation; session = id || ''; void load(); },
    /** Speaks a live (not recovered) assistant reply while the control is shown and on. */
    published(item, recovered = false) {
      if (recovered || !available || !wanted() || item.role !== 'assistant' || typeof item.content !== 'string') return;
      const text = textForSpeech(item.content);
      if (text) void speak(text);
    },
    destroy() { stop(); ++generation; window.removeEventListener('v3:voice-output-changed', reload); setVoiceReplyControl(null); button.remove(); }
  };
}
