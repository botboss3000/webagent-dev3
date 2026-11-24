// Chat page: the destination of the phone header's chat icon. The chat itself is
// stripped, so this is the empty surface the ported components will land in.

export const meta = {
  title: 'Chat',
  note: 'Placeholder',
};

export function render(host) {
  const panel = document.createElement('div');
  panel.className = 'card chat-empty';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Chat lives here. Components land one at a time.';
  panel.append(hint);
  host.append(panel);
}
