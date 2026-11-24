// All sessions: placeholder page. The real list is a later port.

export const meta = {
  title: 'All sessions',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Every conversation, grouped by agent, will be listed here.';
  card.append(hint);
  host.append(card);
}
