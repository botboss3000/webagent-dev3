// @code-index {"version":1,"feature":"chat-session-selector","aliases":["session dropdown config"],"purpose":"Loads component settings from ui/chat/controls/session-dropdown/session-dropdown.json -> common and desktop/mobile/widget overlays; live adoption through the chat.json poll.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/chat/controls/session-dropdown/session-dropdown.json","pointers":["/common","/desktop","/mobile","/widget"]}],"opportunities":[]}}
let documentConfig = {}, agentOverlay = {};
/** Merge component objects without letting absent surface settings erase common values. */
function merge(base, overlay) {
  const result = {...base};
  for (const [key,value] of Object.entries(overlay || {})) {
    if (key === '__proto__' || key === 'constructor' || key === 'prototype') continue;
    result[key] = value && typeof value === 'object' && !Array.isArray(value) ? merge(result[key],value) : value;
  }
  return result;
}
/** Load the independently published component JSON and report whether it changed; failed reloads retain the last valid settings. */
export async function loadSessionDropdownConfig(signal) {
  try {
    const response = await fetch('/v3-ui/chat/controls/session-dropdown/session-dropdown.json', {credentials:'same-origin',cache:'no-store',signal});
    if (!response.ok) throw Error('Session dropdown configuration unavailable');
    const value = await response.json();
    if (!value?.common || typeof value.common !== 'object' || Array.isArray(value.common)) throw Error('Invalid session dropdown configuration');
    const changed = JSON.stringify(value) !== JSON.stringify(documentConfig);
    documentConfig = value;
    return changed;
  } catch (error) { console.warn('Session dropdown configuration could not be loaded', error.message); return false; }
}
/** Sets the open agent's overlay (from ui-config components.session_dropdown), merged over the published file. */
export function setSessionDropdownOverlay(overlay) {
  agentOverlay = overlay && typeof overlay === 'object' && !Array.isArray(overlay) ? overlay : {};
}
/** Resolve only the known active surface; chat.json no longer supplies dropdown internals. */
export function sessionDropdownConfig(surface) {
  const value = merge(documentConfig, agentOverlay);
  return merge(value.common, value[{chat_desktop:'desktop',chat_mobile:'mobile',chat_widget:'widget'}[surface]]);
}
