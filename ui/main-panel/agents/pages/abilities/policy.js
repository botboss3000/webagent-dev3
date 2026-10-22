// Rust owns effective access. These controls edit one layer and display its live result.
const node = (tag, cls = '', text = '') => {
  const el = document.createElement(tag); el.className = cls; el.textContent = text; return el;
};
const permissions = ['deny', 'ask', 'auto'];
const clone = value => structuredClone(value || {});
const title = mode => ({ deny: 'Deny', ask: 'Ask', auto: 'Auto' })[mode] || mode;
const select = (label, choices, current) => {
  const el = node('select', 'ac-input ac-config-sel'); el.setAttribute('aria-label', label);
  for (const [value, name] of choices) {
    const option = node('option', '', name); option.value = value; el.append(option);
  }
  el.value = current; return el;
};
const row = (label, control, hint = '') => {
  const el = node('div', 'ac-ability-row ac-row-static');
  const caption = node('div', 'ac-ability-label');
  caption.append(node('div', 'ac-ability-name', label), node('div', 'ac-ability-desc', hint));
  el.append(caption, control); return el;
};
async function saving(control, status, fn) {
  control.disabled = true; status.textContent = 'Saving…';
  try { await fn(); status.textContent = 'Saved'; }
  catch (error) {
    status.textContent = error.status === 409
      ? 'Settings changed. Reload before saving: ' + error.message : error.message;
    control.value = control.dataset.savedValue;
  } finally { control.disabled = false; }
}
// Readiness describes the port, independently of the current caller's permission.
export function readinessLabel(record) {
  if (record.readiness === 'partial') return 'Partial port: compiled tools available; remaining tools pending';
  if (record.readiness === 'ready' || record.ready === true) return 'Ready';
  if (record.readiness === 'placeholder') return 'Placeholder';
  return 'Pending port';
}
export function effectiveLabel(record) {
  const effective = record.effective || {};
  const permission = effective.permission || record.permission;
  const reason = effective.reason || record.reason;
  return [permission ? title(permission) : '', reason || (record.available === false ? 'Unavailable' : '')]
    .filter(Boolean).join(' · ');
}
export function permissionControl({ data, request, scope, levelId, previewUser, kind, id, status, rerender }) {
  // Members receive a sanitized effective projection, not private configuration.
  // Do not fabricate configured Deny values from the omitted manager-only maps.
  if (!data.authority?.can_manage) {
    const empty = node('span'); empty.hidden = true; return empty;
  }
  const level = data.capability_levels?.levels?.[levelId];
  const policy = level || data.permissions || {};
  const key = kind === 'ability' ? 'ability_permissions' : 'tool_permissions';
  // Base policy and level tool omissions inherit the existing ceilings. A missing
  // level ability grant denies access, so omission is never shown as an allowance.
  const inherits = !level || kind === 'tool';
  const explicit = Object.hasOwn(policy[key] || {}, id);
  const current = explicit ? policy[key][id] : inherits ? '' : (policy[key]?.['*'] || 'deny');
  const choices = permissions.map(mode => [mode, title(mode)]);
  if (inherits) choices.unshift(['', 'Inherit ceiling']);
  const control = select(kind + ' ' + id + ' configured permission', choices, current);
  control.dataset.savedValue = current;
  control.title = explicit ? 'Explicit configured permission' :
    Object.hasOwn(policy[key] || {}, '*') ? 'Inherited from this layer’s wildcard permission' :
      inherits ? 'Inherit the account tier and agent policy ceilings' : 'No ability grant: deny';
  control.disabled = !data.authority?.can_manage || !!previewUser || (!!levelId && !level);
  control.onclick = event => event.stopPropagation();
  control.onchange = () => saving(control, status, async () => {
    const next = clone(policy); next[key] ||= {};
    if (control.value) next[key][id] = control.value;
    else delete next[key][id];
    if (level) {
      await request(scope + '/capability-levels/' + encodeURIComponent(levelId), 'PUT',
        { expected_revision: data.capability_levels.revision, level: next });
    } else {
      await request(scope + '/ability-permissions', 'PUT', {
        expected_revision: data.permissions.revision,
        ability_permissions: next.ability_permissions || {},
        tool_permissions: next.tool_permissions || {},
      });
    }
    await rerender({});
  });
  return control;
}
export async function buildPolicyControls({data,request,scope,levelId,previewUser,rerender,status}) {
  const canManage=data.authority?.can_manage===true;
  const levels=data.capability_levels?.levels||{};
  const order=data.capability_levels?.order||Object.keys(levels);
  const selector=node('div','member-policy-selector');
  const levelSelect=select('Capability level to configure',order.map(id=>[id,levels[id].name||id]),levelId||'operator');
  levelSelect.disabled=!canManage;
  levelSelect.onchange=()=>rerender({levelId:levelSelect.value,previewUser:''}).catch(error=>{status.textContent=error.message;});
  selector.append(levelSelect,node('div','ac-hint',canManage
    ? 'Editing '+(levels[levelId]?.name||'Operator')+'. Higher agent levels and app account policy cap effective access.'
    : 'Read-only effective access for your agent membership.'));
  const panel=node('div','ac-list');
  // Installation lifecycle controls are independent of tenant visibility and levels.
  if (data.authority?.is_installation_admin) {
    const servicePanel = node('div', 'ac-list');
    panel.append(servicePanel);
    try {
      const service = await request('/admin/services/boot_orphan_resume');
      const toggle = node('input'); toggle.type = 'checkbox'; toggle.checked = service.enabled === true;
      toggle.setAttribute('aria-label', 'Startup-only safe automatic resume');
      toggle.onchange = () => saving(toggle, status, async () => {
        const enabled = toggle.checked;
        try {
          await request('/admin/services/boot_orphan_resume', 'PUT',
            { enabled, expected_revision: service.revision });
          await rerender({});
        } catch (error) {
          toggle.checked = service.enabled === true;
          throw error;
        }
      });
      servicePanel.append(row('Startup-only safe automatic resume', toggle,
        'Installation service · ' + readinessLabel(service) +
        '. Resumes eligible interrupted runs when the loop starts. Uncertain writes always require review.'));
    } catch (error) {
      servicePanel.append(node('p', 'ac-hint',
        'Startup resume service unavailable: ' + String(error.message || 'Request failed').slice(0, 240)));
    }
  }
  if(!canManage)return {selector,panel};
  const memberResult=await request(scope+'/ability-members');
  const members=memberResult.members||[];
  const preview=select('Effective access preview user',[['','Your effective access'],...members.filter(m=>m.relationship!=='revoked').map(m=>[m.user_id,m.username||m.user_id])],previewUser);
  preview.onchange=()=>rerender({previewUser:preview.value}).catch(error=>{status.textContent=error.message;});
  panel.append(row('Effective access preview',preview,'Previewing a user disables policy edits.'));
  const links=node('div');
  for(const [view,label]of [['identity','Configure member levels'],['members','Manage members']]){
    const link=node('a','ac-btn',label);link.href='/app?tab=agents&agent='+encodeURIComponent(data.agent_id)+'&agentView='+view;links.append(link);
  }
  panel.append(links);
  return {selector,panel};
}
