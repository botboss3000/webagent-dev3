'use strict';
// Ported from v2 agent-ability-table.js skill row and expansion lifecycle.
import { buildConfigRow } from './ability-row-controls/ability-row-controls.js';
import { createRichTextEditor } from '../../../../shared/rich-text-editor/rich-text-editor.js';
const chevronSvg = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="9 18 15 12 9 6"/></svg>';
export function buildSkillRow(skill, { canEdit, menu, loadContent }) {
let skillRow, skillBody;
const skillMode = skill.mode || 'on_request';
let skillCtrl;
if (skillMode === 'core') {
 // A core skill (skill.json) is always loaded: a locked pill, not a menu.
 skillCtrl = document.createElement('span');
 skillCtrl.className = 'agents-tool-always-on';
 skillCtrl.textContent = 'Core';
 skillCtrl.title = 'Core skill — always loaded, cannot be changed.';
} else {
skillCtrl = document.createElement('label');
skillCtrl.className = 'conn-toggle-wrap ac-ability-toggle-wrap';
skillCtrl.innerHTML = `<input type="checkbox" class="conn-toggle" ${skillMode !== 'off' ? 'checked' : ''} ${canEdit ? '' : 'disabled'}><span class="conn-toggle-track"></span>`;
const skillToggle = skillCtrl.querySelector('input');
skillToggle.setAttribute('aria-label', `Skill availability: ${skillMode}. Choose availability.`);
skillToggle.setAttribute('aria-haspopup', 'menu');
skillCtrl.append(menu);
menu._floatingMenuOwner = skillCtrl;
skillToggle.addEventListener('click', event => {
 event.preventDefault(); event.stopPropagation();
 const rect = skillToggle.getBoundingClientRect();
 menu._floatingMenuAnchorPoint = { x: event.clientX || rect.right, y: event.clientY || rect.bottom };
 menu.click();
});
}
    // "Skill" is the row HEADING and the skill text drops to its own line below it,
    // exactly like a tool row's name + description (shared buildConfigRow).
    skillRow = buildConfigRow({
      name: skill.skill_id === 'core' ? 'Skill' : (skill.title || skill.skill_id),
      desc: skill.when_to_use || 'How-to for this ability',
      descTitle: skill.when_to_use || '',
      control: skillCtrl,
      className: 'ability-skill-row',
    });
    skillBody = document.createElement('div');
    skillBody.style.cssText = 'display:none;';
    let _skillLoaded = false;
    skillRow.classList.add('conn-card-header-clickable');
    const skillChevron = document.createElement('span');
    skillChevron.className = 'ac-row-chevron ac-row-chevron--left';
    skillChevron.innerHTML = chevronSvg;
    skillRow.insertBefore(skillChevron, skillRow.firstChild);
    skillRow.addEventListener('click', async (e) => {
      if (e.target.closest('.conn-toggle-wrap, button')) return;
      e.stopPropagation();
      const open = skillBody.style.display === 'none';
      skillBody.style.display = open ? 'block' : 'none';
      skillChevron.style.transform = open ? 'rotate(90deg)' : '';
      if (open && !_skillLoaded) {
        _skillLoaded = true;
        skillBody.innerHTML = '<div style="font-size:11px;color:var(--fg-3);">Loading…</div>';
        try {
          const d = await loadContent();
          skillBody.innerHTML = '';
          // Skills are read here and edited at the App Default level: the shared editor, read-only.
          skillBody.appendChild(createRichTextEditor({ value: (d && d.content) || 'No skill text.',
            label: 'Skill text', readOnly: true, minHeight: 40, maxHeight: 280 }).element);
        } catch (_) {
          _skillLoaded = false;
          skillBody.innerHTML = '<div style="font-size:11px;color:var(--danger);">Could not load skill content. The v3 runtime must support the skill-content endpoint. Close and reopen to retry.</div>';
        }
      }
    });
    // Keep the optional expanded how-to panel tied to its row so search never
    // leaves an already-open skill body visible after hiding that skill row.
return { row: skillRow, body: skillBody };
}
