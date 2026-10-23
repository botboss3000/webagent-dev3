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
    def completed_holder_and_waiter(self):
        """Leave an older queued request after its actual holder has explicitly completed."""
        holder=self.acquire(self.begin("queue-holder"),["module:engine"])["task"];self.time+=1
        waiter=self.begin("queue-waiter");self.assertFalse(self.acquire(waiter,["module:engine"])["acquired"])
        holder=self.store.validate(holder["id"],holder["owner"],holder["revision"],holder["epoch"],self.evidence(holder),self.request())
        self.store.complete(holder["id"],holder["owner"],holder["revision"],holder["epoch"],self.request())
        return self.current(waiter)
    def test_empty_scope_orphan_does_not_keep_unacquired_fifo_reservation(self):
        waiter=self.completed_holder_and_waiter();self.time+=301;self.store.orphan();orphan=self.current(waiter)
        self.assertEqual(orphan["state"],"orphaned");self.assertEqual(orphan["scopes"],[]);self.assertEqual(orphan["requested"],["module:engine"])
        self.assertTrue(self.acquire(self.begin("new-after-orphan"),["module:engine"])["acquired"])
        self.assertEqual(self.current(waiter),orphan)
    def test_live_waiting_request_keeps_fifo_priority(self):
        waiter=self.completed_holder_and_waiter();self.time+=1
        result=self.acquire(self.begin("new-after-waiter"),["module:engine"])
        self.assertFalse(result["acquired"]);self.assertEqual(result["blockers"][0]["task_id"],waiter["id"])
    def test_orphaned_held_scopes_still_block_new_work(self):
        owner=self.acquire(self.begin("held-owner"),["module:engine","path:src"])["task"];self.time+=301;self.store.orphan()
        result=self.acquire(self.begin("new-after-held-orphan"),["module:engine","path:src/main.rs"])
        self.assertFalse(result["acquired"]);self.assertEqual(result["blockers"][0]["task_id"],owner["id"])
        self.assertEqual(self.current(owner)["scopes"],["module:engine","path:src"])
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
    def test_deleted_isolated_manifest_retains_plugin_scope_without_hiding_core_inputs(self):
        """Deletion uses the sealed baseline only for removed files and never overrides compiler dependencies."""
        import io,tarfile
        root=Path(self.root.name)/"v3";root.mkdir();package="plugins/abilities/Developer/removed"
        path=package+"/main.rs";baseline=root.parent/"baseline.tar"
        body=json.dumps({"dispatch":"isolated"}).encode()
        with tarfile.open(baseline,"w")as archive:
            member=tarfile.TarInfo("v3/"+package+"/plugin.json");member.size=len(body);archive.addfile(member,io.BytesIO(body))
        self.assertNotIn("module:engine",m.Components(root).scopes([path]))
        self.assertIn("module:engine",m.Components(root,[path]).scopes([path]))
        folder=root/package;folder.mkdir(parents=True);(folder/"main.rs").write_text("new source")
        self.assertIn("module:engine",m.Components(root).scopes([path]))
        (folder/"main.rs").unlink();(folder/"plugin.json").write_text('{"dispatch":"native"}')
        self.assertIn("module:engine",m.Components(root).scopes([path]))
        (folder/"plugin.json").unlink();baseline.rename(root.parent/"original.tar");baseline.symlink_to(root.parent/"original.tar")
        with self.assertRaises(PermissionError):m.Components(root).scopes([path])
    def test_symlink_database_rejected(self):
        p=Path(self.root.name)/"link";p.symlink_to(self.store.path)
        with self.assertRaises(PermissionError):m.Ownership(p)

