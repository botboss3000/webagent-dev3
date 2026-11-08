import test from 'node:test';
import assert from 'node:assert/strict';
import { shouldCreateSession } from './session-start.js';

test('no open conversation starts a new session', () => {
  assert.equal(shouldCreateSession(), true);
  assert.equal(shouldCreateSession({ session: '' }), true);
});

test('a conversation that is still loading is not duplicated', () => {
  assert.equal(shouldCreateSession({ session: 'a' }), false);
  assert.equal(shouldCreateSession({ session: 'a', ready: false }), false);
});

test('an untouched open conversation is reused', () => {
  assert.equal(shouldCreateSession({ session: 'a', ready: true }), false);
  assert.equal(shouldCreateSession({ session: 'a', ready: true, draft: '   ' }), false);
});

test('content in the open conversation allows another new session', () => {
  assert.equal(shouldCreateSession({ session: 'a', ready: true, lastSeq: 1 }), true);
  assert.equal(shouldCreateSession({ session: 'a', ready: true, runs: 1 }), true);
  assert.equal(shouldCreateSession({ session: 'a', ready: true, rows: 1 }), true);
  assert.equal(shouldCreateSession({ session: 'a', ready: true, draft: 'hi' }), true);
  assert.equal(shouldCreateSession({ session: 'a', ready: true, attachments: 1 }), true);
});

test('a conversation that failed to load does not block a new session', () => {
  assert.equal(shouldCreateSession({ session: 'gone', ready: false, failed: true }), true);
});
