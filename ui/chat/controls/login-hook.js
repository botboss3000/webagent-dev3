// Focuses the username field after scrolling the left login panel into view
/** Scrolls the left-side login form into view and focuses its username field when present. */
export function showLeftOverlay() { document.getElementById('login')?.scrollIntoView({block:'center'}); document.getElementById('username')?.focus(); }
