import {requestTouchVibration} from '../../../../shared/vibrate.js';

/* A section registers itself with the Identity register; the page decides where
   its content is shown, so a row never expands in place. */
let openRegister = null;
export function setRegisterOpen(fn) { openRegister = fn; }

export function createSettingsRow(host, {id, title, toggleLabel}) {
  const section = document.createElement('section');
  section.className = 'ir-section';
  section.dataset.identitySection = id;
  section.dataset.irTitle = title;
  section.style.webkitTapHighlightColor = 'transparent';
  const header = document.createElement('div');
  header.className = 'ir-section-head';
  const name = document.createElement('span');
  name.className = 'ac-ability-name';
  name.textContent = title;
  header.append(name);
  section.append(header);
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
  const body = document.createElement('div');
  body.className = 'ir-section-body';
  body.id = 'identity-' + id + '-body';
  section.append(body);
  section.addEventListener('click', event => {
    if (event.target.closest('button:not(:disabled),input[type=checkbox]:not(:disabled)')) requestTouchVibration(event);
  }, {capture:true});
  host.append(section);
  /* Opening means going to this section's page; there is nothing to expand in place. */
  const setExpanded = open => { if (open) openRegister?.(id); };
  return {section, body, toggle, setExpanded};
}
