// The back-press component: one claimed press runs its handler once, an unclaimed press
// falls through to the shell, and a surface that closes itself drops its own entry
// without the shell mistaking that pop for a new press.
import assert from 'node:assert/strict';

const stack = [{ webagentBack: false }];
let index = 0;
globalThis.history = {
  get state() {
    return stack[index];
  },
  pushState(state) {
    index += 1;
    stack.length = index + 1;
    stack[index] = state;
  },
  back() {
    index -= 1;
  },
};
globalThis.location = { href: 'https://example.test/web/index.html' };

const { pushBackHandler, consumeBack } = await import('./history-back.js');

assert.equal(consumeBack(), false, 'an unclaimed press falls through to the shell');

let closed = 0;
const release = pushBackHandler(() => {
  closed += 1;
  return true;
});
assert.equal(history.state.webagentBack, true, 'the claim pushes its own history entry');
assert.equal(consumeBack(), true, 'the claimed press is consumed');
assert.equal(closed, 1, 'the handler ran once');

// The press already consumed the entry, so the surface closing itself must not pop again.
const afterPress = index;
release();
assert.equal(index, afterPress, 'releasing a consumed claim leaves history alone');

// A surface that closes itself removes its entry, and the shell swallows its own pop.
const releaseOwn = pushBackHandler(() => false);
const claimed = index;
releaseOwn();
assert.equal(index, claimed - 1, 'closing the surface pops its own entry');
assert.equal(consumeBack(), true, 'the shell swallows the pop it caused itself');

console.log('history-back: claimed, unclaimed and self-pop cases passed');
