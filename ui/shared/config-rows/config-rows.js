'use strict';

// Shared configuration rows: one look and one save behaviour for every setting on a
// config page. Rows use the app's option-row list (.ac-list / .ac-ability-row with the
// control in .ac-config-control, styled in ui/shared/css/app3.css).
//
// Saving. A row either saves as you go (pass onSave: it gets the new value and returns a
// promise; the spinner, check and revert-on-failure come from dom-utils.js) or belongs to
// a form (pass form from createConfigForm and a key: the form collects the draft and
// saves it with one Save button).

import { _markSaving, _flashSaveCheck } from '../models/dom-utils.js';
import { tipBadge } from '../models/field-tip.js';
import { requestTouchVibration } from '../vibrate/vibrate.js';
import { createRichTextEditor } from '../rich-text-editor/rich-text-editor.js';

const STYLE_ID = 'cfr-styles';
const CSS = `
.cfr-stacked{flex-direction:column;align-items:stretch;gap:8px}
.cfr-stacked>.cfr-label{display:block}
.cfr-stacked>.cfr-field{width:100%;min-width:0}
.cfr-choices{display:grid;gap:6px;width:100%}
.cfr-choice{display:flex;gap:10px;align-items:flex-start;padding:10px 12px;border:var(--border-width) solid var(--border);border-radius:10px;cursor:pointer;-webkit-tap-highlight-color:transparent}
.cfr-choice:has(input:checked){border-color:var(--accent);background:color-mix(in srgb, var(--accent) 6%, transparent)}
.cfr-choice:has(input:disabled){opacity:.6;cursor:default}
.cfr-choice input{margin-top:2px}
.cfr-choice-text{display:grid;gap:2px;min-width:0}
.cfr-choice-title{font-size:13px;font-weight:600;color:var(--fg-1)}
.cfr-choice-hint{font-size:12px;color:var(--fg-3)}
.cfr-error{flex-basis:100%;order:2;margin:0;font-size:12px;color:var(--danger)}
.cfr-error:empty{display:none}
.cfr-actions{display:flex;flex-wrap:wrap;gap:8px}
.cfr-form-bar{display:flex;align-items:center;justify-content:flex-end;gap:8px;padding:10px 12px}
.cfr-form-state{margin-right:auto;font-size:12px;color:var(--fg-3)}
.cfr-form-state[data-state="dirty"]{color:var(--warning)}
.cfr-form-state[data-state="error"]{color:var(--danger)}
.cfr-read{width:100%;min-width:0}
`;
const ARROW = d => '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="' + d + '"/></svg>';

function styles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement('style');
  style.id = STYLE_ID;
  style.textContent = CSS;
  document.head.append(style);
}

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
};

/** The bordered list a group of rows lives in. */
export function createConfigList(host, {label = ''} = {}) {
  styles();
  const list = node('div', 'ac-list');
  if (label) list.setAttribute('aria-label', label);
  host.append(list);
  return list;
}

/* Row shell: title (with optional help badge) on the left, the control on the right;
   the hint drops to its own line. Stacked rows put the field under the title. */
function shell(list, {title, hint, tip, stacked = false}) {
  styles();
  const row = node('div', 'ac-ability-row cfr-row' + (stacked ? ' cfr-stacked' : ''));
  const label = node('div', 'ac-ability-label cfr-label');
  const name = node('div', 'ac-ability-name', title);
  const badge = tipBadge(tip);
  if (badge) name.append(' ', badge);
  label.append(name);
  if (hint) label.append(node('div', 'ac-ability-desc', hint));
  const control = node('div', stacked ? 'cfr-field' : 'ac-config-control');
  const error = node('p', 'cfr-error');
  // Announced when it changes, without making every row a status region of the page.
  error.setAttribute('aria-live', 'polite');
  row.append(label, control, error);
  list.append(row);
  return {row, control, error};
}

