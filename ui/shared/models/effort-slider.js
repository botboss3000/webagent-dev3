'use strict';
// Reasoning-effort slider shared by the chat footer picker and the Models page.
// V2 design: the first tick is "default" (no explicit choice), the rest are the
// model's supported levels from its provider file, lowest first.
const LABELS = {
  none: 'No Thinking', minimal: 'Minimal Effort', low: 'Low Effort', medium: 'Medium Effort',
  high: 'High Effort', xhigh: 'Extra High Effort', max: 'Max Effort',
};
export const effortLabel = level => LABELS[level] || level;

/** Supported levels and provider default for a model, from a provider preset (`/models/providers` row). */
export function modelEfforts(preset, model) {
  const listed = preset?.models?.find(row => row.id === model);
  if (listed && Array.isArray(listed.efforts)) return { levels: listed.efforts, providerDefault: listed.default_effort || '' };
  return { levels: preset?.efforts || [], providerDefault: preset?.default_effort || '' };
}

/**
 * Build the slider. `defaultName` names what the first tick means ("Saved default",
 * "Provider default"); `onCommit(level|null)` receives the chosen level, or null for default.
 * The returned element carries the panel-scoped `.cml-effort-*` styles.
 */
export function createEffortSlider({ defaultName = 'Default', onCommit }) {
  const bar = document.createElement('div');
  bar.className = 'cml-effort-bar';
  const ticks = document.createElement('span');
  ticks.className = 'cml-effort-ticks';
  ticks.setAttribute('aria-hidden', 'true');
  const slider = document.createElement('input');
  slider.className = 'cml-effort-slider';
  slider.type = 'range'; slider.min = '0'; slider.max = '0'; slider.step = '1';
  slider.setAttribute('aria-label', 'Reasoning effort');
  // Visual twin of aria-valuetext, so it stays hidden from assistive technology.
  const readout = document.createElement('span');
  readout.className = 'cml-effort-label';
  readout.setAttribute('aria-hidden', 'true');
  bar.append(ticks, slider, readout);
  let stops = ['default'], defaultLevel = '', committed = 0;

  const index = () => Math.max(0, Math.min(stops.length - 1, Math.round(Number(slider.value) || 0)));
  function paint() {
    const level = stops[index()], isDefault = level === 'default';
    const label = isDefault ? `${defaultName}${defaultLevel ? ' · ' + effortLabel(defaultLevel) : ''}` : effortLabel(level);
    const progress = `${(index() / Math.max(1, stops.length - 1)) * 100}%`;
    bar.style.setProperty('--cml-effort-progress', progress);
    slider.classList.toggle('is-default', isDefault);
    slider.setAttribute('aria-valuetext', `Reasoning effort: ${label}`);
    slider.title = `Reasoning effort: ${label}`;
    readout.textContent = label;
    readout.classList.toggle('is-default', isDefault);
  }
  slider.addEventListener('input', paint);
  slider.addEventListener('change', async () => {
    const chosen = index();
    if (chosen === committed) return;
    const level = stops[chosen];
    // Disabling a range blurs it; restore focus so arrow-key adjustment continues.
    const refocus = document.activeElement === slider;
    slider.disabled = true;
    try { await onCommit(level === 'default' ? null : level); committed = chosen; }
    catch { slider.value = String(committed); paint(); }
    finally { slider.disabled = false; if (refocus && document.contains(slider)) slider.focus({ preventScroll: true }); }
  });

  return {
    element: bar,
    /** Show `levels` with `value` selected (null = default); no levels disables the control. */
    set({ levels = [], value = null, defaultEffort = '', disabled = false }) {
      stops = ['default', ...levels];
      defaultLevel = defaultEffort || '';
      slider.max = String(stops.length - 1);
      ticks.replaceChildren(...stops.map(() => document.createElement('i')));
      committed = Math.max(0, stops.indexOf(value || 'default'));
      slider.value = String(committed);
      slider.disabled = disabled || !levels.length;
      paint();
      if (!levels.length) readout.textContent = 'Effort not adjustable for this model';
    },
  };
}
