import { buildSourceControlControls } from "./source-control.js";
// V3 data adapter for the v2 Abilities composition, prompts group, and ability rows.
import { buildAbilitiesOverview } from './overview.js';
import { buildAdministratorControls } from './administrator.js';
import { buildBrowserSites } from './browser-sites.js';
import { buildMcpControls } from './mcp.js';
import { buildPromptsGroup } from './prompts.js';
import { buildGroup, setGroupStatus } from './table.js';
import { buildPolicyControls, effectiveLabel, readinessLabel } from './policy.js';

import { _buildMoreButton, _buildToolRow, _buildAbilitySettingsList, closeFloatingMenu } from '../../../../shared/ability-row-controls.js';
import { buildSkillRow } from './rows.js';
import { createSettingsRegister, createSettingsRow } from '../../../../shared/settings-register.js';
import { _markSaving, _flashSaveCheck } from '../../../../shared/models/dom-utils.js';

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  return element;
};
const chevronSvg = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="9 18 15 12 9 6"/></svg>';
const modes = ['off', 'on_request', 'loaded'];
// Core is set only by tool.json or skill.json and is shown as a locked pill.
const modeLabel = mode => ({ loaded: 'Skill loaded', on_request: 'Skill on request', off: 'Off', core: 'Core' })[mode] || mode;
// Register icons are raw SVG paths: one per ability group, taken from its first ability.
const ICONS = {
  box: '<path d="M12 3 3 7.5v9L12 21l9-4.5v-9L12 3Z"/><path d="M3 7.5 12 12l9-4.5M12 12v9"/>',
  book: '<path d="M4 5a2 2 0 0 1 2-2h12v18H6a2 2 0 0 1-2-2Z"/><path d="M8 3v18"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3a15 15 0 0 1 0 18 15 15 0 0 1 0-18Z"/>',
  terminal: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M13 15h4"/>',
  wrench: '<path d="M14.5 6.5a4 4 0 0 0 5 5L10 21l-4-4 8.5-8.5Z"/>',
  'file-text': '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8Z"/><path d="M14 3v5h5M9 13h6M9 17h4"/>',
  shield: '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10Z"/>',
  cpu: '<rect x="5" y="5" width="14" height="14" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3"/>',
  image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="2"/><path d="m4 18 5-5 4 4 3-3 4 4"/>',
  database: '<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/>',
  users: '<circle cx="9" cy="8" r="3"/><path d="M3 20a6 6 0 0 1 12 0M16 5.5a3 3 0 0 1 0 5.5M18 20a6 6 0 0 0-3-5.2"/>',
  message: '<path d="M21 12a8 8 0 0 1-8 8H8l-4 3v-6a8 8 0 1 1 17-5Z"/>',
  'shopping-bag': '<path d="M6 8h12l1 12H5L6 8Z"/><path d="M9 8V6a3 3 0 0 1 6 0v2"/>',
  'credit-card': '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 10h18"/>',
  code: '<path d="m9 18-6-6 6-6M15 6l6 6-6 6"/>',
  smartphone: '<rect x="7" y="2" width="10" height="20" rx="2"/><path d="M11 18h2"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2"/>',
  briefcase: '<rect x="3" y="7" width="18" height="13" rx="2"/><path d="M9 7V5a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2M3 12h18"/>',
  clipboard: '<rect x="6" y="4" width="12" height="17" rx="2"/><path d="M9 4V3h6v1M9 10h6M9 14h4"/>',
};
// The tile names the category, so a group maps by name rather than by its first ability.
const GROUP_ICONS = [[/administrator/, 'shield'], [/core/, 'cpu'], [/knowledge/, 'book'],
  [/productiv/, 'clipboard'], [/web/, 'globe'], [/communicat/, 'message'], [/social/, 'users'],
  [/commerce/, 'shopping-bag'], [/payment/, 'credit-card'], [/develop/, 'code'],
  [/crm|marketing/, 'briefcase'], [/device/, 'smartphone'], [/system/, 'settings'], [/prompt/, 'file-text']];
