import {requestTouchVibration} from '../../../../shared/vibrate.js';

/** Match Identity's Main Chat row. Expansion is presentation only; callers own persistence and authority. */
export function createSettingsRow(host, {id, title, toggleLabel}) {
  const section = document.createElement('section');
  section.className = 'conn-card ac-row identity-settings-row';
  section.dataset.identitySection = id;
  section.style.webkitTapHighlightColor = 'transparent';
  const header = document.createElement('div');
  header.className = 'conn-card-header ac-ability-row';
  const expand = document.createElement('button');
  expand.type = 'button';
  expand.className = 'ac-ability-label';
  expand.style.cssText = 'display:flex;align-items:center;gap:8px;flex:1;min-width:0;min-height:36px;text-align:left;border:0;background:transparent;color:inherit;cursor:pointer';
  const chevron = document.createElement('span');
  chevron.className = 'ac-row-chevron ac-row-chevron--left';
  chevron.textContent = '›';
  chevron.setAttribute('aria-hidden', 'true');
  const name = document.createElement('span');
  name.className = 'ac-ability-name';
  name.textContent = title;
  expand.append(chevron, name);
  header.append(expand);
  section.append(header);
  const body = document.createElement('div');
  body.className = 'conn-fields ac-ability-body';
  body.id = 'identity-' + id + '-body';
  body.style.cssText = 'display:none;padding:12px;min-width:0';
  section.append(body);
  expand.setAttribute('aria-controls', body.id);
  function setExpanded(open) {
    body.hidden = !open;
    body.style.display = open ? 'block' : 'none';
    section.classList.toggle('expanded', open);
    expand.setAttribute('aria-expanded', String(open));
    expand.setAttribute('aria-label', (open ? 'Collapse ' : 'Expand ') + title);
    chevron.style.transform = open ? 'rotate(90deg)' : '';
  }
  setExpanded(false);
  expand.addEventListener('click', () => setExpanded(body.hidden));
  let toggle = null;
  if (toggleLabel) {
    const wrap = document.createElement('label');
    wrap.className = 'conn-toggle-wrap ac-ability-toggle-wrap';
    toggle = document.createElement('input');
    toggle.type = 'checkbox';
    toggle.className = 'conn-toggle';
    toggle.setAttribute('aria-label', toggleLabel);
    const track = document.createElement('span');
    track.className = 'conn-toggle-track';
    track.setAttribute('aria-hidden', 'true');
    wrap.append(toggle, track);
    header.append(wrap);
  }
  section.addEventListener('click', event => {
    if (event.target.closest('button:not(:disabled),input[type=checkbox]:not(:disabled)')) requestTouchVibration(event);
  }, {capture:true});
  host.append(section);
  return {section, body, toggle, setExpanded};
}
