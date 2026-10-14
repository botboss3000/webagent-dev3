// @code-index {"version":1,"feature":"ui-vibration","aliases":["chat touch feedback","chat control haptics"],"purpose":"Mounts shared chat feedback using the central synchronous pulse helper. Config: ui/vibrate.json -> enabled and duration_ms; loaded once per page, adopted on reload.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
import {mountControlFeedback} from '../widget/window.js';
import {requestTouchVibration} from '../../shared/vibrate.js';

/** Registers panel and optional body-portal/shell actions; the panel destroy lifecycle calls the returned disposer. */
export function mountChatControlFeedback(root, options = {}) {
  return mountControlFeedback(root, {...options, vibrate: requestTouchVibration});
}
