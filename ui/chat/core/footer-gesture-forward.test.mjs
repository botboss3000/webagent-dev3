import test from 'node:test';
import assert from 'node:assert/strict';
import { forwardFooterGestures, pullRefreshOwnsPull } from './footer-gesture-forward.js';

const selection = { removed: 0, removeAllRanges() { this.removed += 1; } };
globalThis.window = {
  getComputedStyle: node => node.style || { overflowY: 'visible' },
  getSelection: () => selection,
};
globalThis.document = { body: {}, documentElement: {}, activeElement: null };

function makeElement() {
  const handlers = new Map();
  const registrations = [];
  return {
    dataset: {},
    nodeType: 1,
    parentElement: null,
    scrollHeight: 0,
    clientHeight: 0,
    addEventListener(type, fn, options = {}) {
      handlers.set(type, fn);
      registrations.push({ type, options });
    },
    dispatch(type, event = {}) {
      const handler = handlers.get(type);
      if (!handler) return undefined;
      const node = {
        ctrlKey: false, deltaY: 100, deltaMode: 0, defaultPrevented: false,
        target: null, prevented: false,
        preventDefault() { this.defaultPrevented = true; this.prevented = true; },
        ...event,
      };
      if (!node.target) node.target = this;
      handler(node);
      return node;
    },
    registrations,
  };
}

function fixture(options = {}) {
  const calls = [];
  const scroller = { clientHeight: 400, scrollBy({ top }) { calls.push(top); } };
  const footer = makeElement();
  const row = makeElement();
  let intent = 0;
  const dispose = forwardFooterGestures({
    targets: [footer, row], scroller, markIntent: () => { intent += 1; }, ...options,
  });
  return { calls, scroller, footer, row, dispose, intent: () => intent };
}

test('wheel over the footer scrolls the transcript and marks intent', () => {
  const f = fixture();
  const event = f.footer.dispatch('wheel', { deltaY: 120 });
  assert.deepEqual(f.calls, [120]);
  assert.equal(event.prevented, true);
  assert.equal(f.intent(), 1);
});

test('wheel ignores Ctrl, handled events, and zero deltas', () => {
  const f = fixture();
  f.footer.dispatch('wheel', { ctrlKey: true });
  f.footer.dispatch('wheel', { defaultPrevented: true });
  f.footer.dispatch('wheel', { deltaY: 0 });
  assert.deepEqual(f.calls, []);
  assert.equal(f.intent(), 0);
});

test('wheel normalizes line and page deltas', () => {
  const f = fixture();
  f.footer.dispatch('wheel', { deltaY: 2, deltaMode: 1 });
  f.footer.dispatch('wheel', { deltaY: 2, deltaMode: 2 });
  assert.deepEqual(f.calls, [32, 800]);
});

test('nested scrollables inside the footer keep their gesture', () => {
  const f = fixture();
  const nested = makeElement();
  nested.style = { overflowY: 'auto' };
  nested.scrollHeight = 500;
  nested.clientHeight = 100;
  nested.parentElement = f.footer;
  f.footer.dispatch('wheel', { target: nested });
  assert.deepEqual(f.calls, []);
  assert.equal(f.intent(), 0);
});

test('touch drag engages after the threshold and scrolls 1:1', () => {
  const f = fixture();
  const start = f.footer.dispatch('touchstart', { touches: [{ clientY: 300 }] });
  assert.equal(start.prevented, false);
  f.footer.dispatch('touchmove', { touches: [{ clientY: 303 }] });
  assert.deepEqual(f.calls, []);
  const move = f.footer.dispatch('touchmove', { touches: [{ clientY: 290 }] });
  assert.deepEqual(f.calls, [10]);
  assert.equal(move.prevented, true);
  assert.equal(f.intent(), 1);
  assert.equal(selection.removed, 1);
  f.footer.dispatch('touchmove', { touches: [{ clientY: 280 }] });
  assert.deepEqual(f.calls, [10, 10]);
  f.footer.dispatch('touchend');
  f.footer.dispatch('touchmove', { touches: [{ clientY: 200 }] });
  assert.deepEqual(f.calls, [10, 10]);
});

