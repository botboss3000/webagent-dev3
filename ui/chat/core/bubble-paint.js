// JSON owns bubble paint; null/missing values leave no local color or border default.
export function applyBubblePaint(bubble, lane = {}) {
  for (const [property, value] of [
    ['--message-bg-dark', lane.bg_dark],
    ['--message-bg-light', lane.bg_light],
    ['--message-border', lane.border],
  ]) {
    if (typeof value === 'string' && value.trim()) bubble.style.setProperty(property, value);
    else bubble.style.removeProperty(property);
  }
  // Clear the old renderer's inline border so config refreshes also clear null values.
  bubble.style.removeProperty('border');
  bubble.classList.add('v3-config-bubble');
}
