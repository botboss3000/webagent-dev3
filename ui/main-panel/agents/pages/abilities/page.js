import { buildSourceControlControls } from "./source-control.js";
// V3 data adapter for the v2 Abilities composition, prompts group, and ability rows.
import { buildAbilitiesOverview } from './overview.js';
import { buildAdministratorControls } from './administrator.js';
import { buildBrowserSites } from './browser-sites.js';
import { buildMcpControls } from './mcp.js';
import { buildPromptsGroup } from './prompts.js';
import { buildPolicyControls, effectiveLabel, readinessLabel } from './policy.js';
import { buildStaleSettings } from './stale-settings.js';
import { APP_DEFAULT, APP_DEFAULTS_CSS, buildAbilityDefaultsPanels, buildAppDefaultsBar, buildGroupDefaultsPanel, buildToolDefaultsPanel, createAppDefaults, loadAppDefaults } from './app-defaults.js';

import { _buildMoreButton, _buildToolRow, _buildAbilitySettingsList, closeFloatingMenu } from './ability-row-controls/ability-row-controls.js';
import { buildSkillRow } from './rows.js';
import { createSubpageLinks, createSubpageList, createSubpage } from '../../../../shared/subpage-list/subpage-list.js';
import { createRichTextEditor } from '../../../../shared/rich-text-editor/rich-text-editor.js';
import { _markSaving, _flashSaveCheck } from '../../../../shared/models/dom-utils.js';

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  return element;
};
const modes = ['off', 'on_request', 'loaded'];
// Core is set only by tool.json or skill.json and is shown as a locked pill.
const modeLabel = mode => ({ loaded: 'Skill loaded', on_request: 'Skill on request', off: 'Off', core: 'Core' })[mode] || mode;
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
  // App Default is no agent level: it edits the app's source files, for administrators only.
  const appDefaultsRead = loadAppDefaults(request);
  const effectiveRead = levelId && levelId !== APP_DEFAULT
    ? request(scope + '/abilities/effective?' + new URLSearchParams({ level_id: levelId })) : null;
  effectiveRead?.catch(() => {});
  const reads = [managed('/slots', { slots: [] }), managed('/context-composition', { tokens: 0 }),
    managed('/repository-connections', { load_failed: true }), managed('/source-control/sites', { load_failed: true })];
  const data = await request(scope + '/abilities');
  const canManage = data.authority?.can_manage === true;
  const [slotsResult, compositionResult, repository, sourceSites] = canManage
    ? await Promise.all(reads) : [{ slots: [] }, { tokens: 0 }, null, null];
  const effective = canManage && effectiveRead ? await effectiveRead : data;
  const appDefaultsDrafts = await appDefaultsRead;
  if (host._abilityGeneration !== generation) return;
  const appDefaults = levelId === APP_DEFAULT && appDefaultsDrafts ? createAppDefaults(request, appDefaultsDrafts) : null;
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
    const group = groups.get(key);
    // Icon and color come from the group's _group.json; Base and Optimization may lack them.
    group.icon ||= ability.group_icon;
    group.color ||= ability.group_color;
    group.svg ||= ability.group_icon_svg;
    group.items.push(ability);
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
  const levelSelector = buildPolicyControls({ data, levelId, rerender, status, appDefaults: !!appDefaultsDrafts });
  if (host._abilityGeneration !== generation) return;
  // Reload sits with the level selector so it is reachable without scrolling the whole list.
  const reload = node('button', 'ac-btn ac-btn-sm', 'Reload abilities');
  reload.type = 'button';
  reload.onclick = () => rerender({ administratorNotice: '' }).catch(error => { status.textContent = error.message; });
  const appBar = appDefaults ? buildAppDefaultsBar(appDefaults, { onStatus: message => { status.textContent = message; } }) : null;
  if (appDefaults) appDefaults.state.error = message => { status.textContent = 'Draft not saved: ' + message; };
  overview.querySelector('[data-tier-selector]').replaceChildren(levelSelector, ...(appBar ? [appBar] : []), reload);

  // The spinner covers the control until the save returns, then a check; the page
  // keeps the returned revision instead of re-reading everything. A failure leaves
  // the control unchanged and shows the reason in red under its row.
  const save = async (ability, kind, mode, control, toolName = null) => {
    const input = control.querySelector('input') || control;
    const row = control.closest('.spl-item');
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
      const item = kind === 'ability' && control.closest('.spl-item');
      if (item) item.dataset.exposure = mode;
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
  // A tool's skill opens in place under its own row, in the shared editor, so the
  // tool page never leaves for a modal. A second press closes it again.
  const toggleToolSkill = (row, ability, tool) => {
    const link = row.querySelector('.ac-tool-skill-link');
    const open = row.nextElementSibling;
    if (open && open.classList.contains('ab-tool-skill')) {
      open.remove();
      row.classList.remove('skill-open');
      link?.setAttribute('aria-expanded', 'false');
      return;
    }
    // One skill at a time: a tool page is short, and two editors would push the
    // rest of its settings off screen.
    for (const stale of row.parentElement?.querySelectorAll('.ab-tool-skill') || []) stale.remove();
    const band = node('div', 'ab-tool-skill');
    // A read view: tool skills are edited, and saved, at the App Default level.
    const editor = createRichTextEditor({
      value: '', label: tool + ' skill', placeholder: 'Loading tool skill…', readOnly: true,
    });
    band.append(editor.element);
    row.after(band);
    row.classList.add('skill-open');
    link?.setAttribute('aria-expanded', 'true');
    request(scope + '/abilities/' + encodeURIComponent(ability.id) + '/tools/' +
      encodeURIComponent(tool) + '/skill-content').then(result => {
        if (band.isConnected) editor.setValue(result.content || 'No skill text.');
      }).catch(error => {
        if (!band.isConnected) return;
        editor.setValue('');
        band.append(node('p', 'ac-hint', error.message || 'Could not load tool skill.'));
      });
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
  // An ability's page is pushed onto the group's subpage, which draws its back header.
  const pageShell = () => { const element = node('div', 'ab-config'); return { element, body: element }; };
  const buildAbilityBody = (detail, ability) => {
    if (detail.childElementCount) return;
    if (appDefaults) {
      // App Default edits this ability's source files, not the agent's settings.
      const config = pageShell();
      config.body.append(node('p', 'ac-hint', 'App Default: edits are drafts of this ability\'s source files until you Save.'));
      config.body.append(...buildAbilityDefaultsPanels(appDefaults, ability));
      for (const tool of ability.tools || []) config.body.append(buildToolDefaultsPanel(appDefaults, ability, tool));
      detail.append(config.element);
      return;
    }
    const config = pageShell();
    if (ability.id === 'mcp') config.body.append(buildMcpControls({ ability, request, scope,
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
        onOpenToolSkill: name => toggleToolSkill(toolRow, ability, name),
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
    config.body.append(tools);
    if (administrator) config.body.append(administrator);
    if (ability.migration_note) config.body.append(node('p', 'v3-ability-note', ability.migration_note));
    detail.append(config.element);
  };
  // Two-stage register: the list shows one row per ability group, and a group's page
  // lists its abilities as the same rows, each opening the shared config page.
  const categories = [...groups].map(([groupId, group]) => ({
    id: 'g-' + groupId, title: group.name, hint: groupHint(group),
    lucide: group.icon || 'shield', color: group.color || '', svg: group.svg || '',
  }));
  if (prompts) categories.push({
    id: 'prompts', title: 'Prompts',
    hint: slots.length + (slots.length === 1 ? ' prompt slot' : ' prompt slots') + ' configured for this agent',
    lucide: 'file-text',
  });
  page.append(overview, search);
  // Saved settings that name a renamed or removed tool, ability or skill no longer apply.
  const stale = canManage && !appDefaults ? buildStaleSettings({ data, abilities: data.abilities || [], request, scope, rerender, status }) : null;
  if (stale) page.append(stale);
  const register = createSubpageList(page, {label: 'Ability groups', categories});
  const abilityPages = new Map();
  for (const [groupId, group] of groups) {
    const section = createSubpage(register.stage, {id: 'g-' + groupId, title: group.name});
    // The group's page lists its abilities; an ability's page is pushed on top of it,
    // under the same back header (back, Escape and the back press return to the list).
    const holder = node('div', 'ab-holder');
    section.body.append(holder);
    const items = [];
    const rowInfo = new Map();
    let current = null;
    const openAbility = (ability, claim = true) => {
      if (current) register.pop();
      const pageHost = node('div', 'ab-page');
      pageHost.dataset.abilityId = ability.id;
      buildAbilityBody(pageHost, ability);
      const entry = {element: pageHost};
      // A restored page claims nothing, so a reload never adds a history entry.
      const pushed = register.push({title: ability.name, body: pageHost, claim, anyWidth: true, onPop: () => {
        if (current === entry) current = null;
        if (expandedAbility === ability.id) expandedAbility = null;
        holder.querySelector('.spl-row[data-register-link="' + ability.id + '"]')?.focus({preventScroll: true});
      }});
      if (pushed) current = entry;
    };
    for (const ability of group.items) {
      const projected = projection(ability);
      // Port status is independent of visibility, credentials and caller permissions.
      const incomplete = ['pending', 'placeholder', 'partial'].includes(ability.readiness) ||
        ability.installed === false || (ability.tools || []).some(tool => tool.supported === false);
      const roleBlocked = ability.role_blocked === true || projected.role_blocked === true;
      const tierBlocked = projected.effective?.reason === 'tier_denied';
      // One line under the name: why the ability is blocked, else what it does.
      const hint = tierBlocked ? 'Unavailable on the agent owner’s tier'
        : roleBlocked ? 'Not available to this level' : (ability.description || '');
      const status = ['Declared: ' + !!ability.declared, 'Installed: ' + !!ability.installed,
        'Configured: ' + !!ability.configured, readinessLabel(ability), effectiveLabel(projected)]
        .filter(Boolean).join(' · ');
      const toggleWrap = node('label', 'conn-toggle-wrap ac-ability-toggle-wrap');
      // The switch stays inside the page: the register must not adopt it as the page toggle.
      toggleWrap.dataset.inlineSwitch = '';
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
      items.push({id: ability.id, title: ability.name, hint, icon: ability.icon || 'box', svg: ability.icon_svg || '',
        trailing: toggleWrap, onSelect: () => openAbility(ability)});
      rowInfo.set(ability.id, {name: ability.name, incomplete, status});
      abilityPages.set(ability.id, {groupId, open: claim => openAbility(ability, claim)});
    }
    // Stage 2 is the register's own row list: the same rows as the group list, with the
    // ability's switch beside each one.
    // App Default edits the group's own card above its abilities.
    if (appDefaults) holder.append(buildGroupDefaultsPanel(appDefaults, groupId, group.name));
    createSubpageLinks(holder, {label: group.name + ' abilities', items});
    for (const [id, info] of rowInfo) {
      const button = holder.querySelector('.spl-row[data-register-link="' + id + '"]');
      const item = button?.closest('.spl-item');
      if (!button || !item) continue;
      item.dataset.abilityId = id;
      button.dataset.abilityId = id;
      if (info.incomplete) button.querySelector('.spl-title').style.color = 'var(--danger)';
      button.title = [info.name, info.status].filter(Boolean).join(' · ');
    }
  }
  let promptsSection = null;
  if (prompts) {
    promptsSection = createSubpage(register.stage, {id: 'prompts', title: 'Prompts'});
    // The subpage replaces the group's head, whose click was the only other loader.
    prompts.classList.add('v3-register-prompts');
    prompts._setExpanded(true);
    prompts.querySelector('.ac-group-head')?.remove();
  }
  const matchesAbility = ability => [ability.name, ability.description, ability.skill_summary,
    ...(ability.tools || []).map(tool => tool.name)]
    .some(value => String(value || '').toLowerCase().includes(searchText));
  const filter = () => {
    searchText = search.value.trim().toLowerCase();
    // Both stages are register rows, so one rule hides a row at either level. A list row
    // sits in an .spl-item wrapper; a group row is appended bare, so hide the row itself.
    for (const row of page.querySelectorAll('.spl-row[data-register-link]')) {
      const ability = abilities.find(item => item.id === row.dataset.abilityId);
      (row.closest('.spl-item') || row).hidden = !!searchText && !matchesAbility(ability);
    }
    const noPrompts = !!searchText && !slots.some(slot =>
      [slot.slot_name, slot.content].some(value =>
        String(value || '').toLowerCase().includes(searchText)));
    for (const row of page.querySelectorAll('.spl-row[data-register-for]')) {
      const id = row.dataset.registerFor;
      const item = row.closest('.spl-item') || row;
      if (id === 'prompts') { item.hidden = noPrompts; continue; }
      item.hidden = !!searchText && !groups.get(id.slice(2)).items.some(matchesAbility);
    }
  };
  search.addEventListener('input', filter);
  page.append(status);
  const style = node('style');
  style.textContent = '.v3-abilities-page .spl-shell{margin-top:2px}'
    + '.v3-abilities-page .spl-item[hidden]{display:none}'
    // Every level shares the group list's inset: the register's page body adds 2px
    // of its own, which would step the ability list and the config page inward.
    + '.v3-abilities-page .spl-detail-body{padding-left:0;padding-right:0}'
    + '.v3-abilities-page .v3-register-prompts{border:0;background:transparent}'
    + '.v3-abilities-page .v3-register-prompts>.ac-group-body{padding:0}'
    // The ability page sits at the register's side inset, so its rows line up with
    // the rows it opened from instead of being indented into the card.
    + '.v3-abilities-page .ab-page{padding:0;margin:0}'
    + '.v3-abilities-page .ab-tool-skill{padding:2px 0 10px}'
    + '.v3-abilities-page .ab-tool-skill .rte-doc{max-height:300px}';
  style.textContent += APP_DEFAULTS_CSS;
  page.append(style);
  closeFloatingMenu();
  host.replaceChildren(page);
  window.lucide?.createIcons({ nodes: [...page.querySelectorAll('[data-lucide]:not(.lucide)')] });
  register.present();
  if (promptsSection) promptsSection.body.append(prompts);
  filter();
  const restored = abilityPages.get(expandedAbility);
  // Reopening after a rerender is not a user action, so it adds no back press.
  if (restored) { register.open('g-' + restored.groupId); restored.open(false); }
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
