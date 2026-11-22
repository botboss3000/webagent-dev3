// Keeps recently opened conversations in this browser's IndexedDB, mirroring the server's rows:
// `sessions` holds one record per conversation (history_revision, last_seq, the activity cursor
// and the message ids whose activity was already read), `chat` one row per message, and
// `activity_summaries` one row per finished activity group (counts and seq range only, never
// events). Reopening a conversation then asks the server only for what is new
// (GET /api/v3/sessions/{id}?since=&history_revision= and /activity?view=summary&after=).
// Entries are per user, at most MAX_ENTRIES conversations, and expire after MAX_AGE_MS; signing
// out clears them, a refused snapshot forgets that conversation and a rewritten history drops its
// activity rows. Without IndexedDB every call is a no-op.
const DB = 'webagent-chat-cache', VERSION = 2, SESSIONS = 'sessions', CHAT = 'chat', ACTIVITY = 'activity_summaries';
const STORES = [SESSIONS, CHAT, ACTIVITY], MAX_ENTRIES = 30, MAX_AGE_MS = 7 * 86400000, MAX_REQUESTED = 5000;
let opening = null;

function database() {
  if (!globalThis.indexedDB) return Promise.resolve(null);
  return opening ||= new Promise(resolve => {
    let request;
    try { request = indexedDB.open(DB, VERSION); } catch { resolve(null); return; }
    // Version 1 kept one blob per conversation; it is dropped once and refills on the next open.
    request.onupgradeneeded = () => {
      const db = request.result;
      for (const name of [...db.objectStoreNames]) db.deleteObjectStore(name);
      db.createObjectStore(SESSIONS);
      db.createObjectStore(CHAT, {keyPath: ['owner', 'seq']});
      db.createObjectStore(ACTIVITY, {keyPath: ['owner', 'id']});
    };
    request.onsuccess = () => {
      const db = request.result;
      // Another tab upgrading the cache closes this connection; the next call reopens it.
      db.onversionchange = () => { db.close(); opening = null; };
      resolve(db);
    };
    request.onerror = request.onblocked = () => { opening = null; resolve(null); };
  });
}

/** Runs one transaction over every store; resolves with out.value, or null when storage is unavailable. */
async function withStores(mode, operation) {
  const db = await database();
  if (!db) return null;
  return new Promise(resolve => {
    try {
      const transaction = db.transaction(STORES, mode), out = {value: null};
      operation(Object.fromEntries(STORES.map(name => [name, transaction.objectStore(name)])), out);
      transaction.oncomplete = () => resolve(out.value);
      transaction.onerror = transaction.onabort = () => resolve(null);
    } catch { resolve(null); }
  });
}

const key = (user, id) => user + '\u0000' + id;
// Every row key [owner, x] of one conversation, whatever x is.
const rows = owner => IDBKeyRange.bound([owner], [owner, []]);
const fresh = record => !!record && Date.now() - (record.saved_at || 0) <= MAX_AGE_MS;
function forget(stores, owner) {
  stores[SESSIONS].delete(owner); stores[CHAT].delete(rows(owner)); stores[ACTIVITY].delete(rows(owner));
}

/** The cached conversation {history_revision, last_seq, messages} for this user, or null when absent or expired. */
export async function readConversation(user, id) {
  if (!user || !id) return null;
  const owner = key(user, id);
  return withStores('readonly', (stores, out) => {
    const record = stores[SESSIONS].get(owner);
    record.onsuccess = () => {
      const session = record.result;
      if (!fresh(session) || typeof session.last_seq !== 'number') return;
      const messages = stores[CHAT].getAll(rows(owner));
      messages.onsuccess = () => { out.value = {...session, messages: messages.result.map(row => row.message)}; };
    };
  });
}

