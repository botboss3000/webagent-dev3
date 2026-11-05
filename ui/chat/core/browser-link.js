// Silent Browser Extension linking. The WebAgent browser extension's content script runs on
// this origin and answers a hello with the account it is linked to; when that is not the
// signed-in account, a one-time code from the server relinks it. Messages stay inside this
// window: the content script and this module both ignore other frames, so a page that
// embeds the widget cannot drive the link.
const SOURCE_PAGE = 'webagent-chat', SOURCE_EXTENSION = 'webagent-browser-extension';

export function mountBrowserLink({request, win = window}) {
  let busy = false;
  const send = message => win.postMessage({source: SOURCE_PAGE, ...message}, win.location.origin);
  async function relink(linkedUser) {
    if (busy) return;
    busy = true;
    try {
      const reply = await request('/browser/link-code', 'POST', {linked_user: linkedUser || null});
      if (reply?.code) send({type: 'browser-link', code: reply.code});
    } catch {
      // Signed out or not allowed: the extension simply stays as it is.
    } finally { busy = false; }
  }
  const onMessage = event => {
    const data = event.data;
    if (event.source !== win || event.origin !== win.location.origin) return;
    if (data?.source !== SOURCE_EXTENSION || data.type !== 'browser-link-state') return;
    void relink(data.linked_user);
  };
  // Asking again on focus catches a sign-in finished in another window or popup.
  const hello = () => send({type: 'browser-link-hello'});
  win.addEventListener('message', onMessage);
  win.addEventListener('focus', hello);
  hello();
  return {
    destroy: () => { win.removeEventListener('message', onMessage); win.removeEventListener('focus', hello); },
  };
}
