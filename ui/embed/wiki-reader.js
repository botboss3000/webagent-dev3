// Wiki reader for the embedded chat: an article the agent linked opens beside the chat
// when the window has room (the host widens it) and over the transcript when it does not.
// Articles come from the Wiki API with the visitor's own access, so only published ones
// open; their HTML is rendered on the server and sanitized again here.

/** Creates the reader. `request(path)` is the embed's authenticated GET; `onToggle(open)` tells the host. */
export function createWikiReader({request, onToggle = () => {}}) {
  const pane = document.createElement('aside');
  pane.className = 'wa-wiki-reader';
  pane.hidden = true;
  pane.setAttribute('aria-label', 'Wiki article');
  const head = document.createElement('header');
  head.className = 'wa-wiki-reader-head';
  const back = button('Back', 'arrow-left', () => {
    history.pop();
    show(history.pop());
  });
  const title = document.createElement('h2');
  title.className = 'wa-wiki-reader-title';
  const page = document.createElement('a');
  page.className = 'wa-wiki-reader-page';
  page.target = '_blank';
  page.rel = 'noopener';
  page.title = 'Open the article page in a new tab';
  page.setAttribute('aria-label', page.title);
  page.innerHTML = '<i data-lucide="external-link" class="lucide-icon"></i>';
  const close = button('Close article', 'x', () => api.close());
  head.append(back, title, page, close);
  const body = document.createElement('div');
  body.className = 'wa-wiki-reader-body md-body';
  pane.append(head, body);
  document.body.append(pane);
  // Articles opened from links inside the reader, so Back can return to them.
  const history = [];
  let generation = 0;

  function button(label, icon, action) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'wa-wiki-reader-button';
    b.title = label;
    b.setAttribute('aria-label', label);
    b.innerHTML = `<i data-lucide="${icon}" class="lucide-icon"></i>`;
    b.addEventListener('click', action);
    return b;
  }

  // Purpose: Load one article into the pane; a newer open supersedes a slower one.
  async function show(slug) {
    if (!slug) return api.close();
    const current = ++generation;
    history.push(slug);
    back.hidden = history.length < 2;
    title.textContent = 'Loading…';
    page.href = '/wiki/' + encodeURIComponent(slug);
    body.replaceChildren();
    try {
      const article = await request('/wiki/articles/' + encodeURIComponent(slug));
      if (current !== generation) return;
      title.textContent = article.title || slug;
      page.hidden = article.status !== 'published';
      body.innerHTML = window.DOMPurify?.sanitize
        ? window.DOMPurify.sanitize(article.html || '', {FORBID_ATTR: ['style']})
        : '';
      if (!window.DOMPurify?.sanitize) body.textContent = article.body || '';
      for (const link of body.querySelectorAll('a[href]:not(.wiki-link)')) {
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
      }
    } catch (error) {
      if (current !== generation) return;
      title.textContent = 'Article unavailable';
      page.hidden = true;
      const note = document.createElement('p');
      note.className = 'wa-wiki-reader-note';
      note.textContent = error.status === 404
        ? 'This article does not exist or is not published.'
        : error.message || 'The article could not be loaded.';
      body.append(note);
    }
    window.lucide?.createIcons();
    body.scrollTop = 0;
  }

  // Wiki links inside an article open in the reader too.
  body.addEventListener('click', (event) => {
    const link = event.target.closest('a.wiki-link[data-wiki-slug]');
    if (!link) return;
    event.preventDefault();
    void show(link.dataset.wikiSlug);
  });
  const onKey = (event) => {
    if (event.key === 'Escape' && !pane.hidden) api.close();
  };
  document.addEventListener('keydown', onKey);

  const api = {
    /** Opens `slug`, replacing the reader's history. */
    open(slug) {
      history.length = 0;
      if (pane.hidden) {
        pane.hidden = false;
        document.body.classList.add('wa-reader-open');
        onToggle(true);
      }
      void show(slug);
      close.focus({preventScroll: true});
    },
    close() {
      if (pane.hidden) return;
      ++generation;
      pane.hidden = true;
      document.body.classList.remove('wa-reader-open');
      onToggle(false);
    },
    destroy() {
      api.close();
      document.removeEventListener('keydown', onKey);
      pane.remove();
    },
  };
  return api;
}
