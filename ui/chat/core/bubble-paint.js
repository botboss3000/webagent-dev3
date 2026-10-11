// @code-index {"version":1,"feature":"chat-message-bubbles","aliases":["chat bubbles","bubble colors","bubble borders"],"purpose":"Applies configured bubble colors and border. Config: data/config/chat.json -> chat_common.message_bubbles.user/agent.{bg_dark,bg_light,border}; chat_desktop/chat_mobile/chat_widget overrides merge onto chat_common.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/message_bubbles/user/bg_dark","/chat_common/message_bubbles/user/bg_light","/chat_common/message_bubbles/user/border","/chat_common/message_bubbles/agent/bg_dark","/chat_common/message_bubbles/agent/bg_light","/chat_common/message_bubbles/agent/border"],"profile_overrides":true}],"opportunities":[]}}
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
