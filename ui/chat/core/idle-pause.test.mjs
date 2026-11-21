import test from 'node:test';
import assert from 'node:assert/strict';
import { idleSettings, idlePaused, watchIdlePause } from './idle-pause.js';

/** A document with visibility and listeners, plus a clock and one-shot timers driven by the test. */
function harness(settings) {
  const listeners = new Map();
  const doc = {
    hidden: false,
    addEventListener(type, fn) { listeners.set(type, fn); },
    removeEventListener(type) { listeners.delete(type); },
  };
  let clock = 0, pending = null;
  const fire = type => listeners.get(type)?.();
  const advance = ms => {
    clock += ms;
    while (pending && pending.at <= clock) { const due = pending; pending = null; due.fn(); }
  };
  const calls = [];
  const watcher = watchIdlePause({
    getConfig: () => ({idle_pause: settings}), doc, now: () => clock,
    schedule: (fn, ms) => (pending = {fn, at: clock + ms}), cancel: () => { pending = null; },
    onPause: () => calls.push('pause'), onResume: () => calls.push('resume'),
  });
  return {doc, fire, advance, calls, watcher, listeners};
}

test('settings are bounded and off unless enabled', () => {
  assert.equal(idleSettings({}), null);
  assert.equal(idleSettings({idle_pause: {enabled: false, hidden_ms: 120000}}), null);
  assert.deepEqual(idleSettings({idle_pause: {enabled: true, hidden_ms: 1, idle_ms: 0}}), {hidden: 30000, idle: 0});
  assert.equal(idleSettings({idle_pause: {enabled: true, hidden_ms: 0, idle_ms: 0}}), null);
});

test('a visible tab pauses after idle_ms without input and resumes on input', () => {
  const h = harness({enabled: true, hidden_ms: 120000, idle_ms: 900000});
  h.advance(899000);
  assert.deepEqual(h.calls, []);
  h.fire('keydown');
  h.advance(899000);
  assert.deepEqual(h.calls, [], 'input restarts the idle period');
  h.advance(61000);
  assert.deepEqual(h.calls, ['pause']);
  assert.equal(idlePaused(), true);
  h.fire('pointerdown');
  assert.deepEqual(h.calls, ['pause', 'resume']);
  assert.equal(idlePaused(), false);
  h.watcher.destroy();
});

test('a hidden tab pauses after hidden_ms and resumes when shown', () => {
  const h = harness({enabled: true, hidden_ms: 120000, idle_ms: 900000});
  h.doc.hidden = true; h.fire('visibilitychange');
  h.advance(121000);
  assert.deepEqual(h.calls, ['pause']);
  h.doc.hidden = false; h.fire('visibilitychange');
  assert.deepEqual(h.calls, ['pause', 'resume']);
  h.watcher.destroy();
  assert.equal(h.listeners.size, 0, 'destroy removes every listener');
});
