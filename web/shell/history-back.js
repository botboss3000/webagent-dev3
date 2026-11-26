'use strict';

// A phone's back press is a history pop. A surface that opens in place claims that pop
// instead of letting the browser leave the page, so back closes the surface and the
// page behind it stays where it was.
//
// Ported from ui/shared/history-back/history-back.js. The shell's single popstate
// listener (web/shell/shell.js) asks consumeBack() first; nothing claims a press by
// itself, so a surface opts in only while it is open.

const layers = [];
let selfPops = 0;

/** Claim the next back press for one open surface; the release drops its entry when the surface closes itself. */
export function pushBackHandler(handler) {
  const layer = { handler };
  try {
    history.pushState({ ...history.state, webagentBack: true }, '', location.href);
  } catch {
    // Without history the surface still opens; back simply leaves it.
    return () => {};
  }
  layers.push(layer);
  return () => {
    const at = layers.lastIndexOf(layer);
    // A back press already consumed the entry, so there is nothing left to drop.
    if (at < 0) return;
    layers.splice(at, 1);
    if (history.state && history.state.webagentBack) {
      selfPops += 1;
      history.back();
    }
  };
}

/** The shell's single popstate listener asks this first; true means a surface consumed the press. */
export function consumeBack() {
  if (selfPops > 0) {
    selfPops -= 1;
    return true;
  }
  const layer = layers.pop();
  if (!layer) return false;
  return layer.handler() !== false;
}
