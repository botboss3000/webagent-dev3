"""validate_source_hunks recomputes hunk header counts from the patch body.

A miscounted header is corrected (the checked git apply in
inspect_source_patch remains the gate for context mismatches); malformed
headers and malformed body lines still raise.
"""
import importlib.util
import unittest
from pathlib import Path

PATH = Path(__file__).resolve().parents[1] / "plugins/abilities/Administrator/v3_build/host.py"
spec = importlib.util.spec_from_file_location("admin_build_hunks", PATH)
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)
validate = host.validate_source_hunks

OK = (
    "diff --git a/src/lib.rs b/src/lib.rs\n"
    "--- a/src/lib.rs\n"
    "+++ b/src/lib.rs\n"
    "@@ -1,3 +1,3 @@ fn main\n"
    " keep\n"
    "-old\n"
    "+new\n"
    " tail\n"
)
MISCOUNTED = OK.replace("@@ -1,3 +1,3 @@", "@@ -1,6 +1,9 @@")
TWO_HUNKS = OK + (
    "@@ -10,2 +10,2 @@ other\n"
    " ctx\n"
    "-a\n"
    "+b\n"
)
TWO_HUNKS_BAD = OK.replace("@@ -1,3 +1,3 @@", "@@ -1,1 +1,1 @@") + (
    "@@ -10,7 +10,7 @@ other\n"
    " ctx\n"
    "-a\n"
    "+b\n"
)
BAD_HEADER = OK.replace("@@ -1,3 +1,3 @@", "@@ -one +two @@")
BAD_BODY = OK.replace(" tail\n", "garbage\n")
NOEOL = (
    "diff --git a/x.txt b/x.txt\n"
    "--- a/x.txt\n"
    "+++ b/x.txt\n"
    "@@ -1,1 +1,1 @@\n"
    "-old\n"
    "\\ No newline at end of file\n"
    "+new\n"
    "\\ No newline at end of file\n"
)


class RecomputeHunkCounts(unittest.TestCase):
    def test_correct_counts_unchanged(self):
        self.assertEqual(validate(OK), OK)

    def test_miscounted_header_corrected(self):
        self.assertEqual(validate(MISCOUNTED), OK)

    def test_each_hunk_corrected_independently(self):
        self.assertEqual(validate(TWO_HUNKS_BAD), TWO_HUNKS)

    def test_malformed_header_raises(self):
        with self.assertRaises(ValueError):
            validate(BAD_HEADER)

    def test_malformed_body_line_raises(self):
        with self.assertRaises(ValueError):
            validate(BAD_BODY)

    def test_no_newline_marker_preserved(self):
        self.assertEqual(validate(NOEOL), NOEOL)


if __name__ == "__main__":
    unittest.main()