/* One save path for every row. Returns commit(value, previous, revert). */
function saver({key, form, onSave, control, error}) {
  if (form && key) form.register(key);
  return async (value, previous, revert) => {
    error.textContent = '';
    if (form && key) { form.set(key, value); return; }
    if (!onSave) return;
    _markSaving(control);
    try {
      await onSave(value);
      _flashSaveCheck(control, true);
    } catch (failure) {
      const message = failure?.message || 'Not saved';
      _flashSaveCheck(control, false, message);
      error.textContent = message;
      revert(previous);
    }
  };
}

/** An on/off switch row. */
export function createToggleRow(list, {title, hint, tip, value = false, disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const wrap = node('label', 'conn-toggle-wrap');
  // A row's switch stays in its row: a subpage only lifts an unmarked switch into its header.
  wrap.dataset.inlineSwitch = '';
  const input = node('input', 'conn-toggle');
  input.type = 'checkbox';
  input.checked = !!value;
  input.disabled = disabled;
  input.setAttribute('aria-label', title);
  const track = node('span', 'conn-toggle-track');
  track.setAttribute('aria-hidden', 'true');
  wrap.append(input, track);
  control.append(wrap);
  const commit = saver({key, form, onSave, control, error});
  if (form && key) form.set(key, input.checked, true);
  input.addEventListener('click', event => requestTouchVibration(event));
  input.addEventListener('change', () => commit(input.checked, !input.checked, prior => { input.checked = prior; }));
  return {row, control: input, getValue: () => input.checked,
    setValue: next => { input.checked = !!next; }, setDisabled: off => { input.disabled = !!off; }};
}

/** A dropdown row; options are [value, label] pairs. */
export function createSelectRow(list, {title, hint, tip, options = [], value = '', disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const select = node('select', 'ac-input ac-input-sm');
  for (const [optionValue, optionLabel] of options) {
    const option = node('option', '', optionLabel);
    option.value = optionValue;
    select.append(option);
  }
  select.value = value;
  select.disabled = disabled;
  select.setAttribute('aria-label', title);
  control.append(select);
  const commit = saver({key, form, onSave, control, error});
  if (form && key) form.set(key, select.value, true);
  let current = select.value;
  select.addEventListener('change', () => {
    const previous = current;
    current = select.value;
    commit(select.value, previous, prior => { select.value = prior; current = prior; });
  });
  return {row, control: select, getValue: () => select.value,
    setValue: next => { select.value = next; current = select.value; }, setDisabled: off => { select.disabled = !!off; }};
}

/** A one-line text row; saves when the field is left, or 700 ms after typing stops. */
export function createTextRow(list, {title, hint, tip, value = '', placeholder = '', maxLength, disabled = false,
  stacked = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip, stacked});
  const input = node('input', 'ac-input');
  input.type = 'text';
  input.value = value;
  input.placeholder = placeholder;
  if (maxLength) input.maxLength = maxLength;
  input.disabled = disabled;
  input.setAttribute('aria-label', title);
  control.append(input);
  const commit = saver({key, form, onSave, control, error});
  if (form && key) form.set(key, input.value, true);
  let saved = input.value;
  let timer = null;
  const flush = () => {
    clearTimeout(timer);
    timer = null;
    if (input.value === saved && !form) return;
    const previous = saved;
    saved = input.value;
    commit(input.value, previous, prior => { input.value = prior; saved = prior; });
  };
  input.addEventListener('input', () => {
    if (form && key) { form.set(key, input.value); return; }
    clearTimeout(timer);
    timer = setTimeout(flush, 700);
  });
  input.addEventListener('blur', () => { if (!form) flush(); });
  return {row, control: input, getValue: () => input.value,
    setValue: next => { input.value = next; saved = next; }, setDisabled: off => { input.disabled = !!off; }};
}

/** A number row drawn as the shared stepper (arrows inside the field, clamped to min/max). */
export function createNumberRow(list, {title, hint, tip, value = 0, min, max, step = 1, disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const stepper = node('span', 'ac-num-stepper');
  const input = node('input', 'ac-input ac-config-num');
  input.type = 'number';
  input.inputMode = 'numeric';
  input.value = String(value);
  if (min != null) input.min = String(min);
  if (max != null) input.max = String(max);
  input.step = String(step);
  input.disabled = disabled;
  input.setAttribute('aria-label', title);
  const down = node('button', 'ac-num-step ac-num-down');
  const up = node('button', 'ac-num-step ac-num-up');
  down.type = up.type = 'button';
  down.innerHTML = ARROW('m15 18-6-6 6-6');
  up.innerHTML = ARROW('m9 18 6-6-6-6');
  down.setAttribute('aria-label', 'Decrease ' + title);
  up.setAttribute('aria-label', 'Increase ' + title);
  stepper.append(down, input, up);
  control.append(stepper);
  const commit = saver({key, form, onSave, control, error});
  const clamp = number => Math.min(max ?? Infinity, Math.max(min ?? -Infinity, number));
  let saved = Number(input.value);
  if (form && key) form.set(key, saved, true);
  const paint = () => {
    down.disabled = disabled || (min != null && Number(input.value) <= min);
    up.disabled = disabled || (max != null && Number(input.value) >= max);
  };
  const settle = () => {
    const number = clamp(Number(input.value));
    if (!Number.isFinite(number) || !input.checkValidity()) { input.setAttribute('aria-invalid', 'true'); return; }
    input.removeAttribute('aria-invalid');
    input.value = String(number);
    paint();
    if (number === saved && !form) return;
    const previous = saved;
    saved = number;
    commit(number, previous, prior => { input.value = String(prior); saved = prior; paint(); });
  };
  const nudge = (event, direction) => {
    requestTouchVibration(event);
    input.value = String(clamp((Number(input.value) || 0) + direction * step));
    settle();
  };
  // In a form, typing marks the draft changed at once so Save is ready before the field is left.
  if (form && key) input.addEventListener('input', () => { if (input.value !== '') form.set(key, Number(input.value)); });
  down.addEventListener('click', event => nudge(event, -1));
  up.addEventListener('click', event => nudge(event, 1));
  input.addEventListener('change', settle);
  paint();
  return {row, control: input, getValue: () => Number(input.value),
    setValue: next => { input.value = String(next); saved = Number(next); paint(); },
    setDisabled: off => { disabled = !!off; input.disabled = disabled; paint(); }};
}

/** A choice row: radio cards under the title; options are {value, title, hint}. */
export function createChoiceRow(list, {title, hint, tip, options = [], value = '', disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip, stacked: true});
  const group = node('div', 'cfr-choices');
  group.setAttribute('role', 'radiogroup');
  group.setAttribute('aria-label', title);
  const name = 'cfr-' + Math.random().toString(36).slice(2);
  const inputs = options.map(option => {
    const card = node('label', 'cfr-choice');
    const input = node('input');
    input.type = 'radio';
    input.name = name;
    input.value = option.value;
    input.checked = option.value === value;
    input.disabled = disabled;
    const text = node('span', 'cfr-choice-text');
    text.append(node('span', 'cfr-choice-title', option.title));
    if (option.hint) text.append(node('span', 'cfr-choice-hint', option.hint));
    card.append(input, text);
    if (option.extra) card.append(option.extra);
    group.append(card);
    return input;
  });
  control.append(group);
  const commit = saver({key, form, onSave, control, error});
  let current = value;
  if (form && key) form.set(key, current, true);
  const select = next => { for (const input of inputs) input.checked = input.value === next; current = next; };
  for (const input of inputs) {
    input.addEventListener('click', event => requestTouchVibration(event));
    input.addEventListener('change', () => {
      if (!input.checked) return;
      const previous = current;
      current = input.value;
      commit(input.value, previous, select);
    });
  }
  return {row, control: group, getValue: () => current, setValue: select,
    setDisabled: off => { for (const input of inputs) input.disabled = !!off; }};
}

/** A long text row: the shared Markdown editor under the title. Saves 900 ms after typing stops. */
export function createLongTextRow(list, {title, hint, tip, value = '', placeholder, defaultText = '', maxLength,
  minHeight = 120, maxHeight = 340, disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip, stacked: true});
  let saved = value;
  let timer = null;
  const commit = saver({key, form, onSave, control, error});
  const editor = createRichTextEditor({value, label: title, placeholder, defaultText, maxLength, minHeight, maxHeight, disabled,
    onChange: next => {
      const text = next.replace(/\n$/, '');
      if (form && key) { form.set(key, text); return; }
      clearTimeout(timer);
      timer = setTimeout(() => {
        if (text === saved) return;
        const previous = saved;
        saved = text;
        commit(text, previous, prior => { editor.setValue(prior); saved = prior; });
      }, 900);
    }});
  if (form && key) form.set(key, value, true);
  control.append(editor.element);
  return {row, control: editor.element, editor, getValue: editor.getValue,
    setValue: next => { editor.setValue(next); saved = next; }, setDisabled: editor.setDisabled};
}

