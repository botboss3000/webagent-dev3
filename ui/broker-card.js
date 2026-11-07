// Broker card page: one broker task's progress, public (no sign-in), the same card the
// chat's Broker chip opens (chat/controls/broker-chip.js renderCard). It reads
// GET /api/v3/builds/tasks/{task_id} (names, states and timings only), counts in the browser and re-reads every 15 s
// while the newest job is queued or running. A failed read keeps the last card, says so with a
// Retry now link and retries sooner (2 s, doubling, up to 15 s), even after the job finished. Linked from the chip's card, each Builds page
// job and Claude Code release updates: /v3-ui/broker-card.html?task_id=<Broker ID>.
import {renderCard} from './chat/controls/broker-chip.js';

const REFRESH_MS = 15000, FETCH_TIMEOUT_MS = 10000;

/** Delay before the next read after `failures` failed reads in a row. */
export function retryMs(failures) {
  return Math.min(2000 * 2 ** (failures - 1), REFRESH_MS);
}

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

let tick = null, timer = null, failures = 0, shown = false;
function retryNote(text) {
  const p = document.createElement('p'); p.className = 'broker-note'; p.dataset.retry = '';
  const a = document.createElement('a'); a.href = location.href; a.textContent = 'Retry now';
  a.addEventListener('click', (event) => { event.preventDefault(); clearTimeout(timer); void show(); });
  p.append(text + ' Retrying in ' + Math.round(retryMs(failures) / 1000) + ' s. ', a);
  return p;
}
async function show() {
  try {
    const response = await fetch('/api/v3/builds/tasks/' + encodeURIComponent(id), {cache: 'no-store', signal: AbortSignal.timeout(FETCH_TIMEOUT_MS)});
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
    failures = 0; shown = true;
    if (view.live) timer = setTimeout(show, REFRESH_MS);
  } catch (error) {
    failures += 1;
    const text = error.name === 'TimeoutError' || error.message === 'Failed to fetch' ? 'Could not reach the server.' : error.message;
    root.querySelector('[data-retry]')?.remove();
    if (shown) root.prepend(retryNote(text)); else root.replaceChildren(retryNote(text));
    timer = setTimeout(show, retryMs(failures));
  }
}

if (root) {
  if (!id) message('Add ?task_id=<Broker ID> to the address.');
  else void show();
}
