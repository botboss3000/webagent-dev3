'use strict';

// A Markdown text editor: a bordered frame, a formatting toolbar, and a writing
// area that takes formatting and undo like a rich editor. Plain DOM and CSS,
// no framework or editor library, so any page can mount it.

const STYLE_ID = 'rte-styles';
const CSS = `
.rte{position:relative;display:flex;flex-direction:column;min-width:0;width:100%;border:var(--border-width) solid var(--border);border-radius:10px;overflow:hidden;background:color-mix(in srgb, var(--fg-1) 3%, transparent);transition:border-color .18s ease}
.rte:focus-within{border-color:var(--accent)}
.rte-bar{display:flex;align-items:center;gap:2px;padding:4px;border-bottom:var(--border-width) solid var(--border);overflow-x:auto;scrollbar-width:none}
.rte-bar::-webkit-scrollbar{display:none}
.rte:not([data-bar-open]) .rte-bar{display:none}
/* The open bar stops short of the chevron so scrolled buttons never slide under it; the line under it moves to the writing area. */
.rte[data-bar-open] .rte-bar{margin-right:32px;border-bottom:0}
.rte[data-bar-open] .rte-doc{border-top:var(--border-width) solid var(--border)}
.rte:not([data-bar-open]):not([data-readonly]) .rte-doc{padding-right:30px}
.rte-toggle{position:absolute;top:7px;right:6px;z-index:1;display:inline-flex;align-items:center;justify-content:center;width:24px;height:24px;padding:0;border:0;border-radius:6px;background:transparent;color:var(--fg-3);cursor:pointer;-webkit-tap-highlight-color:transparent;transition:background-color .15s ease,color .15s ease}
.rte:not([data-bar-open]) .rte-toggle{top:3px;right:4px}
.rte-toggle:hover{background:color-mix(in srgb, var(--fg-1) 6%, transparent);color:var(--fg-1)}
.rte-toggle:focus-visible{outline:2px solid var(--accent);outline-offset:-2px}
.rte-toggle:active{background:color-mix(in srgb, var(--fg-1) 10%, transparent)}
.rte-toggle svg{width:16px;height:16px;transition:transform .18s ease}
.rte[data-bar-open] .rte-toggle svg{transform:rotate(180deg)}
.rte[data-readonly] .rte-toggle{display:none}
.rte-btn{display:inline-flex;align-items:center;justify-content:center;min-width:30px;height:30px;padding:0 6px;border:0;border-radius:7px;background:transparent;color:var(--fg-3);font:inherit;font-size:13px;font-weight:600;line-height:1;cursor:pointer;-webkit-tap-highlight-color:transparent;transition:background-color .15s ease,color .15s ease}
.rte-btn:hover{background:color-mix(in srgb, var(--fg-1) 6%, transparent);color:var(--fg-1)}
.rte-btn:focus-visible{outline:2px solid var(--accent);outline-offset:-2px}
.rte-btn:active{background:color-mix(in srgb, var(--fg-1) 10%, transparent)}
.rte-btn svg{width:15px;height:15px}
.rte-sep{width:1px;height:18px;margin:0 4px;background:var(--border);flex:none}
.rte-doc{min-height:120px;max-height:340px;overflow-y:auto;padding:10px 12px;border:0;background:transparent;-webkit-text-fill-color:currentColor;color:var(--fg-1);font-size:12px;line-height:1.55;white-space:pre-wrap;overflow-wrap:anywhere;outline:none;tab-size:2}
.rte-doc:empty::before{content:attr(data-placeholder);color:var(--fg-3);-webkit-text-fill-color:currentColor;pointer-events:none}
.rte[data-readonly] .rte-bar{display:none}
.rte[data-readonly] .rte-doc{color:var(--fg-2)}
.rte[data-disabled]{opacity:.6}
.rte[data-disabled] .rte-bar{pointer-events:none}
.rte-count{align-self:flex-end;padding:0 10px 6px;font-size:11px;color:var(--fg-3)}
`;

