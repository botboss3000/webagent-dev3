// Chat page: the destination of the phone header's chat icon. It mounts the ported
// chat shell — panel frame with an empty header, transcript and footer.

import { mountChat } from '../../chat/chat.js';

export const meta = {
  title: 'Chat',
  note: '',
};

export async function render(host) {
  const page = document.createElement('div');
  page.className = 'chat-page';
  host.append(page);
  await mountChat(page);
}
