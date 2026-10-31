// Seekable player for a voice reply that arrives as MP3 chunks. Chunks are decoded with Web
// Audio and scheduled back to back, so one position marker spans every portion. The speaker's
// plan (characters per chunk) sizes the track before audio exists: synthesized portions show
// solid, the rest light gray, and the estimate tightens as real durations arrive.
import {requestTouchVibration} from '../../shared/vibrate.js';

// Estimated speech seconds per character until a decoded chunk gives the real rate.
const SECONDS_PER_CHAR = 0.065;
const PLAY = '<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M7 4.5v15l13-7.5z" fill="currentColor"/></svg>';
const PAUSE = '<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M6 4h4v16H6zM14 4h4v16h-4z" fill="currentColor"/></svg>';

let context = null;
/** The shared AudioContext, created on first use. */
export function audioContext() {
  context ??= new (window.AudioContext || window.webkitAudioContext)();
  return context;
}
/** Resume audio inside a user gesture; browsers keep it suspended until one. */
export function unlockAudio() {
  audioContext().resume().catch(() => {});
}

const clock = seconds => {
  const s = Math.max(0, Math.round(seconds));
  return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0');
};

/**
 * Creates a detached player element. Callers append `element`, then feed it with `plan(chunks)`,
 * `add(arrayBuffer)` per MP3 chunk in order, and `finish()` or `fail(message)`. `play`, `pause`
 * and `release` (stop and drop decoded audio) control it from outside.
 */
