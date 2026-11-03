'use strict';
// Shared full-screen image viewer for composer attachment tiles and published
// message attachments. Clicks are delegated per root so tiles opened from any
// re-render still work; the dialog mounts where the chat lives (its own root),
// not blindly in document.body, and carries the critical layout inline so it is
// visible even if the chat stylesheet is not in scope.
// @code-index {"version":1,"feature":"chat-image-preview","aliases":["image preview","attachment zoom","thumbnail lightbox"],"purpose":"Shows composer and message image attachments in a dismissible full-screen viewer Trigger config: none","span_lines":70,"configuration":{"status":"static","references":[],"opportunities":[]}}

let _activePreview = null;
const _boundRoots = new WeakSet();

/** The node the viewer must be appended to so it inherits the chat's own styles. */
function hostRoot(node) {
  const root = node && node.getRootNode ? node.getRootNode() : null;
  if (root && root.nodeType === 11) return root;
  return document.body || document.documentElement;
}

/** Closes the open viewer, removes its Escape listener/backdrop and restores focus to the connected trigger. */
export function closeChatImagePreview() {
  const state = _activePreview;
  if (!state) return;
  _activePreview = null;
  document.removeEventListener('keydown', state.onKeyDown);
  state.backdrop.remove();
  if (state.returnFocus?.isConnected) state.returnFocus.focus();
}

/** Binds one capture-phase click handler per root that opens any [data-image-preview] tile. */
export function bindImagePreviewRoot(node) {
  const root = hostRoot(node);
  if (!root || _boundRoots.has(root)) return;
  _boundRoots.add(root);
  root.addEventListener('click', (event) => {
    const tile = event.target?.closest?.('[data-image-preview]');
    const src = tile?.getAttribute('data-image-preview');
    if (!src) return;
    // Modified clicks keep the browser's open-in-new-tab/download behaviour.
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    openChatImagePreview(src, tile.getAttribute('data-image-preview-alt') || 'Image', tile);
  }, true);
}

/** Opens one image over the chat; a missing source is ignored and an already-open viewer is replaced. */
export function openChatImagePreview(src, alt = 'Image', returnFocus = null) {
  if (!src) return;
  closeChatImagePreview();
  bindImagePreviewRoot(returnFocus);

  const backdrop = document.createElement('div');
  backdrop.className = 'chat-image-preview-backdrop';
  backdrop.style.cssText = 'position:fixed;inset:0;z-index:10070;display:flex;align-items:center;justify-content:center;padding:24px;background:rgba(0,0,0,.74);';

  const figure = document.createElement('figure');
  figure.className = 'chat-image-preview';
  figure.style.cssText = 'position:relative;display:flex;align-items:center;justify-content:center;max-width:100%;max-height:100%;margin:0;';
  figure.setAttribute('role', 'dialog');
  figure.setAttribute('aria-modal', 'true');
  figure.setAttribute('aria-label', alt || 'Image');

  const image = document.createElement('img');
  image.src = src;
  image.alt = alt || 'Image';
  image.decoding = 'async';
  image.style.cssText = 'display:block;width:auto;height:auto;max-width:100%;max-height:calc(100dvh - 48px);object-fit:contain;';

  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'chat-image-preview-close';
  close.textContent = '\u00d7';
  close.title = 'Close image';
  close.setAttribute('aria-label', 'Close image');
  close.style.cssText = 'position:absolute;top:-14px;right:-14px;display:grid;place-items:center;width:36px;height:36px;padding:0;border:0;border-radius:50%;background:rgba(0,0,0,.72);color:#fff;font-size:20px;line-height:1;cursor:pointer;';

  figure.append(image, close);
  backdrop.append(figure);
  hostRoot(returnFocus).append(backdrop);

  // The named handler is removed by closeChatImagePreview with the same identity.
  const onKeyDown = (event) => {
    if (event.key !== 'Escape') return;
    event.stopPropagation();
    closeChatImagePreview();
  };
  _activePreview = {backdrop, returnFocus, onKeyDown};
  document.addEventListener('keydown', onKeyDown);
  close.addEventListener('click', closeChatImagePreview);
  backdrop.addEventListener('click', (event) => {
    if (event.target === backdrop) closeChatImagePreview();
  });
  close.focus();
}
