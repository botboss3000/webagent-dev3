// Locates administrator repository notices in the chat composer
import {createChatAction} from './chat-action-panel.js';
// The poller owns freshness and access; all visible cards come from chat notifications.
export function repositoryNoticeRenderer(documentObject=globalThis.document) {
  let region=null;
  return text=>{
    const parent=documentObject?.getElementById('chat-action-panel');if(!parent)return;
    if(!region||region.parentNode!==parent){
      region=createChatAction({title:'Repository status',message:'',documentObject});region.className+=' v3-repository-notice';
      region.setAttribute('role','status');region.setAttribute('aria-live','polite');parent.prepend(region);
    }
    region.querySelector('.chat-action-message').textContent=text;region.hidden=!text;
  };
}
// Poll notices for one selected session, fencing async results across stop/start transitions.
export function createAdministratorNotices({ request, render = repositoryNoticeRenderer(),
  setIntervalFn = globalThis.setInterval, clearIntervalFn = globalThis.clearInterval }) {
  let session = null, generation = 0, interval = null, inFlight = false, seen = '';
  // Invalidate pending results, clear the interval, and withdraw the notice.
  function stop() {
    ++generation; session = null; seen = '';
    if (interval !== null) clearIntervalFn(interval);
    interval = null; render('');
  }
  // Serialize reads, validate response fields, and render only for the still-current session.
  async function poll() {
    if (!session || inFlight) return;
    const id = session, turn = generation;
    inFlight = true;
    try {
      const result = await request('/sessions/' + encodeURIComponent(id) + '/administrator/notices');
      if (turn !== generation || id !== session) return;
      const notice = result?.notice;
      if (!notice || typeof notice.snapshot_sha256 !== 'string' ||
          !/^[a-f0-9]{64}$/.test(notice.snapshot_sha256) || typeof notice.dirty !== 'boolean' ||
          !Number.isSafeInteger(notice.ahead) || notice.ahead < 0 ||
          typeof notice.notification !== 'string' || !notice.notification.trim() ||
          notice.notification.length > 150) {
        seen = ''; render(''); return;
      }
      if (notice.snapshot_sha256 !== seen) {
        seen = notice.snapshot_sha256; render(notice.notification);
      }
    } catch {
      // A failed live read withdraws only this notice, leaving transport errors intact.
      if (turn === generation && id === session) { seen = ''; render(''); }
    } finally {
      inFlight = false;
      // A pending old-session request never overlaps the replacement poll or renders into it.
      if (turn !== generation && session) void poll();
    }
  }
  // Replace prior polling, ignore non-string, empty, or oversized session IDs, then poll immediately and every ten seconds.
  function start(id) {
    stop();
    if (typeof id !== 'string' || !id || id.length > 128) return;
    session = id;
    // The timer is session-scoped and cleared by stop before a replacement starts.
    interval = setIntervalFn(() => { void poll(); }, 10000);
    void poll();
  }
  return { start, stop };
}
