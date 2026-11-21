// Keeps recently opened conversations in this browser's IndexedDB, so reopening one asks the
// server only for what is new (GET /api/v3/sessions/{id}?since=&history_revision=). Entries
// are per user, at most MAX_ENTRIES, and expire after MAX_AGE_MS; signing out clears them and
// a refused snapshot forgets that conversation. Without IndexedDB every call is a no-op.
const DB = 'webagent-chat-cache', STORE = 'conversations', MAX_ENTRIES = 30, MAX_AGE_MS = 7 * 86400000;
let opening = null;

function database() {
  if (!globalThis.indexedDB) return Promise.resolve(null);
  return opening ||= new Promise(resolve => {
    let request;
    try { request = indexedDB.open(DB, 1); } catch { resolve(null); return; }
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = request.onblocked = () => resolve(null);
  });
}

/** Runs one store operation; resolves with its result, or null when storage is unavailable. */
async function withStore(mode, operation) {
  const db = await database();
  if (!db) return null;
  return new Promise(resolve => {
    try {
      const transaction = db.transaction(STORE, mode), result = operation(transaction.objectStore(STORE));
      transaction.oncomplete = () => resolve(result?.result ?? null);
      transaction.onerror = transaction.onabort = () => resolve(null);
    } catch { resolve(null); }
  });
}

const key = (user, id) => user + '\u0000' + id;

/** The cached conversation for this user, or null when absent or expired. */
export async function readConversation(user, id) {
  if (!user || !id) return null;
  const value = await withStore('readonly', store => store.get(key(user, id)));
  return value && Date.now() - value.saved_at <= MAX_AGE_MS && Array.isArray(value.messages) ? value : null;
}

/** Stores {history_revision, last_seq, messages} and drops entries past the cap or age. */
export async function saveConversation(user, id, value) {
  if (!user || !id) return;
  await withStore('readwrite', store => store.put({...value, saved_at: Date.now()}, key(user, id)));
  await withStore('readwrite', store => {
    const entries = [], cursor = store.openCursor();
    cursor.onsuccess = () => {
      const item = cursor.result;
      if (item) { entries.push([item.key, item.value?.saved_at || 0]); item.continue(); return; }
      entries.sort((a, b) => b[1] - a[1]);
      for (const [stale, savedAt] of entries.filter((entry, index) => index >= MAX_ENTRIES || Date.now() - entry[1] > MAX_AGE_MS))
        if (savedAt >= 0) store.delete(stale);
    };
    return null;
  });
}

/** Forgets one conversation, for example after the server refused it. */
export async function forgetConversation(user, id) {
  if (user && id) await withStore('readwrite', store => store.delete(key(user, id)));
}

/** Removes every cached conversation (sign-out). */
export async function clearConversations() { await withStore('readwrite', store => store.clear()); }

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
