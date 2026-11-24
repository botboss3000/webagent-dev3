// Mounts the chat panel partial into #chat-column and wires its behaviour.
// Behaviour only: markup lives in chat.html, styles in chat.css and the
// component stylesheets. No network calls — this is the ported panel.

const PARTIAL = new URL('./chat.html', import.meta.url);

export async function mountChat(host) {
  if (!host || host.dataset.mounted === 'yes') return host;
  const response = await fetch(PARTIAL, { headers: { accept: 'text/html' } });
  if (!response.ok) throw new Error(`chat partial ${response.status}`);
  host.innerHTML = await response.text();
  host.dataset.mounted = 'yes';
  wireComposer(host);
  wireActivity(host);
  return host;
}

// Send stays disabled until there is text, and Enter sends (Shift+Enter newlines).
function wireComposer(host) {
  const input = host.querySelector('#chat-input');
  const send = host.querySelector('.composer-send');
  const form = host.querySelector('#composer');
  if (!input || !send || !form) return;

  const sync = () => {
    send.disabled = input.value.trim().length === 0;
  };
  sync();
  input.addEventListener('input', sync);
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      if (!send.disabled) form.requestSubmit();
    }
  });
  form.addEventListener('submit', (event) => {
    // Nothing is wired to a backend yet: keep the draft, don't pretend to send.
    event.preventDefault();
  });
}

// Each activity group opens and closes as one block; its rows stay single lines.
function wireActivity(host) {
  for (const head of host.querySelectorAll('.activity-head')) {
    head.addEventListener('click', () => {
      const group = head.closest('.activity');
      if (!group) return;
      const open = !group.classList.contains('activity-open');
      group.classList.toggle('activity-open', open);
      head.setAttribute('aria-expanded', String(open));
    });
  }
}
