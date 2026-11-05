import test from 'node:test';
import assert from 'node:assert/strict';
import {minutes, taskIcon, liveSeconds, stageVerdict, remaining} from './broker-chip.js';

test('times read as minutes with one decimal, counting a tenth every 6 s', () => {
  assert.equal(minutes(0), '0.0');
  assert.equal(minutes(5), '0.0');
  assert.equal(minutes(6), '0.1');
  assert.equal(minutes(125), '2.0');
  assert.equal(minutes(1140), '19.0');
  assert.equal(minutes(-3), '0.0');
});

test('the chip icon follows the newest job and warns before anything else', () => {
  assert.equal(taskIcon({status: 'running'}), 'running');
  assert.equal(taskIcon({status: 'queued'}), 'running');
  assert.equal(taskIcon({status: 'completed'}), 'done');
  for (const status of ['failed', 'uncertain', 'cancelled', 'needs_decision']) assert.equal(taskIcon({status}), 'error');
  assert.equal(taskIcon({status: 'running', warning: 'stalled'}), 'warning');
  assert.equal(taskIcon({status: 'running', warning: 'broker_unresponsive'}), 'warning');
});

test('running counters add the time since the snapshot, on the server clock', () => {
  // Snapshot read at server time 1000 with the stage at 300 s; the browser is 5 s behind.
  assert.equal(liveSeconds(300, 1000, 5, 1_030_000), 335);
  assert.equal(liveSeconds(300, 1000, 0, 990_000), 300);
});

test('a running stage turns slow as soon as it passes p90, and time left counts down', () => {
  const stage = {normal: [300, 600]};
  assert.equal(stageVerdict(stage, 200), 'on track');
  assert.equal(stageVerdict(stage, 400), 'slower than median');
  assert.equal(stageVerdict(stage, 601), 'slow');
  assert.equal(stageVerdict({normal: null}, 50), 'no baseline');
  assert.equal(remaining([600, 1200], 60), '9.0–19.0 min left');
  assert.equal(remaining([600, 1200], 1300), 'past normal');
  assert.equal(remaining(null, 0), null);
});