export function createVoicePlayer() {
  const ctx = audioContext();
  const element = document.createElement('div');
  element.className = 'voice-player';
  element.setAttribute('role', 'group');
  element.setAttribute('aria-label', 'Voice reply');
  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.className = 'voice-player-toggle';
  toggle.style.webkitTapHighlightColor = 'transparent';
  const track = document.createElement('div');
  track.className = 'voice-player-track';
  track.tabIndex = 0;
  track.setAttribute('role', 'slider');
  track.setAttribute('aria-label', 'Voice reply position');
  track.setAttribute('aria-valuemin', '0');
  const loadedBar = document.createElement('div');
  loadedBar.className = 'voice-player-loaded';
  const playedBar = document.createElement('div');
  playedBar.className = 'voice-player-played';
  const thumb = document.createElement('div');
  thumb.className = 'voice-player-thumb';
  track.append(loadedBar, playedBar, thumb);
  const time = document.createElement('span');
  time.className = 'voice-player-time';
  time.setAttribute('aria-live', 'off');
  element.append(toggle, track, time);
  // Taps on the player stay with it; the bubble has its own click actions.
  element.addEventListener('click', event => event.stopPropagation());

  let plan = null, buffers = [], loaded = 0, done = false, failed = '';
  let playing = false, waiting = false, anchorPos = 0, anchorAt = 0, sources = [], scheduled = 0;
  let dragPos = null, frame = 0, decoding = Promise.resolve();

  const loadedChars = () => (plan || []).slice(0, buffers.length).reduce((a, b) => a + b, 0);
  function total() {
    if (!plan) return Math.max(loaded, 1);
    const rest = plan.slice(buffers.length).reduce((a, b) => a + b, 0);
    const rate = loadedChars() ? loaded / loadedChars() : SECONDS_PER_CHAR;
    return Math.max(loaded + rest * rate, 0.1);
  }
  function position() {
    if (dragPos !== null) return dragPos;
    return playing ? Math.min(anchorPos + Math.max(0, ctx.currentTime - anchorAt), loaded) : anchorPos;
  }
  function stopSources() {
    for (const source of sources) { source.onended = null; try { source.stop(); } catch { /* already ended */ } }
    sources = [];
    scheduled = 0;
  }
  // Start every decoded chunk not yet scheduled, back to back from the anchor position.
  function schedule() {
    let start = 0;
    buffers.forEach((buffer, index) => {
      const end = start + buffer.duration;
      if (index >= scheduled) {
        if (end > anchorPos) {
          const source = ctx.createBufferSource();
          source.buffer = buffer;
          source.connect(ctx.destination);
          source.start(anchorAt + Math.max(0, start - anchorPos), Math.max(0, anchorPos - start));
          sources.push(source);
        }
        scheduled = index + 1;
      }
      start = end;
    });
  }
  function paint() {
    const span = total(), at = position(), ready = buffers.length > 0;
    const busy = (!ready && !failed) || (playing && waiting);
    toggle.disabled = !ready;
    toggle.classList.toggle('loading', busy);
    toggle.innerHTML = busy ? '<span class="voice-player-spinner" aria-hidden="true"></span>' : playing ? PAUSE : PLAY;
    toggle.setAttribute('aria-label', busy ? 'Preparing voice reply' : playing ? 'Pause voice reply' : 'Play voice reply');
    loadedBar.style.width = Math.min(100, loaded / span * 100) + '%';
    playedBar.style.width = thumb.style.left = Math.min(100, at / span * 100) + '%';
    thumb.hidden = !ready;
    track.setAttribute('aria-valuemax', String(Math.round(loaded)));
    track.setAttribute('aria-valuenow', String(Math.round(at)));
    track.setAttribute('aria-valuetext', clock(at) + ' of ' + clock(span));
    time.textContent = failed && !ready ? failed : clock(at) + ' / ' + (done ? '' : '~') + clock(span);
    element.classList.toggle('failed', Boolean(failed));
  }
  function tick() {
    cancelAnimationFrame(frame);
    if (!playing) return;
    if (position() >= loaded - 0.02) {
      if (done) { playing = false; stopSources(); anchorPos = loaded; }
      else waiting = true;  // playback caught up with synthesis; the next chunk resumes it
    }
    paint();
    if (playing) frame = requestAnimationFrame(tick);
  }
  function play(at = position()) {
    if (!buffers.length) return;
    if (done && at >= loaded - 0.05) at = 0;
    stopSources();
    anchorPos = Math.max(0, Math.min(at, loaded));
    anchorAt = ctx.currentTime + 0.05;
    playing = true;
    waiting = false;
    ctx.resume().catch(() => {});
    schedule();
    tick();
  }
  function pause() {
    anchorPos = position();
    playing = false;
    stopSources();
    cancelAnimationFrame(frame);
    paint();
  }
  function seek(at) {
    at = Math.max(0, Math.min(at, loaded));
    if (playing) play(at);
    else { anchorPos = at; paint(); }
  }
  const pointAt = event => {
    const box = track.getBoundingClientRect();
    return Math.max(0, Math.min(loaded, (event.clientX - box.left) / box.width * total()));
  };

  toggle.addEventListener('click', event => {
    requestTouchVibration(event);
    if (playing) pause(); else play();
  });
  track.addEventListener('pointerdown', event => {
    if (!buffers.length) return;
    track.setPointerCapture(event.pointerId);
    dragPos = pointAt(event);
    paint();
  });
  track.addEventListener('pointermove', event => { if (dragPos !== null) { dragPos = pointAt(event); paint(); } });
  const drop = () => { if (dragPos === null) return; const at = dragPos; dragPos = null; seek(at); };
  track.addEventListener('pointerup', drop);
  track.addEventListener('pointercancel', drop);
  track.addEventListener('keydown', event => {
    const step = {ArrowLeft: -5, ArrowRight: 5}[event.key];
    if (step !== undefined) seek(position() + step);
    else if (event.key === 'Home') seek(0);
    else if (event.key === 'End') seek(loaded);
    else return;
    event.preventDefault();
  });
  paint();

  return {
    element,
    plan(chunks) { if (Array.isArray(chunks)) { plan = chunks.map(Number); paint(); } },
    /** Decode one MP3 chunk (in arrival order); resolves once it can be played. */
    add(bytes) {
      decoding = decoding.then(async () => {
        const buffer = await ctx.decodeAudioData(bytes);
        const before = loaded;
        buffers.push(buffer);
        loaded += buffer.duration;
        // If playback already ran out (animation frames may be paused in a background tab),
        // resume from where the audio ended; otherwise queue the chunk after the current one.
        const caughtUp = anchorPos + ctx.currentTime - anchorAt >= before - 0.02;
        if (playing) { if (caughtUp) play(before); else schedule(); }
        paint();
      });
      return decoding;
    },
    finish() { decoding.then(() => { done = true; paint(); tick(); }); },
    fail(message) { failed = message; done = true; paint(); },
    get playing() { return playing; },
    play,
    pause,
    /** Stop and drop decoded audio; the element stays as a record but no longer plays. */
    release() { pause(); buffers = []; loaded = 0; anchorPos = 0; if (!done) failed = 'Voice reply stopped'; done = true; paint(); }
  };
}
