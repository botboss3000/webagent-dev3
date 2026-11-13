import { discoverPageControlFeedback } from './page-control-feedback.js';

// Owns native actions on remaining main-panel pages, private access, standalone setup and embed authentication/consent.
// Catalog mounts include optional module pages; App and Agents keep their owners.
// Native semantic controls cover these pages. Editable fields and the Metrics
// table's keyboard scrolling region remain outside the action selector.
// Direct body children cover popup consent; standalone HTML loads this entry
// without the authenticated app shell. Shared discovery handles BFCache teardown.
discoverPageControlFeedback([
  '#main-panel > .tab-content:not(#tab-app):not(#tab-agents)',
  '#app-restricted-overlay', '.setup-box', '.wa-embed-connect',
].join(', '));
