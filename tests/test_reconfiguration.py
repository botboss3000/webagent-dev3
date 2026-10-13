import importlib.util,unittest
from pathlib import Path
path=Path(__file__).resolve().parents[1]/"build-service/reconfigure.py"
spec=importlib.util.spec_from_file_location("test_reconfigure",path);reconfigure=importlib.util.module_from_spec(spec);spec.loader.exec_module(reconfigure)
class Tests(unittest.TestCase):
 def test_port_replaces_only_api_without_compilation(self):
  old={"port":8081,"bind_address":"127.0.0.1","public_origin":"https://dev.example","checkout_root":"/source"}
  result=reconfigure.plan(old,{**old,"port":8083})
  self.assertEqual(result["services"],["api"]);self.assertFalse(result["compile_required"])
  self.assertEqual(reconfigure.plan(old,{**old,"public_origin":"https://other.example"})["services"],["api","loop"])
  self.assertEqual(reconfigure.plan(old,old)["services"],[])
 def test_storage_and_identity_changes_are_rejected_before_effects(self):
  old={"port":8081,"checkout_root":"/source"}
  with self.assertRaises(ValueError):reconfigure.plan(old,{**old,"checkout_root":"/elsewhere"})
 def test_unreconciled_receipt_blocks_a_new_operation(self):
  import tempfile,json
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);path=root/"reconfiguration/receipt/receipt.json";path.parent.mkdir(parents=True)
   path.write_text(json.dumps({"status":"reconfiguring"}))
   with self.assertRaises(RuntimeError):reconfigure.ensure_reconciled(root)
   path.write_text(json.dumps({"status":"rolled_back"}));reconfigure.ensure_reconciled(root)
if __name__=="__main__":unittest.main()

class BootstrapMigrationTests(unittest.TestCase):
 def test_new_bridge_can_be_absent_only_from_previous_live_checkout(self):
  import tempfile
  from unittest.mock import patch
  path=Path(__file__).resolve().parents[1]/"build-service/install.py"
  spec=importlib.util.spec_from_file_location("migration_installer",path);installer=importlib.util.module_from_spec(spec);spec.loader.exec_module(installer)
  with tempfile.TemporaryDirectory()as temporary,patch.object(installer,"V3",Path(temporary)),patch.object(installer,"root_only",lambda:None):
   self.assertTrue(installer.snapshot_context(("v3/ability_plugin_worker.py",)))
   with self.assertRaises(FileNotFoundError):installer.snapshot_context(("v3/worker.py",))

class ListenerPreflightTests(unittest.TestCase):
 def test_active_listener_is_rejected(self):
  """A live listener must block replacement even when address reuse is enabled."""
  import socket
  with socket.socket()as listener:
   listener.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
   listener.bind(("127.0.0.1",0));listener.listen()
   with self.assertRaises(OSError):reconfigure.probe_listener(*listener.getsockname())
 def test_closed_listener_time_wait_does_not_block_return_to_original_port(self):
  """An accepted connection outlives a closed listener without reserving that service."""
  import socket
  with socket.socket()as listener:
   listener.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
   listener.bind(("127.0.0.1",0));address=listener.getsockname();listener.listen()
   with socket.create_connection(address,timeout=2)as client:
    accepted,_=listener.accept();accepted.close()
    self.assertEqual(client.recv(1),b"")
  reconfigure.probe_listener(*address)
