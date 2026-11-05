// Broker card page: one broker task's progress for administrators, the same card the
// chat's Broker chip opens (chat/controls/broker-chip.js renderCard). It reads
// GET /api/v3/admin/builds/tasks/{task_id}, counts in the browser and re-reads every 15 s
// while the newest job is queued or running. Linked from the chip's card, each Builds page
// job and Claude Code release updates: /v3-ui/broker-card.html?task_id=<Broker ID>.
import {renderCard} from './chat/controls/broker-chip.js';

const REFRESH_MS = 15000;

/** The Broker ID in a page query, or '' when it is missing or malformed. */
export function taskParam(search) {
  const id = new URLSearchParams(search).get('task_id') || '';
  return /^[A-Za-z0-9_-]{8,80}$/.test(id) ? id : '';
}

const root = typeof document !== 'undefined' ? document.getElementById('broker-card-root') : null;
const id = root ? taskParam(location.search) : '';

function message(text, link) {
  const p = document.createElement('p'); p.className = 'broker-note'; p.textContent = text;
  if (link) { const a = document.createElement('a'); a.href = link.href; a.textContent = link.text; p.append(' ', a); }
  root.replaceChildren(p);
}

let tick = null, refresh = null;
async function show() {
  try {
    const response = await fetch('/api/v3/admin/builds/tasks/' + encodeURIComponent(id), {credentials: 'same-origin', cache: 'no-store'});
    if (response.status === 401) { clearInterval(refresh); return message('Sign in to view this task.', {href: '/app', text: 'Sign in'}); }
    if (!response.ok) throw Error((await response.json().catch(() => ({}))).error || 'Task progress unavailable');
    const detail = await response.json();
    document.title = 'Broker · ' + (detail.task.name || id);
    const view = renderCard(detail.task, detail);
    const newest = detail.task.jobs?.[0];
    if (newest) {
      const builds = document.createElement('p'); builds.className = 'broker-note';
      const a = document.createElement('a'); a.href = '/app?tab=builds&job_id=' + encodeURIComponent(newest.job_id); a.textContent = 'Open in Builds page';
      builds.append(a); view.box.append(builds);
    }
    root.replaceChildren(view.box); window.lucide?.createIcons();
    clearInterval(tick); tick = setInterval(view.tick, 1000);
    if (!view.live) { clearInterval(refresh); refresh = null; }
  } catch (error) { message(error.message); }
}

if (root) {
  if (!id) message('Add ?task_id=<Broker ID> to the address.');
  else { void show(); refresh = setInterval(show, REFRESH_MS); }
}
