// Account: placeholder page reached from the sidebar footer button.

export const meta = {
  title: 'Manage account',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Account details, sessions and sign-out will live here.';
  card.append(hint);
  host.append(card);
}
