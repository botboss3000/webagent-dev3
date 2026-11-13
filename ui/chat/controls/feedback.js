// Mounts shared chat feedback using the central synchronous pulse helper.
import {mountControlFeedback} from '../widget/window.js';
import {requestTouchVibration} from '../../shared/vibrate.js';

/** Registers panel and optional body-portal/shell actions; the panel destroy lifecycle calls the returned disposer. */
export function mountChatControlFeedback(root, options = {}) {
  return mountControlFeedback(root, {...options, vibrate: requestTouchVibration});
}
