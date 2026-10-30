import { createAdministratorNotices } from './administrator-notices.js';

// V3 transport only: cookie/CSRF request policy is supplied by the shell.
/**
 * Builds the v3 chat transport around the shell-authenticated request helper and an SSE stream.
 * Stream epochs fence stale sessions; snapshot recovery fills gaps after delivery or connection failures.
 * @param {object} options Supplies request, notices, stream creation, and timer adapters.
 * @returns {object} Session, run, attachment, voice, approval, and recovery request methods.
 * Errors from the initial session request propagate; stream recovery retries are bounded.
 */
// @code-index {"version":1,"feature":"chat-transport-recovery","aliases":["chat client","chat event stream","session transport recovery"],"purpose":"Locates the v3 chat transport and sequence-based stream recovery","span_lines":80,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider bounded retry timing and backoff configuration; preserve recovery and publication invariants."]}}
export function createChatClient({request, note, issues, clearNoticeFrom = () => {}, createStream, EventSourceClass = EventSource,
  setTimeoutFn = globalThis.setTimeout, clearTimeoutFn = globalThis.clearTimeout}) {
  const administratorNotices = createAdministratorNotices({ request });
  let source = null, epoch = 0, current = null;
  // Reflects connection state in the chat toggle when the shell control exists.
  function setConnecting(connecting) {
    const button = document.getElementById('chat-toggle-btn');
    if (!button) return;
    button.classList.toggle('connection-connecting', connecting);
    button.title = connecting ? 'Chat connecting…' : 'Toggle chat panel';
  }
  // Invalidates the active epoch, cancels its retry, and closes notices and stream state.
  function disconnect() {
    ++epoch;
    if (current && current.retry !== null) clearTimeoutFn(current.retry);
    current = null;
    administratorNotices.stop(); source?.close(); source = null; setConnecting(false);
  }
  // Rejects callbacks from any connection or session superseded by disconnect/open.
  function valid(state) { return current === state && state.epoch === epoch; }
  // Validates and deduplicates one sequenced event before publishing it to the panel.
  // Invalid events or a rejected render throw so the caller can request snapshot recovery.
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
  /** Fills message gaps from a session snapshot, coalescing concurrent recovery requests. */
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
          const historyRevision=data.session?.history_revision||0;
          if(historyRevision!==state.historyRevision){
            if(historyRevision>state.historyRevision){state.historyChanged=true;state.metadataChanged(data.session);}
            return;
          }
          if(state.historyChanged)return;
          // Include gaps below a newer live event; ID deduplication keeps rows.
          for (const item of [...data.messages].sort((a,b) => a.seq-b.seq)) deliver(state, item, true);
          state.last = Math.max(state.last, data.last_seq || 0);
          const viewRevision=data.app_view?.revision||0;
          if(viewRevision<=state.appViewRevision)data.app_view=null;
          else state.appViewRevision=viewRevision;
          state.componentRevision=data.component_revision||0;
          state.snapshotChanged(data);
        } while (state.again && valid(state));
        state.failures = 0;
        if(issues)issues.resolve('chat_sync',state.id);else clearNoticeFrom('chat-sync');
      } catch(error) {
        if (!valid(state)) return;
        issues?.fail('chat_sync',error,state.id);
        if (++state.failures <= 3) {
          state.retry = setTimeoutFn(() => {state.retry = null; void recover(state);}, 1000 * 2 ** (state.failures-1));
        } else if(!issues)note("Couldn't sync chat history", 'chat-sync');
      } finally { state.inFlight = null; }
    })();
    return state.inFlight;
  }
  // Tracks this session's run statuses and recovers the transcript after terminal changes.
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
  /** Loads a snapshot, invokes render, then opens the session SSE stream and its callbacks. */
  async function open(id, render, publish, metadataChanged = () => {}, snapshotChanged = () => {}) {
    disconnect();
    const turn = epoch;
    const data = await request('/sessions/' + encodeURIComponent(id));
    if (turn !== epoch) return;
    render(data);
    const state = current = {id, epoch:turn, publish, metadataChanged, snapshotChanged, historyRevision:data.session?.history_revision||0,historyChanged:false,last:data.last_seq || 0,
      componentRevision:data.component_revision||0,appViewRevision:data.app_view?.revision||0,seen:new Set(data.messages.map(item => item.id)), runs:new Map((data.runs || []).map(run => [run.id,run.status])),
      inFlight:null, again:false, retry:null, failures:0};
    administratorNotices.start(id);
    const streamUrl='/api/v3/sessions/' + encodeURIComponent(id) + '/events?after=' + state.last;
    source = createStream ? createStream(streamUrl) : new EventSourceClass(streamUrl);
    setConnecting(true);
    source.addEventListener('queue_updated', () => { if (valid(state)) void recover(state); });
    source.addEventListener('session_updated', event => { if (valid(state)) {try {const summary=JSON.parse(event.data).session;if((summary?.history_revision||0)>state.historyRevision)state.historyChanged=true;metadataChanged(summary);if((JSON.parse(event.data).app_view_revision||0)>state.appViewRevision||(JSON.parse(event.data).component_revision||0)>state.componentRevision)void recover(state);} catch {metadataChanged();}} });
    // Validates each SSE payload; rendering/parse failures trigger snapshot recovery.
    source.onmessage = event => {
      if (!valid(state)||state.historyChanged) return;
      try { deliver(state, JSON.parse(event.data)); }
      catch { void recover(state); }
    };
    // Marks transport loss and starts bounded snapshot recovery for the current epoch.
    source.onerror = () => { if (valid(state)) {setConnecting(true); void recover(state);} };
    // Clears the connection indicator and checks for messages missed before reconnect.
    source.onopen = () => { if (valid(state)) {setConnecting(false); void recover(state);} };
  }
  return {
    open, disconnect, updateRuns,
    /** Applies one revision-fenced queued-message delivery choice. */
    delivery:(id,mid,body)=>request('/sessions/'+encodeURIComponent(id)+'/messages/'+encodeURIComponent(mid)+'/delivery','PATCH',body),
    /** Resumes a held delivery queue with its expected revision and request ID. */
    resumeQueue:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/queue/resume','POST',body),
    /** Rewinds a session transcript to the requested public message boundary. */
    undo:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/undo','POST',body),
    /** Forks a session from the supplied message boundary. */
    fork:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/fork','POST',body),
    /** Reads the caller sharing state for a session. */
    sharing:id=>request('/sessions/'+encodeURIComponent(id)+'/sharing'),
    /** Creates a session invitation from the supplied request and expiry. */
    invite:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/invitations','POST',body),
    /** Revokes one invitation belonging to the session. */
    revokeInvite:(id,invite,body)=>request('/sessions/'+encodeURIComponent(id)+'/invitations/'+encodeURIComponent(invite),'DELETE',body),
    /** Redeems an invitation using the supplied token. */
    redeemInvite:body=>request('/chat-invitations/redeem','POST',body),
    /** Updates the caller participation state for a session. */
    participation:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/participation','PUT',body),
    /** Requests assistance for the selected session. */
    help:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/help','POST',body),
    /** Cancels the caller outstanding assistance request. */
    cancelHelp:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/help','DELETE',body),
    /** Reads recorded usage totals for one session. */
    usage:id=>request("/sessions/"+encodeURIComponent(id)+"/usage"),
    /** Reads administrator context-compaction settings. */
    contextConfig:()=>request("/admin/context-compaction"),
    /** Saves administrator context-compaction settings with PUT. */
    saveContextConfig:body=>request("/admin/context-compaction","PUT",body),
    /** Reads one session snapshot containing public messages and run state. */
    snapshot:id=>request('/sessions/'+encodeURIComponent(id)),
    /** Uploads a staged attachment to the selected session. */
    upload:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/attachments','POST',body),
    /** Requests a server-generated description for an uploaded attachment. */
    describeUpload:(id,aid)=>request('/sessions/'+encodeURIComponent(id)+'/attachments/'+encodeURIComponent(aid)+'/describe','POST',{}),
    /** Removes a staged attachment from a session. */
    removeUpload:(id,aid)=>request('/sessions/'+encodeURIComponent(id)+'/attachments/'+encodeURIComponent(aid),'DELETE'),
    /** Sends a voice audio frame for the selected session. */
    voiceAudio:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/audio','POST',body),
    /** Routes a voice utterance to the supported session action. */
    voiceRoute:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/route','POST',body),
    /** Updates voice conversation controls for a session. */
    voiceControl:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/control','POST',body),
    /** Reads the runtime's voice capabilities for one session. */
    voiceCapabilities:id=>request('/sessions/'+encodeURIComponent(id)+'/voice/capabilities'),
    /** Requests speech audio for text in the selected session. */
    voiceSpeech:(id,body)=>request('/sessions/'+encodeURIComponent(id)+'/voice/speech','POST',body),
    /** Reads session-specific mode/model and composer capability state. */
    controls: id => request('/sessions/' + encodeURIComponent(id) + '/controls'),
    /** Saves a session's execution-mode selection with PUT. */
    mode: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/mode', 'PUT', body),
    /** Saves a session's model selection with PUT. */
    model: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/model', 'PUT', body),
    /** Lists sessions for the requested lifecycle status. */
    sessions: (status = 'active') => request('/sessions?status=' + encodeURIComponent(status)),
    /** Searches sessions in pages of 20, optionally continuing a prior cursor. */
    search: ({q, status = 'active', continuation}) => {
      const params = new URLSearchParams({q, status, limit: '20'});
      if (continuation) params.set('continuation', continuation);
      return request('/sessions/search?' + params);
    },
    /** Applies revision-fenced title or pin metadata changes. */
    patch: (id, body) => request('/sessions/' + encodeURIComponent(id), 'PATCH', body),
    /** Archives a session with the caller's metadata revision. */
    archive: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/archive', 'POST', body),
    /** Unarchives an archived session using its current revision. */
    unarchive: (id, body) => request('/sessions/' + encodeURIComponent(id) + '/unarchive', 'POST', body),
    /** Permanently deletes an archived session through the same revision fence. */
    delete: (id, body) => request('/sessions/' + encodeURIComponent(id), 'DELETE', body),
    /** Acknowledges messages through a durable sequence cursor. */
    read: (id, through_seq) => request('/sessions/' + encodeURIComponent(id) + '/read', 'PUT', {through_seq}),
    /** Creates a session from the supplied agent and request identity. */
    create: body => request('/sessions', 'POST', body),
    /** Submits one user message and its attachment IDs to a session. */
    send: (id, body) => request('/sessions/' + id + '/messages', 'POST', body),
    /** Requests cancellation of a run. */
    cancel: (id, body) => request('/runs/' + id + '/cancel', 'POST', body),
    /** Submits a decision for a pending approval. */
    decide: (id, body) => request('/approvals/' + id + '/decision', 'POST', body),
    /** Resolves an uncertain run through its recovery endpoint. */
    recover: (id, body) => request('/runs/' + id + '/recovery', 'POST', body),
    /** Resumes a paused or stopped run. */
    resume: (id, body) => request('/runs/' + id + '/resume', 'POST', body)
  };
}
