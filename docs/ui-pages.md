# Main-panel pages — authoring conventions

A main-panel page is a folder `ui/main-panel/<id>/` containing a `page.json`. Nothing else registers it: the catalog is derived from these folders by `src/api/ui_pages.rs` and served at `GET /api/v3/pages/catalog`; `ui/app.js` builds the tab, loads the page CSS, and mounts the page from the catalog. Read this file before authoring a page — exemplar pages do not need to be read to learn these conventions.

## page.json (required)

- `id`: must equal the folder name; ASCII letters, digits, `_`, `-` only. Folders whose name starts with `_` are skipped, and symlinks are rejected.
- `label`: non-empty string; shown as the tab name.
- `order`: integer, default 0. Pages sort by `order`, then `label`.
- `html`: optional filename of an HTML fragment in the folder.
- `entry`: optional filename of a JS module in the folder.
- `css`: optional array of CSS filenames in the folder.

Asset filenames may contain only ASCII letters, digits, `.`, `_`, `-`, and each must be a real file inside the folder, or the catalog request fails.

## HTML-entry page

page.json: `{"id":"my-page","label":"My Page","order":4,"html":"my-page.html","css":["my-page.css"]}`

The HTML file is a fragment, not a document (no `<html>` or `<head>`): the shell fetches it and sets it as the mount element's innerHTML. Start from a page-scoped root element, e.g. `<section class="my-page-page">...</section>`.

## JS-entry page

page.json: `{"id":"my-page","label":"My Page","order":4,"entry":"my-page.js","css":["my-page.css"]}`

The shell dynamic-imports the module and calls its exported `start(mount)` function; render into the given mount element. `html` and `entry` may be combined: the fragment is mounted first, then `start(mount)` runs.

## CSS

List each stylesheet in `css`. Scope selectors under a page-specific prefix (for example `.my-page-...`) and use the shared design-system variables (for example `var(--bg-1)`, `var(--fg-3)`, `var(--accent)`, `var(--border)`) instead of defining a private palette.

## Releasing

Add or edit the page folder inside an isolated v3_build task; changes under `ui/` release as a UI release. Afterwards verify the page appears in `GET /api/v3/pages/catalog`.
