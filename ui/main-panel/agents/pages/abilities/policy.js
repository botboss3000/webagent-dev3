// Rust owns effective access. These controls edit one layer and display its live result.
const node = (tag, cls = '', text = '') => {
  const el = document.createElement(tag); el.className = cls; el.textContent = text; return el;
};
const title = mode => ({ deny: 'Deny', require_approval: 'Require approval', allow: 'Allow' })[mode] || mode;
const select = (label, choices, current) => {
  const el = node('select', 'ac-input ac-config-sel'); el.setAttribute('aria-label', label);
  for (const [value, name] of choices) {
    const option = node('option', '', name); option.value = value; el.append(option);
  }
  el.value = current; return el;
};
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
export function buildPolicyControls({data,levelId,rerender,status,appDefaults=false}) {
  const canManage=data.authority?.can_manage===true;
  const levels=data.capability_levels?.levels||{};
  const order=data.capability_levels?.order||Object.keys(levels);
  const selector=node('div','member-policy-selector');
  // App Default (administrators only) edits the app's source defaults rather than an agent level.
  const choices=order.map(id=>[id,levels[id].name||id]).concat(appDefaults?[['app_default','App Default']]:[]);
  const levelSelect=select('Capability level to configure',choices,levelId||'operator');
  levelSelect.disabled=!canManage&&!appDefaults;
  levelSelect.onchange=()=>rerender({levelId:levelSelect.value}).catch(error=>{status.textContent=error.message;});
  selector.append(levelSelect,node('div','ac-hint',levelId==='app_default'
    ? 'Editing the app\'s default tool and ability files. Edits are drafts until you Save; Save checks them and applies them at once.'
    : canManage
    ? 'Editing '+(levels[levelId]?.name||'Operator')+'. Higher agent levels and app account policy cap effective access.'
    : 'Read-only effective access for your agent membership.'));
  return selector;
}
