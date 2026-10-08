# v3_source_read

Read bounded lines from one existing file in an open isolated task's source snapshot. For example, `path="ui/main-panel/builds/page.json"` reads a page descriptor. Use `v3_source_list(path="ui/main-panel")` to discover files first; `ui` and `ui/main-panel` are directories, not readable files. A missing path means inspect the list and choose a returned file, not that the broker task is unseeded.

Broker task source ≠ session workspace — see the V3 Build skill. The returned digest can be used for checked source apply. This tool does not build or deploy.

Before authoring or editing a main-panel page, read `docs/ui-pages.md`: it states the `page.json` contract and the HTML-entry and JS-entry page shapes, so exemplar pages do not need to be read to learn the conventions.
