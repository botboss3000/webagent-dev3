// @code-index {"version":1,"feature":"chat-login-hook","aliases":["login overlay","left login form"],"purpose":"Focuses the username field after scrolling the left login panel into view","span_lines":40}
/** Scrolls the left-side login form into view and focuses its username field when present. */
export function showLeftOverlay() { document.getElementById('login')?.scrollIntoView({block:'center'}); document.getElementById('username')?.focus(); }
