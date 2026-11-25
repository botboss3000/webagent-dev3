// Full browser-data reset for the refresh component, ported from the app's
// ui/browser-data-reset.js. The header button and the long pull both keep the
// HttpOnly session cookie, so the user stays signed in.
//
// Removes service workers, Cache Storage, IndexedDB databases, web storage and
// script-readable cookies on this origin before the cache-busting reload.
//
// Limits of the browser sandbox — not oversights:
//  - HttpOnly cookies are invisible to page script, so the sign-in survives.
//  - Other origins' data and the HTTP cache are out of reach; those need the
//    browser's own site-data settings.
//  - An IndexedDB deletion blocked by another tab is bounded by a timeout so the
//    reset always reaches its reload.

const COOKIE_EXPIRY = 'Thu, 01 Jan 1970 00:00:00 GMT';
const DB_DELETE_TIMEOUT_MS = 3000;

// Deletes one IndexedDB database, bounded so a blocked request cannot hang the reset.
function deleteDatabase(name, timeoutMs = DB_DELETE_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error(`Timed out deleting ${name}; another tab may be holding it open`));
    }, timeoutMs);
    const request = indexedDB.deleteDatabase(name);
    request.onsuccess = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve();
    };
    request.onerror = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(request.error || new Error(`Could not delete ${name}`));
    };
    request.onblocked = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new Error(`Deletion blocked for ${name}`));
    };
  });
}

// Every path a cookie on this page could have been set for: its ancestors and root.
function cookiePaths() {
  const paths = new Set(['/']);
  let path = window.location.pathname || '/';
  while (path && path !== '/') {
    paths.add(path);
    const parent = path.replace(/\/[^/]*$/, '');
    path = parent === path ? '/' : parent || '/';
  }
  return [...paths];
}

// Host-only cookie variants; a parent-domain cookie may belong to another deployment.
function cookieDomains() {
  const host = window.location.hostname;
  if (!host || host === 'localhost' || /^[\d.]+$/.test(host)) return [null];
  return [null, host, `.${host}`];
}

function clearCookies() {
  const completed = [];
  const failures = [];
  let names = [];
  try {
    names = document.cookie
      .split(';')
      .map((entry) => entry.split('=')[0].trim())
      .filter(Boolean);
  } catch (error) {
    failures.push({ target: 'cookies', error: String(error?.message || error) });
    return { completed, failures };
  }
  if (!names.length) return { completed, failures };

  const paths = cookiePaths();
  const domains = cookieDomains();
  for (const name of new Set(names)) {
    for (const path of paths) {
      for (const domain of domains) {
        const suffix =
          `; path=${path}; expires=${COOKIE_EXPIRY}; max-age=0` + (domain ? `; domain=${domain}` : '');
        try {
          document.cookie = `${name}=${suffix}`;
        } catch (error) {
          failures.push({ target: `cookie:${name}`, error: String(error?.message || error) });
        }
      }
    }
    completed.push(`cookie:${name}`);
  }
  return { completed, failures };
}

// Wipes every local store this origin exposes and reports what was removed. Never
// throws for one failed target: the caller reloads regardless and the failures list
// is the honest record of anything left behind.
export async function purgeAllBrowserData() {
  const completed = [];
  const failures = [];

  // 1. Service workers — every registration for this origin, not just the controlling one.
  if (navigator.serviceWorker?.getRegistrations) {
    try {
      const registrations = await navigator.serviceWorker.getRegistrations();
      for (const registration of registrations) {
        const target = `service-worker:${registration.scope || 'unknown'}`;
        try {
          await registration.unregister();
          completed.push(target);
        } catch (error) {
          failures.push({ target, error: String(error?.message || error) });
        }
      }
    } catch (error) {
      failures.push({ target: 'service-worker', error: String(error?.message || error) });
    }
  }

  // 2. Cache Storage — every bucket, not only the webagent-* ones.
  if (globalThis.caches?.keys) {
    try {
      for (const name of await caches.keys()) {
        try {
          await caches.delete(name);
          completed.push(`cache:${name}`);
        } catch (error) {
          failures.push({ target: `cache:${name}`, error: String(error?.message || error) });
        }
      }
    } catch (error) {
      failures.push({ target: 'cache-storage', error: String(error?.message || error) });
    }
  }

  // 3. IndexedDB — enumerate and delete every database the origin exposes.
  if (typeof indexedDB.databases === 'function') {
    try {
      const names = (await indexedDB.databases()).map((row) => row?.name).filter(Boolean);
      for (const name of names) {
        try {
          await deleteDatabase(name);
          completed.push(`indexeddb:${name}`);
        } catch (error) {
          failures.push({ target: `indexeddb:${name}`, error: String(error?.message || error) });
        }
      }
    } catch (error) {
      failures.push({ target: 'indexeddb-catalog', error: String(error?.message || error) });
    }
  }

  // 4. Web storage — a full clear, not a key allowlist: the point is to leave nothing behind.
  for (const [label, storage] of [
    ['localStorage', localStorage],
    ['sessionStorage', sessionStorage],
  ]) {
    try {
      storage.clear();
      completed.push(label);
    } catch (error) {
      failures.push({ target: label, error: String(error?.message || error) });
    }
  }

  // 5. Cookies readable by page script.
  const cookies = clearCookies();
  completed.push(...cookies.completed);
  failures.push(...cookies.failures);

  const result = { complete: failures.length === 0, completed_targets: completed, failures };

  // Let any in-memory cache discard state before pagehide.
  try {
    window.dispatchEvent(
      new CustomEvent('webagent-browser-storage-purge', {
        detail: { reason: 'full-browser-reset', ...result },
      }),
    );
  } catch {
    /* nothing listening */
  }

  return result;
}
