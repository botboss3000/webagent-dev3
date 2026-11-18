// The UI element bridge, run first in every element frame. The frame is an opaque origin
// with no network; this is its only channel to the chat. Each call is a message to the
// parent page, which checks it (see ui/chat/controls/artifact-element.js) and answers.
(() => {
  const data = window.__webagent || {};
  const waiting = new Map();
  let next = 0;
  addEventListener('message', event => {
    if (event.source !== parent || !event.data || event.data.webagent !== 1) return;
    const call = waiting.get(event.data.id);
    if (!call) return;
    waiting.delete(event.data.id);
    if (event.data.ok) call.resolve(event.data.value);
    else call.reject(new Error(event.data.error || 'Refused'));
  });
  const ask = (type, body) => new Promise((resolve, reject) => {
    const id = ++next;
    waiting.set(id, {resolve, reject});
    parent.postMessage({webagent: 1, id, type, ...body}, '*');
  });
  window.webagent = Object.freeze({
    params: Object.freeze(data.params || {}),
    card: Object.freeze(data.card || {}),
    preview: Object.freeze({
      status: () => ask('preview.status', {}),
      on: variant => ask('preview.set', {variant: String(variant)}),
      off: () => ask('preview.set', {variant: null}),
    }),
    prompt: text => ask('prompt', {text: String(text)}),
    open: route => ask('open', {route: String(route)}),
    resize: height => ask('resize', {height: Number(height)}),
  });
  // The card follows the element's height.
  const fit = () => ask('resize', {height: document.documentElement.scrollHeight}).catch(() => {});
  addEventListener('load', fit);
  new ResizeObserver(fit).observe(document.documentElement);
})();
