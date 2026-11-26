// Turns the device into an attribute the stylesheets can key on: a coarse pointer means a
// finger, and tokens.css answers it with bigger targets and a step up in type. Nothing in
// the JSONs mentions the pointer; this attribute is the single place it is decided.

const COARSE = '(pointer: coarse)';

/* Writes data-pointer on <html> from the primary pointing device; a mouse-driven laptop with
   a touchscreen stays fine, which is what keeps desktop controls at their smaller size. */
function applyPointer() {
  const coarse = window.matchMedia(COARSE).matches;
  document.documentElement.dataset.pointer = coarse ? 'coarse' : 'fine';
}

/* Installs the pointer listener once per page load; it re-applies when a tablet docks a
   keyboard or a mouse is attached, because the primary pointer can change at runtime. */
export function installEnvironment() {
  applyPointer();
  const query = window.matchMedia(COARSE);
  if (typeof query.addEventListener === 'function') {
    query.addEventListener('change', applyPointer);
  } else if (typeof query.addListener === 'function') {
    query.addListener(applyPointer);
  }
}
