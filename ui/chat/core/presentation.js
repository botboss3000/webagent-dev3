'use strict';
// Adapted shared reference icon refresh; no v2 auth imports.
// @code-index {"version":1,"feature":"chat-message-bubbles","aliases":["message bubble icons","chat lucide icons"],"purpose":"Refreshes Lucide icons for newly rendered chat content","span_lines":24}
/** Renders uninitialized Lucide nodes within a host (or the document); renderer errors are contained and processed SVGs lose their marker. */
export function _refreshLucideIcons(host) {
  if (window.lucide && typeof window.lucide.createIcons === 'function') {
    try {
      const scope = host || document;
      const nodes = Array.from(scope.querySelectorAll('[data-lucide]:not(.lucide)'));
      if (!nodes.length) return;
      window.lucide.createIcons({ nodes });
      // The vendored lucide copies data-lucide onto the svgs it renders (and
      // renders them document-wide regardless of `nodes`). Remove the marker so
      // those svgs never match a future [data-lucide] scan again.
      scope.querySelectorAll('svg[data-lucide].lucide').forEach(function (svg) {
        svg.removeAttribute('data-lucide');
      });
    } catch (_) {}
  }
}

