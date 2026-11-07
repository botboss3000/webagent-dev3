import test from 'node:test';
import assert from 'node:assert/strict';
import {retryMs, taskParam} from './broker-card.js';

test('the page reads a well-formed Broker ID from its query', () => {
  assert.equal(taskParam('?task_id=broker-chip-2'), 'broker-chip-2');
  assert.equal(taskParam('?task_id=short'), '');
  assert.equal(taskParam('?task_id=bad%20id%3Cx%3E'), '');
  assert.equal(taskParam(''), '');
});

test('failed reads retry sooner, doubling up to the normal refresh', () => {
  assert.deepEqual([1, 2, 3, 4, 5].map(retryMs), [2000, 4000, 8000, 15000, 15000]);
});
