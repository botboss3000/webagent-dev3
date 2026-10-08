# v3_source_apply

Apply one reviewed standard Git unified diff to an open isolated task workspace using its current `workspace_inputs_sha256` and a stable request ID. This tool accepts `patch` text only. For new files prepared by `v3_source_prepare`, use `v3_source_apply_prepared`. The broker checks paths, modes, links, the digest, and `git apply --check`. This does not edit the live release. After an uncertain response, inspect `v3_source_receipt` before any retry.

For new files, call `v3_source_prepare` with complete contents, inspect its returned `patch`, and pass its `prepared_id` to `v3_source_apply_prepared`.

For existing-file edits, the `patch` string must begin with `diff --git` and contain standard `---`, `+++`, and `@@` lines. Use paths relative to the task's V3 directory (for example, `a/ui/app.js`), without a leading `v3/` or absolute workspace path. For a new file, use `new file mode 100644` and `--- /dev/null`; the hunk's added-line count must match the actual added lines. For example:

```diff
diff --git a/ui/main-panel/example/page.json b/ui/main-panel/example/page.json
new file mode 100644
--- /dev/null
+++ b/ui/main-panel/example/page.json
@@ -0,0 +1,1 @@
+{"id":"example"}
```

To change an existing file, use `--- a/path`, `+++ b/path`, and an `@@ -old,count +new,count @@` hunk. A one-line replacement may use a zero-context hunk when its old text exactly matches the task snapshot; include nearby context when available. Separate multiple files with another `diff --git` header. End the patch with a newline.

## Replacing a whole small file (existing files)

For a small existing file, replace it in one whole-file hunk instead of composing several counted hunks:

1. Read the current file with `v3_source_read` and note its exact line count N.
2. Count the replacement text's lines exactly (M) — count the text you are sending, never estimate it.
3. Send one hunk `@@ -1,N +1,M @@` whose body is every old line prefixed with `-`, followed by every new line prefixed with `+`.

Counts copied from the actual old and new contents cannot mismatch. Example — replacing a 2-line file with a 3-line file (the read reported 2 lines; the replacement text has 3):

```diff
diff --git a/ui/main-panel/example/notes.txt b/ui/main-panel/example/notes.txt
--- a/ui/main-panel/example/notes.txt
+++ b/ui/main-panel/example/notes.txt
@@ -1,2 +1,3 @@
-old line one
-old line two
+new line one
+new line two
+new line three
```

A hunk-count rejection changes nothing and is always correctable: the broker error reports the declared versus actual counts for that file. Recompute the counts from those numbers and the actual file contents, and retry with a fresh request ID. Do not abandon the task over a hunk rejection.

Do not send `apply_patch` syntax such as `*** Begin Patch`, `*** Add File`, `*** Update File`, or `*** End Patch`. Those markers are not a Git diff and fail before a source receipt is created. If validation reports a hunk mismatch, use its file-specific declared and actual counts to correct that hunk, then obtain the current digest and use a fresh request ID.
