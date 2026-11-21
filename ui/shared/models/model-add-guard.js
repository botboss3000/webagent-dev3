'use strict';

// MODEL-ADD-GUARD: a WebAgent Catalog model already in the roster is not added twice.
// A catalog row's entry_id IS the opaque catalog model id, so it can be matched against
// the picker's selectedModel; a My Models pick (byo:<entry>) matches a row's
// catalog_source_id. BYO and subscription rows have no stable client identity for a
// not-yet-saved pick, so they never produce a duplicate here.
//
// The decision is pure and DOM-free so it can be tested without a page.
export const MODEL_ADD_DUPLICATE = 'This model is already in your roster';

export function modelAddGuard({ platformCatalog, providers, selectedModel }) {
  const dupe = platformCatalog
    ? (providers || []).find(row => [row.entry_id, row.catalog_source_id].map(String).includes(String(selectedModel))) || null
    : null;
  return { dupe, message: dupe ? MODEL_ADD_DUPLICATE : '' };
}
