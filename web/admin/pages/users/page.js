// Administrator users page: placeholder. Gated by the web/admin/ folder.

export const meta = {
  title: 'Users',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Accounts, roles and invitations will be managed here.';
  card.append(hint);
  host.append(card);
}
