import test from 'node:test';
import assert from 'node:assert/strict';
import { mergeDelta, readConversation, saveConversation, clearConversations } from './conversation-cache.js';

const message = (seq, text = 'm' + seq) => ({seq, id: 'id' + seq, text});

test('a delta keeps cached messages older than it and takes the rest from the server', () => {
  const cached = [1, 2, 3, 4, 5].map(seq => message(seq));
  const merged = mergeDelta(cached, {delta: true, messages: [message(4, 'changed'), message(5), message(6)]});
  assert.deepEqual(merged.map(m => m.seq), [1, 2, 3, 4, 5, 6]);
  assert.equal(merged.find(m => m.seq === 4).text, 'changed', 'recent messages take the server copy');
});

test('a full snapshot replaces the cache, and an empty delta keeps it', () => {
  const cached = [message(1), message(2)];
  assert.deepEqual(mergeDelta(cached, {delta: false, messages: [message(9)]}).map(m => m.seq), [9]);
  assert.deepEqual(mergeDelta(cached, {delta: true, messages: []}).map(m => m.seq), [1, 2]);
});

test('without IndexedDB the cache is a quiet no-op', async () => {
  assert.equal(globalThis.indexedDB, undefined);
  await saveConversation('u', 's', {history_revision: 0, last_seq: 1, messages: []});
  assert.equal(await readConversation('u', 's'), null);
  await clearConversations();
});
