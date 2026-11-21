// Locates the session-scoped expandable chat footer interaction Config: config/ui/chat/chat.json -> chat_common.active_footer, chat_common.idle_footer, chat_common.messages.pill_placeholder,…
import { attachGrabHandle } from '../../shared/grab-handle/grab-handle.js';

// SISTER-PANEL: V2-FOOTER-HANDLE — v2 chat-ui.js interaction, v3-only storage scope.
/**
 * Mounts the footer handle's visibility, pointer-drag, click, and session restore behavior.
 * Reads the active session from the URL and persists expansion under a v3-prefixed localStorage key; storage errors propagate.
 * Binds a document-lifetime v3:session-open listener and returns its reload callback, not a teardown.
 * chat-config.applyFooter calls this after populating controls and ignores the returned callback.
 */
export function initFooterHandle() {
// Reads the current session from the address bar when footer state is saved or restored.
const app = { get currentSessionId() { return new URL(location.href).searchParams.get('session_id') || ''; } };
// Reads v3-prefixed browser storage; localStorage access errors propagate.
const kvRead = (_, key) => localStorage.getItem('v3:' + key);
// Writes footer preferences only in the v3 localStorage namespace; storage errors propagate.
const kvWrite = (_, key, value) => localStorage.setItem('v3:' + key, value);
  // ── Footer drag handle (abilities / device / mode chips) ──
  const _footerHandle = document.getElementById('chat-footer-handle');
  const _footerRow = document.getElementById('chat-footer-row');

  // Hide the handle entirely when below_pill has no visible content.
  // Check after the row has been populated by chat-controls-config.
  function _updateFooterHandleVisibility() {
    if (!_footerHandle || !_footerRow) return;
    const controls = _footerRow.querySelectorAll('[data-header-control], [data-element-origin]');
    const hasContent = Array.from(controls).some(
      el => el.style.display !== 'none' && !el.hidden
    );
    _footerHandle.style.display = hasContent ? '' : 'none';
  }
  _updateFooterHandleVisibility();
  // The local adapter defines only currentSessionId, so this legacy refresh-hook branch is dormant here.
  if (app._reapplyChatControls) {
    const _origReapply = app._reapplyChatControls;
    // Rechecks visibility after the existing control refresh completes.
    const _patchedReapply = function() {
      const result = _origReapply.apply(this, arguments);
      setTimeout(_updateFooterHandleVisibility, 50);
      return result;
    };
    app._reapplyChatControls = _patchedReapply;
  }

  // Persists the expanded state under the current session's legacy key shape.
  function _saveFooterExpanded(expanded) {
    const sid = app.currentSessionId;
    if (sid) {
      kvWrite('chat:footerExpanded:' + sid, 'chat_footer_expanded_' + sid, expanded ? '1' : '0');
    }
  }

  // Restores this session's preference, defaulting to an expanded footer.
  function _loadFooterExpanded() {
    const sid = app.currentSessionId;
    if (sid) {
      const saved = kvRead('chat:footerExpanded:' + sid, 'chat_footer_expanded_' + sid);
      if (saved !== undefined && saved !== null) return saved === '1';
    }
    return true; // default to expanded
  }

  // Synchronizes handle and row classes/ARIA state, then saves the preference.
  function _applyFooterExpanded(expanded) {
    if (_footerHandle) {
      _footerHandle.classList.toggle('expanded', expanded);
      _footerHandle.setAttribute('aria-expanded', String(expanded));
    }
    if (_footerRow) _footerRow.classList.toggle('expanded', expanded);
    _saveFooterExpanded(expanded);
  }

  if (_footerHandle && _footerRow) {
    // Apply saved state on init
    _applyFooterExpanded(_loadFooterExpanded());

    // Drag, rubber band and tap bounce come from the shared grab handle; the pill edge stretches with it.
    attachGrabHandle(_footerHandle, {
      panel: _footerHandle.parentElement,
      opens: 'up',
      isExpanded: () => _footerRow.classList.contains('expanded'),
      setExpanded: expanded => _applyFooterExpanded(expanded),
    });
  }

  // Assigns the callback invoked by the persistent v3:session-open document listener.
  app.reloadFooterExpanded = () => _applyFooterExpanded(_loadFooterExpanded());

document.addEventListener('v3:session-open', app.reloadFooterExpanded);
return app.reloadFooterExpanded;
}
