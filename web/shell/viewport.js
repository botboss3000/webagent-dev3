// Tracks the visible viewport height and the mobile soft keyboard, ported from
// ui/boot/viewport.js without the FLIP motion. The shell shrinks with the keyboard instead
// of letting it cover the composer, and returns to rest when the keyboard closes.

const MOBILE_MAX = 800;
// Keyboards are ~250px and up; the floor clears browser-chrome jitter.
const KEYBOARD_FLOOR = 120;

let keyboardWidth = window.innerWidth;
let keyboardHeight = window.innerHeight;
let wasOpen = false;
let dismissed = false;

function visualHeight() {
  return window.visualViewport ? window.visualViewport.height : window.innerHeight;
}

function isEditable(node) {
  return (
    !!node &&
    (node.tagName === 'INPUT' || node.tagName === 'TEXTAREA' || node.isContentEditable)
  );
}

/* The layout follows the visual viewport: dvh does not shrink for a keyboard on Android Chrome. */
function applyHeight() {
  document.documentElement.style.setProperty('--app-height', `${visualHeight()}px`);
}

function applyKeyboard() {
  const mobile = window.innerWidth <= MOBILE_MAX;
  const height = visualHeight();
  const scale = window.visualViewport ? window.visualViewport.scale : 1;
  // Rotation and pinch-zoom cannot establish a keyboard transition.
  const stable = keyboardWidth === window.innerWidth && scale === 1;
  if (!stable) wasOpen = false;
  if (keyboardWidth !== window.innerWidth) keyboardHeight = window.innerHeight;
  keyboardWidth = window.innerWidth;
  // resizes-content shrinks innerHeight too, so the pre-keyboard height is retained.
  if (stable) keyboardHeight = Math.max(keyboardHeight, window.innerHeight, height);

  const editable = isEditable(document.activeElement);
  let open = mobile && stable && editable && keyboardHeight - height > KEYBOARD_FLOOR;

  if (wasOpen && !open && editable) {
    // The OS keyboard-dismiss control leaves the field focused, and some browsers report a
    // shrunken viewport again during the hide animation. Latch it and drop focus, so the
    // composer returns to its idle layout instead of staying raised.
    dismissed = true;
    const active = document.activeElement;
    if (mobile && active && active.classList.contains('composer-input')) active.blur();
  }

  wasOpen = open;
  open = open && !dismissed;
  document.body.classList.toggle('kb-open', open);
}

function applyAll() {
  applyHeight();
  applyKeyboard();
}

/* Installs the trackers once per page load. */
export function installViewport() {
  const viewport = window.visualViewport;
  if (viewport) {
    viewport.addEventListener('resize', applyAll);
    viewport.addEventListener('scroll', applyAll);
  }
  window.addEventListener('resize', applyAll);
  window.addEventListener('orientationchange', () => setTimeout(applyAll, 120));
  document.addEventListener('focusin', applyAll);
  document.addEventListener('focusout', () => setTimeout(applyKeyboard, 0));

  // Only deliberate editor engagement clears the dismissal; programmatic focus must not.
  const engage = (event) => {
    if (!event.isTrusted || !isEditable(event.target)) return;
    dismissed = false;
    applyKeyboard();
  };
  for (const type of ['pointerdown', 'keydown', 'beforeinput', 'input']) {
    document.addEventListener(type, engage, true);
  }

  applyAll();
}
