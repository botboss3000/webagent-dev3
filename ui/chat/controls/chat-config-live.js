// @code-index {"version":1,"feature":"chat-ui-config","aliases":["live chat settings","live chat.json"],"purpose":"Refreshes authenticated mounted chats from the live UI configuration endpoint without reload or compilation. Config: data/config/chat.json -> chat_common.live_reload.enabled/interval_ms; UI settings retain common and surface profile merging.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/live_reload/enabled","/chat_common/live_reload/interval_ms"]}],"opportunities":[]}}
/** Watches visible native chats serially; teardown aborts fetches and prevents stale adoption. */
export function watchChatConfig({getConfig,onConfig,isMounted}) {
  let stopped = false, timer = null, controller = null;
  let previous = JSON.stringify(getConfig());
  /** Reads the opt-in interval, bounded to prevent accidental request floods. */
  const interval = () => {
    const settings = getConfig()?.chat_common?.live_reload;
    return settings?.enabled === true && typeof settings.interval_ms === 'number' && Number.isFinite(settings.interval_ms)
      ? Math.max(1000,Math.min(60000,settings.interval_ms)) : null;
  };
  /** Queues only one subsequent refresh using the latest adopted settings. */
  const schedule = () => { const delay = interval(); if (!stopped && delay !== null) timer = setTimeout(refresh,delay); };
  /** Adopts changed valid documents; failures retain the last usable configuration. */
  async function refresh() {
    timer = null;
    if (stopped) return;
    if (!isMounted()) { stop(); return; }
    if (document.hidden || interval() === null) { schedule(); return; }
    controller = new AbortController();
    const active = controller, timeout = setTimeout(() => active.abort(),10000);
    try {
      const response = await fetch('/api/v3/auth/ui-config',{credentials:'same-origin',cache:'no-store',signal:active.signal});
      if (!response.ok) return;
      const data = await response.json(), config = data.chat_ui || data;
      if (stopped || !isMounted() || active.signal.aborted || !config?.chat_common || typeof config.chat_common !== 'object' || Array.isArray(config.chat_common)) return;
      const signature = JSON.stringify(config);
      if (signature !== previous) { onConfig(config); previous = signature; }
    } catch { /* Transient reads must not clear settings or interrupt a draft. */ }
    finally { clearTimeout(timeout); controller = null; schedule(); }
  }
  /** Refreshes promptly when the user returns, without overlapping an active request. */
  function visible() {
    if (document.hidden || stopped || controller || interval() === null) return;
    clearTimeout(timer); timer = null; void refresh();
  }
  /** Releases this mount's timer/listener and any request; safe to call repeatedly. */
  function stop() {
    stopped = true; clearTimeout(timer); controller?.abort();
    document.removeEventListener('visibilitychange',visible);
  }
  document.addEventListener('visibilitychange',visible); schedule();
  return stop;
}
