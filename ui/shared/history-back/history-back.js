'use strict';

// A phone's back press is a history pop, and the app answers one with a reload.
// A page that opens in place claims the pop instead, so back returns to its list.
const layers = [];
let selfPops = 0;

/** Claim the next back press for one open page; the release drops its entry when the page closes itself. */
export function pushBackHandler(handler) {
  const layer = {handler};
  try {
    history.pushState({...history.state, webagentBack: true}, '', location.href);
  } catch {
    // Without history the page still opens; back simply leaves it.
    return () => {};
  }
  layers.push(layer);
  return () => {
    const at = layers.lastIndexOf(layer);
    // A back press already consumed the entry, so there is nothing left to drop.
    if (at < 0) return;
    layers.splice(at, 1);
    if (history.state && history.state.webagentBack) { selfPops += 1; history.back(); }
  };
}

/** The app's single popstate listener asks this first; true means a page consumed the press. */
export function consumeBack() {
  if (selfPops > 0) { selfPops -= 1; return true; }
  const layer = layers.pop();
  if (!layer) return false;
  return layer.handler() !== false;
}
