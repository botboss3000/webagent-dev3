// File Manager terminals: xterm.js panes on shells the root host file service runs as the
// operator account (no sudo or docker group). Output arrives by long-polling terminal_read;
// keystrokes go out in order through terminal_send. Needs a terminal grant from Host Admin.
const VENDOR = '/ui/main-panel/app/pages/file-manager/vendor/';
let library = null;

function load(tag, attrs) {
  return new Promise((resolve, reject) => {
    const node = document.createElement(tag);
    Object.assign(node, attrs);
    node.onload = resolve;
    node.onerror = () => reject(Error('Could not load the terminal emulator'));
    document.head.appendChild(node);
  });
}

// xterm.js and its fit addon are UMD builds (window.Terminal, window.FitAddon); load once.
function xterm() {
  if (!library) {
    library = Promise.all([
      load('link', {rel: 'stylesheet', href: VENDOR + 'xterm.css'}),
      load('script', {src: VENDOR + 'xterm.js'}).then(() => load('script', {src: VENDOR + 'addon-fit.js'})),
    ]).then(() => ({Terminal: window.Terminal, FitAddon: window.FitAddon.FitAddon}));
    library.catch(() => { library = null; });
  }
  return library;
}

function colors(pane) {
  const style = getComputedStyle(pane);
  const pick = (name, fallback) => style.getPropertyValue(name).trim() || fallback;
  return {background: pick('--files-bg', '#111'), foreground: pick('--files-text-strong', '#ddd'),
          cursor: pick('--files-text-strong', '#ddd')};
}

function bytes(base64) {
  const text = atob(base64 || '');
  const out = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) out[i] = text.charCodeAt(i);
  return out;
}

// Mount tab's terminal into pane. post(action, body) calls the File Manager API.
export async function mountTerminal(pane, tab, post) {
  pane.classList.add('files-terminal-pane');
  const host = document.createElement('div');
  host.className = 'files-terminal-host';
  pane.appendChild(host);
  let lib;
  try { lib = await xterm(); } catch (e) {
    host.textContent = e.message;
    return;
  }
  const term = new lib.Terminal({cursorBlink: true, fontSize: 13, scrollback: 5000,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace', theme: colors(pane)});
  const fit = new lib.FitAddon();
  term.loadAddon(fit);
  term.open(host);
  const state = {term, fit, closed: false, offset: 0, sending: Promise.resolve()};
  tab._terminal = state;
  const resize = () => {
    if (state.closed || !pane.classList.contains('active') || !host.offsetWidth) return;
    try { fit.fit(); } catch (_) { return; }
    post('terminal_resize', {id: tab.terminalId, cols: term.cols, rows: term.rows}).catch(() => {});
  };
  let timer = null;
  state.observer = new ResizeObserver(() => { clearTimeout(timer); timer = setTimeout(resize, 120); });
  state.observer.observe(host);
  term.onData((data) => {
    // Keep keystrokes in order: each send waits for the one before it.
    state.sending = state.sending.then(() => post('terminal_send', {id: tab.terminalId, data})).catch(() => {});
  });
  state.fitNow = resize;
  resize();
  // Read loop: long-poll for output until the shell exits, the tab closes or access ends.
  (async () => {
    let failures = 0;
    while (!state.closed) {
      try {
        const out = await post('terminal_read', {id: tab.terminalId, offset: state.offset, wait: 15});
        failures = 0;
        if (out.truncated && state.offset) term.write('\r\n[older output dropped]\r\n');
        if (out.data) term.write(bytes(out.data));
        state.offset = out.offset;
        if (out.exited) { term.write('\r\n[shell exited]\r\n'); break; }
      } catch (e) {
        if (state.closed) break;
        if (e.needsGrant || /No such terminal/.test(e.message || '')) {
          term.write('\r\n[' + (e.needsGrant ? 'terminal access ended' : 'terminal closed') + ']\r\n');
          break;
        }
        failures += 1;
        await new Promise((resolve) => setTimeout(resolve, Math.min(10000, 1000 * failures)));
      }
    }
  })();
  if (tab.focus !== false) term.focus();
}

// Called when the tab becomes active again: refit to the now-visible size and focus.
export function showTerminal(tab) {
  const state = tab._terminal;
  if (!state) return;
  requestAnimationFrame(() => { state.fitNow(); state.term.focus(); });
}

export function disposeTerminal(tab) {
  const state = tab._terminal;
  if (!state) return;
  state.closed = true;
  state.observer.disconnect();
  state.term.dispose();
  tab._terminal = null;
}