const iconFor = group => {
  const hit = GROUP_ICONS.find(([pattern]) => pattern.test(group.name.toLowerCase()));
  return ICONS[hit ? hit[1] : 'shield'];
};
const groupHint = group => {
  const names = group.items.map(item => item.name);
  const more = names.length > 3 ? ' +' + (names.length - 3) + ' more' : '';
  return names.length + (names.length === 1 ? ' ability · ' : ' abilities · ') + names.slice(0, 3).join(', ') + more;
};
let searchText = '';
let expandedAbility = null;

// The selected tenant and server authority govern every read and save on this page.
export async function renderAbilities(host, request, options) {
  // LIVE-CONTEXT-CHART: this page owns one poll and one visibility listener; a
  // re-render away from it releases both before starting the next.
  if (host._abilityPoll) { clearInterval(host._abilityPoll); host._abilityPoll = null; }
  if (host._abilityVisibilityHandler) {
    document.removeEventListener('visibilitychange', host._abilityVisibilityHandler);
    host._abilityVisibilityHandler = null;
  }
  const { agentId, levelId = 'operator' } = options;
  const scope = '/agents/' + encodeURIComponent(agentId);
  const generation = (host._abilityGeneration || 0) + 1;
  host._abilityGeneration = generation;
  // Start every read at once; manager-only reads are refused for members and ignored below.
  const managed = (path, fallback) => request(scope + path).catch(() => fallback);
  const effectiveRead = levelId
    ? request(scope + '/abilities/effective?' + new URLSearchParams({ level_id: levelId })) : null;
  effectiveRead?.catch(() => {});
  const reads = [managed('/slots', { slots: [] }), managed('/context-composition', { tokens: 0 }),
    managed('/repository-connections', { load_failed: true }), managed('/source-control/sites', { load_failed: true })];
  const data = await request(scope + '/abilities');
  const canManage = data.authority?.can_manage === true;
  const [slotsResult, compositionResult, repository, sourceSites] = canManage
    ? await Promise.all(reads) : [{ slots: [] }, { tokens: 0 }, null, null];
  const effective = canManage && effectiveRead ? await effectiveRead : data;
  if (host._abilityGeneration !== generation) return;
  const effectiveAbilities = effective.abilities || [];
  const projection = ability => effectiveAbilities.find(item => item.id === ability.id) || ability;
  const rerender = next => renderAbilities(host, request, { ...options, ...next });
  const abilities = structuredClone(data.abilities || []);
  const selectedLevel=data.capability_levels?.levels?.[levelId];
  if(selectedLevel&&levelId!=='operator'){
    const mode=(map,key,fallback)=>map?.[key]||map?.['*']||fallback;
    for(const ability of abilities){
      ability.visibility=mode(selectedLevel.ability_exposure,ability.id,ability.visibility);
      if(ability.skill_visibility!=='core')ability.skill_visibility=mode(selectedLevel.skill_exposure,ability.id,ability.skill_visibility);
      for(const skill of ability.selectable_skills||[])skill.mode=mode(selectedLevel.skill_exposure,ability.id+':'+skill.skill_id,skill.mode);
      for(const tool of ability.tools||[])tool.visibility=mode(selectedLevel.tool_exposure,tool.name,tool.visibility||'on_request');
    }
  }
  const slots = slotsResult.slots || [];
  const groups = new Map();
  for (const ability of abilities) {
    const key = ability.group || 'Other';
    if (!groups.has(key)) groups.set(key, { name: ability.group_name || key, items: [] });
    groups.get(key).items.push(ability);
  }
  const page = node('div', 'conn-tab-content v3-abilities-page');
  const overview = buildAbilitiesOverview();
  overview.update(compositionResult, abilities, slots);
  // LIVE-CONTEXT-CHART: the composition read is a server-side estimate over the
  // persisted configuration, so a toggle changes it without changing this page's
  // rows. Re-read only that estimate and repaint only the chart — never re-render
  // the abilities page or reload the browser page for it.
  let compositionRequest = 0;
  let compositionRetry = null;
  // The revision stamp of the estimate currently painted. The poll below re-reads
  // the estimate only when the server stamp differs from it.
  let compositionStamp = compositionResult?.revision ?? null;
  const refreshComposition = (attempt = 0) => {
    if (host._abilityGeneration !== generation || !overview.isConnected) return;
    const serial = ++compositionRequest;
    request(scope + '/context-composition').then(composition => {
      if (serial !== compositionRequest || host._abilityGeneration !== generation || !overview.isConnected) return;
      compositionStamp = composition?.revision ?? null;
      // 'busy' means the pointer or keyboard is inspecting the chart; retry a
      // couple of times, then leave it to the poll.
      if (overview.update(composition, abilities, slots, { live: true }) === 'busy' && attempt < 2) {
        clearTimeout(compositionRetry);
        compositionRetry = setTimeout(() => refreshComposition(attempt + 1), 900);
      }
    }).catch(() => {});
  };
  // Poll the cheap revision stamp instead of the estimate: one save-triggered
  // re-read still happens below, so a change made here is immediate; the stamp
  // covers configuration saved in another tab or chat.
  let revisionRequest = 0;
  const checkComposition = (force = false) => {
    if (host._abilityGeneration !== generation || !overview.isConnected) return;
    const serial = ++revisionRequest;
    request(scope + '/context-composition/revision').then(payload => {
      if (serial !== revisionRequest || host._abilityGeneration !== generation || !overview.isConnected) return;
      const revision = payload?.revision ?? null;
      if (!force && (revision === null || revision === compositionStamp)) return;
      compositionStamp = revision;
      refreshComposition();
    }).catch(() => {});
  };
  // Keep the legend's inventory counts in step with a save before the chart
  // refreshes: the estimate carries token totals, while the counts come from this
  // page's ability model.
  const applySavedMode = (ability, kind, mode, name = null) => {
    if (kind === 'ability') { ability.visibility = mode; return; }
    if (kind === 'tool') {
      const tool = (ability.tools || []).find(item => item.name === name);
      if (tool) tool.mode = mode;
      return;
    }
    if (!name || name === 'core') ability.skill_visibility = mode;
    const skill = [...(ability.general_skills || []), ...(ability.selectable_skills || [])]
      .find(item => (item.skill_id || 'core') === (name || 'core'));
    if (skill) skill.mode = mode;
  };
  const status = node('p', 'v3-ability-status');
  status.setAttribute('role', 'status');
  status.textContent = options.administratorNotice || '';
  const search = node('input', 'ac-input v3-ability-search');
  search.type = 'search';
  search.placeholder = 'Search abilities, skills, tools and prompts';
  search.setAttribute('aria-label', 'Search abilities');
  search.value = searchText;
  const prompts = canManage ? await buildPromptsGroup(request, agentId) : null;
  const levelSelector = buildPolicyControls({ data, levelId, rerender, status });
  if (host._abilityGeneration !== generation) return;
  overview.querySelector('[data-tier-selector]').replaceChildren(levelSelector);

  // The spinner covers the control until the save returns, then a check; the page
  // keeps the returned revision instead of re-reading everything. A failure leaves
  // the control unchanged and shows the reason in red under its row.
  const save = async (ability, kind, mode, control, toolName = null) => {
    const input = control.querySelector('input') || control;
    const row = control.closest('.ac-ability-row');
    row?.nextElementSibling?.classList.contains('v3-toggle-error') && row.nextElementSibling.remove();
    input.disabled = true;
    _markSaving(control);
    try {
      if (levelId !== 'operator') {
        const level = structuredClone(data.capability_levels.levels[levelId]);
        const key = kind === 'ability' ? 'ability_exposure' : kind === 'tool' ? 'tool_exposure' : 'skill_exposure';
        const id = kind === 'tool' ? toolName : kind === 'skill' && toolName && toolName !== 'core' ? ability.id + ':' + toolName : ability.id;
        level[key] ||= {};
        level[key][id] = mode;
        const saved = await request(scope + '/capability-levels/' + encodeURIComponent(levelId), 'PUT',
          { expected_revision: data.capability_levels.revision, level });
        data.capability_levels = saved.capability_levels;
      } else {
        const path = scope + '/abilities/' + encodeURIComponent(ability.id) +
          (kind === 'tool' ? '/tools/' + encodeURIComponent(toolName) + '/mode' :
            kind === 'skill' && toolName && toolName !== 'core'
              ? '/skills/' + encodeURIComponent(toolName) + '/visibility'
              : '/' + (kind === 'skill' ? 'skill-visibility' : 'visibility'));
        const saved = await request(path, 'PUT', { visibility: mode, expected_revision: data.revision });
        data.revision = saved.revision;
      }
      applySavedMode(ability, kind, mode, toolName);
      // The save changed the persisted configuration this estimate is computed from.
      refreshComposition();
      if (input.type === 'checkbox') {
        input.checked = mode !== 'off';
        input.setAttribute('aria-label', input.getAttribute('aria-label').replace(/: \w+\./, ': ' + mode + '.'));
      }
      const card = kind === 'ability' && control.closest('.v3-ability');
      if (card) { card.classList.toggle('enabled', mode !== 'off'); card.dataset.exposure = mode; }
      input.disabled = false;
      _flashSaveCheck(control, true);
    } catch (error) {
      const message = 'Could not change availability: ' + (error.status === 409
        ? 'settings changed elsewhere. Reload and try again.' : error.message || 'unknown error');
      _flashSaveCheck(control, false, message);
      const note = node('p', 'v3-toggle-error', message);
      note.setAttribute('role', 'alert');
      row ? row.after(note) : (status.textContent = message);
      input.disabled = false;
      throw error;
    }
  };
  const modeMenu = (ability, kind, current, toolName = null, anchorOnly = false) => {
    const trigger = _buildMoreButton(() => modes.map(mode => ({
      icon: mode === 'off' ? 'eye-off' : mode === 'loaded' ? 'eye' : 'download',
      label: modeLabel(mode), checked: trigger._mode === mode,
      disabled: !canManage,
      action: () => save(ability, kind, mode, trigger._floatingMenuOwner || trigger, toolName)
        .then(() => { trigger._mode = mode; }, () => {}),
    })), { title: 'Choose ' + kind + ' availability' });
    trigger._mode = current;
    if (anchorOnly) trigger.classList.add('ac-ability-more-anchor');
    return trigger;
  };
  const openToolSkill = async (ability, tool) => {
    const dialog = node('dialog', 'v3-tool-skill-dialog');
    const heading = node('h3', '', tool + '.skill.md');
    const pre = node('pre', '', 'Loading tool skill…');
    const close = node('button', 'ac-btn ac-btn-sm', 'Close');
    close.type = 'button';
    close.addEventListener('click', () => dialog.close());
    dialog.append(heading, pre, close);
    dialog.addEventListener('close', () => dialog.remove(), { once: true });
    document.body.append(dialog);
    dialog.showModal();
    try {
      const result = await request(scope + '/abilities/' + encodeURIComponent(ability.id) +
        '/tools/' + encodeURIComponent(tool) + '/skill-content');
      if (dialog.isConnected) pre.textContent = result.content || 'No skill text.';
    } catch (error) {
      if (dialog.isConnected) pre.textContent = error.message || 'Could not load tool skill.';
    }
  };
  const toolPolicy = () => data.capability_levels?.levels?.[levelId] || data.permissions;
  // Read-only mark: the access a manifest declares for this level, applied only once the
  // ability is switched on (a new ability stays off for existing agents).
  const markDefault = (row, value) => {
    if (!value) return;
    const label = { deny: 'Deny', require_approval: 'Require approval', allow: 'Allow', loaded: modeLabel('loaded'), on_request: modeLabel('on_request'), off: 'Off', on: 'On' }[value] || value;
    const anchor = row.querySelector('.ac-ability-desc') || row.querySelector('.ac-ability-name');
    const mark = node('div', 'ac-hint v3-default-mark', 'Default: ' + label);
    mark.style.whiteSpace = 'nowrap'; // Keep "Default: Auto" on one line on phones.
    anchor?.after(mark);
  };
  const buildAbilityBody = (detail, ability) => {
    if (detail.childElementCount) return;
    if (ability.id === 'mcp') detail.append(buildMcpControls({ ability, request, scope,
      canManage, levelId, status }));
    const tools = _buildAbilitySettingsList();
    tools.classList.add('v3-ability-tools');
    const generalSkills = ability.general_skills ||
      (ability.has_skill ? [{ skill_id: 'core', title: 'Skill',
        when_to_use: ability.skill_summary || '', mode: ability.skill_visibility || 'on_request' }] : []);
    for (const entry of generalSkills) {
      const skill = buildSkillRow(entry, {
        canEdit: canManage && levelId==='operator',
        menu: modeMenu(ability, 'skill', entry.mode || 'on_request', entry.skill_id, true),
        loadContent: () => request(scope + '/abilities/' + encodeURIComponent(ability.id) +
          (entry.skill_id === 'core' ? '/skill-content' :
            '/skills/' + encodeURIComponent(entry.skill_id) + '/content')),
      });
      // A core skill's mode is fixed, so it shows no default.
      if (entry.mode !== 'core') markDefault(skill.row, entry.default_mode);
      tools.append(skill.row, skill.body);
    }
    if (ability.id === "git_control" && canManage) tools.append(buildSourceControlControls({ data: sourceSites, request, scope, enabled: levelId === "operator", status, rerender }));
    const administrator = buildAdministratorControls({ ability, data, repository, request, scope,
      levelId, status,
      // A settings save re-renders the page; keep this ability open, like the tool rows do.
      rerender: options => { expandedAbility = ability.id; return rerender(options); } });
    // Web Search's provider settings stay below its tools; other abilities lead with theirs.
    if (administrator && ability.id !== 'web_search') {
      for (const setting of administrator.querySelectorAll(':scope > .ac-ability-row')) tools.append(setting);
    }
    // Every user sees their own saved sign-ins, whoever manages the agent.
    if (ability.id === 'browser_control') tools.append(buildBrowserSites({ request }));
    for (const tool of ability.tools || []) {
      const effectiveTool = (projection(ability).tools || []).find(item => item.name === tool.name) || tool;
      const saveToolPermission = async mode => {
        const level = data.capability_levels?.levels?.[levelId];
        const policy = structuredClone(level || data.permissions || {});
        policy.tool_permissions ||= {};
        // Choosing the <tool>.tool.json default clears this level's override.
        if (mode === effectiveTool.default) delete policy.tool_permissions[tool.name];
        else policy.tool_permissions[tool.name] = mode;
        try {
          // Update this page's copy from the response instead of re-reading the whole
          // page: the toggle already shows the choice, and the next save needs the new revision.
          if (level) {
            const saved = await request(scope + '/capability-levels/' + encodeURIComponent(levelId), 'PUT',
              { expected_revision: data.capability_levels.revision, level: policy });
            data.capability_levels = saved.capability_levels;
          } else {
            const saved = await request(scope + '/ability-permissions', 'PUT', {
              expected_revision: data.permissions.revision,
              ability_permissions: policy.ability_permissions || {},
              tool_permissions: policy.tool_permissions,
            });
            data.permissions = { ...data.permissions, ...saved };
          }
          status.textContent = 'Tool permission saved.';
          refreshComposition();
        } catch (error) {
          status.textContent = error.status === 409 ? 'Settings changed. Reload before saving: ' + error.message : error.message;
          throw error;
        }
      };
      const toolRow = _buildToolRow({
        ...tool,
        description: tool.description || '',
        callCount: tool.call_count,
        // The control edits the agent's configured layer (the selected level when
        // one exists, as saveToolPermission writes). Effective access can still be
        // Deny because of the account tier, visibility or port status.
        permMode: toolPolicy()?.tool_permissions?.[tool.name] || effectiveTool.default || 'require_approval',
        locked: tool.locked === true || tool.mode === 'core',
        role_blocked: tool.role_blocked === true || effectiveTool.role_blocked === true,
        tierBlocked: effectiveTool.effective?.reason === 'tier_denied',
      }, {
        canEdit: canManage,
        showPerm: true,
        abilityId: ability.id,
        hasToolSkill: tool.has_tool_skill === true,
        onOpenToolSkill: name => openToolSkill(ability, name),
        onExecutionChange: async (_, settings) => {
          try {
            const saved = await request(scope + '/abilities/' + encodeURIComponent(ability.id) +
              '/tools/' + encodeURIComponent(tool.name) + '/execution', 'PUT',
              { expected_revision: data.revision, settings });
            data.revision = saved.revision;
            status.textContent = 'Tool execution settings saved.';
            refreshComposition();
          } catch (error) {
            status.textContent = error.status === 409
              ? 'Settings changed. Reload before saving: ' + error.message : error.message;
            throw error;
          }
        },
        onResultDeduplicationChange: async (_, included) => {
          try {
            const saved = await request(scope + '/abilities/' + encodeURIComponent(ability.id) +
              '/tools/' + encodeURIComponent(tool.name) + '/result-deduplication', 'PUT',
              { expected_revision: data.revision, included });
            data.revision = saved.revision;
            status.textContent = 'Duplicate-result setting saved.';
            refreshComposition();
          } catch (error) {
            status.textContent = error.status === 409
              ? 'Settings changed. Reload before saving: ' + error.message : error.message;
            throw error;
          }
        },
        onPermissionChange: (_, mode) => saveToolPermission(mode),
        onVisibilityChange: (_, mode) => save(ability, 'tool', mode, toolRow.querySelector('.ac-tri'), tool.name),
      });
      if (tool.supported === false) toolRow.querySelector('.ac-tool-name').style.color = 'var(--danger)';
      if (tool.locked !== true && tool.mode !== 'core') markDefault(toolRow, effectiveTool.default);
      toolRow.title = [tool.supported ? '' : 'Unavailable in v3', effectiveLabel(effectiveTool)].filter(Boolean).join(' · ');
      tools.append(toolRow);
    }
    detail.append(tools);
    if (administrator) detail.append(administrator);
    if (ability.migration_note) detail.append(node('p', 'v3-ability-note', ability.migration_note));
  };
  // Two-stage register: the list shows one row per ability group, and a group's page
  // lists the abilities inside that group (each still expands to its own settings).
  const categories = [...groups].map(([groupId, group]) => ({
    id: 'g-' + groupId, title: group.name, hint: groupHint(group), icon: iconFor(group),
  }));
  if (prompts) categories.push({
    id: 'prompts', title: 'Prompts',
    hint: slots.length + (slots.length === 1 ? ' prompt slot' : ' prompt slots') + ' configured for this agent',
    icon: ICONS['file-text'],
  });
  page.append(overview, search);
  const register = createSettingsRegister(page, {label: 'Ability groups', categories});
  const abilityPages = new Map();
  const rowsBySection = new Map();
  for (const [groupId, group] of groups) {
    const section = createSettingsRow(register.stage, {id: 'g-' + groupId, title: group.name});
    // Rows attach after the register builds its pages, so the first ability's switch
    // is not mistaken for a page-level toggle.
    const rows = [];
    rowsBySection.set(section, rows);
    for (const ability of group.items) {
      const row = node('div', 'conn-card ac-row v3-ability' +
        (ability.visibility !== 'off' ? ' enabled' : ''));
      row.dataset.abilityId = ability.id;
      row.dataset.exposure = ability.visibility;
      const header = node('div', 'conn-card-header ac-ability-row conn-card-header-clickable');
      const chevron = node('span', 'ac-row-chevron ac-row-chevron--left');
      chevron.innerHTML = chevronSvg;
      const icon = node('span', 'conn-icon ac-ability-icon');
      const iconGlyph = node('i');
      iconGlyph.setAttribute('data-lucide', ability.icon || 'box');
      iconGlyph.style.cssText = 'width:16px;height:16px';
      icon.append(iconGlyph);
      if (ability.color) icon.style.color = ability.color;
      const label = node('div', 'ac-ability-label');
      const name = node('div', 'conn-name ac-ability-name', ability.name);
      // Port status is independent of visibility, credentials and caller permissions.
      const incomplete = ['pending', 'placeholder', 'partial'].includes(ability.readiness) ||
        ability.installed === false || (ability.tools || []).some(tool => tool.supported === false);
      if (incomplete) name.style.color = 'var(--danger)';
      label.append(name, node('div', 'ac-ability-desc', ability.description));
      const projected = projection(ability);
      // Defaults depend on the viewed level, so read them from its projection.
      const declared = projected.default ?? ability.default;
      markDefault(label, declared && (declared === 'off' ? 'off' : 'on'));
      const availability = node('span', 'ac-ability-status',
        [readinessLabel(ability), effectiveLabel(projected)].filter(Boolean).join(' · '));
      if (projected.effective?.reason === 'tier_denied') {
        // Blocked abilities stay configurable and visible to the agent; say why they fail.
        availability.textContent = 'Unavailable on the agent owner’s tier';
        availability.style.color = 'var(--danger)';
      }
      const roleBlocked = ability.role_blocked === true || projected.role_blocked === true;
      if (roleBlocked) {
        // role_access excludes the viewed level: show it locked with a muted note.
        availability.textContent = 'Not available to this level';
        availability.classList.add('v3-role-note');
      }
      availability.title = ['Declared: ' + !!ability.declared, 'Installed: ' + !!ability.installed,
        'Configured: ' + !!ability.configured, readinessLabel(ability)].join(' · ');
      const toggleWrap = node('label', 'conn-toggle-wrap ac-ability-toggle-wrap');
      const toggle = node('input', 'conn-toggle');
      toggle.type = 'checkbox';
      // Base holds the loaders and core tools, so it is always on.
      toggle.disabled = !canManage || ability.id === 'base' || roleBlocked;
      toggle.checked = ability.visibility !== 'off';
      toggle.setAttribute('aria-label', ability.name + ' on or off');
      // An ability is only a group: On lets its tools and skills follow their own settings.
      // The switch moves only after the save is re-read.
      toggle.addEventListener('click', event => {
        event.preventDefault();
        event.stopPropagation();
        save(ability, 'ability', ability.visibility !== 'off' ? 'off' : 'loaded', toggleWrap).catch(() => {});
      });
      toggleWrap.append(toggle, node('span', 'conn-toggle-track'));
      header.append(chevron, icon, label, toggleWrap);
      header.title = availability.title + ' · ' + availability.textContent;
      const detail = node('div', 'ac-ability-body');
      const open = () => {
        const expanded = row.classList.toggle('expanded');
        row.classList.toggle('conn-card-expanded', expanded);
        header.setAttribute('aria-expanded', String(expanded));
        if (expanded) buildAbilityBody(detail, ability);
      };
      header.tabIndex = 0;
      header.setAttribute('role', 'button');
      header.setAttribute('aria-expanded', 'false');
      header.addEventListener('click', event => {
        if (event.target.closest('button, label, input, .v3-mode-menu')) return;
        open();
      });
      header.addEventListener('keydown', event => {
        if (event.target.closest('button, label, input, select, .v3-mode-menu')) return;
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          open();
        }
      });
      row.dataset.groupId = groupId;
      row.append(header, detail);
      rows.push(row);
      abilityPages.set(ability.id, {groupId, open});
    }
  }
  let promptsSection = null;
  if (prompts) {
    promptsSection = createSettingsRow(register.stage, {id: 'prompts', title: 'Prompts'});
    prompts.classList.add('expanded', 'v3-register-prompts');
    prompts.querySelector('.ac-group-head')?.remove();
  }
  const matchesAbility = ability => [ability.name, ability.description, ability.skill_summary,
    ...(ability.tools || []).map(tool => tool.name)]
    .some(value => String(value || '').toLowerCase().includes(searchText));
  const filter = () => {
    searchText = search.value.trim().toLowerCase();
    for (const row of page.querySelectorAll('.v3-ability')) {
      const ability = abilities.find(item => item.id === row.dataset.abilityId);
      row.hidden = !!searchText && !matchesAbility(ability);
    }
    const noPrompts = !!searchText && !slots.some(slot =>
      [slot.slot_name, slot.content].some(value =>
        String(value || '').toLowerCase().includes(searchText)));
    for (const row of page.querySelectorAll('.sr-row')) {
      const id = row.dataset.registerFor;
      if (id === 'prompts') { row.hidden = noPrompts; continue; }
      row.hidden = !!searchText && !groups.get(id.slice(2)).items.some(matchesAbility);
    }
  };
  search.addEventListener('input', filter);
  page.append(status);
  const style = node('style');
  style.textContent = '.v3-abilities-page .sr-shell{margin-top:2px}'
    + '.v3-abilities-page .sr-detail-body .v3-ability{margin-bottom:8px}'
    + '.v3-abilities-page .sr-detail-body .v3-ability:last-child{margin-bottom:0}'
    + '.v3-abilities-page .v3-register-prompts{border:0;background:transparent}'
    + '.v3-abilities-page .v3-register-prompts>.ac-group-body{padding:0}';
  page.append(style);
  const reload = node('button', 'ac-btn ac-btn-sm', 'Reload abilities');
  reload.type = 'button';
  reload.onclick = () => rerender({ administratorNotice: '' }).catch(error => { status.textContent = error.message; });
  page.append(reload);
  closeFloatingMenu();
  host.replaceChildren(page);
  window.lucide?.createIcons({ nodes: [...page.querySelectorAll('[data-lucide]:not(.lucide)')] });
  register.present();
  for (const [section, rows] of rowsBySection) section.body.append(...rows);
  if (promptsSection) promptsSection.body.append(prompts);
  filter();
  const restored = abilityPages.get(expandedAbility);
  if (restored) { register.open('g-' + restored.groupId); restored.open(); }
  // LIVE-CONTEXT-CHART: poll the cheap revision stamp every two seconds while the
  // tab is visible and the chart is on screen, and re-read the estimate only when
  // the stamp moved. Hidden tabs and unmounted pages poll nothing; returning to the
  // tab re-reads at once; the chart itself refuses to repaint under a pointer.
  host._abilityPoll = setInterval(() => {
    if (host._abilityGeneration !== generation || !overview.isConnected ||
        document.visibilityState !== 'visible' || !overview.getClientRects().length) return;
    checkComposition();
  }, 2000);
  host._abilityVisibilityHandler = () => {
    if (host._abilityGeneration !== generation || !overview.isConnected) {
      document.removeEventListener('visibilitychange', host._abilityVisibilityHandler);
      return;
    }
    if (document.visibilityState === 'visible' && overview.getClientRects().length) {
      checkComposition(true);
    }
  };
  document.addEventListener('visibilitychange', host._abilityVisibilityHandler);
}
