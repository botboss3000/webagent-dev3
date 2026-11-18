"""UI releases start testing only after a tested peer publishes; UI test output counts as progress."""
import threading
import types
import unittest
import uuid

import test_admin_build as fixtures

ui_publication = fixtures.host.ui_publication_module


def job(phase, status="running", wait=None, task="t"):
    value = {"job_id": str(uuid.uuid4()), "task_id": task, "status": status, "phase": phase}
    if wait is not None:
        value["publication_wait"] = wait
    return value


class TestedPeerTests(unittest.TestCase):
    def test_peer_publishing_or_awaiting_publication_after_tests_blocks(self):
        for peer in (job("switching_ui"), job("installing_prepared_assets"),
                     job("waiting_publication", wait={"resource": "deploy_lock", "purpose": "publication"})):
            mine = job("preparing_ui")
            found = ui_publication.tested_peer({p["job_id"]: p for p in (mine, peer)}, mine["job_id"])
            self.assertEqual(found["job_id"], peer["job_id"])

    def test_untested_finished_and_own_jobs_do_not_block(self):
        mine = job("switching_ui")
        others = [
            job("waiting_publication", wait={"purpose": "capture_publication_base"}),
            job("waiting_publication", wait={"purpose": "await_tested_candidate"}),
            job("waiting_publication", wait={"purpose": "publication", "state": "acquired"}),
            job("switching_ui", status="completed"),
            job("frontend_tests"),
        ]
        jobs = {p["job_id"]: p for p in (mine, *others)}
        self.assertIsNone(ui_publication.tested_peer(jobs, mine["job_id"]))


class AwaitTestedPeerTests(unittest.TestCase):
    def publication(self, jobs):
        broker = types.SimpleNamespace(lock=threading.RLock(), jobs=jobs, job_cancellations={}, stopping=False,
                                       _batch_lease=lambda job: None, _save=lambda job: None)
        return ui_publication.UIPublication(broker, None, None, None, None, None)

    def test_waits_until_the_peer_publishes(self):
        peer = job("switching_ui")
        mine = job("preparing_ui")
        publication = self.publication({peer["job_id"]: peer, mine["job_id"]: mine})
        timer = threading.Timer(.3, lambda: peer.update(status="completed"))
        timer.start()
        try:
            publication.await_tested_peer(mine)
        finally:
            timer.cancel()
        self.assertEqual(peer["status"], "completed")
        wait = mine["publication_wait"]
        self.assertEqual((wait["resource"], wait["purpose"], wait["state"]), ("tested_candidate", "await_tested_candidate", "acquired"))
        self.assertEqual(wait["reason"], "Waiting for a tested release to publish")

    def test_timeout_proceeds_instead_of_failing(self):
        peer = job("switching_ui")
        mine = job("preparing_ui")
        publication = self.publication({peer["job_id"]: peer, mine["job_id"]: mine})
        publication.wait_timeout = .05
        publication.await_tested_peer(mine)
        self.assertEqual(mine["publication_wait"]["state"], "timed_out")
        self.assertEqual(mine["status"], "running")

    def test_no_peer_records_no_wait(self):
        mine = job("preparing_ui")
        self.publication({mine["job_id"]: mine}).await_tested_peer(mine)
        self.assertNotIn("publication_wait", mine)


class UITestProgressTests(unittest.TestCase):
    def observer(self, kind):
        wall = [1000.0]
        runner = types.SimpleNamespace(secrets=[], job_context=threading.local(), resource_progress=lambda j: None)
        job_value = {"job_id": str(uuid.uuid4())}
        observer = fixtures.host.CommandObservation(runner, job_value, {"phase": "ui_test", "recipe": "r", "kind": kind},
                                                    123, lambda: 100.0, lambda: wall[0])
        return observer, job_value, wall

    def test_each_finished_node_test_is_progress(self):
        observer, job_value, wall = self.observer("ui_test")
        observer.feed("✔ opens the editor (12.5ms)\n".encode())
        wall[0] += 30
        observer.feed("✖ closes the editor (3ms)\n  nested detail\n".encode())
        observer.flush(final=True, outcome="failed")
        progress = job_value["execution_observation"]["test_progress"]
        self.assertEqual((progress["completed"], progress["failed"]), (2, 1))
        self.assertEqual(job_value["execution_observation"]["last_progress_at"], 1030.0)

    def test_node_lines_are_not_counted_for_rust_tests(self):
        observer, job_value, _ = self.observer("test")
        observer.feed("✔ opens the editor (12.5ms)\n".encode())
        observer.flush(final=True, outcome="completed")
        self.assertEqual(job_value["execution_observation"]["test_progress"]["completed"], 0)

    def test_ui_test_phase_is_the_ui_stage(self):
        self.assertEqual(fixtures.host.STAGE_OF["ui_test"], "UI build and tests")


if __name__ == "__main__":
    unittest.main()
