'use strict';

// Shared form controls: the switch, dropdown, text field, number stepper and choice
// cards every page uses. One file so they share one look and one behaviour; their
// styles are in controls.css beside this file (loaded by index.html for every page).
//
// A control only draws itself and reports changes through onChange. Saving, rows,
// labels and errors belong to the caller (ui/shared/config-rows/ for setting rows).
// Each returns {element, input, getValue, setValue, setDisabled}: element goes in the
// page, input is the focusable part.

import { requestTouchVibration } from '../vibrate/vibrate.js';

const ARROW = d => '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="' + d + '"/></svg>';

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
};

/** The on/off switch. onChange(checked). */
export function createSwitch({label = '', value = false, disabled = false, onChange} = {}) {
  const element = node('label', 'conn-toggle-wrap');
  const input = node('input', 'conn-toggle');
  input.type = 'checkbox';
  input.checked = !!value;
  input.disabled = disabled;
  if (label) input.setAttribute('aria-label', label);
  const track = node('span', 'conn-toggle-track');
  track.setAttribute('aria-hidden', 'true');
  element.append(input, track);
  input.addEventListener('click', event => requestTouchVibration(event));
  input.addEventListener('change', () => onChange?.(input.checked));
  return {element, input, getValue: () => input.checked,
    setValue: next => { input.checked = !!next; }, setDisabled: off => { input.disabled = !!off; }};
}

/** A dropdown; options are [value, label] pairs. small is the compact size rows use. onChange(value). */
export function createSelect({label = '', options = [], value = '', disabled = false, small = true, onChange} = {}) {
  const input = node('select', 'ac-input' + (small ? ' ac-input-sm' : ''));
  for (const [optionValue, optionLabel] of options) {
    const option = node('option', '', optionLabel);
    option.value = optionValue;
    input.append(option);
  }
  input.value = value;
  input.disabled = disabled;
  if (label) input.setAttribute('aria-label', label);
  input.addEventListener('change', () => onChange?.(input.value));
  return {element: input, input, getValue: () => input.value,
    setValue: next => { input.value = next; }, setDisabled: off => { input.disabled = !!off; }};
}

/** A one-line text field. onInput(value) on every keystroke, onChange(value) when it is left. */
export function createTextField({label = '', value = '', placeholder = '', maxLength, type = 'text', disabled = false,
  onInput, onChange} = {}) {
  const input = node('input', 'ac-input');
  input.type = type;
  input.value = value;
  input.placeholder = placeholder;
  if (maxLength) input.maxLength = maxLength;
  input.disabled = disabled;
  if (label) input.setAttribute('aria-label', label);
  input.addEventListener('input', () => onInput?.(input.value));
  input.addEventListener('blur', () => onChange?.(input.value));
  return {element: input, input, getValue: () => input.value,
    setValue: next => { input.value = next; }, setDisabled: off => { input.disabled = !!off; }};
}

/** A number field drawn as a stepper: arrows inside the field, clamped to min/max.
 *  onChange(number) after an arrow press or when a typed value is settled; onInput(number) while typing. */
export function createStepper({label = '', value = 0, min, max, step = 1, disabled = false, onInput, onChange} = {}) {
  const element = node('span', 'ac-num-stepper');
  const input = node('input', 'ac-input ac-config-num');
  input.type = 'number';
  input.inputMode = 'numeric';
  input.value = String(value);
  if (min != null) input.min = String(min);
  if (max != null) input.max = String(max);
  input.step = String(step);
  input.disabled = disabled;
  if (label) input.setAttribute('aria-label', label);
  const down = node('button', 'ac-num-step ac-num-down');
  const up = node('button', 'ac-num-step ac-num-up');
  down.type = up.type = 'button';
  down.innerHTML = ARROW('m15 18-6-6 6-6');
  up.innerHTML = ARROW('m9 18 6-6-6-6');
  down.setAttribute('aria-label', 'Decrease' + (label ? ' ' + label : ''));
  up.setAttribute('aria-label', 'Increase' + (label ? ' ' + label : ''));
  element.append(down, input, up);
  const clamp = number => Math.min(max ?? Infinity, Math.max(min ?? -Infinity, number));
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
    onChange?.(number);
  };
  const nudge = (event, direction) => {
    requestTouchVibration(event);
    input.value = String(clamp((Number(input.value) || 0) + direction * step));
    settle();
  };
  input.addEventListener('input', () => { if (input.value !== '') onInput?.(Number(input.value)); });
  input.addEventListener('change', settle);
  down.addEventListener('click', event => nudge(event, -1));
  up.addEventListener('click', event => nudge(event, 1));
  paint();
  return {element, input, getValue: () => Number(input.value),
    setValue: next => { input.value = String(next); paint(); },
    setDisabled: off => { disabled = !!off; input.disabled = disabled; paint(); }};
}

/** Radio cards, one per option {value, title, hint, extra}; extra is an element shown in the card. onChange(value). */
export function createChoiceCards({label = '', options = [], value = '', disabled = false, onChange} = {}) {
  const element = node('div', 'ctl-choices');
  element.setAttribute('role', 'radiogroup');
  if (label) element.setAttribute('aria-label', label);
  const name = 'ctl-' + Math.random().toString(36).slice(2);
  let current = value;
  const inputs = options.map(option => {
    const card = node('label', 'ctl-choice');
    const input = node('input');
    input.type = 'radio';
    input.name = name;
    input.value = option.value;
    input.checked = option.value === value;
    input.disabled = disabled;
    const text = node('span', 'ctl-choice-text');
    text.append(node('span', 'ctl-choice-title', option.title));
    if (option.hint) text.append(node('span', 'ctl-choice-hint', option.hint));
    card.append(input, text);
    if (option.extra) card.append(option.extra);
    element.append(card);
    input.addEventListener('click', event => requestTouchVibration(event));
    input.addEventListener('change', () => {
      if (!input.checked) return;
      current = input.value;
      onChange?.(current);
    });
    return input;
  });
  const setValue = next => { for (const input of inputs) input.checked = input.value === next; current = next; };
  return {element, input: inputs[0] || null, getValue: () => current, setValue,
    setDisabled: off => { for (const input of inputs) input.disabled = !!off; }};
}