/** A read-only text row (skills, prompt slots): the shared editor without its toolbar. */
export function createReadTextRow(list, {title, hint, tip, value = '', placeholder = 'Loading…', minHeight = 60, maxHeight = 340} = {}) {
  const {row, control} = shell(list, {title, hint, tip, stacked: true});
  control.classList.add('cfr-read');
  const editor = createRichTextEditor({value, label: title, placeholder, readOnly: true, minHeight, maxHeight});
  control.append(editor.element);
  return {row, control: editor.element, setValue: editor.setValue};
}

/** A row of buttons; actions are {label, kind: 'primary'|'danger'|'', onClick}. */
export function createActionRow(list, {title, hint, tip, actions = []} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const bar = node('div', 'cfr-actions');
  const buttons = actions.map(action => {
    const button = node('button', 'ac-btn' + (action.kind ? ' ac-btn-' + action.kind : ''), action.label);
    button.type = 'button';
    button.addEventListener('click', async event => {
      requestTouchVibration(event);
      error.textContent = '';
      button.disabled = true;
      try { await action.onClick?.(event); }
      catch (failure) { error.textContent = failure?.message || 'Action failed'; }
      finally { button.disabled = false; }
    });
    bar.append(button);
    return button;
  });
  control.append(bar);
  return {row, control: bar, buttons};
}