/** Stores {history_revision, last_seq, messages} and drops conversations past the cap or age. */
export async function saveConversation(user, id, {history_revision = 0, last_seq = 0, messages = []} = {}) {
  if (!user || !id) return;
  const owner = key(user, id);
  await withStores('readwrite', stores => {
    const record = stores[SESSIONS].get(owner);
    record.onsuccess = () => {
      const old = record.result || {};
      // A rewritten history moves activity anchors and ranges: its cached groups are dropped.
      const keep = old.history_revision === history_revision;
      if (!keep) stores[ACTIVITY].delete(rows(owner));
      const {activity_cursor, activity_requested} = keep ? old : {};
      stores[SESSIONS].put({history_revision, last_seq, activity_cursor, activity_requested, saved_at: Date.now()}, owner);
      stores[CHAT].delete(rows(owner));
      for (const message of messages)
        if (Number.isSafeInteger(message?.seq)) stores[CHAT].put({owner, seq: message.seq, message});
    };
  });
  await withStores('readwrite', stores => {
    const entries = [], cursor = stores[SESSIONS].openCursor();
    cursor.onsuccess = () => {
      const item = cursor.result;
      if (item) { entries.push([item.key, item.value?.saved_at || 0]); item.continue(); return; }
      entries.sort((a, b) => b[1] - a[1]);
      entries.forEach(([stale, savedAt], index) => {
        if (index >= MAX_ENTRIES || Date.now() - savedAt > MAX_AGE_MS) forget(stores, stale);
      });
    };
  });
}

/** Forgets one conversation, for example after the server refused it. */
export async function forgetConversation(user, id) {
  if (user && id) await withStores('readwrite', stores => forget(stores, key(user, id)));
}

/** Removes every cached conversation, message and activity group (sign-out). */
export async function clearConversations() {
  await withStores('readwrite', stores => { for (const name of STORES) stores[name].clear(); });
}

/** The cached finished activity groups {cursor, requested, segments} of a conversation, or null. */
export async function readActivity(user, id) {
  if (!user || !id) return null;
  const owner = key(user, id);
  return withStores('readonly', (stores, out) => {
    const record = stores[SESSIONS].get(owner);
    record.onsuccess = () => {
      const session = record.result;
      if (!fresh(session) || !Number.isSafeInteger(session.activity_cursor)) return;
      const groups = stores[ACTIVITY].getAll(rows(owner));
      groups.onsuccess = () => {
        out.value = {cursor: session.activity_cursor, requested: session.activity_requested || [],
          segments: groups.result.map(row => row.summary)};
      };
    };
  });
}

/**
 * Replaces a conversation's cached activity: finished groups (counts and seq ranges), the cursor
 * every later change lies after, and the message ids whose groups were already read.
 */
export async function saveActivity(user, id, {cursor, requested = [], segments = []} = {}) {
  if (!user || !id || !Number.isSafeInteger(cursor)) return;
  const owner = key(user, id);
  await withStores('readwrite', stores => {
    const record = stores[SESSIONS].get(owner);
    record.onsuccess = () => {
      stores[SESSIONS].put({...record.result, activity_cursor: cursor,
        activity_requested: requested.slice(-MAX_REQUESTED), saved_at: Date.now()}, owner);
      stores[ACTIVITY].delete(rows(owner));
      for (const summary of segments) stores[ACTIVITY].put({owner, id: summary.id, summary});
    };
  });
}

/** Drops a conversation's cached activity groups and cursor, keeping its messages. */
export async function forgetActivity(user, id) {
  if (!user || !id) return;
  const owner = key(user, id);
  await withStores('readwrite', stores => {
    const record = stores[SESSIONS].get(owner);
    record.onsuccess = () => {
      if (record.result) stores[SESSIONS].put({...record.result, activity_cursor: undefined, activity_requested: undefined}, owner);
      stores[ACTIVITY].delete(rows(owner));
    };
  });
}

/**
 * Merges a delta snapshot into cached messages: cached messages older than the delta stay,
 * the delta's (newer and recently changed) messages replace or follow them.
 */
export function mergeDelta(cachedMessages, data) {
  if (!data?.delta) return data?.messages || [];
  const fresh = data.messages || [];
  const first = fresh.length ? Math.min(...fresh.map(message => message.seq)) : Infinity;
  const ids = new Set(fresh.map(message => message.id));
  return [...cachedMessages.filter(message => message.seq < first && !ids.has(message.id)), ...fresh]
    .sort((a, b) => a.seq - b.seq);
}
