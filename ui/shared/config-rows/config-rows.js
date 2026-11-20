'use strict';

// Shared configuration rows: one look and one save behaviour for every setting on a
// config page. A row is the app's option-row frame (.ac-list / .ac-ability-row, styled in
// ui/shared/option-list/option-list.css) around one control from ui/shared/controls/controls.js, in the
// .ac-config-control slot. What the rows add is styled in config-rows.css.
//
// Saving. A row either saves as you go (pass onSave: it gets the new value and returns a
// promise; the spinner, check and revert-on-failure come from dom-utils.js) or belongs to
// a form (pass form from createConfigForm and a key: the form collects the draft and
// saves it with one Save button).

import { _markSaving, _flashSaveCheck } from '../models/dom-utils.js';
import { tipBadge } from '../models/field-tip.js';
import { requestTouchVibration } from '../vibrate/vibrate.js';
import { createRichTextEditor } from '../rich-text-editor/rich-text-editor.js';
import { createSwitch, createSelect, createTextField, createStepper, createChoiceCards } from '../controls/controls.js';

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
};

/** The bordered list a group of rows lives in. */
export function createConfigList(host, {label = ''} = {}) {
  const list = node('div', 'ac-list');
  if (label) list.setAttribute('aria-label', label);
  host.append(list);
  return list;
}

/* Row shell: title (with optional help badge) on the left, the control on the right;
   the hint drops to its own line. Stacked rows put the field under the title. */
function shell(list, {title, hint, tip, stacked = false}) {
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
  const commit = saver({key, form, onSave, control, error});
  const toggle = createSwitch({label: title, value, disabled,
    onChange: checked => commit(checked, !checked, prior => toggle.setValue(prior))});
  // A row's switch stays in its row: a subpage only lifts an unmarked switch into its header.
  toggle.element.dataset.inlineSwitch = '';
  control.append(toggle.element);
  if (form && key) form.set(key, toggle.getValue(), true);
  return {row, control: toggle.input, getValue: toggle.getValue, setValue: toggle.setValue, setDisabled: toggle.setDisabled};
}

/** A dropdown row; options are [value, label] pairs. */
export function createSelectRow(list, {title, hint, tip, options = [], value = '', disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const commit = saver({key, form, onSave, control, error});
  let current = value;
  const select = createSelect({label: title, options, value, disabled, onChange: next => {
    const previous = current;
    current = next;
    commit(next, previous, prior => { select.setValue(prior); current = prior; });
  }});
  current = select.getValue();
  control.append(select.element);
  if (form && key) form.set(key, current, true);
  return {row, control: select.input, getValue: select.getValue,
    setValue: next => { select.setValue(next); current = select.getValue(); }, setDisabled: select.setDisabled};
}

/** A one-line text row; saves when the field is left, or 700 ms after typing stops. */
export function createTextRow(list, {title, hint, tip, value = '', placeholder = '', maxLength, disabled = false,
  stacked = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip, stacked});
  const commit = saver({key, form, onSave, control, error});
  let saved = value;
  let timer = null;
  const flush = () => {
    clearTimeout(timer);
    timer = null;
    if (field.getValue() === saved && !form) return;
    const previous = saved;
    saved = field.getValue();
    commit(saved, previous, prior => { field.setValue(prior); saved = prior; });
  };
  const field = createTextField({label: title, value, placeholder, maxLength, disabled,
    onInput: next => {
      if (form && key) { form.set(key, next); return; }
      clearTimeout(timer);
      timer = setTimeout(flush, 700);
    },
    onChange: () => { if (!form) flush(); }});
  control.append(field.element);
  if (form && key) form.set(key, value, true);
  return {row, control: field.input, getValue: field.getValue,
    setValue: next => { field.setValue(next); saved = next; }, setDisabled: field.setDisabled};
}

/** A number row drawn as the shared stepper (arrows inside the field, clamped to min/max);
 *  unit, when given, is shown after the field. */
export function createNumberRow(list, {title, hint, tip, value = 0, min, max, step = 1, unit = '', disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const commit = saver({key, form, onSave, control, error});
  let saved = Number(value);
  const stepper = createStepper({label: title, value, min, max, step, disabled,
    // In a form, typing marks the draft changed at once so Save is ready before the field is left.
    onInput: number => { if (form && key) form.set(key, number); },
    onChange: number => {
      if (number === saved && !form) return;
      const previous = saved;
      saved = number;
      commit(number, previous, prior => { stepper.setValue(prior); saved = prior; });
    }});
  control.append(stepper.element);
  if (unit) control.append(node('span', 'cfr-unit', unit));
  if (form && key) form.set(key, saved, true);
  return {row, control: stepper.input, getValue: stepper.getValue,
    setValue: next => { stepper.setValue(next); saved = Number(next); }, setDisabled: stepper.setDisabled};
}

/** A width row: a stepper and a px/% dropdown; the value is {width, unit}. Saves the pair
 *  whenever either part changes. min and max bound the number. */
export function createLengthRow(list, {title, hint, tip, value = {width: 0, unit: 'px'}, min, max, disabled = false, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip});
  const commit = saver({onSave, control, error});
  let saved = {width: Number(value?.width ?? 0), unit: value?.unit === '%' ? '%' : 'px'};
  const changed = () => {
    const next = {width: stepper.getValue(), unit: select.getValue()};
    if (next.width === saved.width && next.unit === saved.unit) return;
    const previous = saved;
    saved = next;
    commit(next, previous, prior => { stepper.setValue(prior.width); select.setValue(prior.unit); saved = prior; });
  };
  const stepper = createStepper({label: title, value: saved.width, min, max, step: 1, disabled, onChange: changed});
  const select = createSelect({label: title + ' unit', options: [['px', 'px'], ['%', '%']], value: saved.unit, disabled,
    onChange: changed});
  const pair = node('span', 'cfr-length');
  pair.append(stepper.element, select.element);
  control.append(pair);
  return {row, control: stepper.input, getValue: () => ({...saved}),
    setValue: next => { saved = {width: Number(next.width), unit: next.unit}; stepper.setValue(saved.width); select.setValue(saved.unit); },
    setDisabled: off => { stepper.setDisabled(off); select.setDisabled(off); }};
}

/** A choice row: radio cards under the title; options are {value, title, hint}. */
export function createChoiceRow(list, {title, hint, tip, options = [], value = '', disabled = false, key, form, onSave} = {}) {
  const {row, control, error} = shell(list, {title, hint, tip, stacked: true});
  const commit = saver({key, form, onSave, control, error});
  let current = value;
  const cards = createChoiceCards({label: title, options, value, disabled, onChange: next => {
    const previous = current;
    current = next;
    commit(next, previous, prior => { cards.setValue(prior); current = prior; });
  }});
  control.append(cards.element);
  if (form && key) form.set(key, current, true);
  return {row, control: cards.element, getValue: cards.getValue,
    setValue: next => { cards.setValue(next); current = next; }, setDisabled: cards.setDisabled};
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
