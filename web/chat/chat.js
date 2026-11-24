// Mounts the chat shell partial into a host element. Behaviour only: the markup
// is chat.html and the frame styles are chat.css. No network calls.

const PARTIAL = new URL('./chat.html', import.meta.url);

export async function mountChat(host) {
  if (!host || host.dataset.mounted === 'yes') return host;
  const response = await fetch(PARTIAL, { headers: { accept: 'text/html' } });
  if (!response.ok) throw new Error(`chat shell ${response.status}`);
  host.innerHTML = await response.text();
  host.dataset.mounted = 'yes';
  return host;
}
