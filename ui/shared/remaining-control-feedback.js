import { discoverPageControlFeedback } from './page-control-feedback.js';

// @code-index {"version":1,"feature":"remaining-page-control-feedback","aliases":["account button feedback","builds button haptics","authentication touch feedback"],"purpose":"Owns native actions on remaining main-panel pages, private access, standalone setup and embed authentication/consent. Chat, header, Agents and App retain their existing owners. Config: ui/vibrate.json -> enabled, duration_ms.","span_lines":16,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
// Catalog mounts include optional module pages; App and Agents keep their owners.
// Native semantic controls cover these pages. Editable fields and the Metrics
// table's keyboard scrolling region remain outside the action selector.
// Direct body children cover popup consent; standalone HTML loads this entry
// without the authenticated app shell. Shared discovery handles BFCache teardown.
discoverPageControlFeedback([
  '#main-panel > .tab-content:not(#tab-app):not(#tab-agents)',
  '#app-restricted-overlay', '.setup-box', '.wa-embed-auth', '.wa-embed-connect',
].join(', '));
