'use strict';

// A configuration page: a back control and title over labelled sections whose rows
// use the settings register's look. A caller mounts it for one setting's detail
// level and fills its body with rows, so every config page shares one shell.

const STYLE_ID = 'cfg-page-styles';
const CSS = `
.cfg-page{display:flex;flex-direction:column;min-width:0}
.cfg-head{display:flex;align-items:center;padding:2px 0 8px}
.cfg-back{display:inline-flex;align-items:center;gap:6px;max-width:100%;padding:6px 10px 6px 4px;border:0;border-radius:8px;background:none;color:var(--fg-1);font:inherit;font-size:13px;font-weight:600;letter-spacing:-0.01em;cursor:pointer;-webkit-tap-highlight-color:transparent;transition:background-color .15s ease}
.cfg-back:hover{background:color-mix(in srgb, var(--fg-1) 6%, transparent)}
.cfg-back:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.cfg-back svg{width:13px;height:13px;flex:none;opacity:.5}
.cfg-title{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.cfg-section + .cfg-section{margin-top:16px}
.cfg-section-title{padding:0 4px 6px;font-size:11px;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:var(--fg-3)}
.cfg-list{display:flex;flex-direction:column;min-width:0}
.cfg-list > * + *{border-top:var(--border-width) solid var(--border-soft)}
.cfg-note{padding:8px 4px;font-size:12px;color:var(--fg-3)}
`;
const CHEVRON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 18 9 12 15 6"/></svg>';

const make = (tag, className = '') => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  return element;
};

/** Mount a configuration page; returns its shell, body and section helper. */
export function createConfigPage({ title = '', backLabel = 'Back', onBack = null } = {}) {
  if (!document.getElementById(STYLE_ID)) {
    const style = document.createElement('style');
    style.id = STYLE_ID;
    style.textContent = CSS;
    document.head.append(style);
  }
  const element = make('div', 'cfg-page');
  const head = make('div', 'cfg-head');
  const back = make('button', 'cfg-back');
  back.type = 'button';
  back.setAttribute('aria-label', backLabel + (title ? ': ' + title : ''));
  const chevron = make('span');
  chevron.innerHTML = CHEVRON;
  const heading = make('span', 'cfg-title');
  heading.textContent = title;
  back.append(chevron.firstElementChild, heading);
  if (onBack) back.addEventListener('click', () => onBack());
  const body = make('div', 'cfg-body');
  head.append(back);
  element.append(head, body);

  // A section is a label over one flush list, so its rows keep the register's
  // hairline rhythm instead of being boxed separately.
  const addSection = (sectionTitle, content) => {
    const section = make('div', 'cfg-section');
    if (sectionTitle) {
      const label = make('div', 'cfg-section-title');
      label.textContent = sectionTitle;
      section.append(label);
    }
    const list = make('div', 'cfg-list');
    list.append(...[content].flat().filter(Boolean));
    section.append(list);
    body.append(section);
    return list;
  };

  return {
    element,
    body,
    back,
    setTitle: next => {
      heading.textContent = next;
      back.setAttribute('aria-label', backLabel + (next ? ': ' + next : ''));
    },
    addSection,
  };
}
