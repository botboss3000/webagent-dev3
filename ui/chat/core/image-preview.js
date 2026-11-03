'use strict';
// Shared full-screen image viewer for composer attachment tiles and published
// message attachments. One document-level viewer exists at a time; Escape, the
// backdrop and the close button dismiss it and focus returns to the tile.
// @code-index {"version":1,"feature":"chat-image-preview","aliases":["image preview","attachment zoom","thumbnail lightbox"],"purpose":"Shows composer and message image attachments in a dismissible full-screen viewer Trigger config: none","span_lines":60,"configuration":{"status":"static","references":[],"opportunities":[]}}

let _activePreview = null;

/** Closes the open viewer, removes its Escape listener/backdrop and restores focus to the connected trigger. */
export function closeChatImagePreview() {
  const state = _activePreview;
  if (!state) return;
  _activePreview = null;
  document.removeEventListener('keydown', state.onKeyDown);
  state.backdrop.remove();
  if (state.returnFocus?.isConnected) state.returnFocus.focus();
}

/** Opens one image over the transcript; a missing source is ignored and an already-open viewer is replaced. */
export function openChatImagePreview(src, alt = 'Image', returnFocus = null) {
  if (!src) return;
  closeChatImagePreview();

  const backdrop = document.createElement('div');
  backdrop.className = 'chat-image-preview-backdrop';
  const figure = document.createElement('figure');
  figure.className = 'chat-image-preview';
  figure.setAttribute('role', 'dialog');
  figure.setAttribute('aria-modal', 'true');
  figure.setAttribute('aria-label', alt || 'Image');

  const image = document.createElement('img');
  image.src = src;
  image.alt = alt || 'Image';
  image.decoding = 'async';

  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'chat-image-preview-close';
  close.textContent = '\u00d7';
  close.title = 'Close image';
  close.setAttribute('aria-label', 'Close image');

  figure.append(image, close);
  backdrop.append(figure);
  document.body.append(backdrop);

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
