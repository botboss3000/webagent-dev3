'use strict';

// A configuration page: a back control and title over a body. A caller mounts it for
// one setting's detail level and fills its body, so every config page shares one shell.

const STYLE_ID = 'cfg-page-styles';
const CSS = `
.cfg-page{display:flex;flex-direction:column;min-width:0}
.cfg-head{display:flex;align-items:center;padding:2px 0 8px}
.cfg-back{display:inline-flex;align-items:center;gap:6px;max-width:100%;padding:6px 10px 6px 4px;border:0;border-radius:8px;background:none;color:var(--fg-1);font:inherit;font-size:13px;font-weight:600;letter-spacing:-0.01em;cursor:pointer;-webkit-tap-highlight-color:transparent;transition:background-color .15s ease}
.cfg-back:hover{background:color-mix(in srgb, var(--fg-1) 6%, transparent)}
.cfg-back:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.cfg-back svg{width:13px;height:13px;flex:none;opacity:.5}
.cfg-title{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
@media (pointer: coarse){.cfg-back{min-height:44px}}
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

  return { element, body };
}
