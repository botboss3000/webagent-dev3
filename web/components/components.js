// The component inventory. The gallery and the pages both read this list, and a
// fixture checks it against the folders on disk, so it cannot drift silently.

export const components = [
  {
    id: 'activity',
    title: 'Activity group',
    summary: 'Collapsible group of model steps and tool calls inside one reply.',
    css: 'activity/activity.css',
  },
  {
    id: 'badge',
    title: 'Badge and avatar',
    summary: 'Small status pill and the identity circle it sits beside.',
    css: 'badge/badge.css',
  },
  {
    id: 'button',
    title: 'Button',
    summary: 'The one clickable primitive: default, primary, quiet, danger, small.',
    css: 'button/button.css',
  },
  {
    id: 'card',
    title: 'Card',
    summary: 'Bordered surface holding one idea; every other block composes it.',
    css: 'card/card.css',
  },
  {
    id: 'chat-header',
    title: 'Chat header',
    summary: 'Agent name row, session chip row and the quiet sub-header strip.',
    css: 'chat-header/chat-header.css',
  },
  {
    id: 'composer',
    title: 'Composer',
    summary: 'Pill input row with attach, dictate and send, plus the token strip.',
    css: 'composer/composer.css',
  },
  {
    id: 'icon',
    title: 'Icon',
    summary: 'Inline SVG icon set; no icon library and no external files.',
    js: 'icon/icon.js',
  },
  {
    id: 'list',
    title: 'List',
    summary: 'Rows of one thing each, with an optional hint and trailing slot.',
    css: 'list/list.css',
  },
  {
    id: 'message',
    title: 'Message',
    summary: 'User and agent bubbles in the chat transcript.',
    css: 'message/message.css',
  },
  {
    id: 'stat',
    title: 'Stat',
    summary: 'One number with its label, for dashboards.',
    css: 'stat/stat.css',
  },
  {
    id: 'tile',
    title: 'Tile',
    summary: 'Compact clickable card for dashboard and settings areas.',
    css: 'tile/tile.css',
  },
];

export function componentById(id) {
  return components.find((component) => component.id === id) || null;
}
