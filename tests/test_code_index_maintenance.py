"""Maintenance reviews are durable assertions, never scan-derived completion."""
import importlib.util,json,tempfile,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location("ledger",ROOT/"plugins/abilities/Administrator/code_index/maintenance.py")
ledger=importlib.util.module_from_spec(spec);spec.loader.exec_module(ledger)
class MaintenanceTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)/"source";self.root.mkdir()
  (self.root/"one.py").write_text("def first():\n    return 1\n")
  self.folder=Path(self.tmp.name)/"ledger";self.db=ledger.Ledger(self.folder)
  self.actor={"user_id":"reviewer","session_id":"session"};self.counter=0
 def tearDown(self):self.tmp.cleanup()
 def call(self,action,**args):
  self.counter+=1;args.update(action=action,request_id=args.get("request_id","request-"+str(self.counter).zfill(8)))
  return self.db.mutate(args,self.actor,"task-key","test-task",self.root)
 def rows(self):return self.db.list({"action":"list","task_key":"task-key"})["results"]
 def child(self):return next(r for r in self.rows()if r["unit_id"]!="__file__")
 def complete(self,row,claimed=None):
  claimed=claimed or self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])["record"]
  return self.call("record",entry_id=row["entry_id"],expected_revision=claimed["revision"],source_hash=claimed["source_hash"],claim_id=claimed["claim"]["id"],status="complete",configuration_assessment={"status":"not_applicable","rationale":"Fixture function is fixed test behavior"},checks={k:"verified"for k in ledger.CHECKS})["record"]
 def test_sync_discovers_pending_and_survives_restart(self):
  self.call("sync");self.assertEqual(len(self.rows()),2);self.assertTrue(all(r["status"]=="pending"for r in self.rows()))
  self.db=ledger.Ledger(self.folder);self.assertEqual(len(self.rows()),2)
 def test_complete_requires_checks_claim_and_current_revision(self):
  self.call("sync");row=self.child()
  with self.assertRaisesRegex(ValueError,"Claim"):self.call("record",entry_id=row["entry_id"],expected_revision=row["revision"],source_hash=row["source_hash"],status="complete",configuration_assessment={"status":"not_applicable","rationale":"Test fixture"},checks={k:"verified"for k in ledger.CHECKS})
  complete=self.complete(row);self.assertEqual(complete["status"],"complete")
  with self.assertRaisesRegex(ValueError,"revision"):self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])
 def test_scan_never_certifies_and_change_invalidates_preserving_review(self):
  self.call("sync");done=self.complete(self.child());self.call("sync")
  self.assertEqual(self.child()["status"],"complete")
  (self.root/"one.py").write_text("# Changed documentation\ndef first():\n    return 1\n")
  self.call("sync");row=self.child();self.assertEqual(row["status"],"needs_review");self.assertEqual(row["previous_review"]["source_hash"],done["source_hash"]);self.assertEqual(row["start_line"],2)
 def test_stale_source_and_stale_scan_rejected(self):
  self.call("sync");row=self.child();(self.root/"one.py").write_text("def changed(): pass\n")
  with self.assertRaisesRegex(ValueError,"Source changed"):self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])
 def test_deduplication_checks_identical_input_and_survives_restart(self):
  first=self.call("sync",request_id="stable-request")
  self.db=ledger.Ledger(self.folder);self.assertEqual(first,self.call("sync",request_id="stable-request"))
  with self.assertRaisesRegex(ValueError,"reused"):self.call("sync",request_id="stable-request",scope="one.py")
 def test_other_actor_cannot_overwrite_active_claim(self):
  self.call("sync");r=self.child();r=self.call("claim",entry_id=r["entry_id"],expected_revision=r["revision"])["record"]
  with self.assertRaisesRegex(ValueError,"another"):self.db.mutate({"action":"claim","entry_id":r["entry_id"],"expected_revision":r["revision"],"request_id":"other-reviewer"},{"user_id":"other"},"task-key","test-task",self.root)
 def test_file_cannot_complete_before_children(self):
  self.call("sync");parent=next(r for r in self.rows()if r["unit_id"]=="__file__")
  claimed=self.call("claim",entry_id=parent["entry_id"],expected_revision=parent["revision"])["record"];self.parent_claim=claimed["claim"]["id"]
  with self.assertRaisesRegex(ValueError,"child"):self.complete(parent,claimed)
  self.complete(self.child());parent=next(r for r in self.rows()if r["unit_id"]=="__file__");self.assertEqual(self.complete(parent,claimed=self.call("claim",entry_id=parent["entry_id"],expected_revision=parent["revision"],claim_id=self.parent_claim)["record"])["status"],"complete")
 def test_not_applicable_requires_rationale_and_freshness_is_required(self):
  self.call("sync");r=self.call("claim",entry_id=self.child()["entry_id"],expected_revision=self.child()["revision"])["record"]
  for checks in ({k:"not_applicable"for k in ledger.CHECKS},{k:{"status":"not_applicable","rationale":"No setting"}for k in ledger.CHECKS}):
   with self.assertRaises(ValueError):self.call("record",entry_id=r["entry_id"],expected_revision=r["revision"],source_hash=r["source_hash"],status="complete",checks=checks)
 def test_removed_symbol_and_rename_are_not_auto_complete(self):
  self.call("sync");old=self.complete(self.child());(self.root/"one.py").write_text("def renamed(): pass\n");self.call("sync")
  rows=self.rows();gone=next(r for r in rows if r["entry_id"]==old["entry_id"]);self.assertTrue(gone["removed"]);self.assertEqual(gone["status"],"needs_review")
  self.assertTrue(any(r["label"]=="renamed"and r["status"]=="pending"for r in rows))
 def test_release_preserves_concrete_next_steps(self):
  self.call("sync");r=self.call("claim",entry_id=self.child()["entry_id"],expected_revision=self.child()["revision"])["record"]
  self.call("release",entry_id=r["entry_id"],expected_revision=r["revision"],claim_id=r["claim"]["id"],remaining_work=["Trace callers before review"])
  self.assertEqual(self.child()["remaining_work"],["Trace callers before review"])
 def test_private_task_cannot_mark_published_source_complete(self):
  self.call("sync");self.complete(self.child())
  self.assertEqual(self.db.list({"action":"list","task_key":"published"})["total"],0)
 def test_symlinked_source_and_path_escape_rejected(self):
  (self.root/"escape.py").symlink_to("/etc/passwd")
  with self.assertRaises((ValueError,OSError)):self.call("sync")
 def test_unknown_mutation_fields_rejected(self):
  with self.assertRaisesRegex(ValueError,"Unknown"):self.call("sync",reviewer="forged")

 def test_same_account_claim_handle_and_expired_recovery(self):
  from unittest.mock import patch
  self.call("sync");r=self.child();claimed=self.call("claim",entry_id=r["entry_id"],expected_revision=r["revision"])["record"]
  with self.assertRaisesRegex(ValueError,"another"):self.call("claim",entry_id=r["entry_id"],expected_revision=claimed["revision"])
  with patch.object(ledger.time,"time",return_value=claimed["claim"]["expires_at"]+1):
   recovered=self.call("claim",entry_id=r["entry_id"],expected_revision=claimed["revision"])["record"]
  self.assertNotEqual(claimed["claim"]["id"],recovered["claim"]["id"])
  self.assertNotIn("id",self.child()["claim"])
 def test_revocation_before_commit_rolls_back(self):
  count=[0]
  def authorize():
   count[0]+=1
   if count[0]>=4:raise PermissionError("revoked")
  with self.assertRaises(PermissionError):self.db.mutate({"action":"sync","request_id":"revoke-test"},self.actor,"task-key","test-task",self.root,authorize)
  self.assertEqual(self.rows(),[])
 def test_concurrent_independent_claims_and_revision_conflict(self):
  import concurrent.futures
  (self.root/"two.py").write_text("def second(): pass\n");self.call("sync")
  units=[r for r in self.rows()if r["unit_id"]!="__file__"]
  def claim(item):
   i,r=item
   return ledger.Ledger(self.folder).mutate({"action":"claim","entry_id":r["entry_id"],"expected_revision":r["revision"],"request_id":"parallel-"+str(i)},{"user_id":str(i)},"task-key","test-task",self.root)
  with concurrent.futures.ThreadPoolExecutor(max_workers=2)as pool:results=list(pool.map(claim,enumerate(units)))
  self.assertEqual(len(results),2)
  with self.assertRaisesRegex(ValueError,"revision"):self.call("claim",entry_id=units[0]["entry_id"],expected_revision=units[0]["revision"])
 def test_published_projection_seed_and_final_source_invalidation(self):
  import sqlite3,os,shutil,uuid
  self.call("sync");done=self.complete(self.child());scanner=self.db.scanner
  output=Path(self.tmp.name)/"published-index"
  manifest=scanner.build_published(self.root,output,str(uuid.uuid4()),"a"*64,self.db.snapshot(["task-key"]))
  conn=sqlite3.connect(output/"index.sqlite")
  rows=[json.loads(r[0])for r in conn.execute("SELECT body FROM maintenance_records")];conn.close()
  child=next(r for r in rows if r["unit_id"]==done["unit_id"]);self.assertEqual(child["status"],"complete")
  self.db.activate("release-one",rows);self.assertEqual(self.db.list({"task_key":"published","status":"complete"})["total"],1)
  (self.root/"one.py").write_text("# shifted\ndef first():\n    return 1\n")
  output2=Path(self.tmp.name)/"next-index";m=scanner.build_published(self.root,output2,str(uuid.uuid4()),"b"*64,self.db.snapshot([]))
  self.assertGreater(m["maintenance"]["invalidated_reviews"],0);self.assertEqual(m["maintenance"]["documentation_gate"],"informational")
  conn=sqlite3.connect(output2/"index.sqlite");new=[json.loads(r[0])for r in conn.execute("SELECT body FROM maintenance_records")];conn.close()
  self.db.activate("release-two",new);self.assertEqual(self.db.list({"task_key":"published","status":"complete"})["total"],0)
  self.db.activate("release-one",rows);self.assertEqual(self.db.list({"task_key":"published","status":"complete"})["total"],1)
  self.assertEqual(len(self.rows()),2)


 def test_deleted_file_can_be_reconciled_without_old_dependencies(self):
  self.call("sync");old=self.child();(self.root/"one.py").unlink();self.call("sync")
  row=next(r for r in self.rows()if r["entry_id"]==old["entry_id"])
  claimed=self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])["record"]
  result=self.call("record",entry_id=row["entry_id"],expected_revision=claimed["revision"],source_hash=None,claim_id=claimed["claim"]["id"],status="complete",checks={k:{"status":"not_applicable","rationale":"Deleted unit reconciled"}if k!="index_freshness"else"verified"for k in ledger.CHECKS},configuration_assessment={"status":"not_applicable","rationale":"Deleted unit"},reconciliation={"kind":"deleted"})["record"]
  self.assertTrue(result["removed"]);self.assertEqual(result["status"],"complete")


 def rename_review(self):
  self.call("sync");old=self.child();(self.root/"one.py").unlink();(self.root/"two.py").write_text("def renamed(): return 1\n");self.call("sync")
  row=next(r for r in self.rows()if r["entry_id"]==old["entry_id"]);target=next(r for r in self.rows()if r["label"]=="renamed")
  claim=self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])["record"]
  args={"entry_id":row["entry_id"],"expected_revision":claim["revision"],"source_hash":None,"claim_id":claim["claim"]["id"],"status":"complete","checks":{k:"verified"for k in ledger.CHECKS},"configuration_assessment":{"status":"not_applicable","rationale":"Renamed fixture"},"reconciliation":{"kind":"renamed","replacement_entry_id":target["entry_id"]}}
  return args,target
 def test_rename_replacement_must_be_current(self):
  args,target=self.rename_review();(self.root/"two.py").write_text("def renamed(): return 2\n")
  with self.assertRaisesRegex(ValueError,"Source changed"):self.call("record",**args)
 def test_rename_target_is_rebound_and_rebase_invalidates_it(self):
  import sqlite3,uuid
  args,target=self.rename_review();done=self.call("record",**args)["record"];scanner=self.db.scanner
  output=Path(self.tmp.name)/"rename-index";scanner.build_published(self.root,output,str(uuid.uuid4()),"a"*64,self.db.snapshot(["task-key"]))
  conn=sqlite3.connect(output/"index.sqlite");rows=[json.loads(r[0])for r in conn.execute("SELECT body FROM maintenance_records")];conn.close()
  published=next(r for r in rows if r["removed"]and r["unit_id"]==done["unit_id"]);replacement=next(r for r in rows if r["label"]=="renamed")
  self.assertEqual(published["status"],"complete");self.assertEqual(published["reconciliation"]["replacement_entry_id"],replacement["entry_id"]);self.assertEqual(replacement["status"],"pending")
  (self.root/"two.py").write_text("# Rebased comment\ndef renamed(): return 1\n")
  other=Path(self.tmp.name)/"rebased-index";scanner.build_published(self.root,other,str(uuid.uuid4()),"b"*64,self.db.snapshot(["task-key"]))
  conn=sqlite3.connect(other/"index.sqlite");rows=[json.loads(r[0])for r in conn.execute("SELECT body FROM maintenance_records")];conn.close()
  published=next(r for r in rows if r["removed"]and r["unit_id"]==done["unit_id"]);self.assertEqual(published["status"],"needs_review");self.assertTrue(published["remaining_work"])
 def test_unicode_published_identity_matches_live_ledger(self):
  import sqlite3,uuid
  (self.root/"one.py").write_text("def café(): return 1\n");self.call("sync");done=self.complete(self.child());scanner=self.db.scanner;output=Path(self.tmp.name)/"unicode-index"
  scanner.build_published(self.root,output,str(uuid.uuid4()),"a"*64,self.db.snapshot(["task-key"]));conn=sqlite3.connect(output/"index.sqlite");rows=[json.loads(r[0])for r in conn.execute("SELECT body FROM maintenance_records")];conn.close()
  self.db.activate("unicode-release",rows);live=self.db.list({"task_key":"published"})["results"]
  self.assertEqual({r["entry_id"]for r in live},{r["entry_id"]for r in rows})
 def test_configuration_defaults_are_specific_to_overlapping_locations(self):
  first={"version":1,"feature":"fixture-options","aliases":["fixture"],"purpose":"Fixture grouping","span_lines":1,"configuration":{"status":"opportunity","opportunities":["First option"]}}
  second={**first,"configuration":{"status":"opportunity","opportunities":["Second option"]}}
  (self.root/"one.py").write_text("def first(): return 1\ndef second(): return 2\n")
  mapfile=self.root/self.db.scanner.FEATURE_MAP_PATH;mapfile.parent.mkdir(parents=True,exist_ok=True);mapfile.write_text(json.dumps({"version":1,"features":[{"id":first["feature"],"aliases":first["aliases"],"purpose":first["purpose"],"locations":[{"path":"one.py","start_line":1,"end_line":1,"configuration":first["configuration"]},{"path":"one.py","start_line":2,"end_line":2,"configuration":second["configuration"]}]}]}));self.call("sync")
  first_row=next(r for r in self.rows()if r["label"]=="first");second_row=next(r for r in self.rows()if r["label"]=="second");group=next(r for r in self.rows()if r["unit_id"]=="feature:fixture-options")
  self.assertEqual(first_row["configuration_assessment"]["opportunities"],["First option"]);self.assertEqual(second_row["configuration_assessment"]["opportunities"],["Second option"]);self.assertEqual(group["configuration_assessment"]["status"],"unassessed")


 def test_new_task_resumes_unfinished_published_deletion(self):
  self.call("sync");old=self.child();(self.root/"one.py").unlink();self.call("sync");self.db.activate("unfinished-deletion",self.db.snapshot(["task-key"]))
  actor={"user_id":"next-reviewer"}
  self.db.mutate({"action":"sync","scope":"one.py","request_id":"resume-deletion"},actor,"next-key","next-task",self.root)
  rows=self.db.list({"task_key":"next-key"})["results"];r=next(r for r in rows if r["unit_id"]==old["unit_id"]);self.assertTrue(r["removed"]);self.assertEqual(r["status"],"needs_review")
  claim=self.db.mutate({"action":"claim","entry_id":r["entry_id"],"expected_revision":r["revision"],"request_id":"resume-claim"},actor,"next-key","next-task",self.root)["record"]
  done=self.db.mutate({"action":"record","entry_id":r["entry_id"],"expected_revision":claim["revision"],"claim_id":claim["claim"]["id"],"source_hash":None,"status":"complete","checks":{k:"verified"for k in ledger.CHECKS},"configuration_assessment":{"status":"not_applicable","rationale":"Deleted fixture"},"reconciliation":{"kind":"deleted"},"request_id":"resume-review"},actor,"next-key","next-task",self.root)["record"]
  self.assertEqual(done["status"],"complete");self.assertEqual(done["reviewer"],actor)

 def test_completed_published_deletion_resumes_when_evidence_changes(self):
  self.call("sync");old=self.child();(self.root/"one.py").write_text("def renamed(): pass\n");self.call("sync")
  row=next(r for r in self.rows()if r["entry_id"]==old["entry_id"]);claim=self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])["record"]
  done=self.call("record",entry_id=row["entry_id"],expected_revision=claim["revision"],claim_id=claim["claim"]["id"],source_hash=row["source_hash"],status="complete",checks={k:"verified"for k in ledger.CHECKS},configuration_assessment={"status":"not_applicable","rationale":"Deleted fixture"},reconciliation={"kind":"deleted"})["record"]
  self.db.activate("completed-deletion",[done])
  def sync(key):
   self.db.mutate({"action":"sync","scope":"one.py","request_id":"sync-"+key},self.actor,key,key,self.root)
   return self.db.list({"task_key":key})["results"]
  self.assertFalse(any(r["unit_id"]==old["unit_id"]for r in sync("unchanged")))
  changed=dict(done);changed["dependencies"]=[{"path":"one.py","sha256":"a"*64}];self.db.activate("changed-dependency",[changed])
  resumed=next(r for r in sync("dependency")if r["unit_id"]==old["unit_id"])
  self.assertTrue(resumed["removed"]);self.assertEqual(resumed["status"],"needs_review");self.assertEqual(resumed["dependencies"],[])
  (self.root/"one.py").write_text("# shifted\ndef renamed(): pass\n")
  resumed=next(r for r in sync("source")if r["unit_id"]==old["unit_id"]);self.assertEqual(resumed["status"],"needs_review")
  (self.root/"one.py").unlink()
  resumed=next(r for r in sync("absent")if r["unit_id"]==old["unit_id"]);self.assertIsNone(resumed["source_hash"]);self.assertEqual(resumed["status"],"needs_review")
  absent=dict(done,source_hash=None,dependencies=[]);self.db.activate("unchanged-absent",[absent])
  self.assertFalse(any(r["unit_id"]==old["unit_id"]for r in sync("absent-valid")))

