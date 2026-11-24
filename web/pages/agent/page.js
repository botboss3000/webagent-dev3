// One agent's dashboard: placeholder page. `ctx.route` carries the agent id.

export const meta = {
  title: 'Agent',
  note: 'Placeholder',
};

export function render(host, ctx) {
  const id = decodeURIComponent((ctx.route || '').replace('/agents/', ''));
  const agent = (ctx.agents || []).find((item) => item.id === id);

  const card = document.createElement('div');
  card.className = 'card';

  const title = document.createElement('h2');
  title.className = 'card-title';
  title.textContent = agent ? agent.name : id || 'Agent';
  card.append(title);

  const hint = document.createElement('p');
  hint.className = 'card-hint';
  hint.textContent = 'Usage statistics, recent sessions and artifacts will be shown here, with a tile per settings area.';
  card.append(hint);

  host.append(card);

  const tiles = document.createElement('div');
  tiles.className = 'tiles';
  for (const name of ['Identity', 'Models', 'Abilities', 'Members', 'Artifacts']) {
    const tile = document.createElement('div');
    tile.className = 'tile tile-static';
    const head = document.createElement('div');
    head.className = 'tile-head';
    const label = document.createElement('span');
    label.className = 'tile-title';
    label.textContent = name;
    head.append(label);
    const note = document.createElement('span');
    note.className = 'tile-hint';
    note.textContent = 'Not ported yet';
    tile.append(head, note);
    tiles.append(tile);
  }
  host.append(tiles);
}
