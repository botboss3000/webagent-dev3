import { PAGE_ACTIONS, discoverPageControlFeedback } from './page-control-feedback.js';

// @code-index {"version":1,"feature":"app-control-feedback","aliases":["App subpage buttons","File Manager haptics"],"purpose":"Covers App Config, Functions, Modules, Settings, Files/Git, Users, User Policies, Page Access and Model Catalog. Config: ui/vibrate.json -> enabled, duration_ms. Custom tree/tab/copy/model selectors follow actual handlers; edits and dragging stay quiet.","span_lines":16,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
export const APP_ACTIONS = [PAGE_ACTIONS,
  '.files-tree-row', '.files-tab', '.files-commit-tab', '.fg-hash', '.fg-click-copy',
  '.ac-group-head', '.conn-card-header-clickable', '.prompt-item', '.ac-tri',
  '.ac-model-item', '.ac-model-search-chevron', '.ac-saved-name-click', '.ac-model-meta-editable',
  '.ac-tip-gallery img', '.ac-tip-lightbox'
].join(', ');
export const APP_PORTALS = '#files-floating-menu, .ac-model-dropdown, .ac-directive-overlay, .ac-tip-pop, .ac-tip-lightbox';

discoverPageControlFeedback('#tab-app', {actions: APP_ACTIONS, portals: APP_PORTALS});
