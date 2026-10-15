// @code-index {"version":1,"feature":"chat-message-bubbles","aliases":["chat bubbles","bubble colors","bubble borders"],"purpose":"Applies configured JSON-only border/box_shadow, bubble paint and JSON-only per-side padding (unset/null means initial), per-corner radius (auto uses the single-line box) and role line_height. Config: data/config/chat.json -> chat_common.message_bubbles.user/agent paint, border, box_shadow, padding_*_px, radius_*_px and line_height; chat_desktop/chat_mobile/chat_widget overrides merge onto chat_common.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/message_bubbles/user","/chat_common/message_bubbles/agent"],"profile_overrides":true}],"opportunities":[]}}
const radiusFrames = new WeakMap();
/** Applies role-local presentation; auto corners use one text line plus vertical padding and borders. */
export function applyBubblePaint(bubble, lane = {}) {
  for (const [property, value] of [
    ['--message-bg-dark', lane.bg_dark],
    ['--message-bg-light', lane.bg_light],
  ]) {
    if (typeof value === 'string' && value.trim()) bubble.style.setProperty(property, value);
    else bubble.style.removeProperty(property);
  }
  // Initial makes unset effects absent and clears prior profile values; presets live only in chat.json.
  for (const [property, value] of [['--message-border', lane.border], ['--message-shadow', lane.box_shadow]]) {
    bubble.style.setProperty(property, typeof value === 'string' && value.trim() ? value.trim() : 'initial');
  }
  // Geometry belongs to each role; no shared padding or device-local settings are consulted.
  const px = (value, fallback) => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : fallback;
  for (const side of ['top','right','bottom','left']) {
    const value = lane['padding_' + side + '_px'];
    // Initial clears prior profile values and prevents shared CSS from supplying padding.
    bubble.style.setProperty('padding-' + side, typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value + 'px' : 'initial');
  }
  const corners = ['top_left','top_right','bottom_right','bottom_left'];
  const baseRadius = lane.radius_px === 'auto' ? 'auto' : px(lane.radius_px,14);
  const radii = corners.map(corner => lane['radius_' + corner + '_px'] === 'auto' ? 'auto' : px(lane['radius_' + corner + '_px'], baseRadius));
  // The shared stylesheet consumes this token with !important; explicit corners stay independent.
  const writeRadius = auto => {
    const radius = radii.map(value => value === 'auto' ? auto : value + 'px').join(' ');
    bubble.style.setProperty('--chat-role-radius', radius); bubble.style.borderRadius = radius;
  };
  bubble.style.lineHeight = String(lane.line_height || 1.5);
  if (radiusFrames.has(bubble)) { cancelAnimationFrame(radiusFrames.get(bubble)); radiusFrames.delete(bubble); }
  writeRadius('calc(1lh / 2)');
  if (radii.includes('auto')) radiusFrames.set(bubble,requestAnimationFrame(() => {
    radiusFrames.delete(bubble);
    if (!bubble.isConnected) return;
    const body = bubble.querySelector('.bubble-body,.md-body') || bubble;
    let lineHeight = Number.parseFloat(getComputedStyle(body).lineHeight);
    if (!Number.isFinite(lineHeight)) {
      // Measure normal line-height without changing the bubble's flow or content.
      const probe = document.createElement('span'); probe.textContent = '\u200b';
      probe.style.cssText = 'position:absolute;visibility:hidden;display:block;white-space:pre;padding:initial;border:initial;margin:initial';
      body.append(probe); lineHeight = probe.getBoundingClientRect().height; probe.remove();
    }
    const style = getComputedStyle(bubble);
    const metrics = [lineHeight,...['paddingTop','paddingBottom','borderTopWidth','borderBottomWidth'].map(key => Number.parseFloat(style[key]))];
    if (metrics.every(Number.isFinite)) writeRadius(metrics.reduce((sum,value) => sum + value,0) / 2 + 'px');
  }));
  // Clear the old renderer's inline border so config refreshes also clear null values.
  bubble.style.removeProperty('border'); bubble.style.removeProperty('box-shadow');
  bubble.classList.add('v3-config-bubble');
}
