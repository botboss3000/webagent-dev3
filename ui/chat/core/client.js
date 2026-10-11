import { createAdministratorNotices } from './administrator-notices.js';

// V3 transport only: cookie/CSRF request policy is supplied by the shell.
export function createChatClient({request, note, clearNoticeFrom = () => {}, createStream, EventSourceClass = EventSource,
  setTimeoutFn = globalThis.setTimeout, clearTimeoutFn = globalThis.clearTimeout}) {
  const administratorNotices = createAdministratorNotices({ request });
  let source = null, epoch = 0, current = null;
  function setConnecting(connecting) {
    const button = document.getElementById('chat-toggle-btn');
    if (!button) return;
    button.classList.toggle('connection-connecting', connecting);
    button.title = connecting ? 'Chat connecting…' : 'Toggle chat panel';
  }
  function disconnect() {
    ++epoch;
    if (current && current.retry !== null) clearTimeoutFn(current.retry);
    current = null;
    administratorNotices.stop(); source?.close(); source = null; setConnecting(false);
  }
  function valid(state) { return current === state && state.epoch === epoch; }
  function deliver(state, item, recovered = false) {
    if (!Number.isSafeInteger(item.seq) || item.seq <= 0 || typeof item.id !== 'string')
      throw Error('Invalid chat event');
    if (state.seen.has(item.id)) return;
    // The browser can commit Last-Event-ID before rendering. Recover failures
    // through a snapshot rather than relying on stream replay alone.
    if (state.publish(item, {recovered}) === false) throw Error('Chat message was not rendered');
    state.seen.add(item.id);
    state.last = Math.max(state.last, item.seq);
  }
  async function recover(state) {
    if (!valid(state)) return;
    if (state.inFlight) { state.again = true; return state.inFlight; }
    if (state.retry !== null) { clearTimeoutFn(state.retry); state.retry = null; }
    state.inFlight = (async () => {
      try {
        do {
          state.again = false;
          const data = await request('/sessions/' + encodeURIComponent(state.id));
          if (!valid(state)) return;
          // Include gaps below a newer live event; ID deduplication keeps rows.
          for (const item of [...data.messages].sort((a,b) => a.seq-b.seq)) deliver(state, item, true);
          state.last = Math.max(state.last, data.last_seq || 0);
        } while (state.again && valid(state));
        state.failures = 0;
        clearNoticeFrom('chat-sync');
      } catch {
        if (!valid(state)) return;
        if (++state.failures <= 3) {
          state.retry = setTimeoutFn(() => {state.retry = null; void recover(state);}, 1000 * 2 ** (state.failures-1));
        } else note("Couldn't sync chat history", 'chat-sync');
      } finally { state.inFlight = null; }
    })();
    return state.inFlight;
  }
  function updateRuns(items) {
    const state = current;
    if (!state) return;
    for (const run of items) {
      if (run.session_id !== state.id) continue;
      const previous = state.runs.get(run.id);
      state.runs.set(run.id, run.status);
      if (previous !== run.status && ['completed','failed','stopped'].includes(run.status)) void recover(state);
    }
  }
  async function open(id, render, publish, metadataChanged = () => {}) {
    disconnect();
    const turn = epoch;
    const data = await request('/sessions/' + encodeURIComponent(id));
    if (turn !== epoch) return;
    render(data);
    const state = current = {id, epoch:turn, publish, last:data.last_seq || 0,
      seen:new Set(data.messages.map(item => item.id)), runs:new Map((data.runs || []).map(run => [run.id,run.status])),
      inFlight:null, again:false, retry:null, failures:0};
    administratorNotices.start(id);
    const streamUrl='/api/v3/sessions/' + encodeURIComponent(id) + '/events?after=' + state.last;
    source = createStream ? createStream(streamUrl) : new EventSourceClass(streamUrl);
    setConnecting(true);
    source.addEventListener('session_updated', () => { if (valid(state)) metadataChanged(); });
    source.onmessage = event => {
      if (!valid(state)) return;
      try { deliver(state, JSON.parse(event.data)); }
      catch { void recover(state); }
    };
    source.onerror = () => { if (valid(state)) {setConnecting(true); void recover(state);} };
    source.onopen = () => { if (valid(state)) {setConnecting(false); void recover(state);} };
  }
  return {
    open, disconnect, updateRuns,
    usage:id=>request("/sessions/"+encodeURIComponent(id)+"/usage"),
    contextConfig:()=>request("/admin/context-compaction"),
    saveContextConfig:body=>request("/admin/context-compaction","PUT",body),
    snapshot:id=>request('/sessions/'+encodeURIComponent(id)),
    upload:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/attachments','POST',body),
    describeUpload:(id,aid)=>request('/sessions/'+encodeURIComponent(id)+'/attachments/'+encodeURIComponent(aid)+'/describe','POST',{}),
    removeUpload:(id,aid)=>request('/sessions/'+encodeURIComponent(id)+'/attachments/'+encodeURIComponent(aid),'DELETE'),
    voiceAudio:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/audio','POST',body),
    voiceRoute:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/route','POST',body),
    voiceControl:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/control','POST',body),
    voiceCapabilities:id=>request('/sessions/'+encodeURIComponent(id)+'/voice/capabilities'),
    voiceSpeech:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/speech','POST',body),
    controls: id => request('/sessions/' + encodeURIComponent(id) + '/controls'),
    mode: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/mode', 'PUT', body),
    model: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/model', 'PUT', body),
    sessions: (status = 'active') => request('/sessions?status=' + encodeURIComponent(status)),
    search: ({q, status = 'active', continuation}) => {
      const params = new URLSearchParams({q, status, limit: '20'});
      if (continuation) params.set('continuation', continuation);
      return request('/sessions/search?' + params);
    },
    patch: (id, body) => request('/sessions/' + encodeURIComponent(id), 'PATCH', body),
    recycle: (id, body) => request('/sessions/' + encodeURIComponent(id), 'DELETE', body),
    restore: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/restore', 'POST', body),
    read: (id, through_seq) => request('/sessions/' + encodeURIComponent(id) + '/read', 'PUT', {through_seq}),
    create: body => request('/sessions', 'POST', body),
    send: (id, body) => request('/sessions/' + id + '/messages', 'POST', body),
    cancel: (id, body) => request('/runs/' + id + '/cancel', 'POST', body),
    decide: (id, body) => request('/approvals/' + id + '/decision', 'POST', body),
    recover: (id, body) => request('/runs/' + id + '/recovery', 'POST', body),
    resume: (id, body) => request('/runs/' + id + '/resume', 'POST', body)
  };
}
