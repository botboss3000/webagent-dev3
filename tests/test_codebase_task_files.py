import hashlib
import json
from pathlib import Path
import unittest
import test_admin_build as fixtures
from test_admin_build import context

class TaskFileTests(unittest.TestCase):
    setUp = fixtures.BuildTests.setUp
    tearDown = fixtures.BuildTests.tearDown
    call = fixtures.BuildTests.call

    def setUp(self):
        fixtures.BuildTests.setUp(self)
        self.runner.blob = context(extra={"v3/docs/example.md": "alpha\nbeta\ngamma\n", "v3/src/sample.rs": "fn main() {}\n"})

    def read(self, task="task-files-01", path="docs/example.md"):
        return self.call("file_read", {"task_id":task,"path":path})

    def edit(self, read, request="task-edit-01", **extra):
        return self.call("file_edit", {"task_id":read["task_id"],"path":read["path"],
            "old_text":"alpha\nbeta", "new_text":"changed\nsecond",
            "request_id":request, "expected_inputs_sha256":read["workspace_inputs_sha256"], **extra})

    def test_read_autocreates_and_multiline_edit_is_isolated(self):
        read=self.read()
        self.assertTrue(read["task_created"])
        result=self.edit(read)
        self.assertEqual(result["status"],"success")
        self.assertEqual(self.read()["lines"],["changed","second","gamma"])
        self.assertNotIn(b"changed",self.runner.blob)
        self.assertEqual(self.broker.jobs,{})
        self.assertEqual(self.runner.calls,[])

    def test_write_autocreates_without_begin_and_supports_no_final_newline(self):
        result=self.call("file_write", {"task_id":"write-files-01","path":"docs/new.md",
            "content":"new file", "request_id":"write-request-01"})
        self.assertTrue(result["task_created"])
        read=self.read("write-files-01","docs/new.md")
        self.assertEqual(read["lines"],["new file"])
        result=self.call("file_write", {"task_id":read["task_id"],"path":read["path"],
            "content":"replacement\n", "request_id":"write-request-02",
            "expected_inputs_sha256":read["workspace_inputs_sha256"]})
        self.assertEqual(result["status"],"success")
        self.assertEqual(self.read(read["task_id"],read["path"])["lines"],["replacement"])

    def test_edit_autocreates_from_known_exact_text(self):
        result=self.call("file_edit", {"task_id":"auto-edit-01","path":"docs/example.md",
            "old_text":"beta", "new_text":"edited", "request_id":"auto-request-01"})
        self.assertTrue(result["task_created"])
        self.assertEqual(self.read("auto-edit-01")["lines"],["alpha","edited","gamma"])

    def test_stale_digest_and_missing_digest_are_rejected(self):
        read=self.read();self.edit(read)
        with self.assertRaisesRegex(ValueError,"stale task"):
            self.edit(read,request="different-request")
        with self.assertRaisesRegex(ValueError,"expected_inputs_sha256"):
            self.call("file_write", {"task_id":read["task_id"],"path":"docs/another.md",
                "content":"x", "request_id":"missing-digest"})

    def test_exact_retry_returns_receipt_and_changed_retry_is_rejected(self):
        read=self.read();first=self.edit(read);again=self.edit(read)
        self.assertEqual(first,again)
        with self.assertRaisesRegex(ValueError,"reused"):
            self.edit(read,new_text="different")
        receipt=self.call("build_work", {"action":"receipt","task_id":read["task_id"],"request_id":"task-edit-01"})
        self.assertEqual(receipt["status"],"success")

    def test_paths_binary_and_symlink_are_rejected(self):
        for path in ["data/user_data/admin/admin.db","data/agent_data/shared_default/shared_default.db",
                     "data/encryption.key","../outside","/live-checkout/src/main.rs"]:
            with self.assertRaises(ValueError):
                self.call("file_write", {"task_id":"invalid-path-01","path":path,"content":"x","request_id":"invalid-request"})
        self.assertEqual(self.broker.work,{})
        read=self.read();item=next(iter(self.broker.work.values()))
        folder=Path(item["workspace_path"])
        (folder/"docs/link.md").symlink_to(folder/"docs/example.md")
        with self.assertRaisesRegex(ValueError,"symlink"):
            self.call("file_write", {"task_id":read["task_id"],"path":"docs/link.md","content":"x","request_id":"symlink-request",
                "expected_inputs_sha256":read["workspace_inputs_sha256"]})

    def test_abandoned_and_closed_tasks_do_not_reopen(self):
        self.read();self.call("build_work", {"action":"abandon","task_id":"task-files-01"})
        with self.assertRaisesRegex(ValueError,"no longer open"):
            self.read()
        self.assertEqual(len(self.broker.work),1)
        next(iter(self.broker.work.values()))["status"]="closed"
        with self.assertRaisesRegex(ValueError,"no longer open"):
            self.read()

    def test_search_autocreates_and_review_returns_generated_diff(self):
        result=self.call("file_search", {"task_id":"search-task-01","query":"fn main"})
        self.assertTrue(result["task_created"])
        self.assertEqual(result["matches"][0]["path"],"src/sample.rs")
        read=self.read("search-task-01");self.edit(read)
        review=self.call("build_work", {"action":"review","task_id":"search-task-01"})
        self.assertIn("v3/docs/example.md",review["paths"])
        self.assertIn("+changed",review["diff"])
        self.assertEqual(review["workspace_inputs_sha256"],self.read("search-task-01")["workspace_inputs_sha256"])

    def test_sessions_do_not_share_tasks(self):
        import uuid
        self.read()
        other=str(uuid.uuid4())
        result=self.call("file_read", {"task_id":"task-files-01","path":"docs/example.md"},
            trusted={**self.trusted,"session_id":other},workspace=other)
        self.assertTrue(result["task_created"])
        self.assertEqual(len(self.broker.work),2)

    def test_file_hash_and_ambiguous_match_are_checked(self):
        read=self.read()
        with self.assertRaisesRegex(ValueError,"changed since inspection"):
            self.edit(read,expected_sha256="0"*64)
        with self.assertRaisesRegex(ValueError,"match exactly once"):
            self.edit(read,old_text="a")


    def test_empty_file_and_noop_write(self):
        result=self.call("file_write", {"task_id":"empty-task-01","path":"docs/empty.md","content":"","request_id":"empty-request-01"})
        self.assertEqual(result["status"],"success")
        read=self.read("empty-task-01","docs/empty.md")
        self.assertEqual(read["lines"],[])
        result=self.call("file_write", {"task_id":read["task_id"],"path":read["path"],"content":"",
            "request_id":"empty-request-02","expected_inputs_sha256":read["workspace_inputs_sha256"]})
        self.assertTrue(result["unchanged"])

    def test_binary_text_guard_and_wrong_task_id(self):
        self.read();item=next(iter(self.broker.work.values()))
        folder=Path(item["workspace_path"])
        (folder/"docs/binary.md").write_bytes(b"binary\0content")
        with self.assertRaisesRegex(ValueError,"text bound"):
            self.read(path="docs/binary.md")
        with self.assertRaisesRegex(ValueError,"task_id"):
            self.read(task="bad")

if __name__ == "__main__": unittest.main()
