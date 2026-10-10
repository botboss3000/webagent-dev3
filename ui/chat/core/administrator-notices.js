// Repository notices are server-authored FYI, separate from chat connection errors.
export function repositoryNoticeRenderer(documentObject = globalThis.document) {
  let region = null;
  return text => {
    const parent = documentObject?.getElementById('chat-above-pill');
    if (!parent) return;
    if (!region || region.parentNode !== parent) {
      region = documentObject.createElement('p');
      region.className = 'ac-hint v3-repository-notice';
      region.setAttribute('role', 'status');
      region.setAttribute('aria-live', 'polite');
      parent.prepend(region);
    }
    region.textContent = text;
    region.hidden = !text;
  };
}


export function buildNoticeRenderer(documentObject = globalThis.document) {
  let region = null;
  return (items, review) => {
    const parent = documentObject?.getElementById('chat-above-pill');
    if (!parent) return;
    if (!region || region.parentNode !== parent) {
      region = documentObject.createElement('section');
      // The shared composer visibility rule recognizes repository notice surfaces.
      region.className = 'ac-hint v3-build-leftovers v3-repository-notice';
      region.setAttribute('role', 'status');
      region.setAttribute('aria-live', 'polite');
      parent.prepend(region);
    }
    region.replaceChildren();
    region.hidden = !items.length;
    for (const item of items) {
      const row = documentObject.createElement('p');
      const text = documentObject.createElement('span');
      text.textContent = item.notification + (Number.isSafeInteger(item.retained_bytes)
        ? ' Retained space: ' + (item.retained_bytes / 1024 ** 2).toFixed(1) + ' MiB.' : '');
      const button = documentObject.createElement('button');
      button.type = 'button';
      button.textContent = 'Mark reviewed';
      button.title = 'Clear this reminder after investigation. Retained work remains intact.';
      button.addEventListener('click', async () => {
        button.disabled = true;
        try { await review(item.id); }
        catch { button.textContent = 'Review unavailable; refresh'; }
        finally { button.disabled = false; }
      });
      row.append(text, button);
      region.append(row);
    }
  };
}

function validBuildNotices(value) {
  if (!Array.isArray(value) || value.length > 20) return [];
  return value.filter(item => item && /^[a-f0-9]{64}$/.test(item.id || '')
    && /^[A-Za-z0-9_-]{8,80}$/.test(item.task_id || '')
    && ['stale_open', 'failed', 'interrupted', 'uncertain', 'stale'].includes(item.status)
    && typeof item.notification === 'string' && item.notification.length > 0 && item.notification.length <= 512
    && (item.retained_bytes == null || (Number.isSafeInteger(item.retained_bytes) && item.retained_bytes >= 0)));
}

export function createAdministratorNotices({ request, render = repositoryNoticeRenderer(), renderBuilds = buildNoticeRenderer(),
  setIntervalFn = globalThis.setInterval, clearIntervalFn = globalThis.clearInterval }) {
  let session = null, generation = 0, interval = null, inFlight = false, seen = '', seenBuilds = '';
  function stop() {
    ++generation; session = null; seen = ''; seenBuilds = '';
    if (interval !== null) clearIntervalFn(interval);
    interval = null; render(''); renderBuilds([], null);
  }
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
  function start(id) {
    stop();
    if (typeof id !== 'string' || !id || id.length > 128) return;
    session = id;
    interval = setIntervalFn(() => { void poll(); }, 10000);
    void poll();
  }
  return { start, stop };
}
