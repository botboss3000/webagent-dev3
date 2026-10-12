const el = (tag, text) => { const n = document.createElement(tag); if (text !== undefined) n.textContent = text; return n; };
async function identity() {
  const r = await fetch('/api/v3/auth/me', { credentials: 'same-origin', cache: 'no-store' });
  if (!r.ok) throw Error('Sign in to manage this module');
  return r.json();
}
async function request(id, action, args, requestId = crypto.randomUUID()) {
  const me = await identity();
  const r = await fetch('/api/v3/modules/' + id + '/call', {
    method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token },
    body: JSON.stringify({ action, args, client_request_id: requestId }),
  });
  const data = await r.json().catch(() => ({}));
  if (!r.ok || data.ok === false) throw Error(data.error || data.message || 'Operation failed (' + r.status + ')');
  return data.result ?? data;
}
function field(form, label, type = 'text') {
  const wrap = el('label', label), input = el('input'); input.type = type;
  wrap.append(input); form.append(wrap); return input;
}

export async function start(mount) {
  const root = el('section'); root.className = 'module-page'; mount.replaceChildren(root);
  root.append(el('h2', 'Outbound notifications'));
  const notice = el('p'); notice.className = 'module-notice'; notice.setAttribute('role', 'status'); root.append(notice);
  const form = el('form'); root.append(form);
  const url = field(form, 'Receiver HTTPS URL', 'url'); url.required = true;
  const events = field(form, 'Events (comma-separated)'); events.value = 'run.completed,run.failed';
  const signing = field(form, 'Signing secret (64 hexadecimal characters)', 'password');
  signing.required = true; signing.autocomplete = 'off'; signing.pattern = '[a-fA-F0-9]{64}'; signing.minLength = 64; signing.maxLength = 64;
  const generate = el('button', 'Generate signing secret'); generate.type = 'button';
  generate.onclick = () => {
    signing.value = Array.from(crypto.getRandomValues(new Uint8Array(32)), v => v.toString(16).padStart(2, '0')).join('');
    signing.type = 'text'; createIntent = null;
    notice.textContent = 'Copy this secret to your receiver before saving. It is not returned after saving.';
  };
  form.append(generate);
  const create = el('button', 'Create paused subscription'); create.type = 'submit'; form.append(create);
  const refresh = el('button', 'Refresh'); root.append(refresh);
  const gap = el('div'); gap.setAttribute('role', 'status'); root.append(gap);
  const list = el('div'); root.append(list);
  root.append(el('h3', 'Delivery history'));
  const history = el('div'); root.append(history);
  const more = el('button', 'Load more deliveries'); more.hidden = true; root.append(more);
  let createIntent = null, historyAfter = null, historyEpoch = 0, gapIntent = null;
  const createFields = () => ({ url: url.value, events: events.value.split(',').map(x => x.trim()).filter(Boolean), signing_key: signing.value });
  // Keep retry identity only in this view's memory; never persist or log signing material.
  form.oninput = () => { if (createIntent && JSON.stringify(createFields()) !== createIntent.fingerprint) createIntent = null; };
  const perform = (action, args, requestId) => request('outbound_notifications', action, args, requestId);

  async function loadHistory(reset = false) {
    if (reset) { historyEpoch++; historyAfter = null; history.replaceChildren(); more.hidden = true; }
    const epoch = historyEpoch;
    more.disabled = true;
    try {
      const args = { limit: 100 }; if (historyAfter) args.after = historyAfter;
      const data = await perform('history', args);
      if (epoch !== historyEpoch) return;
      const rows = data.deliveries || [];
      if (!history.children.length && !rows.length) history.append(el('p', 'No deliveries recorded.'));
      for (const row of rows) {
        const article = el('article');
        article.append(el('p', row.status + ' · ' + row.attempts + ' attempt(s)' + (row.http_status ? ' · HTTP ' + row.http_status : '')));
        article.append(el('p', 'Delivery: ' + row.id), el('p', 'Subscription: ' + row.subscription));
        if (row.status === 'pending') article.append(el('p', 'Next attempt: ' + new Date(row.due_ms).toLocaleString()));
        if (row.status === 'uncertain_withdrawn') article.append(el('p', 'Delivery may have occurred before removal. Reconcile with the receiver before taking further action.'));
        history.append(article);
      }
      historyAfter = typeof data.after === 'string' && data.after ? data.after : null;
      more.hidden = !historyAfter;
    } finally { if (epoch === historyEpoch) more.disabled = false; }
  }

  async function checkGap(cursor) {
    gap.replaceChildren();
    const r = await fetch('/api/v3/admin/events?since=' + encodeURIComponent(cursor) + '&limit=1&wait=0', { credentials: 'same-origin', cache: 'no-store' });
    const data = await r.json().catch(() => ({}));
    if (r.status !== 410 || data.error !== 'cursor_expired') {
      if (!r.ok) throw Error('Event-feed status unavailable (' + r.status + ')');
      gapIntent = null; return;
    }
    if (!Number.isSafeInteger(data.resume_since) || data.resume_since < cursor) throw Error('Invalid event-feed resume cursor');
    const args = { expected_cursor: cursor, resume_since: data.resume_since };
    const fingerprint = JSON.stringify(args);
    if (!gapIntent || gapIntent.fingerprint !== fingerprint) gapIntent = { fingerprint, args, requestId: crypto.randomUUID() };
    gap.append(el('p', 'Events after cursor ' + cursor + ' through ' + data.pruned_through + ' have expired. Pending delivery envelopes are retained.'));
    const acknowledge = el('button', 'Acknowledge expired events and resume at ' + data.resume_since);
    acknowledge.onclick = async () => {
      if (!confirm('Acknowledge that expired source events cannot be delivered, and resume from cursor ' + data.resume_since + '?')) return;
      acknowledge.disabled = true;
      try {
        const intended = gapIntent;
        await perform('acknowledge_gap', intended.args, intended.requestId);
        gapIntent = null; await load(); notice.textContent = 'Event gap acknowledged explicitly.';
      } catch (e) { notice.textContent = e.message; } finally { acknowledge.disabled = false; }
    };
    gap.append(acknowledge);
  }

  async function load() {
    try {
      const me = await identity(); if (!me.is_admin) { root.hidden = true; return; } root.hidden = false;
      const data = await perform('list', {}); list.replaceChildren();
      for (const row of data.subscriptions || []) {
        const article = el('article'); article.append(el('h3', row.url), el('p', (row.enabled ? 'Enabled' : 'Paused') + ' · ' + row.events.join(', ')));
        const mutate = (label, op, args) => {
          const b = el('button', label);
          b.onclick = async () => {
            b.disabled = true;
            try { await perform(op, { id: row.id, expected_revision: row.revision, ...args() }); await load(); notice.textContent = 'Saved'; }
            catch (e) { notice.textContent = e.message; } finally { b.disabled = false; }
          }; article.append(b);
        };
        mutate(row.enabled ? 'Pause' : 'Enable', 'update', () => ({ enabled: !row.enabled }));
        mutate('Rotate key', 'rotate_key', () => {
          const key = prompt('Enter the new 64-character hex signing secret already configured at the receiver');
          if (!key) throw Error('Key rotation cancelled'); return { signing_key: key };
        });
        const remove = el('button', 'Delete');
        remove.onclick = async () => {
          if (!confirm('Remove this subscription and withdraw pending deliveries?')) return;
          try { await perform('delete', { id: row.id, expected_revision: row.revision }); await load(); }
          catch (e) { notice.textContent = e.message; }
        }; article.append(remove); list.append(article);
      }
      await Promise.all([loadHistory(true), checkGap(data.cursor)]);
      notice.textContent = 'Subscriptions loaded';
    } catch (e) { notice.textContent = e.message; }
  }
  form.onsubmit = async event => {
    event.preventDefault(); create.disabled = true;
    const fields = createFields(), fingerprint = JSON.stringify(fields);
    if (!createIntent || createIntent.fingerprint !== fingerprint) createIntent = { fingerprint, args: { id: crypto.randomUUID(), ...fields }, requestId: crypto.randomUUID() };
    try {
      await perform('create', createIntent.args, createIntent.requestId);
      createIntent = null; signing.value = ''; signing.type = 'password';
      await load(); notice.textContent = 'Created paused';
    } catch (e) { notice.textContent = e.message; } finally { create.disabled = false; }
  };
  more.onclick = async () => { try { await loadHistory(); } catch (e) { notice.textContent = e.message; } };
  refresh.onclick = load; window.addEventListener('v3-identity-changed', () => { createIntent = null; gapIntent = null; signing.value = ''; signing.type = 'password'; url.value = ''; list.replaceChildren(); gap.replaceChildren(); history.replaceChildren(); historyEpoch++; load(); }); await load();
}
