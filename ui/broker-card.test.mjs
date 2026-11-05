import test from 'node:test';
import assert from 'node:assert/strict';
import {taskParam} from './broker-card.js';

test('the page reads a well-formed Broker ID from its query', () => {
  assert.equal(taskParam('?task_id=broker-chip-2'), 'broker-chip-2');
  assert.equal(taskParam('?task_id=short'), '');
  assert.equal(taskParam('?task_id=bad%20id%3Cx%3E'), '');
  assert.equal(taskParam(''), '');
});
