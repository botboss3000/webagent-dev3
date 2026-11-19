// Saved settings that name a tool, ability or skill the catalog no longer has (renamed or
// removed in the app's source). They no longer apply; this panel lists them and clears them.
const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  return element;
};
// Built-in loader tools that have no tool.json, so the catalog does not list them.
const BUILT_IN_TOOLS = new Set(['list_abilities', 'load_ability']);
const KINDS = {
  ability_exposure: 'ability availability', ability_permissions: 'ability access',
  skill_exposure: 'skill availability', tool_exposure: 'tool availability', tool_permissions: 'tool access',
};

/** Every saved key that names nothing the catalog has: [{source, level, map, key}]. */
export function staleSettings(data, abilities) {
  const abilityIds = new Set(abilities.map(ability => ability.id));
  const tools = new Set(BUILT_IN_TOOLS);
  const skills = new Set();
  for (const ability of abilities) {
    for (const tool of ability.tools || []) tools.add(tool.name);
    skills.add(ability.id);
    for (const skill of [...(ability.general_skills || []), ...(ability.selectable_skills || [])]) {
      skills.add(ability.id + ':' + (typeof skill === 'string' ? skill : skill.skill_id || 'core'));
    }
  }
  const known = (map, key) => key === '*' || (map.startsWith('tool_') ? tools.has(key)
    : map.startsWith('skill_') ? skills.has(key) : abilityIds.has(key));
  const found = [];
  const scan = (source, level, value) => {
    for (const map of Object.keys(KINDS)) {
      for (const key of Object.keys(value?.[map] || {})) {
        if (!known(map, key)) found.push({ source, level, map, key });
      }
    }
  };
  for (const [id, level] of Object.entries(data.capability_levels?.levels || {})) scan('level', id, level);
  scan('permissions', null, data.permissions);
  return found;
}

/** The panel, or null when nothing is stale. Clearing saves the level or permissions without those keys. */
export function buildStaleSettings({ data, abilities, request, scope, rerender, status }) {
  const stale = staleSettings(data, abilities);
  if (!stale.length) return null;
  const levels = data.capability_levels?.levels || {};
  const panel = node('div', 'ac-list v3-stale-settings');
  const head = node('div', 'ac-ability-row');
  head.append(node('span', 'ac-ability-name', 'Saved settings for removed items'));
  const clearAll = node('button', 'ac-btn ac-btn-sm', 'Clear all');
  clearAll.type = 'button';
  head.append(clearAll);
  panel.append(head, node('p', 'ac-hint', 'These settings name a tool, ability or skill that was renamed or removed, so they no longer apply.'));
  const clear = async entries => {
    const byLevel = new Map();
    for (const entry of entries) {
      const id = entry.source === 'level' ? entry.level : '';
      if (!byLevel.has(id)) byLevel.set(id, []);
      byLevel.get(id).push(entry);
    }
    for (const [id, items] of byLevel) {
      if (id) {
        const level = structuredClone(levels[id]);
        for (const { map, key } of items) delete level[map][key];
        const saved = await request(scope + '/capability-levels/' + encodeURIComponent(id), 'PUT',
          { expected_revision: data.capability_levels.revision, level });
        data.capability_levels = saved.capability_levels;
      } else {
        const permissions = structuredClone(data.permissions);
        for (const { map, key } of items) delete permissions[map][key];
        const saved = await request(scope + '/ability-permissions', 'PUT', {
          expected_revision: data.permissions.revision,
          ability_permissions: permissions.ability_permissions || {},
          tool_permissions: permissions.tool_permissions || {},
        });
        data.permissions = { ...data.permissions, ...saved };
      }
    }
  };
  const run = async (entries, button) => {
    button.disabled = true;
    try {
      await clear(entries);
      status.textContent = entries.length === 1 ? 'Cleared one stale setting.' : 'Cleared ' + entries.length + ' stale settings.';
      await rerender({});
    } catch (error) {
      status.textContent = 'Could not clear: ' + (error.status === 409 ? 'settings changed elsewhere. Reload and try again.' : error.message);
      button.disabled = false;
    }
  };
  clearAll.onclick = () => run(stale, clearAll);
  for (const entry of stale) {
    const row = node('div', 'ac-ability-row');
    const label = node('span', 'ac-ability-label');
    label.append(node('span', 'ac-ability-name', entry.key),
      node('span', 'ac-ability-desc', (entry.source === 'level' ? (levels[entry.level]?.name || entry.level) + ' level · ' : 'Agent permissions · ') + KINDS[entry.map]));
    const button = node('button', 'ac-btn ac-btn-sm', 'Clear');
    button.type = 'button';
    button.onclick = () => run([entry], button);
    row.append(label, button);
    panel.append(row);
  }
  return panel;
}
