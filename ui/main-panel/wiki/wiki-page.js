// Wiki tab: the shared, searchable knowledge base, ported from the v2 Wiki page
// to the v3 API (/api/v3/wiki). Markup: wiki.html · styles: wiki.css.
//
// Everyone signed in can browse, search and read published articles. When the
// API reports can_edit (administrators) the page also shows drafts and the
// New / Edit / Publish / History / Restore / Delete controls; the server
// enforces the same rules, so hiding controls is only presentation.
//
// Deep links: ?tab=wiki&article=<slug> opens an article on start, and the open
// article is mirrored back into the address bar. Other pages can open an
// article with document.dispatchEvent(new CustomEvent('webagent:wiki-show',
// {detail: {slug}})).
//
// Safety: every string from the API is inserted with textContent; only the
// server-rendered, already-escaped article `html` field goes through innerHTML.

const API = '/api/v3/wiki';
const UNCATEGORIZED = 'General';
const SEARCH_DELAY_MS = 250;

/** Mount the Wiki page into `mount` (which already contains wiki.html). */
export async function start(mount) {
  // A re-mount replaces the previous instance's document listeners and timers.
  mount._wikiStop?.();
  const abort = new AbortController();
  const {signal} = abort;
  const root = mount.querySelector('.wiki-page');
  if (!root) return;
  const q = (selector) => root.querySelector(selector);

  const state = {
    articles: [],          // full browse list (summaries)
    canEdit: false,        // from the latest API response
    current: null,         // full article open in the reader, or null
    editing: null,         // null | 'new' | 'edit'
    searchResults: null,   // ranked results while a search is active
    filter: null,          // {type: 'category'|'tag', value} or null
    collapsed: new Set(),  // collapsed category names in the tree
    history: null,         // latest history response for the open article
    revision: null,        // revision snapshot being viewed (read-only)
    busy: false,           // a mutation is in flight
  };
  const requestIds = new Map();   // logical action key -> client_request_id
  let searchTimer = null;
  let toastTimer = null;
  let deleteTimer = null;
  mount._wikiStop = () => {
    abort.abort();
    clearTimeout(searchTimer); clearTimeout(toastTimer); clearTimeout(deleteTimer);
  };

  // ── Small helpers ──────────────────────────────────────────────────────────

  /** Create an element with an optional class and text content. */
  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }

  /** Lucide icon placeholder; rendered by icons(). */
  function icon(name) {
    const node = el('i', 'lucide-icon');
    node.dataset.lucide = name;
    return node;
  }

  function icons() { window.lucide?.createIcons(); }

  /** Show a short status message at the bottom of the page. */
  function toast(message, isError = false) {
    const box = q('.wiki-toast');
    box.textContent = message;
    box.classList.toggle('error', isError);
    box.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { box.hidden = true; }, isError ? 5000 : 2600);
  }

  /** Friendly relative date: today / yesterday / N days ago / Mon D, YYYY. */
  function relDate(iso) {
    const date = new Date(iso);
    if (!iso || Number.isNaN(date.getTime())) return '';
    const days = Math.round((Date.now() - date.getTime()) / 86400000);
    if (days <= 0) return 'today';
    if (days === 1) return 'yesterday';
    if (days < 7) return `${days} days ago`;
    return date.toLocaleDateString(undefined, {month: 'short', day: 'numeric', year: 'numeric'});
  }

  function fullDate(iso) {
    const date = new Date(iso);
    return !iso || Number.isNaN(date.getTime()) ? '' : date.toLocaleString(undefined, {dateStyle: 'medium', timeStyle: 'short'});
  }

  const categoryOf = (article) => (article.category || '').trim() || UNCATEGORIZED;
  const isPublished = (article) => article?.status === 'published';
  const splitList = (text) => String(text || '').split(',').map((item) => item.trim()).filter(Boolean);
  const plural = (count, word) => `${count} ${word}${count === 1 ? '' : 's'}`;

  // ── API ────────────────────────────────────────────────────────────────────

  /**
   * Call the wiki API. Mutations carry the CSRF token and one client_request_id
   * per logical action (same method, path and body), reused when that action is
   * retried after an uncertain failure and dropped once the server answers.
   */
  async function api(path, {method = 'GET', body} = {}) {
    const headers = {};
    let payload;
    let key = null;
    if (method !== 'GET') {
      const meResponse = await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'});
      if (!meResponse.ok) throw Object.assign(Error('Sign in required'), {status: 401});
      const me = await meResponse.json();
      key = JSON.stringify([method, path, body]);
      if (!requestIds.has(key)) requestIds.set(key, crypto.randomUUID());
      headers['Content-Type'] = 'application/json';
      headers['x-csrf-token'] = me.csrf_token;
      payload = JSON.stringify({...body, client_request_id: requestIds.get(key)});
    }
    const response = await fetch(API + path, {method, headers, body: payload, credentials: 'same-origin', cache: 'no-store'});
    const data = await response.json().catch(() => null);
    // A definite answer (success or a 4xx refusal) ends the logical action; a 5xx
    // may have applied, so its ID is kept for an exact retry.
    if (key && response.status < 500) requestIds.delete(key);
    if (!response.ok) {
      const fallback = {403: 'Administrator access required', 404: 'Article not found', 409: 'This article was changed by someone else'};
      throw Object.assign(Error(data?.error || fallback[response.status] || `Request failed (${response.status})`), {status: response.status});
    }
    return data;
  }

  /** Run one mutation at a time; reload on a 409 conflict and report errors. */
  async function mutate(action, {reloadOnConflict = true} = {}) {
    if (state.busy) return null;
    state.busy = true;
    root.classList.add('wiki-busy');
    try {
      return await action();
    } catch (error) {
      if (error.status === 409 && reloadOnConflict && state.current) {
        toast('Someone else changed this article. The latest version has been loaded; try again.', true);
        await openArticle(state.current.slug);
      } else {
        toast(error.message, true);
      }
      return null;
    } finally {
      state.busy = false;
      root.classList.remove('wiki-busy');
    }
  }

  // ── Edit visibility, URL and sidebar popover ───────────────────────────────

  /** Show or hide every administrator control according to can_edit. */
  function applyEditVisibility(canEdit) {
    if (typeof canEdit === 'boolean') state.canEdit = canEdit;
    for (const node of root.querySelectorAll('[data-edit-only]')) node.hidden = !state.canEdit;
  }

  /** Mirror the open article (or none) into the address bar without reloading. */
  function syncUrl(slug) {
    const url = new URL(location.href);
    url.searchParams.set('tab', 'wiki');
    if (slug) url.searchParams.set('article', slug);
    else url.searchParams.delete('article');
    history.replaceState(history.state, '', url);
  }

  function setSidebarOpen(open) {
    root.classList.toggle('wiki-sidebar-open', open);
    q('.wiki-sidebar-toggle').setAttribute('aria-expanded', String(open));
  }

  // ── Browse list, search and the sidebar ────────────────────────────────────

  /** Load the browse list, then redraw the sidebar and welcome landing. */
  async function refreshList() {
    try {
      const data = await api('');
      state.articles = data.articles || [];
      applyEditVisibility(!!data.can_edit);
      renderSidebar();
      renderWelcome();
    } catch (error) {
      q('.wiki-tree').replaceChildren(el('div', 'wiki-loading', `Couldn't load the wiki: ${error.message}`));
    }
  }

  /** Run the current search box query against the search endpoint. */
  async function runSearch() {
    const query = q('.wiki-search-input').value.trim();
    if (!query) { state.searchResults = null; renderSidebar(); return; }
    q('.wiki-tree').replaceChildren(el('div', 'wiki-loading', 'Searching…'));
    try {
      const data = await api(`/search?q=${encodeURIComponent(query)}`);
      if (q('.wiki-search-input').value.trim() !== query) return;   // superseded
      state.searchResults = data.results || [];
      applyEditVisibility(!!data.can_edit);
      renderSidebar();
    } catch (error) {
      q('.wiki-tree').replaceChildren(el('div', 'wiki-loading', `Search failed: ${error.message}`));
    }
  }

  function clearSearch() {
    const input = q('.wiki-search-input');
    input.value = '';
    q('.wiki-search-clear').hidden = true;
    clearTimeout(searchTimer);
    state.searchResults = null;
    renderSidebar();
  }

  /** One sidebar row; drafts get a small dot. */
  function treeItem(article, withSnippet) {
    const item = el('button', 'wiki-tree-item');
    item.type = 'button';
    item.dataset.slug = article.slug;
    item.title = article.title || article.slug;
    if (!isPublished(article)) item.classList.add('is-draft');
    if (state.current?.slug === article.slug) item.classList.add('active');
    const title = el('span', 'wiki-tree-item-title');
    if (!isPublished(article)) {
      const dot = el('span', 'wiki-tree-draft-dot');
      dot.title = 'Draft';
      title.append(dot);
    }
    title.append(document.createTextNode(article.title || article.slug));
    item.append(title);
    if (withSnippet && article.snippet) item.append(el('span', 'wiki-tree-snippet', article.snippet));
    const li = el('li');
    li.append(item);
    return li;
  }

  function flatList(articles, withSnippet) {
    const list = el('ul', 'wiki-tree-items wiki-tree-flat');
    for (const article of articles) list.append(treeItem(article, withSnippet));
    return list;
  }

  /** Sidebar shows search results, else the active filter, else the category tree. */
  function renderSidebar() {
    const tree = q('.wiki-tree');
    const count = q('.wiki-sidebar-count');
    const filterBar = q('.wiki-filter-active');
    filterBar.hidden = !state.filter || !!state.searchResults;
    if (state.filter) {
      q('.wiki-filter-active-text').textContent = `Filtered by ${state.filter.type === 'tag' ? '#' : ''}${state.filter.value}`;
    }
    let shown;
    if (state.searchResults) {
      shown = state.searchResults;
      count.textContent = plural(shown.length, 'result');
      tree.replaceChildren(shown.length ? flatList(shown, true) : el('div', 'wiki-tree-empty', 'No matching articles.'));
    } else if (state.filter) {
      shown = state.articles.filter((article) => state.filter.type === 'category'
        ? categoryOf(article) === state.filter.value
        : (article.tags || []).includes(state.filter.value));
      count.textContent = plural(shown.length, 'article');
      tree.replaceChildren(shown.length ? flatList(shown, false) : el('div', 'wiki-tree-empty', 'Nothing matches this filter.'));
    } else {
      shown = state.articles;
      count.textContent = plural(shown.length, 'article');
      tree.replaceChildren(shown.length ? categoryTree(shown) : el('div', 'wiki-tree-empty', 'No articles yet.'));
    }
    count.hidden = false;
    const latest = state.articles.reduce((max, article) => (article.updated_at > max ? article.updated_at : max), '');
    q('.wiki-sidebar-foot').hidden = !latest;
    q('.wiki-sidebar-foot-text').textContent = latest ? `Last updated ${relDate(latest)}` : '';
    icons();
  }

  /** Group articles by category into collapsible groups ("General" last). */
  function categoryTree(articles) {
    const groups = new Map();
    for (const article of articles) {
      const category = categoryOf(article);
      if (!groups.has(category)) groups.set(category, []);
      groups.get(category).push(article);
    }
    const fragment = document.createDocumentFragment();
    for (const category of sortedCategories(groups.keys())) {
      const items = groups.get(category).sort((a, b) => (a.title || '').localeCompare(b.title || ''));
      const collapsed = state.collapsed.has(category);
      const group = el('div', `wiki-tree-group${collapsed ? ' collapsed' : ''}`);
      const head = el('button', 'wiki-tree-cat');
      head.type = 'button';
      head.dataset.cat = category;
      head.setAttribute('aria-expanded', String(!collapsed));
      const caret = el('span', 'wiki-tree-caret');
      caret.append(icon('chevron-right'));
      head.append(caret, el('span', 'wiki-tree-cat-name', category), el('span', 'wiki-tree-cat-count', String(items.length)));
      const list = el('ul', 'wiki-tree-items');
      for (const article of items) list.append(treeItem(article, false));
      group.append(head, list);
      fragment.append(group);
    }
    return fragment;
  }

  function sortedCategories(names) {
    return [...names].sort((a, b) => (a === UNCATEGORIZED) - (b === UNCATEGORIZED) || a.localeCompare(b));
  }

  /** Filter the sidebar by category or tag; selecting the active filter clears it. */
  function applyFilter(type, value) {
    const same = state.filter?.type === type && state.filter.value === value;
    state.filter = same ? null : {type, value};
    if (q('.wiki-search-input').value) clearSearch();
    else renderSidebar();
    setSidebarOpen(!!state.filter);
  }

  // ── Welcome landing ────────────────────────────────────────────────────────

  /** Build the "Recently updated" cards and "Browse by category" chips. */
  function renderWelcome() {
    const recent = [...state.articles].sort((a, b) => String(b.updated_at).localeCompare(String(a.updated_at))).slice(0, 3);
    const cards = q('.wiki-welcome-cards');
    cards.replaceChildren(...recent.map((article) => {
      const card = el('button', 'wiki-welcome-card');
      card.type = 'button';
      card.dataset.slug = article.slug;
      card.append(el('span', 'wiki-welcome-card-cat', categoryOf(article)),
        el('span', 'wiki-welcome-card-title', article.title || article.slug),
        el('span', 'wiki-welcome-card-excerpt', (article.snippet || '').replace(/\s+/g, ' ').trim()));
      return card;
    }));
    q('[data-welcome="featured"]').hidden = !recent.length;

    const counts = new Map();
    for (const article of state.articles) counts.set(categoryOf(article), (counts.get(categoryOf(article)) || 0) + 1);
    const chips = q('.wiki-welcome-chips');
    chips.replaceChildren(...sortedCategories(counts.keys()).map((name) => {
      const chip = el('button', 'wiki-welcome-chip');
      chip.type = 'button';
      chip.dataset.filterType = 'category';
      chip.dataset.filterValue = name;
      chip.append(el('span', null, name), el('span', 'wiki-welcome-chip-count', String(counts.get(name))));
      return chip;
    }));
    q('[data-welcome="categories"]').hidden = !counts.size;
  }

  function showWelcome() {
    state.current = null; state.editing = null; state.revision = null; state.history = null;
    q('.wiki-welcome').hidden = false;
    q('.wiki-article').hidden = true;
    renderWelcome();
    renderSidebar();
    syncUrl(null);
  }

  // ── Reader ─────────────────────────────────────────────────────────────────

  /** Switch the article surface to one of: reader, history, editor. */
  function showPanel(panel) {
    q('.wiki-welcome').hidden = true;
    q('.wiki-article').hidden = false;
    q('.wiki-reader-layout').hidden = panel !== 'reader';
    q('.wiki-revisions').hidden = panel !== 'history';
    q('.wiki-editor').hidden = panel !== 'editor';
    q('.wiki-revision-banner').hidden = !state.revision;
    q('.wiki-toolbar-actions[data-mode="reading"]').hidden = panel === 'editor' || !!state.revision;
    q('.wiki-toolbar-actions[data-mode="editing"]').hidden = panel !== 'editor';
    q('.wiki-article-scroll').scrollTop = 0;
  }

  /** Fetch and show an article; also used to reload after changes. */
  async function openArticle(slug) {
    if (!slug) return;
    setSidebarOpen(false);
    try {
      const article = await api(`/articles/${encodeURIComponent(slug)}`);
      state.current = article; state.editing = null; state.revision = null; state.history = null;
      applyEditVisibility(!!article.can_edit);
      renderReader(article);
      showPanel('reader');
      renderSidebar();
      syncUrl(article.slug);
      loadBacklinks(article.slug);
    } catch (error) {
      toast(`Couldn't open article: ${error.message}`, true);
    }
  }

  /** Fill the breadcrumb and reader from an article, or a revision snapshot (raw body). */
  function renderReader(article, snapshot = false) {
    const title = article.title || article.slug || 'Untitled';
    const category = (article.category || '').trim();
    for (const selector of ['.wiki-breadcrumb-cat', '.wiki-reader-category']) {
      const node = q(selector);
      node.hidden = !category;
      node.textContent = category;
      node.dataset.filterValue = category;
    }
    q('.wiki-breadcrumb-sep').hidden = !category;
    q('.wiki-breadcrumb-title').textContent = title;
    q('.wiki-reader-title').textContent = title;
    q('.wiki-reader-status').hidden = snapshot || isPublished(article);
    const by = article.updated_by ? ` by ${article.updated_by}` : '';
    q('.wiki-reader-updated').textContent = article.updated_at ? `Updated ${relDate(article.updated_at)}${by}` : '';

    const body = q('.wiki-reader-body');
    const raw = q('.wiki-reader-raw');
    body.hidden = snapshot;
    raw.hidden = !snapshot;
    if (snapshot) {
      body.replaceChildren();
      raw.textContent = article.body || '';
    } else {
      body.innerHTML = article.html || '';   // server-rendered and escaped
      raw.textContent = '';
      for (const link of body.querySelectorAll('a[href^="http"]:not([data-wiki-slug])')) {
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
      }
    }

    const tags = q('.wiki-reader-tags');
    tags.replaceChildren(...(article.tags || []).map((tag) => {
      const chip = el('button', 'wiki-chip');
      chip.type = 'button';
      chip.dataset.filterType = 'tag';
      chip.dataset.filterValue = tag;
      chip.append(icon('hash'), document.createTextNode(tag));
      return chip;
    }));
    tags.hidden = !(article.tags || []).length;
    q('.wiki-backlinks').hidden = true;
    renderToolbar(article);
    icons();
  }

  /** Reading-mode toolbar: public link state and publish label. */
  function renderToolbar(article) {
    const published = isPublished(article);
    const copy = q('[data-act="copy-link"]');
    copy.setAttribute('aria-disabled', String(!published));
    copy.title = published ? 'Copy public link' : 'Publish to get a public link';
    const open = q('.wiki-open-public');
    open.hidden = !published || !article.url;
    if (article.url) open.href = article.url;
    q('.wiki-publish-label').textContent = published ? 'Unpublish' : 'Publish';
    q('[data-act="toggle-status"]').title = published ? 'Return to draft (administrators only)' : 'Publish for everyone';
    resetDelete();
  }

  /** Show which articles link to this one ("Linked from"). */
  async function loadBacklinks(slug) {
    try {
      const data = await api(`/articles/${encodeURIComponent(slug)}/backlinks`);
      if (state.current?.slug !== slug || state.revision) return;
      const links = data.linked_from || [];
      q('.wiki-backlinks-list').replaceChildren(...links.map((link) => {
        const button = el('button', 'wiki-backlink', link.title || link.slug);
        button.type = 'button';
        button.dataset.slug = link.slug;
        return button;
      }));
      q('.wiki-backlinks').hidden = !links.length;
    } catch { /* backlinks are best-effort */ }
  }

  // ── History and revisions (administrators) ─────────────────────────────────

  /** Toggle the revision history panel in place of the reader. */
  async function toggleHistory() {
    if (!state.current) return;
    if (!q('.wiki-revisions').hidden) { showPanel('reader'); return; }
    const list = q('.wiki-revisions-list');
    list.replaceChildren(el('div', 'wiki-loading', 'Loading history…'));
    showPanel('history');
    try {
      state.history = await api(`/articles/${encodeURIComponent(state.current.slug)}/history`);
      const revisions = state.history.revisions || [];
      if (!revisions.length) { list.replaceChildren(el('div', 'wiki-revisions-empty', 'No revisions yet.')); return; }
      list.replaceChildren(...revisions.map(historyRow));
    } catch (error) {
      list.replaceChildren(el('div', 'wiki-loading', `Couldn't load history: ${error.message}`));
    }
  }

  function historyRow(revision) {
    const row = el('div', 'wiki-revision-row');
    const meta = el('div', 'wiki-revision-meta', `Revision ${revision.revision} · ${fullDate(revision.updated_at)}`);
    if (revision.updated_by) meta.append(el('span', 'wiki-revision-who', ` · ${revision.updated_by}`));
    const isCurrent = revision.revision === state.history.current_revision;
    if (isCurrent) meta.append(el('span', 'wiki-revision-current', 'Current'));
    const actions = el('div', 'wiki-revision-actions');
    const view = el('button', 'wiki-btn wiki-btn-sm', 'View');
    view.type = 'button';
    view.dataset.revAct = 'view';
    view.dataset.revId = revision.revision_id;
    actions.append(view);
    if (!isCurrent) {
      const restore = el('button', 'wiki-btn wiki-btn-sm', 'Restore');
      restore.type = 'button';
      restore.dataset.revAct = 'restore';
      restore.dataset.revId = revision.revision_id;
      actions.append(restore);
    }
    row.append(meta, el('div', 'wiki-revision-snippet', `${revision.title || ''} — ${revision.snippet || ''}`), actions);
    return row;
  }

  /** Show one revision snapshot read-only, with Restore / Back controls. */
  async function viewRevision(revisionId) {
    try {
      const snapshot = await api(`/revisions/${encodeURIComponent(revisionId)}`);
      state.revision = {...snapshot, revision_id: revisionId};
      renderReader(snapshot, true);
      q('.wiki-revision-banner-text').textContent =
        `Viewing revision ${snapshot.revision ?? ''} from ${fullDate(snapshot.updated_at)}. This is read-only.`;
      q('[data-act="restore-this"]').hidden = !state.canEdit || snapshot.revision === state.history?.current_revision;
      showPanel('reader');
    } catch (error) {
      toast(`Couldn't load revision: ${error.message}`, true);
    }
  }

  /** Restore a revision as the new current version (history keeps the old one). */
  async function restoreRevision(revisionId) {
    if (!state.current || !state.history) return;
    if (!window.confirm('Restore this version? The current version stays in history.')) return;
    const slug = state.current.slug;
    const done = await mutate(() => api(`/articles/${encodeURIComponent(slug)}`, {method: 'PATCH',
      body: {action: 'restore', revision_id: revisionId, expected_revision: state.history.current_revision}}));
    if (!done) return;
    toast('Restored.');
    await openArticle(slug);
    refreshList();
  }

  // ── Status, link and delete ────────────────────────────────────────────────

  /** Publish a draft or return a published article to draft. */
  async function toggleStatus() {
    const article = state.current;
    if (!article) return;
    const status = isPublished(article) ? 'draft' : 'published';
    const done = await mutate(() => api(`/articles/${encodeURIComponent(article.slug)}`, {method: 'PATCH',
      body: {action: 'status', status, expected_revision: article.revision}}));
    if (!done) return;
    toast(status === 'published' ? 'Published.' : 'Returned to draft.');
    await openArticle(article.slug);
    refreshList();
  }

  /** Copy the public link of a published article. */
  async function copyLink() {
    const article = state.current;
    if (!isPublished(article) || !article.url) { toast('Publish to get a public link.'); return; }
    const link = location.origin + article.url;
    try {
      await navigator.clipboard.writeText(link);
      toast('Link copied.');
    } catch {
      window.prompt('Copy this link:', link);
    }
  }

  function resetDelete() {
    clearTimeout(deleteTimer);
    const button = q('[data-act="delete"]');
    button.classList.remove('confirming');
    q('.wiki-delete-label').textContent = '';
    button.title = 'Delete article (history is kept)';
  }

  /** Two-click delete: the first click arms the button, the second deletes. */
  async function deleteArticle() {
    const article = state.current;
    const button = q('[data-act="delete"]');
    if (!article) return;
    if (!button.classList.contains('confirming')) {
      button.classList.add('confirming');
      q('.wiki-delete-label').textContent = 'Confirm delete';
      toast('Click again to delete. Its revision history is kept.');
      deleteTimer = setTimeout(resetDelete, 5000);
      return;
    }
    resetDelete();
    const done = await mutate(() => api(`/articles/${encodeURIComponent(article.slug)}`, {method: 'PATCH',
      body: {action: 'delete', expected_revision: article.revision}}));
    if (!done) return;
    toast('Deleted. Its revision history is kept.');
    showWelcome();
    refreshList();
  }

  // ── Editor ─────────────────────────────────────────────────────────────────

  /** Open the editor for a new article (null) or the current one. */
  function openEditor(article) {
    const form = q('.wiki-editor');
    state.editing = article ? 'edit' : 'new';
    state.revision = null;
    form.elements.title.value = article?.title || '';
    form.elements.category.value = article?.category || '';
    form.elements.status.value = article?.status === 'published' ? 'published' : 'draft';
    form.elements.tags.value = (article?.tags || []).join(', ');
    form.elements.keywords.value = (article?.keywords || []).join(', ');
    form.elements.body.value = article?.body || '';
    q('.wiki-breadcrumb-cat').hidden = true;
    q('.wiki-breadcrumb-sep').hidden = true;
    q('.wiki-breadcrumb-title').textContent = article ? `Editing: ${article.title}` : 'New article';
    if (!article) { state.current = null; syncUrl(null); renderSidebar(); }
    showPanel('editor');
    form.elements.title.focus();
  }

  /** Leave the editor: back to the article, or the welcome screen for a new one. */
  function cancelEdit() {
    state.editing = null;
    if (state.current) { renderReader(state.current); showPanel('reader'); loadBacklinks(state.current.slug); }
    else showWelcome();
  }

  /** Create or update from the editor form, then show the saved article. */
  async function save() {
    const form = q('.wiki-editor');
    const fields = {
      title: form.elements.title.value.trim(),
      body: form.elements.body.value,
      tags: splitList(form.elements.tags.value),
      keywords: splitList(form.elements.keywords.value),
      category: form.elements.category.value.trim(),
      status: form.elements.status.value,
    };
    if (!fields.title) { toast('A title is required.', true); form.elements.title.focus(); return; }
    const existing = state.editing === 'edit' ? state.current : null;
    let conflict = false;
    const saved = await mutate(async () => {
      try {
        return existing
          ? await api(`/articles/${encodeURIComponent(existing.slug)}`, {method: 'PATCH',
            body: {action: 'update', expected_revision: existing.revision, ...fields}})
          : await api('/articles', {method: 'POST', body: fields});
      } catch (error) {
        if (error.status !== 409 || !existing) throw error;
        conflict = true;
        return null;
      }
    }, {reloadOnConflict: false});
    if (conflict) {
      // Keep the user's text; pick up the newer revision so a deliberate re-save can proceed.
      try {
        const latest = await api(`/articles/${encodeURIComponent(existing.slug)}`);
        state.current = latest;
        toast(`Someone else changed this article (now revision ${latest.revision}). Your text is kept; Cancel to read their version, or Save again to replace it.`, true);
      } catch (error) { toast(error.message, true); }
      return;
    }
    if (!saved) return;
    toast('Saved.');
    await openArticle(saved.slug);
    refreshList();
  }

  // ── Events ─────────────────────────────────────────────────────────────────

  const search = q('.wiki-search-input');
  search.addEventListener('input', () => {
    q('.wiki-search-clear').hidden = !search.value;
    if (search.value.trim()) state.filter = null;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(runSearch, SEARCH_DELAY_MS);
  }, {signal});
  search.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') { event.preventDefault(); clearSearch(); }
  }, {signal});

  // Delegated clicks for the whole page.
  root.addEventListener('click', (event) => {
    const target = event.target;
    const wikiLink = target.closest('a.wiki-link[data-wiki-slug]');
    if (wikiLink) { event.preventDefault(); openArticle(wikiLink.dataset.wikiSlug); return; }
    const category = target.closest('.wiki-tree-cat');
    if (category) {
      const name = category.dataset.cat;
      if (state.collapsed.has(name)) state.collapsed.delete(name); else state.collapsed.add(name);
      renderSidebar();
      return;
    }
    const item = target.closest('.wiki-tree-item, .wiki-welcome-card, .wiki-backlink');
    if (item?.dataset.slug) { openArticle(item.dataset.slug); return; }
    const filter = target.closest('[data-filter-type]');
    if (filter?.dataset.filterValue) { applyFilter(filter.dataset.filterType, filter.dataset.filterValue); return; }
    const revision = target.closest('[data-rev-act]');
    if (revision) {
      if (revision.dataset.revAct === 'view') viewRevision(revision.dataset.revId);
      else restoreRevision(revision.dataset.revId);
      return;
    }
    const button = target.closest('[data-act]');
    if (!button) return;
    const actions = {
      'toggle-sidebar': () => setSidebarOpen(!root.classList.contains('wiki-sidebar-open')),
      'clear-search': () => { clearSearch(); search.focus(); },
      'clear-filter': () => { state.filter = null; renderSidebar(); },
      new: () => openEditor(null),
      edit: () => openEditor(state.current),
      cancel: cancelEdit,
      save,
      delete: deleteArticle,
      'toggle-status': toggleStatus,
      'copy-link': copyLink,
      history: toggleHistory,
      'restore-this': () => state.revision && restoreRevision(state.revision.revision_id),
      'exit-revision': () => state.current && openArticle(state.current.slug),
    };
    actions[button.dataset.act]?.();
  }, {signal});
  q('.wiki-editor').addEventListener('submit', (event) => { event.preventDefault(); save(); }, {signal});

  // Ctrl/Cmd+K focuses search and Escape closes the contents popover, only while the page is visible.
  document.addEventListener('keydown', (event) => {
    if (!root.isConnected || !root.offsetParent) return;
    if (event.key === 'Escape' && root.classList.contains('wiki-sidebar-open')) { setSidebarOpen(false); return; }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      search.focus();
      search.select();
    }
  }, {signal});
  // Close the contents popover on any press outside it (capture phase, so other handlers can't swallow it).
  document.addEventListener('pointerdown', (event) => {
    if (!root.classList.contains('wiki-sidebar-open')) return;
    if (!q('.wiki-sidebar').contains(event.target) && !q('.wiki-sidebar-toggle').contains(event.target)) setSidebarOpen(false);
  }, {signal, capture: true});
  // Other pages open an article with the webagent:wiki-show event.
  document.addEventListener('webagent:wiki-show', (event) => {
    const slug = event.detail?.slug;
    if (slug) openArticle(String(slug));
  }, {signal});

  // ── Initial load ───────────────────────────────────────────────────────────

  applyEditVisibility(false);
  setSidebarOpen(false);
  icons();
  await refreshList();
  const deepLink = new URL(location.href).searchParams.get('article');
  if (deepLink) await openArticle(deepLink.trim());
}