if __name__=="__main__":unittest.main()

class LedgerCapacityTests(unittest.TestCase):
 def test_oversized_transaction_rolls_back_and_keeps_the_store_readable(self):
  with tempfile.TemporaryDirectory()as temporary:
   db=ledger.Ledger(Path(temporary)/"ledger",max_bytes=64*1024)
   with db.connect()as conn:
    conn.execute("CREATE TABLE capacity_probe(value BLOB)")
   with self.assertRaisesRegex(ValueError,"storage budget"):
    with db.connect()as conn:
     conn.execute("INSERT INTO capacity_probe VALUES (?)",(b"x"*(128*1024),))
   with db.connect()as conn:
    self.assertEqual(conn.execute("SELECT COUNT(*) FROM capacity_probe").fetchone()[0],0)
   self.assertLessEqual(db.path.stat().st_size,db.max_bytes)
 def test_constructor_budget_remains_bounded_and_the_default_preserves_growth(self):
  with tempfile.TemporaryDirectory()as temporary:
   for value in (True,4095,1024*1024*1024+1):
    with self.assertRaises(ValueError):ledger.Ledger(Path(temporary)/"invalid",max_bytes=value)
   db=ledger.Ledger(Path(temporary)/"ledger")
   self.assertEqual(db.max_bytes,256*1024*1024)