/** A form: rows that save together with one Save button, plus Reset and an unsaved-changes note.
 *  onSave(values) and onReset() return promises; values maps each row key to its draft. */
export function createConfigForm(host, {onSave, onReset, saveLabel = 'Save', resetLabel = ''} = {}) {
  styles();
  const values = new Map();
  const savedValues = new Map();
  const bar = node('div', 'cfr-form-bar');
  const state = node('span', 'cfr-form-state');
  state.setAttribute('role', 'status');
  const reset = resetLabel ? node('button', 'ac-btn', resetLabel) : null;
  const save = node('button', 'ac-btn ac-btn-primary', saveLabel);
  if (reset) reset.type = 'button';
  save.type = 'button';
  save.disabled = true;
  bar.append(state, ...(reset ? [reset] : []), save);
  const dirty = () => [...values].some(([key, value]) => savedValues.get(key) !== value);
  const paint = (text = dirty() ? 'Unsaved changes' : '', kind = dirty() ? 'dirty' : '') => {
    state.textContent = text;
    state.dataset.state = kind;
    save.disabled = !dirty();
  };
  const form = {
    element: bar,
    register: key => { if (!values.has(key)) values.set(key, undefined); },
    set: (key, value, initial = false) => {
      values.set(key, value);
      if (initial) savedValues.set(key, value);
      paint();
    },
    values: () => Object.fromEntries(values),
    attach: () => host.append(bar),
  };
  save.addEventListener('click', async event => {
    requestTouchVibration(event);
    save.disabled = true;
    paint('Saving…', '');
    try {
      await onSave?.(form.values());
      for (const [key, value] of values) savedValues.set(key, value);
      paint('Saved', '');
    } catch (failure) {
      paint(failure?.message || 'Not saved', 'error');
      save.disabled = false;
    }
  });
  reset?.addEventListener('click', async event => {
    requestTouchVibration(event);
    try { await onReset?.(); paint(); }
    catch (failure) { paint(failure?.message || 'Not reset', 'error'); }
  });
  return form;
}