test('touch inside a nested scroller is ignored', () => {
  const f = fixture();
  const nested = makeElement();
  nested.style = { overflowY: 'scroll' };
  nested.scrollHeight = 500;
  nested.clientHeight = 100;
  f.footer.dispatch('touchstart', { target: nested, touches: [{ clientY: 300 }] });
  f.footer.dispatch('touchmove', { target: nested, touches: [{ clientY: 280 }] });
  assert.deepEqual(f.calls, []);
  assert.equal(f.intent(), 0);
});

test('a press on the footer keeps the focused editor instead of closing its keyboard', () => {
  const f = fixture();
  const editor = { matches: () => true };
  globalThis.document.activeElement = editor;
  f.footer.contains = node => node === editor;
  assert.equal(f.footer.dispatch('pointerdown', { button: 0 }).prevented, true);
  // A press that lands in an editor keeps its caret placement.
  assert.equal(f.footer.dispatch('pointerdown', { button: 0, target: { closest: () => editor } }).prevented, false);
  // A press outside this surface, another button, or with no focused editor is untouched.
  f.footer.contains = () => false;
  assert.equal(f.footer.dispatch('pointerdown', { button: 0 }).prevented, false);
  f.footer.contains = node => node === editor;
  assert.equal(f.footer.dispatch('pointerdown', { button: 2 }).prevented, false);
  globalThis.document.activeElement = {};
  assert.equal(f.footer.dispatch('pointerdown', { button: 0 }).prevented, false);
  globalThis.document.activeElement = null;
});

test('a claimed touch move cancels forwarding for the rest of the gesture', () => {
  const f = fixture();
  f.footer.dispatch('touchstart', { touches: [{ clientY: 300 }] });
  f.footer.dispatch('touchmove', { touches: [{ clientY: 280 }], defaultPrevented: true });
  f.footer.dispatch('touchmove', { touches: [{ clientY: 260 }] });
  assert.deepEqual(f.calls, []);
});

test('pull-refresh owns only downward movement while its indicator is visible', () => {
  const visible = { hidden: false };
  const hidden = { hidden: true };
  assert.equal(pullRefreshOwnsPull({ lastY: 100, currentY: 120, indicator: visible }), true);
  assert.equal(pullRefreshOwnsPull({ lastY: 100, currentY: 90, indicator: visible }), false);
  assert.equal(pullRefreshOwnsPull({ lastY: 100, currentY: 120, indicator: hidden }), false);
});

test('a deferred downward pull starts scrolling when the finger reverses upward', () => {
  const indicator = { hidden: false };
  const f = fixture({
    touchGuard: args => pullRefreshOwnsPull({ ...args, indicator }),
  });
  f.footer.dispatch('touchstart', { touches: [{ clientY: 300 }] });
  f.footer.dispatch('touchmove', { touches: [{ clientY: 330 }] });
  assert.deepEqual(f.calls, []);
  assert.equal(f.intent(), 0);
  f.footer.dispatch('touchmove', { touches: [{ clientY: 290 }] });
  assert.deepEqual(f.calls, [40]);
  assert.equal(f.intent(), 1);
});

test('canForward vetoes wheel and touch without claiming the gesture', () => {
  let allow = false;
  const f = fixture({ canForward: () => allow });
  f.footer.dispatch('wheel', { deltaY: 80 });
  f.footer.dispatch('touchstart', { touches: [{ clientY: 300 }] });
  f.footer.dispatch('touchmove', { touches: [{ clientY: 280 }] });
  assert.deepEqual(f.calls, []);
  assert.equal(f.intent(), 0);
  allow = true;
  f.footer.dispatch('wheel', { deltaY: 80 });
  assert.deepEqual(f.calls, [80]);
});

test('wiring is idempotent and the disposer aborts every listener', () => {
  const f = fixture();
  const duplicate = forwardFooterGestures({
    targets: [f.footer], scroller: f.scroller, markIntent: () => {},
  });
  f.footer.dispatch('wheel', { deltaY: 50 });
  assert.deepEqual(f.calls, [50]);
  duplicate();
  f.dispose();
  assert.ok(f.footer.registrations.every(({ options }) => options.signal.aborted));
});