class StableReviewTests(unittest.TestCase):
 setUp=MaintenanceTests.setUp
 tearDown=MaintenanceTests.tearDown
 call=MaintenanceTests.call
 child=MaintenanceTests.child
 rows=MaintenanceTests.rows
 complete=MaintenanceTests.complete
 def test_unchanged_sync_keeps_claim_and_review_revision(self):
  self.call("sync");row=self.child()
  claimed=self.call("claim",entry_id=row["entry_id"],expected_revision=row["revision"])["record"]
  self.call("sync");self.assertEqual(self.child()["revision"],claimed["revision"])
  done=self.complete(row,claimed);self.call("sync")
  self.assertEqual(self.child()["revision"],done["revision"])

class LosslessHistoryTests(unittest.TestCase):
 setUp=MaintenanceTests.setUp
 tearDown=MaintenanceTests.tearDown
 call=MaintenanceTests.call
 rows=MaintenanceTests.rows
 child=MaintenanceTests.child
 def test_compaction_preserves_every_event_current_record_and_retry_receipt(self):
  self.call("sync",request_id="original-sync-receipt")
  with self.db.connect()as conn:
   event=conn.execute("SELECT entry_id,at,body FROM events LIMIT 1").fetchone()
   body=ledger.canonical(ledger.decode_event(event[2]))
   conn.execute("UPDATE events SET body=?",(body,))
   conn.executemany("INSERT INTO events(entry_id,at,body)VALUES(?,?,?)",[(event[0],event[1],body)]*300)
   records=[tuple(r)for r in conn.execute("SELECT * FROM records ORDER BY entry_id")]
   receipts=[tuple(r)for r in conn.execute("SELECT * FROM receipts ORDER BY actor,request_id")]
   expected=[ledger.decode_event(r[0])for r in conn.execute("SELECT body FROM events ORDER BY id")]
  report=self.db.compact_history()
  self.assertTrue(report["history_preserved"]);self.assertLess(report["bytes_after"],report["bytes_before"])
  self.assertTrue((self.folder/report["backup"]).is_file())
  with self.db.connect()as conn:
   self.assertEqual(records,[tuple(r)for r in conn.execute("SELECT * FROM records ORDER BY entry_id")])
   self.assertEqual(receipts,[tuple(r)for r in conn.execute("SELECT * FROM receipts ORDER BY actor,request_id")])
   self.assertEqual(expected,[ledger.decode_event(r[0])for r in conn.execute("SELECT body FROM events ORDER BY id")])
  self.assertEqual(self.call("sync",request_id="original-sync-receipt")["scan"]["index_sha256"],self.call("sync",request_id="original-sync-receipt")["scan"]["index_sha256"])
  entry=self.child();self.assertTrue(self.db.list({"action":"get","entry_id":entry["entry_id"]})["results"][0]["history"])
 def test_history_decoder_accepts_legacy_and_rejects_trailing_or_oversized_frames(self):
  import zlib
  value={"event":"fixture","record":{"revision":1}}
  packed=ledger.encode_event(value)
  self.assertEqual(ledger.decode_event(packed),value)
  self.assertEqual(ledger.decode_event(ledger.canonical(value)),value)
  for invalid in [packed[:-1],packed+b"trailing",b"WAE1"+zlib.compress(b"x"*(4*1024*1024+1)),b"unknown"]:
   with self.assertRaises((ValueError,zlib.error)):ledger.decode_event(invalid)
 def test_history_compaction_checks_authority_before_any_backup(self):
  def denied():raise PermissionError("revoked")
  with self.assertRaises(PermissionError):self.db.compact_history(denied)
  self.assertFalse(list(self.folder.glob("history-backup-*")))


class ScopedInventoryTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)/"source";self.root.mkdir();self.cache=Path(self.tmp.name)/"cache";self.scanner=ledger.load_scanner()
  self.write("wanted.py","def wanted(): return 1\n");self.write("other.py","def other(): return 2\n");self.write("config/settings.json",'{"value":1}')
  feature={"id":"wanted-feature","aliases":["wanted"],"purpose":"Fixture feature","locations":[{"path":"wanted.py","start_line":1,"end_line":1,"configuration":{"status":"configured","references":[{"path":"config/settings.json","pointers":["/value"]}]}}]}
  self.map={"version":1,"features":[feature]};self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map))
 def tearDown(self):self.tmp.cleanup()
 def write(self,path,body):
  target=self.root/path;target.parent.mkdir(parents=True,exist_ok=True);target.write_text(body)
 def scan(self,scope="wanted.py",cache=None):
  return ledger.source_inventory(self.scanner,self.root,scope,cache)
 def test_scope_matches_full_units_and_dependencies_without_parsing_unrelated(self):
  from unittest.mock import patch
  full,full_units,_=self.scan("")
  with patch.object(self.scanner,"scan_data",wraps=self.scanner.scan_data)as parse:
   files,units,info=self.scan()
  self.assertEqual([x["path"]for x in files],["wanted.py"]);self.assertEqual(units["wanted.py"],full_units["wanted.py"])
  self.assertEqual(files[0]["dependencies"],next(x for x in full if x["path"]=="wanted.py")["dependencies"])
  self.assertEqual({c.args[0]for c in parse.call_args_list},{"wanted.py","config/settings.json"})
  self.assertEqual(info["freshness_scope"],"wanted.py");self.assertEqual(info["gaps"],0)
 def test_cache_reuse_and_config_hash_refresh(self):
  first,_,_=self.scan(cache=self.cache)
  files,_,info=self.scan(cache=self.cache);self.assertEqual(info["parsed_files"],0);self.assertGreater(info["reused_files"],0)
  self.write("config/settings.json",'{"value":2}')
  changed,_,_=self.scan(cache=self.cache);self.assertNotEqual(first[0]["dependencies"],changed[0]["dependencies"])
  self.assertEqual(first[0]["sha256"],changed[0]["sha256"])
 def test_every_map_target_is_validated_even_outside_scope(self):
  self.map["features"][0]["locations"].append({"path":"other.py","start_line":50,"end_line":50})
  self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map))
  with self.assertRaisesRegex(ValueError,"feature map target is stale"):self.scan()
 def test_unsupported_map_targets_and_symlinks_still_reject(self):
  self.write("note.md","note");self.map["features"][0]["locations"][0]["path"]="note.md";self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map))
  with self.assertRaisesRegex(ValueError,"unindexed"):self.scan()
  self.map["features"][0]["locations"][0]["path"]="wanted.py";self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map))
  (self.root/"wanted.py").unlink();(self.root/"wanted.py").symlink_to("other.py")
  with self.assertRaises((ValueError,OSError)):self.scan()
 def test_dependency_race_does_not_replace_saved_seed(self):
  from unittest.mock import patch
  self.scan(cache=self.cache);before=(self.cache/"index.sqlite").read_bytes();read=self.scanner.read_source;calls=0
  def raced(fd,path):
   nonlocal calls
   if path=="config/settings.json":
    calls+=1
    if calls==2:self.write(path,'{"value":3}')
   return read(fd,path)
  with patch.object(self.scanner,"read_source",side_effect=raced),self.assertRaisesRegex(ValueError,"dependency changed"):self.scan(cache=self.cache)
  self.assertEqual((self.cache/"index.sqlite").read_bytes(),before)
 def test_deleted_scope_reconciles_without_certifying_unrelated_source(self):
  self.scan(cache=self.cache);(self.root/"wanted.py").unlink();self.map["features"]=[];self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map))
  files,units,info=self.scan(cache=self.cache);self.assertEqual(files,[]);self.assertEqual(units,{})
  self.assertEqual(info["unindexed_count"],0);self.assertEqual(info["freshness_scope"],"wanted.py")
 def test_markers_and_empty_files_are_preserved(self):
  self.map["features"]=[];self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map))
  self.write("wanted.py",'# @code-index {"version":1,"feature":"inline","aliases":["inline"],"purpose":"Fixture marker","span_lines":1}\ndef wanted(): return 1\n')
  _,units,_=self.scan();self.assertTrue(any(u["unit_id"]=="feature:inline"for u in units["wanted.py"]))
  self.write("wanted.py","");files,units,info=self.scan();self.assertEqual(files[0]["line_count"],0);self.assertEqual(info["gaps"],0)
 def test_runtime_configuration_reference_never_reads_excluded_data(self):
  from unittest.mock import patch
  self.map["features"][0]["locations"][0]["configuration"]["references"]=[{"path":"data/private.json","pointers":["/value"]}]
  self.write(self.scanner.FEATURE_MAP_PATH,json.dumps(self.map));self.write("data/private.json",'{"value":"private"}')
  with patch.object(self.scanner,"read_source",wraps=self.scanner.read_source)as read:
   files,_,_=self.scan()
  self.assertFalse(any(c.args[1]=="data/private.json"for c in read.call_args_list))
  self.assertFalse(any(d["path"].startswith("data/")for d in files[0]["dependencies"]))
 def test_deadline_and_dependency_byte_limits_preserve_saved_seed(self):
  from unittest.mock import patch
  self.scan(cache=self.cache);before=(self.cache/"index.sqlite").read_bytes()
  with patch.object(ledger.time,"monotonic",side_effect=[0,31]),self.assertRaisesRegex(ValueError,"deadline"):
   self.scan(cache=self.cache)
  self.assertEqual((self.cache/"index.sqlite").read_bytes(),before)
  with patch.object(self.scanner,"MAX_TOTAL_BYTES",1),self.assertRaisesRegex(ValueError,"bounds"):self.scan(cache=self.cache)
  self.assertEqual((self.cache/"index.sqlite").read_bytes(),before)
 def test_scoped_cache_still_requires_complete_published_freshness(self):
  import shutil,uuid
  self.scan(cache=self.cache)
  candidate=Path(self.tmp.name)/"candidate";shutil.copytree(self.root,candidate/"v3")
  manifest=self.scanner.build_published(candidate,candidate/".code-index",str(uuid.uuid4()),"a"*64,private_seeds=[self.cache])
  self.assertEqual(manifest["gaps"],0);self.assertEqual(manifest["stale_count"],0);self.assertEqual(manifest["unindexed_count"],0)
  self.assertGreaterEqual(manifest["indexing"]["reused_files"],1)
  # The unscanned file must be parsed and validated during full publication.
  self.assertGreaterEqual(manifest["indexing"]["parsed_files"],1)
