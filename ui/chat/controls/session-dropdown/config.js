// @code-index {"version":1,"feature":"chat-session-selector","aliases":["session dropdown config"],"purpose":"Loads component settings from ui/chat/controls/session-dropdown/session-dropdown.json -> common and desktop/mobile/widget overlays; reload adoption.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/chat/controls/session-dropdown/session-dropdown.json","pointers":["/common","/desktop","/mobile","/widget"]}],"opportunities":[]}}
let documentConfig = {};
/** Merge component objects without letting absent surface settings erase common values. */
function merge(base, overlay) {
  const result = {...base};
  for (const [key,value] of Object.entries(overlay || {})) {
    if (key === '__proto__' || key === 'constructor' || key === 'prototype') continue;
    result[key] = value && typeof value === 'object' && !Array.isArray(value) ? merge(result[key],value) : value;
  }
  return result;
}
/** Load the independently published component JSON; failed reloads retain the last valid settings. */
export async function loadSessionDropdownConfig() {
  try {
    const response = await fetch('/v3-ui/chat/controls/session-dropdown/session-dropdown.json', {credentials:'same-origin',cache:'no-store'});
    if (!response.ok) throw Error('Session dropdown configuration unavailable');
    const value = await response.json();
    if (!value?.common || typeof value.common !== 'object' || Array.isArray(value.common)) throw Error('Invalid session dropdown configuration');
    documentConfig = value;
  } catch (error) { console.warn('Session dropdown configuration could not be loaded', error.message); }
}
/** Resolve only the known active surface; chat.json no longer supplies dropdown internals. */
export function sessionDropdownConfig(surface) {
  return merge(documentConfig.common, documentConfig[{chat_desktop:'desktop',chat_mobile:'mobile',chat_widget:'widget'}[surface]]);
}