class ReleaseClaimTests(unittest.TestCase):
 setUp=OwnershipTests.setUp
 tearDown=OwnershipTests.tearDown
 request=OwnershipTests.request
 begin=OwnershipTests.begin
 acquire=OwnershipTests.acquire
 current=OwnershipTests.current
 def test_partial_release_fences_old_epoch_preserves_other_claims_and_unblocks(self):
  a=self.acquire(self.begin("release-owner"),["module:engine","module:broker"])["task"]
  args=(a["id"],a["owner"],a["revision"],a["epoch"],["module:engine"],"Defer engine checks",self.request())
  r=self.store.release_scopes(*args);self.assertEqual(r,self.store.release_scopes(*args))
  self.assertEqual(r["scopes"],["module:broker"]);self.assertEqual(r["evidence"],{})
  self.assertTrue(self.acquire(self.begin("engine-writer"),["module:engine"])["acquired"])
  with self.assertRaises(PermissionError):self.store.assert_owned(a["id"],a["owner"],a["epoch"],["module:broker"])
 def test_release_requires_owner_exact_revision_and_held_scope(self):
  a=self.acquire(self.begin("release-validation"),["module:broker"])["task"]
  for who,rev,scopes in (("other",a["revision"],["module:broker"]),(a["owner"],0,["module:broker"]),(a["owner"],a["revision"],["module:engine"])):
   with self.assertRaises((ValueError,PermissionError)):self.store.release_scopes(a["id"],who,rev,a["epoch"],scopes,"Preserve draft",self.request())
  self.assertEqual(self.current(a),a)
 def test_release_all_retains_task_and_allows_reacquisition(self):
  a=self.acquire(self.begin("release-all"),["module:engine"])["task"]
  r=self.store.release_scopes(a["id"],a["owner"],a["revision"],a["epoch"],a["scopes"],"Preserve source for later",self.request())
  self.assertEqual(r["state"],"waiting");self.assertEqual(r["scopes"],[])
  self.assertTrue(self.acquire(r,["module:broker"])["acquired"])

class BrokerTestClassificationTests(unittest.TestCase):
 def test_index_and_ui_policy_tests_do_not_reserve_engine(self):
  with tempfile.TemporaryDirectory()as root:
   for p in ("tests/test_code_index_maintenance.py","tests/test_published_code_index.py","tests/test_index_queue.py","tests/test_ui_policy.py"):
    self.assertEqual(m.Components(root).scopes([p]),["module:broker","path:"+p])
    self.assertIn("module:engine",m.Components(root,[p]).scopes([p]))
 def test_unrelated_tests_keep_conservative_engine_scope(self):
  with tempfile.TemporaryDirectory()as root:
   self.assertIn("module:engine",m.Components(root).scopes(["tests/test_worker.py"]))

class UIPathTests(unittest.TestCase):
 def test_ui_files_are_independent_but_legacy_remains_broad(self):
  with tempfile.TemporaryDirectory()as root:
   a=m.Components(root,ui_file_reservations=True).scopes(["ui/chat/a.js"])
   b=m.Components(root,ui_file_reservations=True).scopes(["ui/chat/b.js"])
   self.assertFalse(set(a)&set(b))
   self.assertIn("module:ui:chat",m.Components(root).scopes(["ui/chat/a.js"]))
 def test_compiler_attestation_dominates_file_and_directory_claims(self):
  with tempfile.TemporaryDirectory()as root:
   c=m.Components(root,["ui/chat/embedded.js"],ui_file_reservations=True)
   self.assertIn("module:engine",c.scopes(["ui/chat"]))
   self.assertIn("module:engine",c.scopes(["ui/chat/embedded.js"]))
   self.assertNotIn("module:engine",c.scopes(["ui/chat/unrelated.js"]))

class LegacyUIReservationTests(unittest.TestCase):
 def test_legacy_module_still_excludes_new_disjoint_paths_in_that_component(self):
  self.assertTrue(m.scope_overlap("module:ui:chat","path:ui/chat/disjoint.js"))
  self.assertTrue(m.scope_overlap("path:ui/chat/new.js","module:ui:chat"))
  self.assertFalse(m.scope_overlap("module:ui:chat","path:ui/chat-other/a.js"))
  self.assertFalse(m.scope_overlap("path:ui/chat/a.js","path:ui/chat/b.js"))

class SubmissionBindingTests(unittest.TestCase):
    setUp=OwnershipTests.setUp
    tearDown=OwnershipTests.tearDown
    request=OwnershipTests.request
    begin=OwnershipTests.begin
    acquire=OwnershipTests.acquire
    current=OwnershipTests.current

    def test_submission_binding_is_atomic_receipted_and_epoch_fenced(self):
        value=self.acquire(self.begin("submitted-owner"),["module:engine"])["task"]
        args=(value["id"],value["owner"],value["revision"],value["epoch"],"a"*64,"submitted-first")
        result=self.store.submitted(*args)
        self.assertEqual(self.store.submitted(*args),result)
        self.assertEqual(result["evidence"]["submission"],
                         {"inputs_sha256":"a"*64,"epoch":value["epoch"],"generation":value["generation"]+1})
        with self.assertRaisesRegex(ValueError,"Request ID reused"):
            self.store.submitted(*args[:-2],"b"*64,args[-1])
        reopened=m.Ownership(self.store.path,lambda:self.time)
        self.assertEqual(reopened.get(value["id"])["evidence"],result["evidence"])
        released=self.store.release_scopes(result["id"],result["owner"],result["revision"],result["epoch"],result["scopes"],"fixture yield","submitted-release")
        self.assertEqual(released["evidence"],{})
        with self.assertRaisesRegex(ValueError,"Stale"):
            self.store.submitted(result["id"],result["owner"],released["revision"],result["epoch"],"a"*64,"submitted-stale")
        for invalid in ("", "short", None):
            with self.assertRaises(ValueError):
                self.store.submitted(released["id"],released["owner"],released["revision"],released["epoch"],invalid,"submitted-invalid")


class CargoModuleReservationTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)/"v3";self.root.mkdir()
        self.write("Cargo.toml",'[package]\nname="app"\nversion="0.1.0"\n[workspace]\nmembers=["crates/modules/models","crates/modules/diagnostics"]\n[dependencies]\nmodels={path="crates/modules/models"}\n')
        self.write("crates/modules/models/Cargo.toml",'[package]\nname="models"\nversion="0.1.0"\n')
        self.write("crates/modules/diagnostics/Cargo.toml",'[package]\nname="diagnostics"\nversion="0.1.0"\n')
        self.write("crates/modules/models/src/private.rs","fn helper() {}\n")
        self.write("crates/modules/diagnostics/src/private.rs","fn helper() {}\n")
    def tearDown(self):self.temp.cleanup()
    def write(self,path,text):
        p=self.root/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text)
    def scopes(self,path,patch=None):return m.Components(self.root).scopes([path],patch)
    def test_private_modules_acquire_independently_but_same_module_blocks(self):
        db=m.Ownership(Path(self.temp.name)/"claims.sqlite")
        def acquire(name,path):
            v=db.begin(identity(name),name,name,"begin-"+name)
            return db.acquire(v['id'],name,v['revision'],v['epoch'],self.scopes(path),"acquire-"+name)
        self.assertTrue(acquire('models','crates/modules/models/src/private.rs')['acquired'])
        self.assertTrue(acquire('diagnostics','crates/modules/diagnostics/src/private.rs')['acquired'])
        self.assertFalse(acquire('models-again','crates/modules/models/src/other.rs')['acquired'])
    def test_public_module_and_general_writer_can_acquire_concurrently(self):
        self.write('crates/modules/models/src/public.rs','pub fn value() -> u8 { 1 }\n')
        self.write('src/app.rs','fn app() {}\n')
        db=m.Ownership(Path(self.temp.name)/'claims.sqlite')
        for name,path in [('models','crates/modules/models/src/public.rs'),('general','src/app.rs')]:
            value=db.begin(identity(name),name,name,'begin-'+name)
            result=db.acquire(value['id'],name,value['revision'],value['epoch'],self.scopes(path),'claim-'+name)
            self.assertTrue(result['acquired'])
        self.assertEqual(m.Components(self.root).scopes(['crates/modules/models/src/public.rs','src/app.rs']),
            ['module:backend:models','module:general','path:crates/modules/models/src/public.rs','path:src/app.rs'])
    def test_patch_exports_in_other_files_do_not_contaminate_claims(self):
        self.assertEqual(self.scopes('crates/modules/models/src/private.rs','+pub struct Elsewhere;\n'),
            ['module:backend:models','path:crates/modules/models/src/private.rs'])
    def test_export_reserves_only_edited_file_owner(self):
        self.write('crates/modules/models/src/public.rs','pub struct Item;\n')
        scopes=self.scopes('crates/modules/models/src/public.rs')
        self.assertNotIn('module:general',scopes);self.assertIn('module:backend:models',scopes)
        self.assertNotIn('module:backend:diagnostics',scopes);self.assertNotIn('module:engine',scopes)
    def test_checked_patch_exports_do_not_reserve_unedited_consumers(self):
        scopes=self.scopes('crates/modules/models/src/private.rs','+pub struct Export;\n')
        self.assertNotIn('module:general',scopes)
        self.assertNotIn('module:general',self.scopes('crates/modules/models/src/private.rs'))
    def test_transitive_dependencies_do_not_expand_write_ownership(self):
        self.write('crates/middle/Cargo.toml','[package]\nname="middle"\nversion="0.1.0"\n[target.\'cfg(unix)\'.dependencies]\nmodels={path="../modules/models"}\n')
        self.write('Cargo.toml','[package]\nname="app"\nversion="0.1.0"\n[workspace.dependencies]\nmiddle={path="crates/middle"}\n[dependencies]\nmiddle={workspace=true}\n')
        scopes=self.scopes('crates/modules/models/Cargo.toml')
        self.assertNotIn('module:general',scopes);self.assertNotIn('module:crate:crates/middle',scopes)
    def test_baseline_owner_survives_removed_dependency_or_manifest(self):
        import io,tarfile
        with tarfile.open(self.root.parent/'baseline.tar','w')as archive:
            for p in self.root.rglob('Cargo.toml'):
                raw=p.read_bytes();item=tarfile.TarInfo('v3/'+str(p.relative_to(self.root)));item.size=len(raw);archive.addfile(item,io.BytesIO(raw))
        self.write('Cargo.toml','[package]\nname="app"\nversion="0.1.0"\n')
        (self.root/'crates/modules/models/Cargo.toml').unlink()
        self.assertEqual(self.scopes('crates/modules/models/Cargo.toml'),['module:backend:models','path:crates/modules/models/Cargo.toml'])
    def test_unknown_or_invalid_inventory_is_conservative(self):
        self.assertIn('module:engine',self.scopes('crates/unknown/src/lib.rs'))
        self.write('crates/modules/models/Cargo.toml','invalid toml = [')
        self.assertIn('module:engine',self.scopes('crates/modules/models/src/private.rs'))
    def test_legacy_engine_fences_new_scopes_in_both_directions(self):
        for scope in ['module:backend:models','module:general','module:crate:crates/helper']:
            self.assertTrue(m.scope_overlap('module:engine',scope));self.assertTrue(m.scope_overlap(scope,'module:engine'))
        self.assertFalse(m.scope_overlap('module:backend:models','module:backend:diagnostics'))
        self.assertFalse(m.scope_overlap('module:engine','module:broker'))
    def test_compiler_attestation_does_not_turn_private_rust_into_shared_contract(self):
        path='crates/modules/models/src/private.rs'
        self.assertNotIn('module:general',m.Components(self.root,[path]).scopes([path]))
        embedded='crates/modules/models/defaults.json';self.write(embedded,'{}')
        self.assertEqual(m.Components(self.root,[embedded]).scopes([embedded]),['module:backend:models','path:'+embedded])
    def test_baseline_public_surface_does_not_expand_write_ownership(self):
        import io,tarfile
        path='crates/modules/models/src/private.rs';raw=b'pub struct Previous;'
        with tarfile.open(self.root.parent/'baseline.tar','w')as archive:
            item=tarfile.TarInfo('v3/'+path);item.size=len(raw);archive.addfile(item,io.BytesIO(raw))
        self.assertNotIn('module:general',self.scopes(path))
    def test_directory_claim_covers_contained_owners_only(self):
        self.assertIn('module:backend:models',self.scopes('crates/modules'))
        self.assertIn('module:backend:diagnostics',self.scopes('crates/modules'))
        self.assertNotIn('module:general',self.scopes('crates/modules'))
    def test_existing_component_scope_survives_cargo_discovery(self):
        self.write('crates/boot-service/Cargo.toml','[package]\nname="boot"\nversion="0.1.0"\n')
        self.write('crates/boot-service/src/helper.rs','fn helper() {}')
        self.assertIn('module:engine-component:boot-supervisor',self.scopes('crates/boot-service/src/helper.rs'))
    def test_symlink_inventory_cannot_narrow_claims(self):
        p=self.root/'crates/modules/models/Cargo.toml';p.rename(p.with_suffix('.real'));p.symlink_to(p.with_suffix('.real').name)
        self.assertIn('module:engine',self.scopes('crates/modules/models/src/private.rs'))

    def test_general_owns_unsplit_application_source(self):
        self.write('src/chat.rs','fn chat() {}')
        self.assertEqual(self.scopes('src/chat.rs'),['module:general','path:src/chat.rs'])
    def test_unregistered_manifest_cannot_split_an_existing_owner(self):
        self.write('crates/modules/models/child/Cargo.toml','[package]\nname="child"\nversion="0.1.0"\n')
        self.write('crates/modules/models/child/src/helper.rs','fn helper() {}')
        self.assertIn('module:backend:models',self.scopes('crates/modules/models/child/src/helper.rs'))
        self.assertNotIn('module:crate:crates/modules/models/child',self.scopes('crates/modules/models/child/src/helper.rs'))
    def test_invalid_cargo_table_falls_back_to_global_without_blocking_repair(self):
        self.write('crates/modules/models/Cargo.toml','[package]\nname="models"\nversion="0.1.0"\ntarget=1\n')
        self.write('Cargo.toml','target=1\n[package]\nname="app"\nversion="0.1.0"\n[dependencies]\nmodels={path="crates/modules/models"}\n')
        self.assertIn('module:engine',self.scopes('crates/modules/models/Cargo.toml'))
