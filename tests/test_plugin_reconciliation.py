"""Operator continuation verifies effects without repeating activation."""
import tempfile,types,unittest,uuid,time
from pathlib import Path
from unittest.mock import Mock
import test_admin_build as fixtures
class ReconciliationTests(unittest.TestCase):
 def setUp(self):
  self.temporary=tempfile.TemporaryDirectory();self.root=Path(self.temporary.name)
  self.broker=object.__new__(fixtures.host.Broker);self.broker.state=self.root
  self.broker.runner=Mock();self.runner=self.broker.runner
  self.broker._phase=lambda job,phase:job.update(phase=phase)
  self.broker._save=Mock();self.broker._start_next=Mock()
  self.broker._sync_report=Mock(return_value={"status":"synchronized"})
  self.blob=fixtures.context();self.broker._current_release_blob=lambda:self.blob
  ident=str(uuid.uuid4());(self.root/ident).mkdir();(self.root/ident/"candidate.tar").write_bytes(self.blob)
  self.job={"job_id":ident,"inputs_sha256":fixtures.host.context_digest(self.blob)[0],
   "operation":"plugin_deploy","status":"uncertain","phase":"checking_ui",
   "hot_assets":True,"effects_started":True,"images":{"api":"a","loop":"b"},
   "consumer_before":{"api":{"id":"api-id"},"loop":{"id":"loop-id"}}}
  self.job.update(user_id="codex-local",workspace="testspace",access_fingerprint="f"*64,lease_until=0)
  self.trusted={"user_id":"codex-local","local_caller":True,"lease_until":time.time()+60,"access_fingerprint":"f"*64}
  self.runner.current_asset_id.return_value=ident;self.runner.healthy.return_value=True
  self.runner.inspect.side_effect=lambda name:{"Id":"api-id" if name==fixtures.host.TARGETS[0] else "loop-id"}
  self.runner.verify_plugin_adoption.return_value={"api":{},"loop":{}}
 def tearDown(self):self.temporary.cleanup()
 def test_verified_publication_promotes_source_without_replaying_effects(self):
  result=self.broker._reconcile_plugin_publication(self.job,self.trusted,"testspace")
  self.assertEqual(result["status"],"completed");self.assertFalse(result["effects_replayed"])
  self.runner.promote_checkout.assert_called_once()
  self.runner.activate_assets.assert_not_called();self.runner.build.assert_not_called();self.runner.stage_assets.assert_not_called()
  self.assertGreater(self.job["lease_until"],time.time())
 def test_expired_or_revoked_authority_cannot_authorize_source_promotion(self):
  self.trusted["lease_until"]=time.time()-1
  with self.assertRaises(ValueError):self.broker._reconcile_plugin_publication(self.job,self.trusted,"testspace")
  self.trusted["lease_until"]=time.time()+60;self.job["revoked"]=True
  with self.assertRaises(PermissionError):self.broker._reconcile_plugin_publication(self.job,self.trusted,"testspace")
  self.runner.promote_checkout.assert_not_called()
 def test_pointer_candidate_identity_or_adoption_mismatch_never_promotes(self):
  self.temporary.cleanup()
  for kind in ("pointer","candidate","identity","adoption"):
   with self.subTest(kind=kind):
    self.setUp()
    if kind=="pointer":self.runner.current_asset_id.return_value=str(uuid.uuid4())
    if kind=="candidate":self.job["inputs_sha256"]="0"*64
    if kind=="identity":self.runner.inspect.side_effect=lambda name:{"Id":"changed"}
    if kind=="adoption":self.runner.verify_plugin_adoption.side_effect=RuntimeError("not adopted")
    with self.assertRaises((ValueError,RuntimeError)):self.broker._reconcile_plugin_publication(self.job,self.trusted,"testspace")
    self.runner.promote_checkout.assert_not_called()
    self.temporary.cleanup()
class AdoptionProbeTests(unittest.TestCase):
 def test_ready_response_and_both_receipts_are_required_with_bounded_timeout(self):
  from unittest.mock import patch
  runner=fixtures.host.Runner();job=str(uuid.uuid4());receipt={r:{"revision":job,"catalog_version":"catalog"}for r in ("api","loop")}
  runner.command=Mock(return_value=fixtures.json.dumps(receipt))
  response=Mock(status=200);response.read.return_value=b'{"catalog_version":"catalog"}'
  connection=Mock();connection.getresponse.return_value=response
  with patch.object(fixtures.host.http.client,"HTTPConnection",return_value=connection)as connect:
   self.assertEqual(runner.verify_plugin_adoption(job),receipt)
  self.assertEqual(connect.call_args.kwargs["timeout"],15)
  self.assertTrue(runner.command.call_args.kwargs["private_raw"])
  connection.close.assert_called_once()
 def test_structured_role_receipts_survive_environment_value_scrubbing(self):
  import json
  runner=fixtures.host.Runner();runner.secrets=["loop"]
  receipt={"api":{"revision":"test","catalog_version":"catalog"},"loop":{"revision":"test","catalog_version":"catalog"}}
  program="print("+repr(json.dumps(receipt))+")"
  argv=["python3","-B","-c",program]
  self.assertIn("[redacted]",runner.command(argv))
  self.assertEqual(json.loads(runner.command(argv,private_raw=True)),receipt)
if __name__=="__main__":unittest.main()
