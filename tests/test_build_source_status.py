import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec=importlib.util.spec_from_file_location("source_status",Path(__file__).resolve().parents[1]/"build-service/source_status.py")
source_status=importlib.util.module_from_spec(spec);spec.loader.exec_module(source_status)

class SourceStatusTests(unittest.TestCase):
    def test_rename_and_bounds(self):
        rows,truncated=source_status.parse_status(b" M ui/a.js\0R  src/new.rs\0src/old.rs\0")
        self.assertEqual(len(rows),2);self.assertEqual(rows[1]["path"],"src/new.rs");self.assertFalse(truncated)
        rows,truncated=source_status.parse_status(b"?? ui/a.js\0"*101)
        self.assertEqual(len(rows),100);self.assertTrue(truncated)

    def test_actual_git_is_read_only_and_excludes_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            def git(*args):subprocess.run(["/usr/bin/git","-C",directory,*args],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            git("init");(root/"ui").mkdir();(root/"ui/a.js").write_text("fixture one\n")
            git("add","ui/a.js");index=(root/".git/index").read_bytes()
            (root/"ui/a.js").write_text("fixture two\n");(root/"data").mkdir();(root/"data/private.txt").write_text("excluded fixture")
            value=source_status.snapshot(root)
            self.assertTrue(value["available"]);self.assertEqual(value["changes"],[{"state":"AM","path":"ui/a.js"}])
            self.assertEqual(index,(root/".git/index").read_bytes())
            self.assertIs(source_status.snapshot(root),value)

    def test_missing_repo_and_symlink_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            self.assertFalse(source_status.snapshot(root)["available"])
            (root/"alias").symlink_to(root,target_is_directory=True)
            self.assertFalse(source_status.snapshot(root/"alias")["available"])

if __name__=="__main__":unittest.main()
