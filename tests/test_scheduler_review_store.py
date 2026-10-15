"""Partitioned review storage preserves validation, history and independent writes."""
import concurrent.futures
import contextlib
import importlib.util
import json
import sqlite3
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
import test_code_index_maintenance as fixtures
spec = importlib.util.spec_from_file_location("review_store", ROOT / "build-service/review_store.py")
store = importlib.util.module_from_spec(spec)
spec.loader.exec_module(store)
Legacy = fixtures.ledger.Ledger
Partitioned = store.ledger_class(fixtures.ledger)


class PartitionedRegressionTests(fixtures.MaintenanceTests):
    """Run existing authority, claims, freshness and projection contracts unchanged."""
    def setUp(self):
        self.engine_patch = patch.object(fixtures.ledger, "Ledger", Partitioned)
        self.engine_patch.start()
        try:
            super().setUp()
            self.db.partition_storage()
        except BaseException:
            self.engine_patch.stop()
            raise

    def tearDown(self):
        try:
            super().tearDown()
        finally:
            self.engine_patch.stop()


class StorageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.folder = Path(self.tmp.name) / "reviews"
        self.root = Path(self.tmp.name) / "source"
        self.root.mkdir()
        (self.root / "one.py").write_text("def first():\n    return 1\n")
        self.actor = {"user_id":"reviewer","session_id":"session"}
        self.db = Partitioned(self.folder)
        self.sequence = 0

    def tearDown(self):
        self.tmp.cleanup()

    def call(self, action="sync", key="task-a", **args):
        self.sequence += 1
        args.update(action=action, request_id=args.get("request_id", "review-request-" + str(self.sequence).zfill(6)))
        return self.db.mutate(args, self.actor, key, key, self.root)

    def child(self, key="task-a"):
        return next(r for r in self.db.list({"task_key":key})["results"] if r["unit_id"] != "__file__")

    def test_no_implicit_migration_and_exact_retry_after_explicit_migration(self):
        result = self.call(request_id="before-migration")
        row = self.child()
        claim = self.call("claim", entry_id=row["entry_id"], expected_revision=row["revision"], request_id="claim-before-migrate")
        before = self.db.snapshot(["task-a"])
        history = self.db.list({"action":"get","entry_id":row["entry_id"]})
        summary = self.db.partition_storage()
        self.assertTrue(summary["history_preserved"])
        self.assertTrue((self.folder / "ledger.sqlite").is_dir())
        self.db = Partitioned(self.folder)
        self.assertEqual(self.call(request_id="before-migration"), result)
        self.assertEqual(self.call("claim", entry_id=row["entry_id"], expected_revision=row["revision"], request_id="claim-before-migrate"), claim)
        self.assertEqual(self.db.snapshot(["task-a"]), before)
        self.assertEqual(self.db.list({"action":"get","entry_id":row["entry_id"]}), history)
        with self.assertRaises((ValueError, sqlite3.Error, IsADirectoryError)):
            Legacy(self.folder)
        self.assertEqual(self.db.partition_storage()["layout"], 1)

    def test_many_task_migration_has_a_linear_database_open_budget(self):
        for n in range(40):
            self.call(key="task-"+str(n).zfill(4))
        with patch.object(store,"connect_file",wraps=store.connect_file)as opened:
            result=self.db.partition_storage()
        self.assertLessEqual(opened.call_count,4*len(result["stores"])+4)
        for n in range(40):
            self.assertTrue(self.db.list({"task_key":"task-"+str(n).zfill(4)})["results"])

    def test_request_identity_cannot_move_to_another_task(self):
        self.call(request_id="shared-request-id")
        self.db.partition_storage()
        with self.assertRaisesRegex(ValueError, "reused"):
            self.call(key="task-b", request_id="shared-request-id")
        self.assertEqual(self.db.list({"task_key":"task-b"})["results"], [])

    def test_independent_task_writer_does_not_hold_coordinator(self):
        self.call()
        self.call(key="task-b")
        self.db.partition_storage()
        child = self.child("task-b")
        with self.db._using("task-a"), self.db.connect() as conn:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("UPDATE records SET body=body")
            def claim():
                other = Partitioned(self.folder)
                return other.mutate({"action":"claim","entry_id":child["entry_id"],"expected_revision":child["revision"],"request_id":"independent-claim"}, self.actor,"task-b","task-b",self.root)
            with concurrent.futures.ThreadPoolExecutor() as pool:
                result = pool.submit(claim).result(timeout=2)
                self.assertEqual(result["record"]["status"], "in_progress")

    def test_private_task_reads_do_not_wait_for_another_task_transaction(self):
        self.call()
        self.call(key="task-b")
        self.db.partition_storage()
        expected=self.db.list({"task_key":"task-b"})
        with self.db._using("task-a"), self.db.connect() as conn:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("UPDATE records SET body=body")
            other=Partitioned(self.folder)
            with concurrent.futures.ThreadPoolExecutor() as pool:
                result=pool.submit(other.list,{"task_key":"task-b"}).result(timeout=2)
                self.assertEqual(result,expected)

    def test_publication_can_commit_during_private_task_transaction(self):
        self.call()
        self.db.partition_storage()
        with self.db._using("task-a"), self.db.connect() as conn:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("UPDATE records SET body=body")
            with concurrent.futures.ThreadPoolExecutor() as pool:
                other = Partitioned(self.folder)
                pool.submit(other.activate,"publication-during-review",[]).result(timeout=2)
        self.assertEqual(self.db.active_release(),"publication-during-review")

    def test_global_snapshot_waits_for_shard_commit_then_is_consistent(self):
        self.call()
        self.db.partition_storage()
        entered = threading.Event()
        other = Partitioned(self.folder)
        def snapshot():
            entered.set()
            return other.snapshot(["task-a"])
        with concurrent.futures.ThreadPoolExecutor() as pool:
            with self.db._using("task-a"), self.db.connect() as conn:
                conn.execute("BEGIN IMMEDIATE")
                conn.execute("UPDATE records SET revision=revision+1")
                future = pool.submit(snapshot)
                self.assertTrue(entered.wait(1))
                self.assertFalse(future.done())
            rows = future.result(timeout=2)
        self.assertTrue(rows)
        self.assertTrue(all(r["task_key"] == "task-a" for r in rows))

    def test_aggregate_limit_rolls_back_review_and_allocation_atomically(self):
        self.call()
        self.db.partition_storage()
        row = self.child()
        with self.db._catalog() as db:
            before = [tuple(r) for r in db.execute("SELECT task_key,allocated FROM stores ORDER BY task_key")]
        self.db.max_bytes = self.db.path.stat().st_size + sum(r[1] for r in before)
        # A new global route may itself require another catalog page. Both
        # rejection locations must preserve the actual review record.
        try:
            with patch.object(self.db, "_save", side_effect=lambda conn, row, actor, event: Legacy._save(self.db, conn, {**row,"padding":"x"*65536}, actor, event)):
                with self.assertRaisesRegex(ValueError, "budget"):
                    self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])
        finally:
            self.db.max_bytes = 256 * 1024**2
        self.assertEqual(self.child()["revision"], row["revision"])
        with self.db._catalog() as db:
            self.assertEqual([tuple(r) for r in db.execute("SELECT task_key,allocated FROM stores ORDER BY task_key")], before)

    def test_migration_restart_recovers_every_switch_checkpoint(self):
        for phase in ("prepared","legacy_moved","layout_published"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as d:
                db = Partitioned(Path(d) / "reviews")
                expected = db.mutate({"action":"sync","request_id":"checkpoint-retry"},self.actor,"task-a","task-a",self.root)
                def crash(current):
                    if current == phase:
                        raise RuntimeError("simulated process loss")
                with self.assertRaisesRegex(RuntimeError,"simulated"):
                    db.partition_storage(checkpoint=crash)
                reopened = Partitioned(Path(d) / "reviews")
                self.assertEqual(reopened.mutate({"action":"sync","request_id":"checkpoint-retry"},self.actor,"task-a","task-a",self.root),expected)
                self.assertEqual(reopened.storage_status()["layout"],1)

    def test_changed_prepared_database_fails_closed(self):
        self.call()
        def crash(phase):
            raise RuntimeError("simulated")
        with self.assertRaises(RuntimeError):
            self.db.partition_storage(checkpoint=crash)
        layout=json.loads(self.db.pending_path.read_text())
        path=self.folder/layout["directory"]/"coordinator.sqlite"
        with path.open("ab") as f:
            f.write(b"corruption")
        with self.assertRaisesRegex(ValueError,"changed"):
            Partitioned(self.folder)
        self.assertTrue((self.folder/"ledger.sqlite").is_file())

    def test_revoked_preparation_retains_original_database(self):
        self.call()
        before=store.file_hash(self.db.path)
        count=[0]
        def authorize():
            count[0]+=1
            if count[0]>2:
                raise PermissionError("revoked")
        with self.assertRaises(PermissionError):
            self.db.partition_storage(authorize)
        self.assertEqual(store.file_hash(self.db.path),before)
        self.assertFalse(self.db.layout_path.exists())

    def test_symlinked_store_and_catalog_are_rejected(self):
        self.call()
        self.db.partition_storage()
        path=self.db._store_path("task-a")
        original=path.with_suffix(".saved")
        path.rename(original)
        path.symlink_to(original)
        with self.assertRaises(ValueError):
            self.db.snapshot(["task-a"])

    def test_history_only_entries_survive_migration(self):
        self.call()
        child=self.child()
        self.call("claim",entry_id=child["entry_id"],expected_revision=child["revision"])
        with self.db.connect() as db:
            db.execute("DELETE FROM records WHERE entry_id=?",(child["entry_id"],))
        self.db.partition_storage()
        with self.db._using("task-a"),self.db.connect() as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM events WHERE entry_id=?",(child["entry_id"],)).fetchone()[0],2)

if __name__ == "__main__":
    unittest.main(verbosity=2)
