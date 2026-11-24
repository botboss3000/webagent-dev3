// Administrator settings page: placeholder. Gated by the web/admin/ folder.

export const meta = {
  title: 'Settings',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Installation-wide configuration will be edited here.';
  card.append(hint);
  host.append(card);
}
