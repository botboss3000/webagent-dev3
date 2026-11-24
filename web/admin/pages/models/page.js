// Administrator models page: placeholder. Gated by the web/admin/ folder.

export const meta = {
  title: 'Models',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Model connections, assignments and health will be shown here.';
  card.append(hint);
  host.append(card);
}
