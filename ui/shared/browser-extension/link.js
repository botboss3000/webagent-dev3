// Runs only on WebAgent's own pages (the app and the widget iframe it serves to company
// sites). It answers the chat panel's hello with the linked account and passes a link code
// to the background worker. Only messages from this same window and origin count, so the
// company page around a widget iframe cannot link or unlink anything.
(() => {
  const PAGE = 'webagent-chat', EXTENSION = 'webagent-browser-extension';
  const reply = message => window.postMessage({source: EXTENSION, ...message}, location.origin);
  window.addEventListener('message', async event => {
    const data = event.data;
    if (event.source !== window || event.origin !== location.origin || data?.source !== PAGE) return;
    try {
      if (data.type === 'browser-link-hello') {
        const state = await chrome.runtime.sendMessage({type: 'link-state'});
        reply({type: 'browser-link-state', linked_user: state?.user_id || null});
      } else if (data.type === 'browser-link' && typeof data.code === 'string') {
        await chrome.runtime.sendMessage({type: 'link', code: data.code, origin: location.origin});
      }
    } catch {
      // The extension was reloaded or updated; the page asks again on its next focus.
    }
  });
})();
