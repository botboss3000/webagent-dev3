// MCP connections are agent-scoped. These controls do not grant tool permission.
const node = (tag, cls = '', text = '') => {
  const el = document.createElement(tag); el.className = cls; el.textContent = text; return el;
};
const button = text => { const el = node('button', 'ac-btn ac-btn-sm', text); el.type = 'button'; return el; };
const input = (label, value = '', type = 'text') => {
  const el = node('input', 'ac-input'); el.type = type; el.value = value;
  el.maxLength = 2048; el.setAttribute('aria-label', label); return el;
};
const field = (label, control) => {
  const el = node('label', 'ac-hint', label); el.append(control); return el;
};
const output = result => {
  if (result?.status && result.status !== 'ok' && result.status !== 'success' && result.status !== 'completed') {
    throw new Error('MCP operation was not completed.');
  }
  return result?.output || result;
};
const httpsEndpoint = value => {
  try {
    const url = new URL(value);
    return url.protocol === 'https:' && !url.username && !url.password && !url.hash;
  } catch { return false; }
};

export function buildMcpControls({ ability, request, scope, canManage, levelId, previewUser, status }) {
  if (ability.id !== 'mcp') return null;
  const panel = node('section', 'ac-list v3-mcp-settings');
  const notice = node('p', 'ac-hint', 'Loading MCP connections…');
  notice.setAttribute('role', 'status');
  const list = node('div', 'ac-list');
  const editor = node('div', 'ac-list');
  const discovery = node('div', 'ac-list');
  const reload = button('Reload MCP connections');
  panel.append(node('h4', '', 'MCP connections'), notice, list, discovery, editor, reload);
  const editable = canManage === true && !levelId && !previewUser;
  const base = scope + '/mcp';
  let revision = null, generation = 0, busy = false, available = false;
  const current = token => panel.isConnected && token === generation;
  const report = text => {
    if (!panel.isConnected) return;
    notice.textContent = text;
    if (status?.isConnected) status.textContent = text;
  };
  const locks = () => {
    for (const el of panel.querySelectorAll('button, input')) {
      el.disabled = busy || (el !== reload && (!editable || !available || revision === null));
    }
  };
  const failure = error => report(error?.status === 409
    ? 'Connections changed. Reload MCP connections before saving again.'
    : 'MCP request failed. Check the connection and your access, then reload before retrying a save.');
  const act = async (operation, onSuccess) => {
    if (busy || !panel.isConnected) return;
    busy = true; const token = ++generation; locks();
    try {
      const result = await operation();
      if (current(token)) await onSuccess(result, token);
    } catch (error) { if (current(token)) failure(error); }
    finally { if (current(token)) { busy = false; locks(); } }
  };
  const load = async () => {
    if (busy) return;
    await act(() => request(base), result => {
      revision = result.revision; available = result.available === true;
      editor.replaceChildren(); list.replaceChildren();
      for (const connection of result.connections || []) {
        const row = node('div', 'conn-card');
        row.style.overflowWrap = 'anywhere';
        row.append(node('strong', '', connection.name), node('p', 'ac-hint', connection.endpoint),
          node('p', 'ac-hint', connection.enabled ? 'On' : 'Off'));
        row.append(node('p', 'ac-hint', 'Allowed tools: ' + ((connection.allowed_tools || []).join(', ') || 'None')));
        if (editable && available && connection.can_edit !== false) {
          const edit = button('Edit connection'); edit.onclick = () => openEditor(connection);
          const toggle = button(connection.enabled ? 'Disable connection' : 'Enable connection');
          toggle.onclick = () => {
            if (!window.confirm((connection.enabled ? 'Disable' : 'Enable') + ' this MCP connection?')) return;
            mutate(connection.id, 'PUT', { connection: {
              name: connection.name, endpoint: connection.endpoint, enabled: !connection.enabled,
              allowed_tools: connection.allowed_tools || [],
            } });
          };
          const remove = button('Remove connection');
          remove.onclick = () => {
            if (window.confirm('Remove this MCP connection and its stored credential?')) mutate(connection.id, 'DELETE', {});
          };
          row.append(edit, toggle, remove);
        }
        list.append(row);
      }
      if (!list.childElementCount) list.append(node('p', 'ac-hint', 'No saved MCP connections.'));
      report(available ? (editable ? 'Choose the tools each connection may expose to this agent.' : 'MCP connections are read-only in this view.')
        : 'The MCP plugin is unavailable. Install and enable it to manage connections.');
      renderDiscovery();
    });
  };
  const mutate = (id, method, body) => {
    if (!editable || !available || revision === null || busy) return;
    // Revision fences reject another tab's edits; an uncertain write is never replayed.
    act(async () => {
      try {
        return await request(base + '/connections/' + encodeURIComponent(id), method,
          { expected_revision: revision, ...body });
      } catch (error) {
        revision = null; // Reconcile a failed or uncertain write with a fresh read.
        throw error;
      }
    }, async (_, token) => {
        revision = null;
        report('MCP connection saved. Reloading…');
        const result = await request(base);
        if (!current(token)) return;
        revision = result.revision;
        busy = false;
        await load();
      });
  };
  const openEditor = (connection = {}) => {
    if (!editable || !available || busy || connection.can_edit === false) return;
    editor.replaceChildren();
    const name = input('Connection name', connection.name || ''); name.maxLength = 120;
    const endpoint = input('MCP server HTTPS endpoint', connection.endpoint || '', 'url');
    const bearer = input('Bearer credential (optional)', '', 'password');
    bearer.autocomplete = 'new-password';
    const enabled = input('Enable this connection', '', 'checkbox'); enabled.checked = connection.enabled === true;
    const tools = node('div', 'ac-list');
    const inspect = button('Inspect server tools');
    const save = button('Save connection');
    const cancel = button('Cancel'); cancel.onclick = () => editor.replaceChildren();
    let inspectedEndpoint = connection.id ? endpoint.value : null;
    let selected = new Set(connection.allowed_tools || []);
    let knownTools = connection.tools || [];
    const renderTools = () => {
      tools.replaceChildren(node('p', 'ac-hint', 'Select allowed tools explicitly. No tools are selected for a new connection.'));
      for (const tool of knownTools) {
        const check = input('Allow MCP tool ' + tool.name, '', 'checkbox'); check.checked = selected.has(tool.name);
        check.onchange = () => check.checked ? selected.add(tool.name) : selected.delete(tool.name);
        const label = field(tool.name, check); label.append(node('span', 'ac-hint', tool.description || ''));
        tools.append(label);
      }
      if (!knownTools.length) tools.append(node('p', 'ac-hint', 'No tools discovered. Inspect the endpoint before adding a connection.'));
      locks();
    };
    endpoint.oninput = () => { inspectedEndpoint = null; selected.clear(); knownTools = []; renderTools(); };
    inspect.onclick = () => {
      if (!httpsEndpoint(endpoint.value.trim())) { report('Enter an HTTPS endpoint without embedded credentials or a fragment.'); return; }
      act(() => request(base + '/inspect', 'POST', { endpoint: endpoint.value.trim(), ...(bearer.value ? { bearer: bearer.value } : {}) }), result => {
        knownTools = output(result).tools || []; selected.clear(); inspectedEndpoint = endpoint.value.trim(); renderTools();
        report('Tools inspected. Select allowed tools, then save.');
      });
    };
    save.onclick = () => {
      if (!name.value.trim() || !httpsEndpoint(endpoint.value.trim())) { report('Enter a name and valid HTTPS endpoint.'); return; }
      if (inspectedEndpoint !== endpoint.value.trim()) { report('Inspect this endpoint and select allowed tools before saving.'); return; }
      const body = { connection: { name: name.value.trim(), endpoint: endpoint.value.trim(),
        enabled: enabled.checked, allowed_tools: [...selected] } };
      if (bearer.value) body.bearer = bearer.value;
      bearer.value = '';
      mutate(connection.id || crypto.randomUUID(), 'PUT', body);
    };
    editor.append(node('h4', '', connection.id ? 'Edit MCP connection' : 'Add MCP connection'),
      field('Name', name), field('HTTPS endpoint', endpoint), field('Bearer credential (optional; stored privately)', bearer),
      node('p', 'ac-hint', connection.id ? 'Leave the credential blank to retain it.' : 'Credentials are never shown to the agent.'),
      field('Enabled', enabled), inspect, tools, save, cancel);
    renderTools();
  };
  const renderDiscovery = () => {
    discovery.replaceChildren();
    if (!editable || !available) return;
    const query = input('Search MCP servers', '', 'search'); query.maxLength = 200;
    const search = button('Search MCP servers');
    const add = button('Add HTTPS endpoint'); add.onclick = () => openEditor();
    const results = node('div', 'ac-list');
    search.onclick = () => act(() => request(base + '/search', 'POST', { query: query.value.trim() }), result => {
      results.replaceChildren();
      for (const server of output(result).servers || []) {
        const row = node('div', 'conn-card'); row.style.overflowWrap = 'anywhere';
        row.append(node('strong', '', server.name), node('p', 'ac-hint', server.description || ''),
          node('p', 'ac-hint', server.endpoint || ''));
        const choose = button('Review and add');
        choose.onclick = () => openEditor({ name: server.name, endpoint: server.endpoint });
        row.append(choose); results.append(row);
      }
      if (!results.childElementCount) results.append(node('p', 'ac-hint', 'No matching servers. Add a known HTTPS endpoint instead.'));
      report('Search complete. Review a server before adding it.');
    });
    discovery.append(field('Search MCP servers', query), search, add, results);
  };
  reload.onclick = load;
  // The ability body is attached synchronously after construction. All later
  // callbacks require the same mounted generation and cannot resurrect a page.
  queueMicrotask(load);
  return panel;
}