const ICONS = {
  chevron: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 15 6-6 6 6"/></svg>',
  bullet: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r="1.2" fill="currentColor" stroke="none"/><circle cx="4.5" cy="12" r="1.2" fill="currentColor" stroke="none"/><circle cx="4.5" cy="18" r="1.2" fill="currentColor" stroke="none"/></svg>',
  ordered: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M10 6h11M10 12h11M10 18h11"/><path d="M3 5.4h1.4v4.2M2.7 9.6h2.2"/><path d="M2.7 13.2h1.7c.5 0 .8.3.8.7 0 1.3-2.5 1.6-2.5 3.4h2.6"/></svg>',
  quote: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 7H5v5h4V9.5C9 8 9.8 7 11 6.5"/><path d="M19 7h-4v5h4V9.5c0-1.5.8-2.5 2-3"/></svg>',
  link: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1"/><path d="M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1"/></svg>',
  undo: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-4"/></svg>',
  redo: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 14 5-5-5-5"/><path d="M20 9H9a5 5 0 0 0 0 10h4"/></svg>',
};

/* Inserts text at the caret, keeping the browser's own undo history. */
function insert(doc, text) {
  doc.focus();
  if (!document.execCommand('insertText', false, text)) {
    const selection = window.getSelection();
    if (!selection || !selection.rangeCount) return;
    const range = selection.getRangeAt(0);
    range.deleteContents();
    range.insertNode(document.createTextNode(text));
    selection.collapseToEnd();
  }
}

/* Wraps the selected text, or inserts an empty pair with the caret held between. */
function wrap(doc, before, after = before, placeholder = 'text') {
  const picked = window.getSelection()?.toString() || placeholder;
  insert(doc, before + picked + after);
  for (let i = 0; i < after.length; i += 1) {
    window.getSelection()?.modify?.('move', 'backward', 'character');
  }
}

const TOOLS = [
  { id: 'heading', label: 'Heading', text: 'H', run: doc => insert(doc, '## ') },
  { id: 'bold', label: 'Bold', text: 'B', run: doc => wrap(doc, '**') },
  { id: 'italic', label: 'Italic', text: 'I', run: doc => wrap(doc, '*') },
  { id: 'strike', label: 'Strikethrough', text: 'S', run: doc => wrap(doc, '~~') },
  { id: 'code', label: 'Inline code', text: '<>', run: doc => wrap(doc, '`') },
  { sep: true },
  { id: 'bullet', label: 'Bulleted list', html: ICONS.bullet, run: doc => insert(doc, '- ') },
  { id: 'ordered', label: 'Numbered list', html: ICONS.ordered, run: doc => insert(doc, '1. ') },
  { id: 'quote', label: 'Quote', html: ICONS.quote, run: doc => insert(doc, '> ') },
  { sep: true },
  { id: 'link', label: 'Add link', html: ICONS.link, run: doc => wrap(doc, '[', '](https://)', 'text') },
  { sep: true },
  { id: 'undo', label: 'Undo', html: ICONS.undo, run: doc => { doc.focus(); document.execCommand('undo'); } },
  { id: 'redo', label: 'Redo', html: ICONS.redo, run: doc => { doc.focus(); document.execCommand('redo'); } },
];

/** Mount a Markdown editor; returns the frame plus read and write helpers.
 *  The toolbar starts hidden; the chevron at the top edge shows and hides it.
 *  defaultText shows (greyed) while the field is empty: what applies when nothing is written.
 *  maxLength stops typing and pasting past that many characters and shows a counter.
 *  disabled keeps the text visible but not editable (readOnly also hides the toolbar). */
