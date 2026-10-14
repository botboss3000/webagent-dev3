"""Fatal cap evidence and primary packaging error precedence at the host authority boundary."""
import importlib.util,threading,types,unittest,uuid
from pathlib import Path
from unittest.mock import patch
spec=importlib.util.spec_from_file_location("packaging_executor",Path(__file__).resolve().parents[1]/"build-service/executor.py")
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
class PackagingEvidenceTests(unittest.TestCase):
    """Use fixed fake lifecycle collaborators; kernel evidence is checked in disposable probes."""
    def run_case(self, initial=False, completion=False, oom=False, prior=None, warning=False, recovered=None):
        """Drive exact initial/completion proof gates without any Docker or privileged effect."""
        identity="a"*64
        class Resources:
            enabled=True
            lock=threading.RLock()
            rows={}
            def enter(self,name,*args,**kwargs):self.rows[name]={}
            def attached(self,*args):pass
        class Runner:
            job_context=types.SimpleNamespace()
            def command(self,args,**kwargs):return (("WARNING: disposable stderr fixture\n"if warning else "")+identity)if args[1]=="create"else ""
            def resource_request(self,args):
                if args["operation"]=="identity":return recovered if recovered is not None else {"container_id":identity,"state":"created","phase":"package"}
                self.count+=1
                if initial or completion and self.count>1:raise RuntimeError("fixture initial failure"if initial else"fixture completion failure")
                return {"container_id":identity,"memory_max":4026531840,"swap_max":0,"oom_events":int(oom and self.count>1),"oom_kills":int(oom and self.count>1),"peak_memory":123}
        runner=Runner();runner.count=0;executor=module.Executor(runner,Resources(),{"instance":"development"})
        cleanup=[];executor.finish_phase=lambda *args:cleanup.append(args)
        job={"job_id":str(uuid.uuid4()),"package_memory_cap":4026531840}
        if prior is not None:job["package_cap_outcomes"]=[prior.copy()]
        with patch.object(module.subprocess,"run"):
            try:
                with executor.packager(job):pass
            except Exception as error:return error,job,cleanup
        return None,job,cleanup
    def test_primary_initial_error_survives_missing_completion_evidence(self):
        """Do not mislabel a startup failure as a later cap read failure."""
        error,job,cleanup=self.run_case(initial=True)
        self.assertEqual(str(error),"fixture initial failure")
        self.assertEqual([x["stage"]for x in job["package_failures"]],["initial_attestation","completion_attestation"])
        self.assertFalse(cleanup[0][2])
    def test_missing_completion_evidence_is_still_fatal(self):
        """Successful workload cannot certify packaging without final kernel counters."""
        error,job,cleanup=self.run_case(completion=True)
        self.assertIn("completion evidence unavailable",str(error))
        self.assertEqual(job["package_cap_outcomes"][0]["outcome"],"failed")
        self.assertIsNone(job["package_cap_outcomes"][0]["oom_kills"])
        self.assertFalse(cleanup[0][2])
    def test_known_oom_is_distinct_and_fatal(self):
        """Actual positive OOM counters remain a cap failure rather than missing evidence."""
        error,job,cleanup=self.run_case(oom=True)
        self.assertIn("memory cap exceeded",str(error))
        self.assertEqual(job["package_cap_outcomes"][0]["outcome"],"oom")
        self.assertFalse(cleanup[0][2])
    def test_later_role_failure_preserves_successful_receipt(self):
        """A missing initial loop proof must never overwrite the completed API evidence."""
        prior={"role":"api","container_id":"b"*64,"outcome":"completed","oom_kills":0,"peak_memory":456}
        error,job,cleanup=self.run_case(initial=True,prior=prior)
        self.assertEqual(str(error),"fixture initial failure")
        self.assertEqual(job["package_cap_outcomes"],[prior])
    def test_create_diagnostics_do_not_replace_protected_identity(self):
        """Docker diagnostics cannot contaminate the host helper's exact validated ID."""
        error,job,cleanup=self.run_case(warning=True)
        self.assertIsNone(error)
        self.assertEqual(job["package_cap_outcomes"][0]["container_id"],"a"*64)
        self.assertEqual(job["package_cap_outcomes"][0]["outcome"],"completed")
    def test_invalid_protected_creation_identity_never_starts(self):
        """Missing, malformed or unexpected-state identities remain reconciliation failures."""
        for recovered in ({"missing":True},{"container_id":"invalid","state":"created","phase":"package"},{"container_id":"a"*64,"state":"running","phase":"package"},{"container_id":"a"*64,"state":"created","phase":"compile_release"}):
            with self.subTest(recovered=recovered):
                error,job,cleanup=self.run_case(recovered=recovered)
                self.assertIsInstance(error,ValueError)
                self.assertEqual([x["stage"]for x in job["package_failures"]],["container_identity"])
                self.assertNotIn("package_cap_outcomes",job)
                self.assertIsNone(cleanup[0][1])
                self.assertFalse(cleanup[0][2])

