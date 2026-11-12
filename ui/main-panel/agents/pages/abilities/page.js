import { buildSourceControlControls } from "./source-control.js";
// V3 data adapter for the v2 Abilities composition, prompts group, and ability rows.
import { buildAbilitiesOverview } from './overview.js';
import { buildAdministratorControls } from './administrator.js';
import { buildBrowserSites } from './browser-sites.js';
import { buildMcpControls } from './mcp.js';
import { buildPromptsGroup } from './prompts.js';
import { buildGroup, setGroupStatus } from './table.js';
import { buildPolicyControls, permissionControl, effectiveLabel, readinessLabel } from './policy.js';

import { _buildMoreButton, _buildToolRow, _buildAbilitySettingsList, closeFloatingMenu } from '../../../../shared/ability-row-controls.js';
import { buildSkillRow } from './rows.js';
import { _markSaving, _flashSaveCheck } from '../../../../shared/models/dom-utils.js';

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  return element;
};
const chevronSvg = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="9 18 15 12 9 6"/></svg>';
const modes = ['off', 'discoverable', 'visible'];
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
  const { agentId, levelId = 'operator', previewUser = '' } = options;
  const scope = '/agents/' + encodeURIComponent(agentId);
  const generation = (host._abilityGeneration || 0) + 1;
  host._abilityGeneration = generation;
  // Start every read at once; manager-only reads are refused for members and ignored below.
  const managed = (path, fallback) => request(scope + path).catch(() => fallback);
  const effectiveRead = levelId || previewUser
    ? request(scope + '/abilities/effective?' + new URLSearchParams({
        ...(levelId ? { level_id: levelId } : {}),
        ...(previewUser ? { user_id: previewUser } : {}),
      })) : null;
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
      ability.skill_visibility=mode(selectedLevel.skill_exposure,ability.id,ability.skill_visibility);
      for(const skill of ability.selectable_skills||[])skill.mode=mode(selectedLevel.skill_exposure,ability.id+':'+skill.skill_id,skill.mode);
      for(const tool of ability.tools||[])tool.visibility=mode(selectedLevel.tool_exposure,tool.name,tool.visibility||'visible');
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
  const list = node('div', 'conn-grid ac-list v3-abilities-list');
  const prompts = canManage ? await buildPromptsGroup(request, agentId) : null;
  if (prompts) list.append(prompts);
  const controls = await buildPolicyControls({ data, request, scope, levelId, previewUser, rerender, status });
  if (host._abilityGeneration !== generation) return;
  overview.querySelector('[data-tier-selector]').replaceChildren(controls.selector);

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
      icon: mode === 'off' ? 'eye-off' : mode === 'visible' ? 'eye' : 'download',
      label: mode[0].toUpperCase() + mode.slice(1), checked: trigger._mode === mode,
      disabled: !canManage || !!previewUser,
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
    const label = { deny: 'Off', ask: 'Ask', auto: 'Auto', visible: 'Visible', discoverable: 'Discoverable', off: 'Off', on: 'On' }[value] || value;
    const anchor = row.querySelector('.ac-ability-desc') || row.querySelector('.ac-ability-name');
    const mark = node('div', 'ac-hint v3-default-mark', 'Default: ' + label);
    mark.style.whiteSpace = 'nowrap'; // Keep "Default: Auto" on one line on phones.
    anchor?.after(mark);
  };
  const buildAbilityBody = (detail, ability) => {
    if (detail.childElementCount) return;
    if (ability.id === 'mcp') detail.append(buildMcpControls({ ability, request, scope,
      canManage, levelId, previewUser, status }));
    const tools = _buildAbilitySettingsList();
    tools.classList.add('v3-ability-tools');
    const generalSkills = ability.general_skills ||
      (ability.has_skill ? [{ skill_id: 'core', title: 'Skill',
        when_to_use: ability.skill_summary || '', mode: ability.skill_visibility || 'visible' }] : []);
    for (const entry of generalSkills) {
      const skill = buildSkillRow(entry, {
        canEdit: canManage && levelId==='operator' && !previewUser,
        menu: modeMenu(ability, 'skill', entry.mode || 'visible', entry.skill_id, true),
        loadContent: () => request(scope + '/abilities/' + encodeURIComponent(ability.id) +
          (entry.skill_id === 'core' ? '/skill-content' :
            '/skills/' + encodeURIComponent(entry.skill_id) + '/content')),
      });
      markDefault(skill.row, entry.default_mode);
      tools.append(skill.row, skill.body);
    }
    if (ability.id === "git_control" && canManage) tools.append(buildSourceControlControls({ data: sourceSites, request, scope, enabled: levelId === "operator" && !previewUser, status, rerender }));
    const administrator = buildAdministratorControls({ ability, data, repository, request, scope,
      levelId, previewUser, status,
      // A settings save re-renders the page; keep this ability open, like the tool rows do.
      rerender: options => { expandedAbility = ability.id; return rerender(options); } });
    if (administrator) {
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
        policy.tool_permissions[tool.name] = mode;
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
        permMode: toolPolicy()?.tool_permissions?.[tool.name] ||
          toolPolicy()?.tool_permissions?.['*'] || effectiveTool.default || 'auto',
        // The tool.json policy_ceiling: picks looser than it are shown but disabled.
        ceiling: { allow: 'auto', require_approval: 'ask', deny: 'deny' }[tool.policy],
      }, {
        canEdit: canManage && !previewUser && levelId==='operator',
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
      markDefault(toolRow, effectiveTool.default);
      toolRow.title = [tool.supported ? '' : 'Unavailable in v3', effectiveLabel(effectiveTool)].filter(Boolean).join(' · ');
      tools.append(toolRow);
    }
    detail.append(tools);
    // V3-specific access layering stays available outside the reference row title.
    if (canManage) {
      const access = node('details', 'v3-ability-access');
      access.append(node('summary', 'ac-hint', 'V3 access policy'));
      access.append(permissionControl({ data, request, scope, levelId, previewUser,
        kind: 'ability', id: ability.id, status, rerender }));
      access.append(node('p', 'ac-hint', [readinessLabel(ability), effectiveLabel(projection(ability))].join(' · ')));
      detail.append(access);
    }
    if (administrator) detail.append(administrator);
    if (ability.migration_note) detail.append(node('p', 'v3-ability-note', ability.migration_note));
  };
  for (const [groupId, group] of groups) {
    const groupRow = buildGroup(groupId, group.name, 'shield', 'var(--brand)', '');
    setGroupStatus(groupRow, group.items);
    const body = groupRow.querySelector('.ac-group-body');
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
      availability.title = ['Declared: ' + !!ability.declared, 'Installed: ' + !!ability.installed,
        'Configured: ' + !!ability.configured, readinessLabel(ability)].join(' · ');
      const toggleWrap = node('label', 'conn-toggle-wrap ac-ability-toggle-wrap');
      const toggle = node('input', 'conn-toggle');
      toggle.type = 'checkbox';
      // Base holds the loaders and core tools, so it is always on.
      toggle.disabled = !canManage || !!previewUser || ability.id === 'base';
      toggle.checked = ability.visibility !== 'off';
      toggle.setAttribute('aria-label', ability.name + ' on or off');
      // An ability is only a group: On lets its tools and skills follow their own settings.
      // The switch moves only after the save is re-read.
      toggle.addEventListener('click', event => {
        event.preventDefault();
        event.stopPropagation();
        save(ability, 'ability', ability.visibility !== 'off' ? 'off' : 'visible', toggleWrap).catch(() => {});
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
      row.append(header, detail);
      body.append(row);
      if (expandedAbility === ability.id) {
        groupRow.classList.add('expanded');
        groupRow.querySelector('.ac-group-head').setAttribute('aria-expanded', 'true');
        open();
      }
    }
    list.append(groupRow);
  }
  const filter = () => {
    searchText = search.value.trim().toLowerCase();
    for (const row of list.querySelectorAll('.v3-ability')) {
      const ability = abilities.find(item => item.id === row.dataset.abilityId);
      row.hidden = !!searchText && ![ability.name, ability.description, ability.skill_summary,
        ...(ability.tools || []).map(tool => tool.name)]
        .some(value => String(value || '').toLowerCase().includes(searchText));
    }
    for (const group of list.querySelectorAll('.ac-group:not(.ac-group-prompts)')) {
      group.hidden = !group.querySelector('.v3-ability:not([hidden])');
      if (searchText && !group.hidden) {
        group.classList.add('expanded');
        group.querySelector('.ac-group-head').setAttribute('aria-expanded', 'true');
      }
    }
    if (prompts) prompts.hidden = !!searchText && !slots.some(slot =>
      [slot.slot_name, slot.content].some(value =>
        String(value || '').toLowerCase().includes(searchText)));
  };
  search.addEventListener('input', filter);
  page.append(overview, controls.panel, search, list, status);
  const reload = node('button', 'ac-btn ac-btn-sm', 'Reload abilities');
  reload.type = 'button';
  reload.onclick = () => rerender({ administratorNotice: '' }).catch(error => { status.textContent = error.message; });
  page.append(reload);
  closeFloatingMenu();
  host.replaceChildren(page);
  window.lucide?.createIcons({ nodes: [...page.querySelectorAll('[data-lucide]:not(.lucide)')] });
  filter();
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
