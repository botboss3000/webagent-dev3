// Wiki: placeholder page. Everyone sees it, not just administrators.

export const meta = {
  title: 'Wiki',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Shared notes and documentation pages will live here.';
  card.append(hint);
  host.append(card);
}