class CompilerIdentityTests(unittest.TestCase):
    """Keep compiler creation identity at the existing protected host authority boundary."""
    def run_case(self, reply=None):
        """Model mixed Docker output with independently authenticated helper identity."""
        identity="a"*64
        class Resources:
            enabled=True
            lock=threading.RLock()
            rows={}
            def enter(self,name,*args,**kwargs):
                """Admit only the fixture's attributable phase."""
                self.rows[name]={}
            def attached(self,*args):
                """Retain the same successful-attachment seam as the real controller."""
                pass
        class Runner:
            job_context=types.SimpleNamespace()
            commands=[]
            def command(self,args,**kwargs):
                """Return a warning-bearing create stream without any Docker effect."""
                self.commands.append(args[:2])
                return "WARNING: disposable stderr fixture\n"+identity if args[1]=="create" else "fixture check passed"
            def resource_request(self,args):
                """Keep creation identity independent of kernel counter reads."""
                if args["operation"]=="identity":
                    return reply if reply is not None else {"container_id":identity,"state":"created","phase":"compile_test"}
                return {"container_id":identity,"memory_max":4026531840,"swap_max":0,"oom_events":0,"oom_kills":0,"peak_memory":123}
        runner=Runner();executor=module.Executor(runner,Resources(),{"instance":"development","service_uid":1000,"service_gid":1000})
        cleanup=[];executor.finish_phase=lambda *args:cleanup.append(args)
        job={"job_id":str(uuid.uuid4()),"compiler_memory_cap":4026531840}
        import tempfile
        with tempfile.TemporaryDirectory() as temporary:
            folder=Path(temporary)
            try:
                executor.phase(job,"compile_test","engine",folder,folder,["cargo","check","--locked"],"fixture",30)
            except Exception as error:
                return error,job,runner.commands,cleanup
        return None,job,runner.commands,cleanup
    def test_compiler_diagnostics_do_not_replace_protected_identity(self):
        """Warnings cannot replace canonical authenticated identity or successful cap proof."""
        error,job,commands,cleanup=self.run_case()
        self.assertIsNone(error)
        self.assertIn(["docker","start"],commands)
        self.assertIn(["docker","exec"],commands)
        self.assertEqual(job["compiler_cap_outcomes"][0]["container_id"],"a"*64)
        self.assertEqual(job["compiler_cap_outcomes"][0]["outcome"],"completed")
        self.assertEqual(job["compiler_cap_outcomes"][0]["oom_kills"],0)
        self.assertTrue(cleanup[0][2])
    def test_invalid_compiler_identity_never_starts(self):
        """Missing, malformed and wrong-state/phase identities forbid execution."""
        for reply in ({"missing":True},{"container_id":"invalid","state":"created","phase":"compile_test"},
                      {"container_id":"a"*64,"state":"running","phase":"compile_test"},
                      {"container_id":"a"*64,"state":"created","phase":"package"}):
            with self.subTest(reply=reply):
                error,job,commands,cleanup=self.run_case(reply)
                self.assertIsInstance(error,ValueError)
                self.assertNotIn(["docker","start"],commands)
                self.assertNotIn(["docker","exec"],commands)
                self.assertNotIn("compiler_cap_outcomes",job)
                self.assertIsNone(cleanup[0][1])
                self.assertFalse(cleanup[0][2])
