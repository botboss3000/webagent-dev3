// @code-index {"version":1,"feature":"chat-administrator-notices","aliases":["repository status reminder","retained build notice"],"purpose":"Locates administrator repository and retained-build notices in the chat composer","span_lines":80,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider notice presentation preferences; access authority remains enforced by policy."]}}
import {createChatNotification} from './notification-panel.js';
// The poller owns freshness and access; all visible cards come from chat notifications.
export function repositoryNoticeRenderer(documentObject=globalThis.document) {
  let region=null;
  return text=>{
    const parent=documentObject?.getElementById('notification-panel');if(!parent)return;
    if(!region||region.parentNode!==parent){
      region=createChatNotification({title:'Repository status',message:'',documentObject});region.className+=' v3-repository-notice';
      region.setAttribute('role','status');region.setAttribute('aria-live','polite');parent.prepend(region);
    }
    region.querySelector('.chat-notification-message').textContent=text;region.hidden=!text;
  };
}
export function buildNoticeRenderer(documentObject=globalThis.document) {
  let region=null;
  return(items,review)=>{
    const parent=documentObject?.getElementById('notification-panel');if(!parent)return;
    if(!region||region.parentNode!==parent){region=documentObject.createElement('div');region.className='v3-build-leftovers v3-repository-notice';region.setAttribute('aria-live','polite');parent.prepend(region);}
    region.replaceChildren();region.hidden=!items.length;
    for(const item of items){
      const node=createChatNotification({title:'Retained work',message:item.notification+(Number.isSafeInteger(item.retained_bytes)?' Retained space: '+(item.retained_bytes/1024**2).toFixed(1)+' MiB.':''),documentObject,actions:[{
        label:'Mark reviewed',kind:'neutral',title:'Clear this reminder after investigation. Retained work remains intact.',
        run:async()=>{const button=node.querySelector('button');button.disabled=true;try{await review(item.id);}catch{button.textContent='Review unavailable; refresh';}finally{button.disabled=false;}}
      }]});region.append(node);
    }
  };
}

// Bound and validate response entries before they reach the notice renderer.
function validBuildNotices(value) {
  if (!Array.isArray(value) || value.length > 20) return [];
  // Retain only bounded, well-shaped notice rows with supported status values.
  return value.filter(item => item && /^[a-f0-9]{64}$/.test(item.id || '')
    && /^[A-Za-z0-9_-]{8,80}$/.test(item.task_id || '')
    && ['stale_open', 'failed', 'interrupted', 'uncertain', 'stale'].includes(item.status)
    && typeof item.notification === 'string' && item.notification.length > 0 && item.notification.length <= 512
    && (item.retained_bytes == null || (Number.isSafeInteger(item.retained_bytes) && item.retained_bytes >= 0)));
}

// Poll notices for one selected session, fencing async results across stop/start transitions.
export function createAdministratorNotices({ request, render = repositoryNoticeRenderer(), renderBuilds = buildNoticeRenderer(),
  setIntervalFn = globalThis.setInterval, clearIntervalFn = globalThis.clearInterval }) {
  let session = null, generation = 0, interval = null, inFlight = false, seen = '', seenBuilds = '';
  // Invalidate pending results, clear the interval, and withdraw both notice surfaces.
  function stop() {
    ++generation; session = null; seen = ''; seenBuilds = '';
    if (interval !== null) clearIntervalFn(interval);
    interval = null; render(''); renderBuilds([], null);
  }
  // Serialize reads, validate response fields, and render only for the still-current session.
  async function poll() {
    if (!session || inFlight) return;
    const id = session, turn = generation;
    inFlight = true;
    try {
      const result = await request('/sessions/' + encodeURIComponent(id) + '/administrator/notices');
      if (turn !== generation || id !== session) return;
      const items = result?.build_notices_unavailable ? [] : validBuildNotices(result?.build_notices);
      const revision = JSON.stringify(items);
      if (revision !== seenBuilds) {
        seenBuilds = revision;
        // Review is an explicit POST; refresh only while the same notice session remains active.
        renderBuilds(items, async noticeId => {
          if (turn !== generation || id !== session) return;
          await request('/sessions/' + encodeURIComponent(id) + '/administrator/notices/' + encodeURIComponent(noticeId) + '/review', 'POST', {});
          if (turn === generation && id === session) await poll();
        });
      }
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
      if (turn === generation && id === session) { seen = ''; seenBuilds = ''; render(''); renderBuilds([], null); }
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
