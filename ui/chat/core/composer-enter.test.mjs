import test from 'node:test';
import assert from 'node:assert/strict';
import { ENTER_BEHAVIORS, resolveEnterBehavior, enterSubmits } from './composer-enter.js';

const press = (overrides = {}) => ({ key: 'Enter', shiftKey: false, ctrlKey: false, metaKey: false, isComposing: false, ...overrides });

test('unset or unknown enter_behavior defaults to newline', () => {
  assert.equal(resolveEnterBehavior(null), 'newline');
  assert.equal(resolveEnterBehavior({}), 'newline');
  assert.equal(resolveEnterBehavior({ composer: {} }), 'newline');
  assert.equal(resolveEnterBehavior({ composer: { enter_behavior: 'bogus' } }), 'newline');
});

test('configured behaviors resolve unchanged', () => {
  assert.deepEqual(ENTER_BEHAVIORS, ['newline', 'send', 'auto']);
  for (const behavior of ENTER_BEHAVIORS) {
    assert.equal(resolveEnterBehavior({ composer: { enter_behavior: behavior } }), behavior);
  }
});

test('newline inserts a line break and sends only with Ctrl/Cmd+Enter', () => {
  assert.equal(enterSubmits(press(), 'newline', false), false);
  assert.equal(enterSubmits(press({ ctrlKey: true }), 'newline', false), true);
  assert.equal(enterSubmits(press({ metaKey: true }), 'newline', false), true);
});

test('send submits on Enter for desktop and touch devices', () => {
  assert.equal(enterSubmits(press(), 'send', false), true);
  assert.equal(enterSubmits(press(), 'send', true), true);
});

test('auto sends on desktop and inserts a newline on touch devices', () => {
  assert.equal(enterSubmits(press(), 'auto', false), true);
  assert.equal(enterSubmits(press(), 'auto', true), false);
  assert.equal(enterSubmits(press({ ctrlKey: true }), 'auto', true), true);
});

test('Shift+Enter and IME composition never submit, and other keys are ignored', () => {
  for (const behavior of ENTER_BEHAVIORS) {
    assert.equal(enterSubmits(press({ shiftKey: true }), behavior, false), false);
    assert.equal(enterSubmits(press({ isComposing: true }), behavior, false), false);
    assert.equal(enterSubmits(press({ key: 'a' }), behavior, false), false);
  }
});
