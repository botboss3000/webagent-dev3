# web/: the new WebAgent UI

The replacement for `ui/`, built beside it and switched over page by page. Once every
page the app keeps has moved here, `ui/` is deleted.

- Served at `/web/...`, released like `ui/`: a change to `web/` alone publishes without
  a compile, and its `*.test.mjs`/`*.test.cjs` fixtures run in the UI test container.
- `web/admin/` is served only to a signed-in administrator, like
  `ui/main-panel/Administrator/`.
- Old code may import new code, never the other way: `web/` never imports from `/ui/`.
  Copy what a new page needs from `ui/shared/` into `web/` instead, so deleting `ui/`
  breaks nothing.
- Each new page carries its own styles and does not rely on `ui/shell/app.css`.
- To switch a page of the current app over, keep its `ui/main-panel/<page>/page.json`
  and make its entry module re-export the new page:
  `export {start} from '/web/<page>/page.js';`.
