'use strict';

import { pushBackHandler } from '../../shared/history-back/history-back.js';

// Shared full-screen image viewer for composer attachment tiles and published
// message attachments. A click is delegated per root so tiles opened from any
// re-render still work. The dialog mounts in the browser's top layer and carries
// the critical layout inline, so it stays above the chat's own stacking layers.

let _activePreview = null;
const _boundRoots = new WeakSet();
const IMAGE_NAME = /\.(?:png|jpe?g|gif|webp|avif|bmp|svg|heic|heif)$/i;
const MIN_SCALE = 1;
const MAX_SCALE = 5;

/** The node the viewer must be appended to so it inherits the chat's own styles. */
function hostRoot(node) {
  const root = node && node.getRootNode ? node.getRootNode() : null;
  if (root && root.nodeType === 11) return root;
  return document.body || document.documentElement;
}

/** Clamps one number between a low and a high bound. */
function clamp(value, low, high) {
  return Math.min(high, Math.max(low, value));
}

/** Straight-line distance between two pointers. */
function distance(a, b) {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

/** Middle point between two pointers, used as the pinch anchor. */
function midpoint(a, b) {
  return {x: (a.x + b.x) / 2, y: (a.y + b.y) / 2};
}

/** The attachment name without its image extension, for the caption. */
function captionFor(label) {
  const text = String(label || '').trim();
  return text.replace(IMAGE_NAME, '') || 'Image';
}

/** Closes the open viewer, removes its Escape listener/backdrop and restores focus to the connected trigger. */
export function closeChatImagePreview() {
  const state = _activePreview;
  if (!state) return;
  _activePreview = null;
  // The viewer is gone, so the back press it claimed returns to the chat.
  state.releaseBack?.();
  state.pointers.clear();
  document.removeEventListener('keydown', state.onKeyDown);
  if (state.backdrop.open) state.backdrop.close();
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

/** The clicked tile's own attachment row, so the viewer never leaves this one message. */
function galleryFor(tile, src, alt) {
  const row = tile?.closest?.('.chat-attachments');
  const links = row ? Array.from(row.querySelectorAll('a.chat-attachment-image')) : [];
  const position = links.indexOf(tile);
  if (position < 0) return {items: [{src, label: alt}], index: 0};
  const items = links.map((link) => {
    const thumb = link.querySelector('img');
    return {
      src: thumb?.currentSrc || thumb?.getAttribute('src') || link.getAttribute('href') || src,
      label: thumb?.alt || alt || 'Image'
    };
  });
  return {items, index: position};
}

/** Opens the images of the clicked message tile, or one image when the tile has no row. */
export function openChatImagePreview(src, alt = 'Image', returnFocus = null) {
  if (!src) return;
  const gallery = galleryFor(returnFocus, src, alt);
  openChatImageGallery(gallery.items, gallery.index, returnFocus);
}

/** A chevron button that pages one message's images. */
function createNavButton(direction, title) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'chat-image-preview-nav ' + (direction < 0 ? 'is-prev' : 'is-next');
  button.dataset.role = direction < 0 ? 'previous' : 'next';
  button.title = title;
  button.setAttribute('aria-label', title);
  button.innerHTML = '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="' + (direction < 0 ? 'm15 18-6-6 6-6' : 'm9 18 6-6-6-6') + '"/></svg>';
  button.style.cssText = 'position:absolute;top:50%;' + (direction < 0 ? 'left:12px;' : 'right:12px;') + 'z-index:3;display:none;place-items:center;width:44px;height:44px;padding:0;transform:translateY(-50%);border:1px solid rgba(255,255,255,.26);border-radius:50%;background:rgba(0,0,0,.55);color:#fff;cursor:pointer;-webkit-tap-highlight-color:transparent;';
  return button;
}

/** Opens one paged image viewer; swipe and chevrons reach only the supplied images. */
export function openChatImageGallery(items, startIndex = 0, returnFocus = null) {
  const list = (Array.isArray(items) ? items : []).filter((item) => item && item.src);
  if (!list.length) return;
  closeChatImagePreview();
  bindImagePreviewRoot(returnFocus);

  const backdrop = document.createElement('dialog');
  backdrop.className = 'chat-image-preview-backdrop';
  backdrop.style.cssText = 'position:fixed;inset:0;box-sizing:border-box;width:100%;height:100%;max-width:none;max-height:none;margin:0;border:0;padding:24px;background:rgba(0,0,0,.88);display:flex;align-items:center;justify-content:center;';

  const figure = document.createElement('figure');
  figure.className = 'chat-image-preview';
  figure.style.cssText = 'position:relative;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px;max-width:100%;max-height:100%;margin:0;';

  const stage = document.createElement('div');
  stage.className = 'chat-image-preview-stage';
  stage.style.cssText = 'position:relative;display:flex;align-items:center;justify-content:center;max-width:100%;max-height:calc(100dvh - 120px);overflow:hidden;border:1px solid rgba(255,255,255,.16);border-radius:12px;background:#111;box-shadow:0 18px 60px rgba(0,0,0,.45);touch-action:none;cursor:zoom-in;-webkit-tap-highlight-color:transparent;';

  const image = document.createElement('img');
  image.decoding = 'async';
  image.draggable = false;
  image.style.cssText = 'display:block;width:auto;height:auto;max-width:100%;max-height:calc(100dvh - 120px);object-fit:contain;border:0;border-radius:0;background:transparent;box-shadow:none;transform-origin:center center;will-change:transform;-webkit-user-select:none;user-select:none;-webkit-user-drag:none;';

  const caption = document.createElement('figcaption');
  caption.className = 'chat-image-preview-caption';
  caption.style.cssText = 'display:flex;align-items:baseline;justify-content:center;gap:10px;max-width:min(100%,720px);font-size:12.5px;line-height:1.35;text-align:center;color:#fff;text-shadow:0 1px 3px rgba(0,0,0,.6);';
  const label = document.createElement('span');
  label.className = 'chat-image-preview-label';
  label.style.cssText = 'overflow-wrap:anywhere;';
  const counter = document.createElement('span');
  counter.className = 'chat-image-preview-count';
  counter.style.cssText = 'flex:0 0 auto;font-variant-numeric:tabular-nums;color:rgba(255,255,255,.64);';
  caption.append(label, counter);

  const prev = createNavButton(-1, 'Previous image');
  const next = createNavButton(1, 'Next image');

  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'chat-image-preview-close';
  close.textContent = '\u00d7';
  close.title = 'Close image';
  close.setAttribute('aria-label', 'Close image');
  close.style.cssText = 'position:absolute;top:-18px;right:-18px;display:grid;place-items:center;width:44px;height:44px;padding:0;border:1px solid rgba(255,255,255,.26);border-radius:50%;background:rgba(0,0,0,.72);color:#fff;font-size:22px;line-height:1;cursor:pointer;-webkit-tap-highlight-color:transparent;';

  const state = {
    backdrop, returnFocus, items: list,
    index: clamp(startIndex, 0, list.length - 1),
    scale: MIN_SCALE, tx: 0, ty: 0,
    pointers: new Map(), gesture: null, onKeyDown: null
  };

  /** The unscaled image centre in client coordinates. */
  function layoutCentre() {
    const rect = image.getBoundingClientRect();
    return {x: rect.left + rect.width / 2 - state.tx, y: rect.top + rect.height / 2 - state.ty};
  }

  /** Writes the current pan and zoom to the image, animating only deliberate changes. */
  function applyTransform(animate) {
    image.style.transition = animate ? 'transform 180ms ease-out' : 'none';
    image.style.transform = 'translate(' + state.tx + 'px,' + state.ty + 'px) scale(' + state.scale + ')';
    stage.style.cursor = state.scale > MIN_SCALE + 0.001 ? 'grab' : 'zoom-in';
  }

  /** Keeps the zoomed image from being dragged past its own edges. */
  function clampTranslation() {
    const maxX = Math.max(0, (state.scale - MIN_SCALE) * image.offsetWidth / 2);
    const maxY = Math.max(0, (state.scale - MIN_SCALE) * image.offsetHeight / 2);
    state.tx = clamp(state.tx, -maxX, maxX);
    state.ty = clamp(state.ty, -maxY, maxY);
  }

  /** Zooms to a scale while keeping the given client point under the pointer. */
  function zoomTo(scale, point, animate) {
    const nextScale = clamp(scale, MIN_SCALE, MAX_SCALE);
    if (Math.abs(nextScale - state.scale) < 0.001) return;
    const centre = layoutCentre();
    const px = point.x - centre.x;
    const py = point.y - centre.y;
    state.tx = px - (px - state.tx) * nextScale / state.scale;
    state.ty = py - (py - state.ty) * nextScale / state.scale;
    state.scale = nextScale;
    clampTranslation();
    applyTransform(animate);
  }

  /** Renders the current image, its caption and both chevron states. */
  function render() {
    const item = state.items[state.index];
    image.src = item.src;
    image.alt = item.label || 'Image';
    label.textContent = captionFor(item.label);
    const many = state.items.length > 1;
    counter.textContent = many ? (state.index + 1) + ' / ' + state.items.length : '';
    counter.style.display = many ? '' : 'none';
    for (const [button, disabled] of [[prev, state.index === 0], [next, state.index === state.items.length - 1]]) {
      button.style.display = many ? 'grid' : 'none';
      button.disabled = disabled;
      button.style.opacity = disabled ? '.3' : '1';
      button.style.cursor = disabled ? 'default' : 'pointer';
    }
    backdrop.setAttribute('aria-label', captionFor(item.label) + (many ? ' (' + (state.index + 1) + ' of ' + state.items.length + ')' : ''));
  }

  /** Shows another image from this message and clears any zoom. */
  function go(delta) {
    const nextIndex = state.index + delta;
    if (nextIndex < 0 || nextIndex >= state.items.length) return;
    state.index = nextIndex;
    state.scale = MIN_SCALE;
    state.tx = 0;
    state.ty = 0;
    render();
    applyTransform(false);
  }

  /** Starts a swipe, a pan or a two-finger pinch from the pointers currently down. */
  function startGesture() {
    const points = Array.from(state.pointers.values());
    if (points.length === 1) {
      state.gesture = {
        kind: state.scale > MIN_SCALE + 0.001 ? 'pan' : 'swipe',
        x0: points[0].x, y0: points[0].y,
        tx0: state.tx, ty0: state.ty,
        centre: layoutCentre(), dx: 0
      };
    } else if (points.length === 2) {
      state.gesture = {
        kind: 'pinch',
        span: Math.max(1, distance(points[0], points[1])),
        mid0: midpoint(points[0], points[1]),
        scale0: state.scale, tx0: state.tx, ty0: state.ty,
        centre: layoutCentre()
      };
    }
  }

  function onPointerDown(event) {
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    stage.setPointerCapture?.(event.pointerId);
    state.pointers.set(event.pointerId, {x: event.clientX, y: event.clientY});
    startGesture();
  }

  function onPointerMove(event) {
    if (!state.pointers.has(event.pointerId)) return;
    state.pointers.set(event.pointerId, {x: event.clientX, y: event.clientY});
    const gesture = state.gesture;
    if (!gesture) return;
    event.preventDefault();
    if (gesture.kind === 'pinch') {
      const points = Array.from(state.pointers.values());
      if (points.length < 2) return;
      const scale = clamp(gesture.scale0 * distance(points[0], points[1]) / gesture.span, MIN_SCALE, MAX_SCALE);
      const mid = midpoint(points[0], points[1]);
      const cx = gesture.mid0.x - gesture.centre.x - gesture.tx0;
      const cy = gesture.mid0.y - gesture.centre.y - gesture.ty0;
      state.scale = scale;
      state.tx = mid.x - gesture.centre.x - cx * scale / gesture.scale0;
      state.ty = mid.y - gesture.centre.y - cy * scale / gesture.scale0;
      clampTranslation();
      applyTransform(false);
      return;
    }
    if (gesture.kind === 'pan') {
      state.tx = gesture.tx0 + event.clientX - gesture.x0;
      state.ty = gesture.ty0 + event.clientY - gesture.y0;
      clampTranslation();
      applyTransform(false);
      return;
    }
    const dx = event.clientX - gesture.x0;
    const dy = event.clientY - gesture.y0;
    if (Math.abs(dx) > Math.abs(dy)) {
      gesture.dx = dx;
      image.style.transition = 'none';
      image.style.transform = 'translate(' + (state.tx + dx) + 'px,' + state.ty + 'px) scale(' + state.scale + ')';
    }
  }

  function onPointerUp(event) {
    if (!state.pointers.delete(event.pointerId)) return;
    const gesture = state.gesture;
    if (!gesture) return;
    if (gesture.kind === 'pinch') {
      if (state.pointers.size === 1) startGesture();
      else if (!state.pointers.size) {
        state.gesture = null;
        clampTranslation();
        applyTransform(true);
      }
      return;
    }
    state.gesture = null;
    if (gesture.kind === 'swipe') {
      const threshold = clamp(stage.clientWidth * 0.14, 48, 120);
      if (gesture.dx <= -threshold) { go(1); return; }
      if (gesture.dx >= threshold) { go(-1); return; }
      applyTransform(true);
      return;
    }
    clampTranslation();
    applyTransform(true);
  }

  function onPointerCancel(event) {
    if (!state.pointers.delete(event.pointerId)) return;
    state.gesture = null;
    clampTranslation();
    applyTransform(true);
  }

  function onWheel(event) {
    event.preventDefault();
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 100 : 1;
    zoomTo(state.scale * Math.exp(-event.deltaY * unit * 0.0018), {x: event.clientX, y: event.clientY}, false);
  }

  function onDoubleClick(event) {
    event.preventDefault();
    zoomTo(state.scale > MIN_SCALE + 0.001 ? MIN_SCALE : 2.5, {x: event.clientX, y: event.clientY}, true);
  }

  const onKeyDown = (event) => {
    if (event.key === 'Escape') { event.stopPropagation(); closeChatImagePreview(); return; }
    if (event.key === 'ArrowLeft') { event.preventDefault(); event.stopPropagation(); go(-1); return; }
    if (event.key === 'ArrowRight') { event.preventDefault(); event.stopPropagation(); go(1); }
  };

  stage.append(image);
  // The chevrons sit against the screen edges; the close control stays on the image corner.
  figure.append(stage, caption, close);
  backdrop.append(figure, prev, next);
  hostRoot(returnFocus).append(backdrop);
  backdrop.showModal();

  _activePreview = state;
  // Back closes the viewer before it leaves the chat.
  state.releaseBack = pushBackHandler(() => { closeChatImagePreview(); return true; });
  state.onKeyDown = onKeyDown;
  document.addEventListener('keydown', onKeyDown);
  // Escape is the dialog's native cancel; prevent it so closeChatImagePreview owns focus return.
  backdrop.addEventListener('cancel', (event) => { event.preventDefault(); closeChatImagePreview(); });
  close.addEventListener('click', closeChatImagePreview);
  backdrop.addEventListener('click', (event) => { if (event.target === backdrop) closeChatImagePreview(); });
  stage.addEventListener('pointerdown', onPointerDown);
  stage.addEventListener('pointermove', onPointerMove);
  stage.addEventListener('pointerup', onPointerUp);
  stage.addEventListener('pointercancel', onPointerCancel);
  stage.addEventListener('wheel', onWheel, {passive: false});
  stage.addEventListener('dblclick', onDoubleClick);
  prev.addEventListener('click', (event) => { event.stopPropagation(); go(-1); });
  next.addEventListener('click', (event) => { event.stopPropagation(); go(1); });

  render();
  close.focus();
}
