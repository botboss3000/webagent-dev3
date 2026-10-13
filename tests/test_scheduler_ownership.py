"""Reservation acceptance using real broker metadata, never application databases."""
import importlib.util,json,tempfile,unittest,hashlib
from pathlib import Path
path=Path(__file__).resolve().parents[1]/"build-service/ownership.py"
spec=importlib.util.spec_from_file_location("ownership",path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
def identity(name):return hashlib.sha256(name.encode()).hexdigest()
class OwnershipTests(unittest.TestCase):
    def setUp(self):
        self.root=tempfile.TemporaryDirectory();self.time=1000
        self.store=m.Ownership(Path(self.root.name)/"ownership.sqlite",lambda:self.time);self.sequence=0
    def tearDown(self):self.root.cleanup()
    def request(self):
        self.sequence+=1;return "ownership-test-"+str(self.sequence)
    def begin(self,name,owner="agent"):
        return self.store.begin(identity(name),owner,name,self.request())
    def acquire(self,value,scopes):
        return self.store.acquire(value["id"],value["owner"],value["revision"],value["epoch"],scopes,self.request())
    def current(self,value):return self.store.get(value["id"])
    def evidence(self,value):
        return {"generation":value["generation"],"release_id":"release","inputs_sha256":"a"*64,"promoted_hashes":{"module.py":"b"*64},"checks":["test-receipt"],"acceptance":["acceptance-receipt"],"commit":"c"*40}
    def test_same_module_remains_blocked_until_agent_completion(self):
        """Release and acceptance do not themselves release ownership."""
        a=self.acquire(self.begin("owner"),["module:engine","path:src"])["task"];b=self.begin("waiting")
        self.assertFalse(self.acquire(b,["module:engine"])["acquired"])
        a=self.store.validate(a["id"],a["owner"],a["revision"],a["epoch"],self.evidence(a),self.request())
        self.assertFalse(self.acquire(self.current(b),["module:engine"])["acquired"])
        self.store.complete(a["id"],a["owner"],a["revision"],a["epoch"],self.request())
        self.assertTrue(self.acquire(self.current(b),["module:engine"])["acquired"])
    def test_unrelated_components_acquire_independently(self):
        for name in ["one","two"]:self.assertTrue(self.acquire(self.begin(name),["module:ability:Developer/"+name])["acquired"])
    def test_correction_invalidates_acceptance_without_releasing_scope(self):
        a=self.acquire(self.begin("one"),["module:engine"])["task"]
        a=self.store.validate(a["id"],a["owner"],a["revision"],a["epoch"],self.evidence(a),self.request())
        a=self.store.edited(a["id"],a["owner"],a["revision"],a["epoch"],self.request())
        self.assertEqual(a["state"],"owned");self.assertEqual(a["evidence"],{});self.assertEqual(a["generation"],1)
        with self.assertRaises(ValueError):self.store.complete(a["id"],a["owner"],a["revision"],a["epoch"],self.request())
    def test_atomic_administrator_handoff_fences_old_owner(self):
        a=self.acquire(self.begin("one"),["module:engine"])["task"];b=self.begin("followup","other")
        r=self.store.transfer(a["id"],"administrator",a["revision"],a["epoch"],b["id"],b["owner"],self.request(),True,"Owner stopped; no effects remain")
        self.assertEqual(r["task"]["scopes"],["module:engine"])
        with self.assertRaises(PermissionError):self.store.assert_owned(a["id"],a["owner"],a["epoch"],["module:engine"])
        self.store.assert_owned(b["id"],b["owner"],r["task"]["epoch"],["module:engine"])
    def test_handoff_requires_owner(self):
        a=self.acquire(self.begin("one"),["module:engine"])["task"];b=self.begin("followup")
        with self.assertRaises(PermissionError):self.store.transfer(a["id"],"imposter",a["revision"],a["epoch"],b["id"],b["owner"],self.request())
        self.assertEqual(self.current(a)["state"],"owned")
    def test_orphan_survives_restart_and_requires_explicit_recovery(self):
        a=self.acquire(self.begin("one"),["module:engine"])["task"];self.time+=301;self.store.orphan()
        reopened=m.Ownership(self.store.path,lambda:self.time);a=reopened.get(a["id"]);self.assertEqual(a["state"],"orphaned")
        with self.assertRaises(PermissionError):reopened.assert_owned(a["id"],a["owner"],a["epoch"],["module:engine"])
        self.assertFalse(self.acquire(self.begin("other"),["module:engine"])["acquired"])
        a=self.store.heartbeat(a["id"],a["owner"],a["revision"],a["epoch"],self.request());self.assertEqual(a["state"],"owned")
    def test_expansion_is_atomic_and_detects_cycle(self):
        a=self.acquire(self.begin("one"),["module:one"])["task"];b=self.acquire(self.begin("two"),["module:two"])["task"]
        r=self.acquire(a,["module:two"]);self.assertFalse(r["acquired"]);self.assertEqual(r["task"]["scopes"],["module:one"])
        with self.assertRaisesRegex(ValueError,"cycle"):self.acquire(b,["module:one"])
    def test_three_owner_cycle_is_rejected_atomically(self):
        """A longer dependency cycle cannot pin all writers indefinitely."""
        a=self.acquire(self.begin("cycle-a"),["module:a"])["task"]
        b=self.acquire(self.begin("cycle-b"),["module:b"])["task"]
        c=self.acquire(self.begin("cycle-c"),["module:c"])["task"]
        self.assertFalse(self.acquire(a,["module:b"])["acquired"])
        self.assertFalse(self.acquire(b,["module:c"])["acquired"])
        with self.assertRaisesRegex(ValueError,"cycle"):self.acquire(c,["module:a"])
        self.assertEqual(self.current(c)["requested"],[])
    def test_directory_overlap_cycle_is_rejected(self):
        """Wait edges follow directory ancestry rather than exact path equality."""
        a=self.acquire(self.begin("path-a"),["path:src/a"])["task"]
        b=self.acquire(self.begin("path-b"),["path:src/b"])["task"]
        self.assertFalse(self.acquire(a,["path:src/b/child"])["acquired"])
        with self.assertRaisesRegex(ValueError,"cycle"):self.acquire(b,["path:src/a/child"])
    def test_waiter_does_not_block_current_owner_corrections(self):
        waiting=self.begin("older");active=self.acquire(self.begin("active"),["module:one","path:module/a"])["task"]
        self.assertFalse(self.acquire(waiting,["module:one","path:module"])["acquired"])
        self.assertTrue(self.acquire(active,["module:one","path:module/b"])["acquired"])
    def test_retries_are_exactly_once(self):
        a=self.begin("one");req=self.request();args=(a["id"],a["owner"],a["revision"],a["epoch"],["module:one"],req)
        self.assertEqual(self.store.acquire(*args),self.store.acquire(*args))
        with self.assertRaises(ValueError):self.store.acquire(a["id"],a["owner"],a["revision"],a["epoch"],["module:two"],req)
    def test_directory_scope_does_not_include_prefix_siblings(self):
        a=self.acquire(self.begin("one"),["path:src/agent"])["task"]
        self.store.assert_owned(a["id"],a["owner"],a["epoch"],["path:src/agent/loop.rs"])
        with self.assertRaises(PermissionError):self.store.assert_owned(a["id"],a["owner"],a["epoch"],["path:src/agent-other/x"])
    def test_traversal_and_state_are_rejected(self):
        for p in ["data/config/x","../src","src/../data","target/bin","src//main"]:
            with self.assertRaises(ValueError):m.source_path(p)
    def test_manifest_cannot_hide_compiled_input(self):
        root=Path(self.root.name);folder=root/"plugins/abilities/Developer/test";folder.mkdir(parents=True)
        (folder/"plugin.json").write_text(json.dumps({"dispatch":"isolated"}));p="plugins/abilities/Developer/test/src/main.rs"
        self.assertNotIn("module:engine",m.Components(root).scopes([p]));self.assertIn("module:engine",m.Components(root,[p]).scopes([p]))
    def test_symlink_database_rejected(self):
        p=Path(self.root.name)/"link";p.symlink_to(self.store.path)
        with self.assertRaises(PermissionError):m.Ownership(p)
