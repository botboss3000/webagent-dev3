"""Queued source applies: a grant moves a waiting patch onto current source when it still fits.

The broker stores a blocked apply's patch on the waiting task, dry-runs it after each release and
notes the task on the blocker board when it stops fitting. At the grant it applies the patch to the
refreshed tree only on a clean context match, else refuses with the claims held and the paths named.
"""
import difflib,subprocess,time,unittest
from pathlib import Path
from unittest.mock import patch
import test_admin_build as fixtures
import test_scheduler_broker as base

SHARED="".join(f"line {i}\n" for i in range(1,41))

def edited(text,number,value):
    lines=text.splitlines(True);lines[number-1]=value+"\n";return "".join(lines)

def diff(path,before,after,new=False,mode=None):
    """A Git-style unified diff with three context lines, as a session sends it."""
    head=f"diff --git a/{path} b/{path}\n"+("new file mode 100644\n"if new else"")+(f"old mode 100644\nnew mode {mode}\n"if mode else"")
    return head+"".join(difflib.unified_diff(before.splitlines(True),after.splitlines(True),"/dev/null"if new else"a/"+path,"b/"+path))

class QueueRebaseTests(unittest.TestCase):
    setUp=base.SchedulerBrokerTests.setUp;tearDown=base.SchedulerBrokerTests.tearDown
    call=base.SchedulerBrokerTests.call;begin=base.SchedulerBrokerTests.begin
    write=base.SchedulerBrokerTests.write;board=base.SchedulerBrokerTests.board
    def prepare(self):
        self.runner.blob=fixtures.context(extra={"v3/README.md":"original\n","v3/docs/shared.md":SHARED})
    def tree(self,item):return Path(item["workspace_path"]).parent
    def digest(self,item):return self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(self.tree(item)))
    def read(self,item,path):return (self.tree(item)/"v3"/path).read_text()
    def apply(self,item,text,request,expected=None):
        return self.call("v3_source_apply",{"task_id":item["task_id"],"request_id":request,"patch":text,
            "expected_inputs_sha256":expected or self.digest(item)})
    def queued(self,item,text,request):
        """Apply while another task holds the file: the request queues and stores its patch."""
        expected=self.digest(item)
        with self.assertRaises(fixtures.host.scheduler_module.Conflict)as blocked:self.apply(item,text,request,expected)
        self.assertEqual(blocked.exception.details["queued_patch"]["request_id"],request)
        return expected,blocked.exception.details
    def release(self,first,free=True):
        """first's tree becomes the published source; free drops its claims as a completed release does."""
        self.runner.blob=self.broker.scheduler_task_snapshot(self.tree(first))
        if free:
            first["status"]="abandoned";self.broker.scheduler.store.retire(first["key"],"Fixture release",abandon=True)
    def claims(self,item):return self.broker.scheduler.store.get(item["key"])["scopes"]

    def test_unchanged_base_applies_without_rebase(self):
        self.prepare();first=self.begin("rb-first-a");waiter=self.begin("rb-waiter-a")
        self.write(first,"docs/shared.md",edited(SHARED,2,"first"))
        expected,_=self.queued(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-a")
        first["status"]="abandoned";self.broker.scheduler.store.retire(first["key"],"Dropped unreleased",abandon=True)
        result=self.apply(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-a",expected)
        self.assertEqual(result["status"],"success");self.assertNotIn("rebased",result)
        self.assertNotIn("queued_patch",waiter)

    def test_unrelated_hunk_is_rebased_and_receipted(self):
        self.prepare();first=self.begin("rb-first-b");waiter=self.begin("rb-waiter-b")
        self.write(first,"docs/shared.md",edited(SHARED,2,"first"))
        text=diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter"))
        expected,_=self.queued(waiter,text,"rb-wait-b")
        # The same request replayed before the grant queues again without a receipt.
        with self.assertRaises(fixtures.host.scheduler_module.Conflict):self.apply(waiter,text,"rb-wait-b",expected)
        self.assertNotIn("rb-wait-b",waiter.get("applies",{}))
        self.release(first)
        live=self.root/"live";(live/"docs").mkdir(parents=True);(live/"docs/shared.md").write_text(edited(SHARED,2,"first"))
        def git(*args):subprocess.run(["git","-C",str(live),*args],capture_output=True,check=True)
        git("init","-q");git("config","user.email","fixture@example.invalid");git("config","user.name","Fixture")
        git("add","docs/shared.md");git("commit","-qm","Upstream fixture change")
        self.runner.source=live
        result=self.apply(waiter,text,"rb-wait-b",expected)
        self.assertEqual([c.split(" ",1)[1]for c in result["base_commits"]],["Upstream fixture change"])
        self.assertEqual((result["status"],result["rebased"],result["base_changed_paths"]),("success",True,["docs/shared.md"]))
        self.assertIn("different base",result["check_evidence"])
        self.assertEqual(self.read(waiter,"docs/shared.md"),edited(edited(SHARED,2,"first"),30,"waiter"))
        self.assertIn("path:docs/shared.md",self.claims(waiter))
        # The same request after the grant returns the first receipt.
        self.assertEqual(self.apply(waiter,text,"rb-wait-b",expected),result)
        self.assertEqual(waiter["rebased"]["request_id"],"rb-wait-b")
        # A second apply on the rebased task is judged against the current digest only.
        second=diff("docs/shared.md",self.read(waiter,"docs/shared.md"),edited(self.read(waiter,"docs/shared.md"),35,"again"))
        with self.assertRaisesRegex(ValueError,"stale source inputs"):self.apply(waiter,second,"rb-second-b",expected)
        self.assertEqual(self.apply(waiter,second,"rb-second-b")["status"],"success")
        self.assertNotIn("rebased",waiter["applies"]["rb-second-b"])

    def test_same_hunk_is_refused_with_paths_named_and_claims_held(self):
        self.prepare();first=self.begin("rb-first-c");waiter=self.begin("rb-waiter-c")
        self.write(first,"docs/shared.md",edited(SHARED,30,"first"))
        expected,_=self.queued(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-c")
        self.release(first)
        with self.assertRaisesRegex(ValueError,r"Queued task refreshed to current source; the patch no longer fits: docs/shared.md \(changed upstream"):
            self.apply(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-c",expected)
        self.assertIn("path:docs/shared.md",self.claims(waiter))
        self.assertEqual(self.read(waiter,"docs/shared.md"),edited(SHARED,30,"first"))
        self.assertNotIn("queued_patch",waiter)
        # The refused request is not receipted; replaying its old digest is now stale.
        with self.assertRaisesRegex(ValueError,"stale source inputs"):
            self.apply(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-c",expected)
        current=self.read(waiter,"docs/shared.md")
        fixed=self.apply(waiter,diff("docs/shared.md",current,edited(current,30,"both")),"rb-fixed-c")
        self.assertEqual(fixed["status"],"success");self.assertNotIn("rebased",fixed)

    def test_claims_kept_after_a_refusal_drop_when_the_heartbeat_lapses(self):
        """A dead session cannot hold refused claims forever: the orphan rules release them."""
        self.prepare();first=self.begin("rb-first-d");waiter=self.begin("rb-waiter-d")
        self.write(first,"docs/shared.md",edited(SHARED,30,"first"))
        expected,_=self.queued(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-d")
        self.release(first)
        with self.assertRaisesRegex(ValueError,"no longer fits"):
            self.apply(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-d",expected)
        scheduler=self.broker.scheduler;self.assertTrue(self.claims(waiter))
        later=time.time()+scheduler.orphan_release+max(scheduler.orphan_after,1800)+60
        with patch.object(scheduler.store,"clock",lambda:later):
            scheduler.housekeeping();scheduler.housekeeping()
        self.assertEqual(self.claims(waiter),[])

    def test_refuses_rather_than_guesses(self):
        cases={
            "deleted":(lambda f:self.call("file_write",{"task_id":f["task_id"],"path":"docs/shared.md","delete":True,
                        "expected_inputs_sha256":self.digest(f),"request_id":"rb-delete-up"}),
                       lambda:diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"deleted or renamed upstream"),
            "exists":(lambda f:self.write(f,"docs/new.md","theirs\n"),
                      lambda:diff("docs/new.md","","mine\n",new=True),"added upstream"),
            "mode":(lambda f:self.write(f,"docs/shared.md",edited(SHARED,2,"first")),
                    lambda:diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter"),mode="100755"),"file mode changed"),
            "mixed":(lambda f:self.write(f,"docs/shared.md",edited(SHARED,2,"first")),
                     lambda:diff("build-service/fixture_tool.py","","x = 1\n",new=True)+diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),
                     "build-service/ mixed with other files"),
        }
        for name,(upstream,make,reason)in cases.items():
            with self.subTest(name):
                self.prepare();first=self.begin("rb-first-"+name);waiter=self.begin("rb-waiter-"+name)
                upstream(first)
                expected,_=self.queued(waiter,make(),"rb-wait-"+name)
                self.release(first)
                with self.assertRaisesRegex(ValueError,"no longer fits: .*"+reason):
                    self.apply(waiter,make(),"rb-wait-"+name,expected)
                self.assertTrue(self.claims(waiter))
                waiter["status"]="abandoned";self.broker.scheduler.store.retire(waiter["key"],"Fixture done",abandon=True)

    def test_prepared_path_is_rebased_too(self):
        self.prepare();first=self.begin("rb-first-p");waiter=self.begin("rb-waiter-p")
        self.write(first,"docs/fresh.md","first\n");self.write(first,"README.md","released\n")
        tree=self.tree(first);(tree/"v3/docs/fresh.md").unlink()  # first holds the claim but publishes only README.md
        expected=self.digest(waiter)
        prepared=self.call("v3_source_prepare",{"task_id":waiter["task_id"],"files":[{"path":"docs/fresh.md","content":"mine\n"}],
            "expected_inputs_sha256":expected})
        args={"task_id":waiter["task_id"],"request_id":"rb-prepared","prepared_id":prepared["prepared_id"],"expected_inputs_sha256":expected}
        with self.assertRaises(fixtures.host.scheduler_module.Conflict):self.call("v3_source_apply_prepared",args)
        self.release(first)
        result=self.call("v3_source_apply_prepared",args)
        self.assertEqual((result["status"],result["rebased"],result["base_changed_paths"]),("success",True,[]))
        self.assertEqual((self.read(waiter,"docs/fresh.md"),self.read(waiter,"README.md")),("mine\n","released\n"))

    def test_stored_request_alone_may_use_its_pre_refresh_digest_once(self):
        """Another call took the grant: only the exact stored request applies with the old digest."""
        self.prepare();first=self.begin("rb-first-s");waiter=self.begin("rb-waiter-s")
        self.write(first,"docs/shared.md",edited(SHARED,2,"first"))
        text=diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter"))
        expected,_=self.queued(waiter,text,"rb-wait-s")
        self.release(first)
        value=self.broker.scheduler.store.get(waiter["key"])
        granted=self.call("build_work",{"action":"reserve","task_id":waiter["task_id"],"paths":["docs/shared.md"],
            "request_id":"rb-reserve-s","expected_revision":value["revision"],"expected_epoch":value["epoch"]})
        self.assertTrue(granted["acquired"])
        other=diff("docs/shared.md",SHARED,edited(SHARED,31,"other"))
        with self.assertRaisesRegex(ValueError,"stale source inputs"):self.apply(waiter,other,"rb-other-s",expected)
        result=self.apply(waiter,text,"rb-wait-s",expected)
        self.assertEqual((result["status"],result["rebased"]),("success",True))
        self.assertNotIn("queued_patch",waiter)
        with self.assertRaisesRegex(ValueError,"stale source inputs"):self.apply(waiter,other,"rb-other2-s",expected)

    def test_dry_run_after_release_notes_the_waiter_and_a_replacement_keeps_its_place(self):
        self.prepare();self.board()
        first=self.begin("rb-first-n");waiter=self.begin("rb-waiter-n");later=self.begin("rb-later-n")
        self.write(first,"docs/shared.md",edited(SHARED,30,"first"))
        expected,details=self.queued(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-n")
        self.assertTrue(details["queued_patch"]["fit"]["fits"])
        self.queued(later,diff("docs/shared.md",SHARED,edited(SHARED,5,"later")),"rb-later-n")
        # first's release is published but it still holds its claims (commit pending).
        self.release(first,free=False);self.broker.queued_patch_fits()
        fit=waiter["queued_patch"]["fit"]
        self.assertEqual((fit["fits"],sorted(fit["reasons"])),(False,["docs/shared.md"]))
        self.assertTrue(later["queued_patch"]["fit"]["fits"])
        notes=self.broker.scheduler.board.notes("rb-waiter-n")["notes"]
        self.assertEqual(len(notes),1);self.assertIn("docs/shared.md",notes[0]["text"]);self.assertIn("keeps your place",notes[0]["text"])
        self.assertEqual(self.broker.scheduler.board.notes("rb-later-n")["notes"],[])
        self.broker.queued_patch_fits()  # Once per release: no second note.
        self.assertEqual(len(self.broker.scheduler.board.notes("rb-waiter-n")["notes"]),1)
        # Releasing task's preflight names the waiting tasks it will move.
        published=edited(SHARED,30,"first")
        replacement=diff("docs/shared.md",published,edited(published,30,"first and waiter"))
        created=self.broker.scheduler.store.get(waiter["key"])["created"]
        _,details=self.queued(waiter,replacement,"rb-replace-n")
        self.assertTrue(details["queued_patch"]["fit"]["fits"])
        self.assertEqual(self.broker.scheduler.store.get(waiter["key"])["created"],created)
        self.assertEqual(waiter["queued_patch"]["request_id"],"rb-replace-n")
        first["status"]="abandoned";self.broker.scheduler.store.retire(first["key"],"Fixture release",abandon=True)
        # The waiter is ahead of later in the queue: later still waits behind it.
        with self.assertRaises(fixtures.host.scheduler_module.Conflict):self.apply(later,diff("docs/shared.md",SHARED,edited(SHARED,5,"later")),"rb-later-n")
        result=self.apply(waiter,replacement,"rb-replace-n",expected)
        self.assertEqual((result["status"],result["rebased"],result["base_changed_paths"]),("success",True,[]))
        self.assertEqual(self.read(waiter,"docs/shared.md"),edited(SHARED,30,"first and waiter"))

    def test_stale_digest_matching_nothing_is_refused(self):
        self.prepare();first=self.begin("rb-first-x");waiter=self.begin("rb-waiter-x")
        self.write(first,"docs/shared.md",edited(SHARED,2,"first"))
        text=diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter"))
        self.queued(waiter,text,"rb-wait-x")
        self.release(first)
        with self.assertRaisesRegex(ValueError,"stale source inputs"):self.apply(waiter,text,"rb-wait-x","0"*64)

    def test_preflight_lists_waiting_tasks_this_release_moves(self):
        self.prepare();first=self.begin("rb-first-w");waiter=self.begin("rb-waiter-w")
        self.write(first,"docs/shared.md",edited(SHARED,2,"first"))
        self.queued(waiter,diff("docs/shared.md",SHARED,edited(SHARED,30,"waiter")),"rb-wait-w")
        report=self.call("build_work",{"action":"preflight","task_id":first["task_id"]})
        self.assertEqual([(w["task_id"],w["paths"])for w in report["waiting_overlaps"]],[("rb-waiter-w",["docs/shared.md"])])

if __name__=="__main__":unittest.main()
