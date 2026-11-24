// Administrator dashboard: placeholder page. Lives under web/admin/, so the
// existing server gate answers 404 to anyone who is not an administrator.

export const meta = {
  title: 'Dashboard',
  note: 'Placeholder',
};

export function render(host) {
  const card = document.createElement('div');
  card.className = 'card';
  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Runs, builds and usage across the installation will be summarised here.';
  card.append(hint);
  host.append(card);
}