export function createRichTextEditor({ value = '', label = 'Editor', placeholder, defaultText = '',
  maxLength = 0, readOnly = false, disabled = false, minHeight = 120, maxHeight = 340, onChange = null } = {}) {
  if (!document.getElementById(STYLE_ID)) {
    const style = document.createElement('style');
    style.id = STYLE_ID;
    style.textContent = CSS;
    document.head.append(style);
  }
  const frame = document.createElement('div');
  frame.className = 'rte';
  if (readOnly) frame.dataset.readonly = '';
  const bar = document.createElement('div');
  bar.className = 'rte-bar';
  bar.setAttribute('role', 'toolbar');
  bar.setAttribute('aria-label', 'Formatting');
  const doc = document.createElement('div');
  doc.className = 'rte-doc';
  doc.contentEditable = readOnly ? 'false' : 'true';
  doc.spellcheck = true;
  doc.setAttribute('role', 'textbox');
  doc.setAttribute('aria-multiline', 'true');
  doc.setAttribute('aria-label', label);
  doc.dataset.placeholder = placeholder ?? (defaultText || 'Write something\u2026');
  doc.style.minHeight = minHeight + 'px';
  doc.style.maxHeight = maxHeight + 'px';
  doc.textContent = value;
  for (const tool of TOOLS) {
    if (tool.sep) {
      const sep = document.createElement('span');
      sep.className = 'rte-sep';
      sep.setAttribute('aria-hidden', 'true');
      bar.append(sep);
      continue;
    }
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'rte-btn';
    button.dataset.tool = tool.id;
    button.setAttribute('aria-label', tool.label);
    button.title = tool.label;
    if (tool.html) button.innerHTML = tool.html;
    else button.textContent = tool.text;
    // Keep the caret where it is: the press must not move focus off the document.
    button.addEventListener('mousedown', event => event.preventDefault());
    button.addEventListener('click', () => tool.run(doc));
    bar.append(button);
  }
  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.className = 'rte-toggle';
  toggle.innerHTML = ICONS.chevron;
  const paintToggle = () => {
    const open = 'barOpen' in frame.dataset;
    toggle.setAttribute('aria-expanded', String(open));
    toggle.setAttribute('aria-label', open ? 'Hide formatting' : 'Show formatting');
    toggle.title = toggle.getAttribute('aria-label');
  };
  toggle.addEventListener('mousedown', event => event.preventDefault());
  toggle.addEventListener('click', () => { frame.toggleAttribute('data-bar-open'); paintToggle(); });
  paintToggle();
  const count = maxLength ? document.createElement('span') : null;
  const paintCount = () => { if (count) count.textContent = doc.innerText.replace(/\n$/, '').length + ' / ' + maxLength; };
  if (maxLength) {
    count.className = 'rte-count';
    doc.addEventListener('beforeinput', event => {
      if (!event.inputType.startsWith('insert')) return;
      const picked = window.getSelection()?.toString().length || 0;
      const adding = (event.data ?? event.dataTransfer?.getData('text/plain') ?? '').length || 1;
      if (doc.innerText.replace(/\n$/, '').length - picked + adding > maxLength) event.preventDefault();
    });
    doc.addEventListener('input', paintCount);
  }
  if (onChange) doc.addEventListener('input', () => onChange(doc.innerText));
  const setDisabled = off => {
    if (readOnly) return;
    doc.contentEditable = off ? 'false' : 'true';
    if (off) frame.dataset.disabled = ''; else delete frame.dataset.disabled;
    for (const button of bar.querySelectorAll('button')) button.disabled = !!off;
  };
  setDisabled(disabled);
  frame.append(toggle, bar, doc, ...(count ? [count] : []));
  paintCount();
  return {
    element: frame,
    area: doc,
    getValue: () => doc.innerText.replace(/\n$/, ''),
    setValue: next => { doc.textContent = next; paintCount(); },
    setDisabled,
    setDefaultText: text => { doc.dataset.placeholder = text || placeholder || 'Write something\u2026'; },
    focus: () => doc.focus(),
  };
}
