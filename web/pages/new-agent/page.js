// New agent: placeholder page. The real creation form is a later port.

export const meta = {
  title: 'New agent',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Name, model and tool grants will be chosen here.';
  card.append(hint);
  host.append(card);
}
